mod common;

use common::{FakeApi, fixture_repo, hoist};
use serde_json::Value;

const SITE: &str = "https://github.com/acme/x.git";
const CANARY: &str = "hoist-canary-auth-1d2e";

type Route = (
    &'static str,
    u16,
    &'static str,
    &'static [(&'static str, &'static str)],
);

fn routes(overrides: &[Route]) -> Vec<Route> {
    let mut routes: Vec<Route> = vec![
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_page2.json",
            &[],
        ),
        (
            "GET /repos/acme/x/environments",
            200,
            "github/environments_empty.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            "github/actions_permissions.json",
            &[],
        ),
        (
            "GET /user/tokens/verify",
            200,
            "cloudflare/token_verify_active.json",
            &[],
        ),
        (
            "GET /accounts/acc/pages/projects/keelstack-landing",
            200,
            "cloudflare/pages_project.json",
            &[],
        ),
    ];
    routes.retain(|(key, ..)| !overrides.iter().any(|(k, ..)| k == key));
    routes.extend_from_slice(overrides);
    routes
}

fn auth(api: &FakeApi) -> assert_cmd::Command {
    let mut cmd = hoist();
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("GITHUB_TOKEN", CANARY)
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc");
    cmd
}

fn run(api: &FakeApi, repo: &std::path::Path, code: i32) -> Value {
    let out = auth(api)
        .current_dir(repo)
        .args(["auth", "check", "--json"])
        .assert()
        .code(code)
        .get_output()
        .stdout
        .clone();
    api.assert_only_get();
    serde_json::from_slice(&out).expect("one JSON object")
}

fn item<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("no item {key}"))
}

#[test]
fn all_permissions_ok_exits_0() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[]));
    let report = run(&api, repo.path(), 0);
    assert_eq!(report["command"], "auth check");
    for key in [
        "github:administration:read",
        "github:secrets:read",
        "github:environments:read",
        "github:actions:read",
        "cloudflare:token:active",
        "cloudflare:pages:read",
    ] {
        let i = item(&report, key);
        assert_eq!(i["action"], "ok", "{key}: {i}");
        assert_eq!(i["kind"], "permission");
    }
    assert_eq!(api.requests().len(), 6);
}

#[test]
fn secrets_forbidden_exits_1_and_names_permission() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[(
        "GET /repos/acme/x/actions/secrets",
        403,
        "github/forbidden.json",
        &[],
    )]));
    let report = run(&api, repo.path(), 1);
    let i = item(&report, "github:secrets:read");
    assert_eq!(i["action"], "unknown");
    assert!(i["reason"].as_str().unwrap().contains("Secrets"), "{i}");
    assert_eq!(item(&report, "github:administration:read")["action"], "ok");
}

#[test]
fn server_error_on_a_probe_exits_1() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[("GET /repos/acme/x/environments", 500, "", &[])]));
    let report = run(&api, repo.path(), 1);
    let i = item(&report, "github:environments:read");
    assert_eq!(i["action"], "unknown");
    assert!(i["reason"].as_str().unwrap().contains("500"), "{i}");
}

#[test]
fn inactive_cloudflare_token_exits_1() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[(
        "GET /user/tokens/verify",
        200,
        "cloudflare/token_verify_inactive.json",
        &[],
    )]));
    let report = run(&api, repo.path(), 1);
    assert_eq!(
        item(&report, "cloudflare:token:active")["action"],
        "unknown"
    );
    assert_eq!(item(&report, "github:secrets:read")["action"], "ok");
}

#[test]
fn reports_token_source_not_value() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[]));
    let out = auth(&api)
        .current_dir(repo.path())
        .args(["auth", "check"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("env GITHUB_TOKEN"), "{stdout}");
    assert!(stdout.contains("env CLOUDFLARE_API_TOKEN"), "{stdout}");
    assert!(!stdout.contains(CANARY));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(CANARY));

    let json = run(&api, repo.path(), 0);
    let token = item(&json, "github");
    assert_eq!(token["kind"], "token");
    assert_eq!(token["reason"], "env GITHUB_TOKEN");
    assert_eq!(token["source"], "env GITHUB_TOKEN");
}

#[test]
fn gh_token_env_is_labelled() {
    let repo = fixture_repo("bare", SITE);
    let api = FakeApi::start(&routes(&[]));
    let out = auth(&api)
        .env_remove("GITHUB_TOKEN")
        .env("GH_TOKEN", CANARY)
        .current_dir(repo.path())
        .args(["auth", "check", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(item(&report, "github")["source"], "env GH_TOKEN");
}

#[test]
fn bare_repo_checks_github_only() {
    let repo = fixture_repo("bare", SITE);
    let api = FakeApi::start(&routes(&[]));
    let out = auth(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .args(["auth", "check", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["area"] == "github")
    );
}

#[test]
fn no_github_token_exits_3() {
    let repo = fixture_repo("bare", SITE);
    hoist()
        .current_dir(repo.path())
        .args(["auth", "check"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("no GitHub token"));
}

#[test]
fn github_401_makes_token_unknown_and_exits_1() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[
        (
            "GET /repos/acme/x/rulesets",
            401,
            "github/bad_credentials.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            401,
            "github/bad_credentials.json",
            &[],
        ),
        (
            "GET /repos/acme/x/environments",
            401,
            "github/bad_credentials.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            401,
            "github/bad_credentials.json",
            &[],
        ),
    ]));
    let report = run(&api, repo.path(), 1);
    let token = item(&report, "github");
    assert_eq!(token["kind"], "token");
    assert_eq!(token["action"], "unknown", "{token}");
    assert_eq!(token["reason"], "token rejected (401)");
    assert_eq!(item(&report, "cloudflare")["action"], "ok");
    assert!(!report.to_string().contains(CANARY));
}

#[test]
fn cloudflare_401_makes_token_unknown() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[(
        "GET /user/tokens/verify",
        401,
        "cloudflare/forbidden.json",
        &[],
    )]));
    let report = run(&api, repo.path(), 1);
    let token = item(&report, "cloudflare");
    assert_eq!(token["action"], "unknown", "{token}");
    assert_eq!(token["reason"], "token rejected (401)");
    assert_eq!(item(&report, "github")["action"], "ok");
}

#[test]
fn missing_cloudflare_token_is_unknown_not_exit_3() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[]));
    let out = auth(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .current_dir(repo.path())
        .args(["auth", "check", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    api.assert_only_get();
    let report: Value = serde_json::from_slice(&out).unwrap();
    let token = item(&report, "cloudflare");
    assert_eq!(token["kind"], "token");
    assert_eq!(token["action"], "unknown");
    assert_eq!(token["reason"], "no token; set CLOUDFLARE_API_TOKEN");
    assert_eq!(item(&report, "github:secrets:read")["action"], "ok");
    assert!(
        api.requests()
            .iter()
            .all(|r| !r.path.contains("/accounts/") && !r.path.contains("/user/tokens"))
    );
}

#[test]
fn no_github_token_exits_3_even_with_a_cloudflare_token() {
    let repo = fixture_repo("site", SITE);
    hoist()
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .current_dir(repo.path())
        .args(["auth", "check"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("no GitHub token"));
}

#[test]
fn missing_account_id_does_not_exit_3() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[]));
    let out = auth(&api)
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .args(["auth", "check", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    api.assert_only_get();
    let report: Value = serde_json::from_slice(&out).unwrap();
    let pages = item(&report, "cloudflare:pages:read");
    assert_eq!(pages["action"], "unknown");
    assert!(
        pages["reason"]
            .as_str()
            .unwrap()
            .contains("CLOUDFLARE_ACCOUNT_ID"),
        "{pages}"
    );
    assert_eq!(item(&report, "github:secrets:read")["action"], "ok");
}

#[test]
fn denied_probe_quotes_the_api_message() {
    let repo = fixture_repo("site", SITE);
    let api = FakeApi::start(&routes(&[(
        "GET /repos/acme/x/rulesets",
        403,
        "github/forbidden.json",
        &[],
    )]));
    let report = run(&api, repo.path(), 1);
    let reason = item(&report, "github:administration:read")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("not readable: Resource not accessible by personal access token"),
        "{reason}"
    );
    assert!(!reason.contains("missing or not granted"), "{reason}");
    assert!(!reason.contains("(403)"), "{reason}");
}
