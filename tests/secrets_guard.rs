//! Guards of `hoist secrets exec` (spec §15.7.2): where decryption may run at all.

use hoist::secrets::guard::{Ci, check};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TAG: &str = "refs/tags/site-v1.0.0";

/// A GitHub push to main of acme/site, as the runner sets it.
fn github(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let base = [
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/main"),
    ];
    env(&base, extra)
}

fn circleci(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let base = [
        ("CI", "true"),
        ("CIRCLECI", "true"),
        ("CIRCLE_PROJECT_REPONAME", "site"),
        ("CIRCLE_BRANCH", "main"),
    ];
    env(&base, extra)
}

/// `base` with `extra` applied; an empty value removes the variable.
fn env(base: &[(&str, &str)], extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = base
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (k, v) in extra {
        match v.is_empty() {
            true => map.remove(*k),
            false => map.insert(k.to_string(), v.to_string()),
        };
    }
    map
}

fn payload() -> Value {
    json!({ "repository": { "default_branch": "main", "id": 7 } })
}

/// A workflow_run payload whose triggering run's head repository is `head`.
fn run_payload(head: Value) -> Value {
    json!({ "repository": { "default_branch": "main", "id": 7 },
            "workflow_run": { "head_repository": head } })
}

fn run(env: &BTreeMap<String, String>, event: Option<&Value>) -> Result<Ci, String> {
    check(&|name| env.get(name).cloned(), event)
}

#[test]
fn allowed_runs() {
    let ok = |env: BTreeMap<String, String>, event: Value| run(&env, Some(&event));
    assert_eq!(ok(github(&[]), payload()), Ok(Ci::Github));
    assert_eq!(
        ok(
            github(&[("GITHUB_EVENT_NAME", "workflow_dispatch")]),
            payload()
        ),
        Ok(Ci::Github)
    );
    // A real schedule payload has no `repository`; a schedule runs on the default branch.
    let schedule = github(&[("GITHUB_EVENT_NAME", "schedule")]);
    assert_eq!(
        ok(schedule, json!({ "schedule": "0 4 * * 1" })),
        Ok(Ci::Github)
    );
    assert_eq!(
        ok(github(&[("GITHUB_REF", TAG)]), payload()),
        Ok(Ci::Github)
    );
    let release = github(&[("GITHUB_EVENT_NAME", "release"), ("GITHUB_REF", TAG)]);
    assert_eq!(ok(release, payload()), Ok(Ci::Github));
    let wr = github(&[("GITHUB_EVENT_NAME", "workflow_run")]);
    let same = run_payload(json!({ "id": 7, "full_name": "Acme/Site" }));
    assert_eq!(ok(wr, same), Ok(Ci::Github));
    assert_eq!(run(&circleci(&[]), None), Ok(Ci::Circleci));
}

#[test]
fn refused_runs_say_why() {
    let p = || Some(payload());
    let wr = |extra: &[(&str, &str)]| {
        let mut e = github(&[("GITHUB_EVENT_NAME", "workflow_run")]);
        e.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        e
    };
    let cases: Vec<(BTreeMap<String, String>, Option<Value>, &str)> = vec![
        (github(&[("CI", "")]), p(), "only in CI"),
        (github(&[("CI", "false")]), p(), "only in CI"),
        (github(&[("CIRCLECI", "true")]), p(), "exactly one of"),
        (github(&[("GITHUB_ACTIONS", "")]), p(), "exactly one of"),
        (github(&[("CLAUDECODE", "1")]), p(), "owner- or CI-only"),
        (
            github(&[("CLAUDE_CODE_REMOTE", "true")]),
            p(),
            "owner- or CI-only",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "pull_request")]),
            p(),
            "event `pull_request`",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "pull_request_target")]),
            p(),
            "event",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "merge_group")]),
            p(),
            "event",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "issue_comment")]),
            p(),
            "event",
        ),
        (github(&[("GITHUB_EVENT_NAME", "")]), p(), "event"),
        (
            github(&[("GITHUB_REF", "refs/heads/agent/x")]),
            p(),
            "not the default branch",
        ),
        (
            github(&[("GITHUB_REF", "refs/heads/main/x")]),
            p(),
            "not the default branch",
        ),
        (
            github(&[("GITHUB_REF", "refs/tags/my-very-bad")]),
            p(),
            "not the default branch",
        ),
        (
            github(&[("GITHUB_REF", "refs/tags/v2.0.0")]),
            p(),
            "not the default branch",
        ),
        (
            github(&[("GITHUB_REF", "refs/tags/--v1")]),
            p(),
            "not the default branch",
        ),
        (
            github(&[
                ("GITHUB_EVENT_NAME", "workflow_dispatch"),
                ("GITHUB_REF", TAG),
            ]),
            p(),
            "only for push",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "schedule"), ("GITHUB_REF", TAG)]),
            p(),
            "only for push",
        ),
        (wr(&[("GITHUB_REF", TAG)]), p(), "only for push"),
        (github(&[]), None, "default branch is unknown"),
        (github(&[]), Some(json!({})), "default branch is unknown"),
        (
            wr(&[]),
            Some(run_payload(json!({ "id": 9, "full_name": "acme/site" }))),
            "head repository",
        ),
        (wr(&[]), Some(run_payload(Value::Null)), "head repository"),
        (wr(&[]), p(), "head repository"),
        (circleci(&[("CIRCLE_PR_REPONAME", "fork")]), None, "fork"),
        (circleci(&[("CIRCLE_PR_REPONAME", "site")]), None, "fork"),
        (circleci(&[("CIRCLE_PR_USERNAME", "evil")]), None, "fork"),
        (
            circleci(&[("CIRCLE_BRANCH", "agent/x")]),
            None,
            "not the default branch",
        ),
        (
            circleci(&[("CIRCLE_BRANCH", ""), ("CIRCLE_TAG", "site-v1.0.0")]),
            None,
            "not the default",
        ),
        (circleci(&[("CLAUDECODE", "1")]), None, "owner- or CI-only"),
    ];
    for (env, event, want) in cases {
        let err = run(&env, event.as_ref()).expect_err(want);
        assert!(err.contains(want), "{want}: {err}");
        assert!(err.starts_with("secrets: "), "{err}");
    }
}
