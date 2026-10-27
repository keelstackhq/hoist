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
    check(&|name| env.get(name).cloned(), event, Some(BUILD))
}

const BUILD: &str = ".github/workflows/build.yml";

/// A workflow_run payload for a same-repo run of `event` from workflow file `path`.
fn triggered(event: &str, path: &str) -> Value {
    let mut p = triggered_value(event.into());
    p["workflow_run"]["path"] = path.into();
    p
}

/// A same-repo workflow_run payload whose `workflow_run.event` is `event`, of any JSON type.
fn triggered_value(event: Value) -> Value {
    let mut p = run_payload(json!({ "id": 7 }));
    p["workflow_run"]["event"] = event;
    p
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
    for trigger in ["push", "pull_request"] {
        let same = triggered(trigger, BUILD);
        assert_eq!(ok(wr.clone(), same), Ok(Ci::Github));
    }
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
            "event \"pull_request\"",
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
        (github(&[]), None, "missing or empty"),
        (github(&[]), Some(json!({})), "missing or empty"),
        (
            wr(&[]),
            Some(run_payload(json!({ "id": 9, "full_name": "acme/site" }))),
            "head repository",
        ),
        (wr(&[]), Some(run_payload(Value::Null)), "head repository"),
        (wr(&[]), p(), "head repository"),
        (
            wr(&[]),
            Some(triggered("issue_comment", "x.yml")),
            "triggered by \"issue_comment\"",
        ),
        (wr(&[]), Some(triggered("", "x.yml")), "triggered by"),
        (
            github(&[]),
            Some(json!({ "repository": { "default_branch": "" } })),
            "missing or empty",
        ),
        (
            github(&[
                ("GITHUB_EVENT_NAME", "schedule"),
                ("GITHUB_REF", "refs/heads/"),
            ]),
            Some(json!({})),
            "missing or empty",
        ),
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
            "tag",
        ),
        (circleci(&[("CIRCLE_TAG", "site-v1.0.0")]), None, "tag"),
        (circleci(&[("CLAUDECODE", "1")]), None, "owner- or CI-only"),
        (wr(&[]), Some(triggered_value(json!(7))), "triggered by"),
        (
            wr(&[]),
            Some(run_payload(json!({ "id": 7 }))),
            "triggered by",
        ),
        (
            github(&[]),
            Some(json!({ "repository": { "default_branch": 1 } })),
            "missing or empty",
        ),
        (
            github(&[("GITHUB_EVENT_NAME", "schedule"), ("GITHUB_REF", TAG)]),
            None,
            "missing or empty",
        ),
    ];
    for (env, event, want) in cases {
        let err = run(&env, event.as_ref()).expect_err(want);
        assert!(err.contains(want), "{want}: {err}");
        assert!(err.starts_with("secrets: "), "{err}");
    }
}

#[test]
fn workflow_run_needs_its_trigger_workflow_path() {
    let env = github(&[("GITHUB_EVENT_NAME", "workflow_run")]);
    let at = |event: &Value, path| check(&|n| env.get(n).cloned(), Some(event), path);
    let event = triggered("pull_request", BUILD);
    assert_eq!(at(&event, Some(BUILD)), Ok(Ci::Github));
    let other = at(&event, Some(".github/workflows/other.yml")).unwrap_err();
    assert!(
        other.contains("not \".github/workflows/other.yml\""),
        "{other}"
    );
    let mut no_path = event.clone();
    for (event, path) in [(&event, None), (&event, Some(""))] {
        let err = at(event, path).unwrap_err();
        assert!(err.contains("--workflow-run-path"), "{path:?}: {err}");
    }
    for path in [Value::Null, json!(7)] {
        no_path["workflow_run"]["path"] = path;
        let err = at(&no_path, Some(BUILD)).unwrap_err();
        assert!(err.contains("workflow is \"\""), "{err}");
    }
    let mut evil = event.clone();
    evil["workflow_run"]["path"] = "x.yml\n::add-mask::y".into();
    let err = at(&evil, Some("x.yml")).unwrap_err();
    assert!(
        !err.contains('\n') && err.contains("\\n::add-mask"),
        "{err}"
    );
    // Only a workflow_run has a trigger workflow: a push needs no path.
    let push = github(&[]);
    let ok = check(&|n| push.get(n).cloned(), Some(&payload()), None);
    assert_eq!(ok, Ok(Ci::Github));
    let evil_ref = github(&[("GITHUB_REF", "refs/heads/x\n::error::y")]);
    let err = run(&evil_ref, Some(&payload())).unwrap_err();
    assert!(!err.contains('\n'), "{err}");
}

/// Accepted release tags are a subset of the owner's `*-v*` tag ruleset (GitHub's `*` never
/// matches `/`), so a tag nobody but the owner can create is the only tag that decrypts.
#[test]
fn accepted_release_tags_are_inside_the_tag_ruleset() {
    let candidates = [
        "cli-v1.2.0",
        "my.tool_x-v0.1.0-rc.1",
        "a.b_c-d-v10.20.30+build.5",
        "v1.2.0",
        "cli@1.2.0",
        "a/b-v1.2.0",
        "@keelstackhq/cli-v1.2.0",
        "Cli-v1.2.0",
        "cli-v1.2",
        "cli-v",
        "cli-v1",
        "-v1.2.0",
        "cli-V1.2.0",
        "cli-v1.2.0/x",
        "cli-v1.2.0-",
        "cli-v01.2.0",
        "cli-v1.2.0-01",
        "x-v1.2.0\n",
        "cli-v1.2.0+",
        "_x-v1.2.0",
        "cli-v1.2.0+001",
        "cli-v1.2.0-x.0",
        "a-v1-v1.2.0",
        "cli-v1.2.0-rc..1",
        "cli-v1.2.0-rc.",
        "cli-v1.2.0+a..b",
        "cli-v\u{661}.2.0",
    ];
    let mut accepted = Vec::new();
    for tag in candidates {
        let env = github(&[("GITHUB_REF", &format!("refs/tags/{tag}"))]);
        if run(&env, Some(&payload())).is_ok() {
            accepted.push(tag);
            assert!(
                !tag.contains('/') && tag.contains("-v"),
                "{tag} is outside `*-v*`"
            );
        }
    }
    let want = [
        "cli-v1.2.0",
        "my.tool_x-v0.1.0-rc.1",
        "a.b_c-d-v10.20.30+build.5",
        "cli-v1.2.0+001",
        "cli-v1.2.0-x.0",
        "a-v1-v1.2.0",
    ];
    assert_eq!(accepted, want);
}
