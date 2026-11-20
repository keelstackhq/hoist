mod common;

use common::{FakeApi, fixture_repo};
use hoist::auth::Secret;
use hoist::change::{Change, Reviewers};
use hoist::desired::{Desired, build_desired};
use hoist::github_write::execute_github;
use hoist::http::{Http, Service};
use serde_json::{Value, json};

fn desired() -> (tempfile::TempDir, Desired) {
    let dir = fixture_repo("site", "https://github.com/acme/x.git");
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[cloudflare]\naccount_id = \"acc\"\n",
    )
    .unwrap();
    let d = build_desired(dir.path()).expect("desired");
    (dir, d)
}

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, Secret::from("dummy"), &api.url()).max_sleep_ms(0)
}

fn body(api: &FakeApi, index: usize) -> Value {
    let requests = api.requests();
    serde_json::from_str(requests[index].body.as_deref().expect("body")).unwrap()
}

fn ruleset(existing_id: Option<u64>, merge_queue: bool) -> Change {
    Change::Ruleset {
        existing_id,
        branch: "main".into(),
        required_checks: vec!["ci-ok".into()],
        merge_queue,
    }
}

fn base_ruleset() -> Value {
    json!({
        "name": "hoist: default branch",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": [{"type": "required_status_checks", "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": [{"context": "ci-ok"}]
        }}]
    })
}

#[test]
fn settings_patch_sends_only_changed_fields() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("PATCH /repos/acme/x", 200, "empty.json", &[])]);
    let mut patch = serde_json::Map::new();
    patch.insert("allow_auto_merge".into(), json!(true));
    execute_github(&http(&api), &d, &Change::RepoSettings { patch }).unwrap();
    assert_eq!(api.requests().len(), 1);
    assert_eq!(api.requests()[0].method, "PATCH");
    assert_eq!(body(&api, 0), json!({"allow_auto_merge": true}));
}

#[test]
fn ruleset_create_body_exact() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("POST /repos/acme/x/rulesets", 201, "created.json", &[])]);
    execute_github(&http(&api), &d, &ruleset(None, false)).unwrap();
    assert_eq!(api.requests().len(), 1);
    assert_eq!(body(&api, 0), base_ruleset());
}

#[test]
fn ruleset_update_uses_put_by_id() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("PUT /repos/acme/x/rulesets/11", 200, "empty.json", &[])]);
    execute_github(&http(&api), &d, &ruleset(Some(11), false)).unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[0].path, "/repos/acme/x/rulesets/11");
    assert_eq!(body(&api, 0), base_ruleset());
}

#[test]
fn ruleset_with_merge_queue_adds_rule() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("POST /repos/acme/x/rulesets", 201, "created.json", &[])]);
    execute_github(&http(&api), &d, &ruleset(None, true)).unwrap();
    let mut want = base_ruleset();
    want["rules"].as_array_mut().unwrap().push(json!({
        "type": "merge_queue",
        "parameters": {
            "merge_method": "MERGE",
            "grouping_strategy": "ALLGREEN",
            "max_entries_to_build": 5,
            "min_entries_to_merge": 1,
            "max_entries_to_merge": 5,
            "min_entries_to_merge_wait_minutes": 5,
            "check_response_timeout_minutes": 60
        }
    }));
    assert_eq!(body(&api, 0), want);
}

#[test]
fn actions_permissions_put() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[(
        "PUT /repos/acme/x/actions/permissions/workflow",
        204,
        "",
        &[],
    )]);
    execute_github(
        &http(&api),
        &d,
        &Change::ActionsPermissions {
            default: "read".into(),
        },
    )
    .unwrap();
    assert_eq!(api.requests()[0].method, "PUT");
    assert_eq!(
        body(&api, 0),
        json!({"default_workflow_permissions": "read"})
    );
}

fn environment() -> Change {
    Change::Environment {
        name: "pro duction".into(),
        branches: vec!["main".into()],
        release: false,
        reviewers: Reviewers::Unresolved,
    }
}

#[test]
fn environment_put_then_branch_policy() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        (
            "PUT /repos/acme/x/environments/pro%20duction",
            200,
            "empty.json",
            &[],
        ),
        (
            "POST /repos/acme/x/environments/pro%20duction/deployment-branch-policies",
            200,
            "created.json",
            &[],
        ),
    ]);
    execute_github(&http(&api), &d, &environment()).unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(
        body(&api, 0),
        json!({"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}})
    );
    assert_eq!(requests[1].method, "POST");
    assert_eq!(body(&api, 1), json!({"name": "main", "type": "branch"}));
}

fn release_environment() -> Change {
    Change::Environment {
        name: "release".into(),
        branches: vec!["main".into()],
        release: true,
        reviewers: Reviewers::Users(vec![42]),
    }
}

const USER: (&str, u16, &str, &[(&str, &str)]) =
    ("GET /user", 200, r#"{"id":42,"login":"owner"}"#, &[]);

#[test]
fn release_422_about_reviewers_creates_the_environment_without_one() {
    for message in [
        r#"{"message":"Failed to create the environment: required reviewers are not available on this plan"}"#,
        r#"{"message":"Environment protection rules are not available for this repository"}"#,
    ] {
        let (_dir, d) = desired();
        let api = FakeApi::start(&[
            USER,
            ("PUT /repos/acme/x/environments/release", 422, message, &[]),
            ("PUT /repos/acme/x/environments/release", 200, "{}", &[]),
            (
                "POST /repos/acme/x/environments/release/deployment-branch-policies",
                200,
                "{}",
                &[],
            ),
        ]);
        let err = execute_github(&http(&api), &d, &release_environment()).unwrap_err();
        assert!(
            matches!(err, hoist::error::HoistError::PlanLimited(_)),
            "{err}"
        );
        let puts: Vec<_> = api
            .requests()
            .into_iter()
            .filter(|r| r.method == "PUT")
            .collect();
        assert_eq!(puts.len(), 2);
        assert!(!puts[1].body.as_deref().unwrap().contains("reviewers"));
    }
}

#[test]
fn release_422_for_another_reason_is_a_real_error() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        USER,
        (
            "PUT /repos/acme/x/environments/release",
            422,
            r#"{"message":"Validation Failed: deployment_branch_policy is invalid"}"#,
            &[],
        ),
    ]);
    let err = execute_github(&http(&api), &d, &release_environment()).unwrap_err();
    assert!(
        !matches!(err, hoist::error::HoistError::PlanLimited(_)),
        "{err}"
    );
    assert!(err.to_string().contains("422"), "{err}");
    let puts = api
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .count();
    assert_eq!(puts, 1);
}

#[test]
fn branch_policy_already_exists_is_ok() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        (
            "PUT /repos/acme/x/environments/pro%20duction",
            200,
            "empty.json",
            &[],
        ),
        (
            "POST /repos/acme/x/environments/pro%20duction/deployment-branch-policies",
            422,
            "github/policy_exists.json",
            &[],
        ),
    ]);
    execute_github(&http(&api), &d, &environment()).unwrap();
}

#[test]
fn other_422_on_branch_policy_is_error() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        (
            "PUT /repos/acme/x/environments/pro%20duction",
            200,
            "empty.json",
            &[],
        ),
        (
            "POST /repos/acme/x/environments/pro%20duction/deployment-branch-policies",
            422,
            "unprocessable.json",
            &[],
        ),
    ]);
    let err = execute_github(&http(&api), &d, &environment()).unwrap_err();
    assert!(err.to_string().contains("422"), "{err}");
}

#[test]
fn forbidden_write_is_rejected_error() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[(
        "POST /repos/acme/x/rulesets",
        403,
        "github/forbidden.json",
        &[],
    )]);
    let err = execute_github(&http(&api), &d, &ruleset(None, false)).unwrap_err();
    assert_eq!(err.exit_code(), 3);
    let text = err.to_string();
    assert!(
        text.contains("POST /repos/acme/x/rulesets")
            && text.contains("Resource not accessible by personal access token"),
        "{text}"
    );
}

#[test]
fn non_github_change_is_usage_error() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[]);
    let err = execute_github(
        &http(&api),
        &d,
        &Change::Domain {
            project: "p".into(),
            host: "h".into(),
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("not a GitHub change"));
    assert!(api.requests().is_empty());
}

#[test]
fn branch_policy_303_is_success_and_not_followed() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        (
            "PUT /repos/acme/x/environments/pro%20duction",
            200,
            "empty.json",
            &[],
        ),
        (
            "POST /repos/acme/x/environments/pro%20duction/deployment-branch-policies",
            303,
            "",
            &[("Location", "/elsewhere")],
        ),
    ]);
    execute_github(&http(&api), &d, &environment()).unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests.iter().all(|r| r.path != "/elsewhere"));
}

#[test]
fn ruleset_update_with_merge_queue_puts_queue_rule() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("PUT /repos/acme/x/rulesets/11", 200, "empty.json", &[])]);
    execute_github(&http(&api), &d, &ruleset(Some(11), true)).unwrap();
    let sent = body(&api, 0);
    let rules = sent["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[1]["type"], "merge_queue");
}

#[test]
fn environment_without_branches_has_no_branch_restriction() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[(
        "PUT /repos/acme/x/environments/pro%20duction",
        200,
        "empty.json",
        &[],
    )]);
    let open = Change::Environment {
        name: "pro duction".into(),
        branches: Vec::new(),
        release: false,
        reviewers: Reviewers::Unresolved,
    };
    execute_github(&http(&api), &d, &open).unwrap();
    assert_eq!(api.requests().len(), 1);
    assert_eq!(body(&api, 0), json!({"deployment_branch_policy": null}));
}

#[test]
fn environment_posts_one_policy_per_branch() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[
        (
            "PUT /repos/acme/x/environments/pro%20duction",
            200,
            "empty.json",
            &[],
        ),
        (
            "POST /repos/acme/x/environments/pro%20duction/deployment-branch-policies",
            200,
            "created.json",
            &[],
        ),
    ]);
    let two = Change::Environment {
        name: "pro duction".into(),
        branches: vec!["main".into(), "release/*".into()],
        release: false,
        reviewers: Reviewers::Unresolved,
    };
    execute_github(&http(&api), &d, &two).unwrap();
    assert_eq!(api.requests().len(), 3);
    assert_eq!(body(&api, 1), json!({"name": "main", "type": "branch"}));
    assert_eq!(
        body(&api, 2),
        json!({"name": "release/*", "type": "branch"})
    );
}
