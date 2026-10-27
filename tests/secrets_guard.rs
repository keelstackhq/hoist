//! Guards of `hoist secrets exec` (spec §15.7.2): where decryption may run at all.

use hoist::secrets::guard::{Ci, check};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A GitHub push to main of acme/site, as the runner sets it.
fn github(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let base = [
        ("CI", "true"),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_REPOSITORY", "acme/site"),
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
    json!({ "repository": { "default_branch": "main" } })
}

fn run(env: &BTreeMap<String, String>, event: Option<&Value>) -> Result<Ci, String> {
    check(&|name| env.get(name).cloned(), event)
}

#[test]
fn allowed_runs() {
    assert_eq!(run(&github(&[]), Some(&payload())), Ok(Ci::Github));
    for event in ["workflow_dispatch", "schedule"] {
        let env = github(&[("GITHUB_EVENT_NAME", event)]);
        assert_eq!(run(&env, Some(&payload())), Ok(Ci::Github), "{event}");
    }
    let release = github(&[
        ("GITHUB_EVENT_NAME", "release"),
        ("GITHUB_REF", "refs/tags/site-v1.2.0"),
    ]);
    assert_eq!(run(&release, Some(&payload())), Ok(Ci::Github));
    let tag = github(&[("GITHUB_REF", "refs/tags/v2.0.0")]);
    assert_eq!(run(&tag, Some(&payload())), Ok(Ci::Github));
    let same = json!({ "repository": { "default_branch": "main" },
                       "workflow_run": { "head_repository": { "full_name": "acme/site" } } });
    let wr = github(&[("GITHUB_EVENT_NAME", "workflow_run")]);
    assert_eq!(run(&wr, Some(&same)), Ok(Ci::Github));
    assert_eq!(run(&circleci(&[]), None), Ok(Ci::Circleci));
    let tagged = circleci(&[("CIRCLE_BRANCH", ""), ("CIRCLE_TAG", "site-v1.0.0")]);
    assert_eq!(run(&tagged, None), Ok(Ci::Circleci));
}

#[test]
fn refused_runs_say_why() {
    let fork = json!({ "repository": { "default_branch": "main" },
                       "workflow_run": { "head_repository": { "full_name": "evil/site" } } });
    let wr = github(&[("GITHUB_EVENT_NAME", "workflow_run")]);
    let cases: Vec<(BTreeMap<String, String>, Option<Value>, &str)> = vec![
        (github(&[("CI", "")]), Some(payload()), "only in CI"),
        (github(&[("CI", "false")]), Some(payload()), "only in CI"),
        (
            github(&[("CIRCLECI", "true")]),
            Some(payload()),
            "exactly one of",
        ),
        (
            github(&[("GITHUB_ACTIONS", "")]),
            Some(payload()),
            "exactly one of",
        ),
        (
            github(&[("CLAUDECODE", "1")]),
            Some(payload()),
            "owner- or CI-only",
        ),
        (
            github(&[("CLAUDE_CODE_REMOTE", "true")]),
            Some(payload()),
            "owner- or CI-only",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "pull_request")]),
            Some(payload()),
            "event `pull_request`",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "pull_request_target")]),
            Some(payload()),
            "event",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "merge_group")]),
            Some(payload()),
            "event",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "issue_comment")]),
            Some(payload()),
            "event",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "")]),
            Some(payload()),
            "event",
        ),
        (
            github(&[("GITHUB_REF", "refs/heads/agent/x")]),
            Some(payload()),
            "not the default branch",
        ),
        (
            github(&[("GITHUB_REF", "refs/tags/my-very-bad")]),
            Some(payload()),
            "not the default branch",
        ),
        (github(&[]), None, "default branch is unknown"),
        (github(&[]), Some(json!({})), "default branch is unknown"),
        (wr.clone(), Some(fork), "head repository"),
        (wr, Some(payload()), "head repository"),
        (circleci(&[("CIRCLE_PR_REPONAME", "fork")]), None, "fork"),
        (
            circleci(&[("CIRCLE_BRANCH", "agent/x")]),
            None,
            "not the default branch",
        ),
        (
            circleci(&[("CIRCLE_BRANCH", ""), ("CIRCLE_TAG", "nope")]),
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
