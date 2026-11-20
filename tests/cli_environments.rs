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
