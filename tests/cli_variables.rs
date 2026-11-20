mod common;

use common::{FakeApi, RouteSpec, fixture_repo, hoist};
use serde_json::Value;
use std::path::Path;

const SITE: &str = "https://github.com/acme/x.git";
const VARS: &str = "GET /repos/acme/x/actions/variables";
const ORG_VARS: &str = "GET /repos/acme/x/actions/organization-variables";

/// A repo with no Cloudflare target whose workflow reads `vars.API_URL` and `vars['region']`.
fn repo() -> tempfile::TempDir {
    let dir = fixture_repo("bare", SITE);
    std::fs::write(
        dir.path().join(".github/workflows/deploy.yml"),
        "name: deploy\non: [push]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ${{ vars.API_URL }}\n      - run: echo ${{ vars['region'] }}\n",
    )
    .unwrap();
    dir
}

fn write_toml(dir: &Path, text: &str) {
    std::fs::write(dir.join("hoist.toml"), text).unwrap();
}

fn base() -> Vec<(String, u16, String)> {
    [
        ("GET /repos/acme/x", "github/repo_admin.json"),
        ("GET /repos/acme/x/rulesets", "github/rulesets_list.json"),
        ("GET /repos/acme/x/rulesets/11", "github/ruleset_ok.json"),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            "github/actions_permissions.json",
        ),
        (
            "GET /repos/acme/x/environments",
            "github/environments_empty.json",
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            r#"{"total_count":0,"secrets":[]}"#,
        ),
        ("POST /repos/acme/x/actions/variables", "empty.json"),
        (
            "PATCH /repos/acme/x/actions/variables/API_URL",
            "empty.json",
        ),
    ]
    .iter()
    .map(|(k, b)| {
        (
            k.to_string(),
            if k.starts_with("POST") || k.starts_with("PATCH") {
                201
            } else {
                200
            },
            b.to_string(),
        )
    })
    .collect()
}

fn api(extra: &[(&str, u16, &str)]) -> FakeApi {
    let mut r = base();
    for (k, s, b) in extra {
        r.retain(|(rk, ..)| rk != k);
        r.push((k.to_string(), *s, b.to_string()));
    }
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

const NO_VARS: &str = r#"{"total_count":0,"variables":[]}"#;

#[test]
fn a_missing_variable_without_a_value_is_pending_with_the_command() {
    let dir = repo();
    let api = api(&[(VARS, 200, NO_VARS), (ORG_VARS, 200, NO_VARS)]);
    let report = run(&api, dir.path(), &["check", "--json"], 2);
    for name in ["API_URL", "REGION"] {
        let it = item(&report, "variable", name);
        assert_eq!(it["action"], "pending", "{it}");
        assert!(
            it["reason"].as_str().unwrap().contains(&format!(
                "gh variable set {name} --repo acme/x --body '<value>'"
            )),
            "{it}"
        );
        assert!(
            it["source"]
                .as_str()
                .unwrap()
                .starts_with(".github/workflows/deploy.yml:")
        );
    }
    assert_eq!(
        item(&report, "variable", "API_URL")["source"],
        ".github/workflows/deploy.yml:7"
    );
    assert_eq!(
        item(&report, "variable", "REGION")["source"],
        ".github/workflows/deploy.yml:8"
    );
    let step = report["human_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "variable:API_URL")
        .expect("step");
    assert!(
        step["title"]
            .as_str()
            .unwrap()
            .contains("gh variable set API_URL --repo acme/x --body '<value>'"),
        "{step}"
    );
    assert!(
        step["title"].as_str().unwrap().contains("hoist.toml"),
        "{step}"
    );
    assert_eq!(step["status"], "pending");
    assert_eq!(
        step["url"],
        "https://github.com/acme/x/settings/variables/actions"
    );
    api.assert_only_get();
}

#[test]
fn present_repository_and_organization_variables_are_ok() {
    let dir = repo();
    let api = api(&[
        (
            VARS,
            200,
            r#"{"total_count":1,"variables":[{"name":"API_URL","value":"https://x"}]}"#,
        ),
        (
            ORG_VARS,
            200,
            r#"{"total_count":1,"variables":[{"name":"REGION","value":"eu"}]}"#,
        ),
    ]);
    let report = run(&api, dir.path(), &["check", "--json"], 0);
    assert_eq!(item(&report, "variable", "API_URL")["action"], "ok");
    let it = item(&report, "variable", "REGION");
    assert_eq!(it["action"], "ok");
    assert!(
        it["reason"].as_str().unwrap().contains("organization"),
        "{it}"
    );
}

#[test]
fn a_forbidden_organization_list_counts_as_none_with_a_note() {
    let dir = repo();
    let api = api(&[
        (VARS, 200, NO_VARS),
        (
            ORG_VARS,
            403,
            r#"{"message":"Resource not accessible by integration"}"#,
        ),
    ]);
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "variable", "API_URL");
    assert_eq!(it["action"], "pending", "{it}");
    assert!(
        it["reason"]
            .as_str()
            .unwrap()
            .contains("organization variables not readable"),
        "{it}"
    );
}

#[test]
fn an_unreadable_variable_list_is_unknown() {
    let dir = repo();
    let api = api(&[(VARS, 403, r#"{"message":"Resource not accessible"}"#)]);
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    assert_eq!(item(&report, "variable", "API_URL")["action"], "unknown");
}

#[test]
fn hoist_toml_values_create_update_or_match() {
    let dir = repo();
    write_toml(
        dir.path(),
        "[github.variables]\nAPI_URL = \"https://new\"\nregion = \"eu\"\nEXTRA = \"1\"\n",
    );
    let api = api(&[
        (
            VARS,
            200,
            r#"{"total_count":2,"variables":[{"name":"API_URL","value":"https://old"},{"name":"REGION","value":"eu"}]}"#,
        ),
        (ORG_VARS, 404, r#"{"message":"Not Found"}"#),
    ]);
    let report = run(&api, dir.path(), &["plan", "--json"], 0);
    let it = item(&report, "variable", "API_URL");
    assert_eq!(it["action"], "update", "{it}");
    assert_eq!(it["reason"], "live \"https://old\", want \"https://new\"");
    assert_eq!(it["source"], "hoist.toml:github.variables.API_URL");
    assert_eq!(item(&report, "variable", "REGION")["action"], "ok");
    let it = item(&report, "variable", "EXTRA");
    assert_eq!(it["action"], "create", "{it}");
    assert_eq!(it["source"], "hoist.toml:github.variables.EXTRA");
    assert!(report["human_steps"].as_array().unwrap().is_empty());
    api.assert_only_get();

    let _ = run(&api, dir.path(), &["apply", "--yes", "--json"], 1);
    let w = writes(&api);
    assert_eq!(
        w,
        [
            "PATCH /repos/acme/x/actions/variables/API_URL",
            "POST /repos/acme/x/actions/variables"
        ],
        "{w:?}"
    );
    let post = api
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(post.body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"name": "EXTRA", "value": "1"})
    );
    let patch = api
        .requests()
        .into_iter()
        .find(|q| q.method == "PATCH")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(patch.body.as_deref().unwrap()).unwrap(),
        serde_json::json!({"name": "API_URL", "value": "https://new"})
    );
}

#[test]
fn optional_variables_are_listed_apart() {
    let dir = repo();
    write_toml(dir.path(), "[github]\noptional_variables = [\"region\"]\n");
    let api = api(&[
        (
            VARS,
            200,
            r#"{"total_count":1,"variables":[{"name":"API_URL","value":"x"}]}"#,
        ),
        (ORG_VARS, 200, NO_VARS),
    ]);
    let report = run(&api, dir.path(), &["check", "--json"], 0);
    assert_eq!(item(&report, "optional_variable", "REGION")["action"], "ok");
}

#[test]
fn no_variable_reads_without_a_variable() {
    let dir = fixture_repo("bare", SITE);
    let api = api(&[]);
    let _ = run(&api, dir.path(), &["plan", "--json"], 0);
    assert!(
        api.requests().iter().all(|r| !r.path.contains("variables")),
        "{:?}",
        api.requests()
    );
}

#[test]
fn auth_check_probes_variables_only_when_used() {
    let dir = repo();
    let api = api(&[(VARS, 200, NO_VARS)]);
    let report = run(&api, dir.path(), &["auth", "check", "--json"], 0);
    assert_eq!(
        item(&report, "permission", "github:variables:read")["action"],
        "ok"
    );
    let plain = fixture_repo("bare", SITE);
    let report = run(&api, plain.path(), &["auth", "check", "--json"], 0);
    assert!(
        !report["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["key"] == "github:variables:read")
    );
}
