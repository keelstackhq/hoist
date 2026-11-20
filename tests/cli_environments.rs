mod common;

use common::{FakeApi, RouteSpec, fixture_repo, hoist};
use serde_json::Value;
use std::path::Path;

const SITE: &str = "https://github.com/acme/x.git";
const EMPTY: &str = "github/environments_empty.json";

fn workflow(dir: &Path, name: &str, text: &str) {
    let path = dir.join(".github/workflows").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The `publish` fixture (release environment) plus jobs in four more shapes.
fn repo() -> tempfile::TempDir {
    let dir = fixture_repo("publish", SITE);
    workflow(
        dir.path(),
        "bot.yml",
        "name: bot\non: [push]\njobs:\n  splice:\n    runs-on: ubuntu-latest\n    environment: splice-bot\n    steps:\n      - run: echo hi\n  pr:\n    runs-on: ubuntu-latest\n    environment:\n      name: release-pr\n      url: https://example.com\n    steps:\n      - run: echo hi\n",
    );
    workflow(
        dir.path(),
        "deploy.yml",
        "name: deploy\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    environment: production\n    steps:\n      - run: echo hi\n",
    );
    dir
}

fn routes(envs: &[(&str, u16, &str)]) -> Vec<(String, u16, String)> {
    let mut r: Vec<(String, u16, String)> = vec![
        (
            "GET /repos/acme/x".into(),
            200,
            "github/repo_admin.json".into(),
        ),
        (
            "GET /repos/acme/x/rulesets".into(),
            200,
            "github/rulesets_list.json".into(),
        ),
        (
            "GET /repos/acme/x/rulesets/11".into(),
            200,
            "github/ruleset_ok.json".into(),
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow".into(),
            200,
            "github/actions_permissions.json".into(),
        ),
        (
            "GET /repos/acme/x/actions/secrets".into(),
            200,
            r#"{"total_count":0,"secrets":[]}"#.into(),
        ),
        (
            "GET /user".into(),
            200,
            r#"{"id":42,"login":"owner"}"#.into(),
        ),
        (
            "GET /@acme%2Flib".into(),
            200,
            r#"{"name":"@acme/lib"}"#.into(),
        ),
        (
            "GET /api/v1/crates/tool".into(),
            200,
            r#"{"crate":{}}"#.into(),
        ),
    ];
    for (k, s, b) in envs {
        r.push((k.to_string(), *s, b.to_string()));
    }
    r
}

fn start(r: &[(String, u16, String)]) -> FakeApi {
    let specs: Vec<RouteSpec> = r
        .iter()
        .map(|(k, s, b)| (k.as_str(), *s, b.as_str(), &[][..]))
        .collect();
    FakeApi::start(&specs)
}

fn run(api: &FakeApi, dir: &Path, args: &[&str], code: i32) -> Value {
    let out = hoist()
        .current_dir(dir)
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .args(args)
        .assert()
        .code(code)
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("{e}: {stdout}\n{}", String::from_utf8_lossy(&out.stderr)))
}

fn item<'a>(report: &'a Value, kind: &str, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {report}"))
}

fn writes(api: &FakeApi) -> Vec<String> {
    api.requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

#[test]
fn plan_lists_every_workflow_environment() {
    let dir = repo();
    let api = start(&routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    for env in ["splice-bot", "release-pr", "production"] {
        let it = item(&report, "environment", env);
        assert_eq!(it["action"], "create", "{it}");
        assert_eq!(it["area"], "github", "{it}");
    }
    let it = item(&report, "environment", "release");
    assert_eq!(it["action"], "create", "{it}");
    api.assert_only_get();
}

#[test]
fn apply_creates_every_workflow_environment_with_a_branch_policy() {
    let dir = repo();
    let mut r = routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]);
    for env in ["splice-bot", "release-pr", "production", "release"] {
        r.push((
            format!("PUT /repos/acme/x/environments/{env}"),
            200,
            "empty.json".into(),
        ));
        r.push((
            format!("POST /repos/acme/x/environments/{env}/deployment-branch-policies"),
            200,
            "empty.json".into(),
        ));
    }
    let api = start(&r);
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let w = writes(&api);
    for env in ["splice-bot", "release-pr", "production", "release"] {
        assert!(
            w.contains(&format!("PUT /repos/acme/x/environments/{env}")),
            "{w:?}"
        );
        assert!(
            w.contains(&format!(
                "POST /repos/acme/x/environments/{env}/deployment-branch-policies"
            )),
            "{w:?}"
        );
    }
}

#[test]
fn an_environment_expression_is_an_unknown_item_naming_the_line() {
    let dir = repo();
    workflow(
        dir.path(),
        "matrix.yml",
        "name: m\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    environment: ${{ matrix.env }}\n    steps:\n      - run: echo hi\n",
    );
    let api = start(&routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "${{ matrix.env }}");
    assert_eq!(it["action"], "unknown", "{it}");
    assert_eq!(it["source"], ".github/workflows/matrix.yml:6", "{it}");
    let names: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "environment")
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("matrix") && *n != "${{ matrix.env }}")
    );
    api.assert_only_get();
}

#[test]
fn a_reusable_workflow_caller_input_creates_the_environment() {
    let dir = repo();
    workflow(
        dir.path(),
        "call.yml",
        "name: c\non: [push]\njobs:\n  deploy:\n    uses: ./.github/workflows/reusable.yml\n    with:\n      environment: staging\n",
    );
    workflow(
        dir.path(),
        "reusable.yml",
        "name: r\non:\n  workflow_call:\n    inputs:\n      environment:\n        type: string\njobs:\n  go:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n",
    );
    let api = start(&routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "staging");
    assert_eq!(it["action"], "create", "{it}");
    assert_eq!(it["source"], ".github/workflows/call.yml:7", "{it}");
}

fn write_toml(dir: &Path, text: &str) {
    std::fs::write(dir.join("hoist.toml"), text).unwrap();
}

#[test]
fn hoist_toml_lists_environments_beyond_the_workflows() {
    let dir = repo();
    write_toml(
        dir.path(),
        "[github.environments.canary]\nbranches = [\"main\", \"canary\"]\n",
    );
    let api = start(&routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "canary");
    assert_eq!(it["action"], "create", "{it}");
    assert_eq!(
        it["source"], "hoist.toml:github.environments.canary",
        "{it}"
    );
    // The workflow-inferred one keeps citing its workflow.
    let it = item(&report, "environment", "production");
    assert_eq!(it["source"], ".github/workflows/deploy.yml:6", "{it}");
}

#[test]
fn a_hoist_toml_environment_with_manage_false_is_left_alone() {
    let dir = repo();
    write_toml(
        dir.path(),
        "[github.environments.splice-bot]\nmanage = false\n",
    );
    let mut r = routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]);
    for env in ["release-pr", "production", "release"] {
        r.push((
            format!("PUT /repos/acme/x/environments/{env}"),
            200,
            "empty.json".into(),
        ));
        r.push((
            format!("POST /repos/acme/x/environments/{env}/deployment-branch-policies"),
            200,
            "empty.json".into(),
        ));
    }
    let api = start(&r);
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "splice-bot");
    assert_eq!(it["action"], "ok", "{it}");
    assert!(
        it["reason"].as_str().unwrap().contains("not managed"),
        "{it}"
    );
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let w = writes(&api);
    assert!(!w.iter().any(|l| l.contains("splice-bot")), "{w:?}");
    assert!(
        w.contains(&"PUT /repos/acme/x/environments/production".to_string()),
        "{w:?}"
    );
}

#[test]
fn environment_branches_come_from_hoist_toml() {
    let dir = repo();
    write_toml(
        dir.path(),
        "[github.environments.production]\nbranches = [\"main\", \"hotfix/*\"]\n[github.environments.splice-bot]\nbranches = []\n",
    );
    let mut r = routes(&[("GET /repos/acme/x/environments", 200, EMPTY)]);
    for env in ["splice-bot", "release-pr", "production", "release"] {
        r.push((
            format!("PUT /repos/acme/x/environments/{env}"),
            200,
            "empty.json".into(),
        ));
        r.push((
            format!("POST /repos/acme/x/environments/{env}/deployment-branch-policies"),
            200,
            "empty.json".into(),
        ));
    }
    let api = start(&r);
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let sent = |method: &str, path: &str| -> Vec<Value> {
        api.requests()
            .iter()
            .filter(|q| q.method == method && q.path == path)
            .map(|q| serde_json::from_str(q.body.as_deref().unwrap()).unwrap())
            .collect()
    };
    let policies = sent(
        "POST",
        "/repos/acme/x/environments/production/deployment-branch-policies",
    );
    let names: Vec<&str> = policies
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["main", "hotfix/*"]);
    let open = sent("PUT", "/repos/acme/x/environments/splice-bot");
    assert!(open[0]["deployment_branch_policy"].is_null(), "{open:?}");
    assert!(
        sent(
            "POST",
            "/repos/acme/x/environments/splice-bot/deployment-branch-policies"
        )
        .is_empty()
    );
    // The default stays the default branch.
    let pr = sent(
        "POST",
        "/repos/acme/x/environments/release-pr/deployment-branch-policies",
    );
    assert_eq!(pr[0]["name"], "main");
}

#[test]
fn an_unknown_environment_key_exits_3() {
    let dir = repo();
    write_toml(
        dir.path(),
        "[github.environments.production]\nbranch = \"main\"\n",
    );
    let out = hoist()
        .current_dir(dir.path())
        .env("GITHUB_TOKEN", "t")
        .args(["plan", "--json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("hoist.toml") && err.contains("branch"),
        "{err}"
    );
}

const ONLY_PRODUCTION: &str = "GET /repos/acme/x/environments";
const POLICIES: &str = "GET /repos/acme/x/environments/production/deployment-branch-policies";

/// A repo whose only environment is `production`.
fn production_only() -> tempfile::TempDir {
    let dir = fixture_repo("bare", SITE);
    workflow(
        dir.path(),
        "deploy.yml",
        "name: deploy\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    environment: production\n    steps:\n      - run: echo hi\n",
    );
    dir
}

fn envs_json(policy: &str) -> String {
    format!(
        r#"{{"total_count":1,"environments":[{{"id":1,"name":"production","deployment_branch_policy":{policy}}}]}}"#
    )
}

fn live(policy: &str, policies: &str) -> Vec<(String, u16, String)> {
    let mut r = routes(&[]);
    r.retain(|(k, ..)| !k.contains("/user") && !k.contains("lib") && !k.contains("crates"));
    r.push((ONLY_PRODUCTION.into(), 200, envs_json(policy)));
    r.push((POLICIES.into(), 200, policies.into()));
    r.push((
        "PUT /repos/acme/x/environments/production".into(),
        200,
        "empty.json".into(),
    ));
    r.push((
        "POST /repos/acme/x/environments/production/deployment-branch-policies".into(),
        200,
        "empty.json".into(),
    ));
    r
}

const CUSTOM: &str = r#"{"protected_branches":false,"custom_branch_policies":true}"#;

fn policy_rows(names: &[&str]) -> String {
    let rows: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"id":1,"name":"{n}","type":"branch"}}"#))
        .collect();
    format!(
        r#"{{"total_count":{},"branch_policies":[{}]}}"#,
        rows.len(),
        rows.join(",")
    )
}

#[test]
fn an_environment_with_the_wanted_branch_policy_is_ok() {
    let dir = production_only();
    let api = start(&live(CUSTOM, &policy_rows(&["main"])));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "production");
    assert_eq!(it["action"], "ok", "{it}");
    api.assert_only_get();
}

#[test]
fn an_unrestricted_environment_is_an_update_and_apply_restricts_it() {
    let dir = production_only();
    let api = start(&live("null", &policy_rows(&[])));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "production");
    assert_eq!(it["action"], "update", "{it}");
    assert!(
        it["reason"].as_str().unwrap().contains("any branch"),
        "{it}"
    );
    api.assert_only_get();
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let w = writes(&api);
    assert_eq!(
        w,
        [
            "PUT /repos/acme/x/environments/production",
            "POST /repos/acme/x/environments/production/deployment-branch-policies"
        ]
    );
}

#[test]
fn a_missing_branch_policy_is_added_without_touching_the_others() {
    let dir = production_only();
    write_toml(
        dir.path(),
        "[github.environments.production]\nbranches = [\"main\", \"hotfix/*\"]\n",
    );
    let api = start(&live(CUSTOM, &policy_rows(&["main", "legacy"])));
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "environment", "production");
    assert_eq!(it["action"], "update", "{it}");
    let reason = it["reason"].as_str().unwrap();
    assert!(
        reason.contains("hotfix/*") && reason.contains("legacy"),
        "{reason}"
    );
    assert_eq!(it["source"], "hoist.toml:github.environments.production");
    api.assert_only_get();
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    // No PUT (the environment's reviewers stay), no delete, one POST for the missing branch.
    assert_eq!(
        writes(&api),
        ["POST /repos/acme/x/environments/production/deployment-branch-policies"]
    );
    let post = api
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(post.body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"name": "hotfix/*", "type": "branch"})
    );
}

#[test]
fn an_unreadable_branch_policy_list_is_unknown() {
    let dir = production_only();
    let mut r = live(CUSTOM, "");
    r.retain(|(k, ..)| k != POLICIES);
    r.push((
        POLICIES.into(),
        403,
        r#"{"message":"Resource not accessible"}"#.into(),
    ));
    let api = start(&r);
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    assert_eq!(
        item(&report, "environment", "production")["action"],
        "unknown"
    );
}

#[test]
fn adopt_reports_a_branch_policy_gap_as_drift_and_writes_nothing() {
    let dir = production_only();
    let api = start(&live("null", &policy_rows(&[])));
    let report = run(
        &api,
        dir.path(),
        &["apply", "--adopt", "--yes", "--json"],
        1,
    );
    let it = item(&report, "environment", "production");
    assert_eq!(it["action"], "drift", "{it}");
    assert!(it["reason"].as_str().unwrap().contains("--adopt"), "{it}");
    assert!(writes(&api).is_empty(), "{:?}", writes(&api));
}

#[test]
fn restricting_an_existing_environment_keeps_its_reviewers_and_wait_timer() {
    let dir = production_only();
    let mut r = live("null", &policy_rows(&[]));
    r.push((
        "GET /repos/acme/x/environments/production".into(),
        200,
        r#"{"name":"production","protection_rules":[{"type":"wait_timer","wait_timer":30},{"type":"required_reviewers","prevent_self_review":true,"reviewers":[{"type":"User","reviewer":{"id":5}}]}]}"#.into(),
    ));
    let api = start(&r);
    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let put = api
        .requests()
        .into_iter()
        .find(|q| q.method == "PUT")
        .unwrap();
    let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["wait_timer"], 30, "{body}");
    assert_eq!(body["prevent_self_review"], true, "{body}");
    assert_eq!(
        body["reviewers"],
        serde_json::json!([{"type":"User","id":5}]),
        "{body}"
    );
}
