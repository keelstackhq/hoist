mod common;

use common::{FakeApi, fixture_repo, hoist};
use predicates::prelude::*;
use serde_json::Value;

const SITE: &str = "https://github.com/acme/x.git";

type Route = (
    &'static str,
    u16,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const SECRETS_LINK: &[(&str, &str)] = &[(
    "Link",
    r#"<{base}/repos/acme/x/actions/secrets?page=2>; rel="next""#,
)];

/// Routes under which the `site` fixture is in sync except where a test overrides them.
fn github_routes() -> Vec<Route> {
    vec![
        ("GET /repos/acme/x", 200, "github/repo.json", &[]),
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets/11",
            200,
            "github/ruleset_ok.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            "github/actions_permissions.json",
            &[],
        ),
        (
            "GET /repos/acme/x/environments",
            200,
            "github/environments_empty.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_page1.json",
            SECRETS_LINK,
        ),
        (
            "GET /repos/acme/x/actions/secrets?page=2",
            200,
            "github/secrets_page2.json",
            &[],
        ),
        (
            "GET /user/installations",
            200,
            "github/installations.json",
            &[],
        ),
        (
            "GET /user/installations/501/repositories",
            200,
            "github/installation_repos.json",
            &[],
        ),
    ]
}

const PROJECT: Route = (
    "GET /accounts/acc/pages/projects/keelstack-landing",
    200,
    "cloudflare/pages_project.json",
    &[],
);

fn fake(overrides: &[Route]) -> FakeApi {
    let mut routes: Vec<Route> = github_routes()
        .into_iter()
        .chain([PROJECT])
        .filter(|(key, ..)| !overrides.iter().any(|(k, ..)| k == key))
        .collect();
    routes.extend_from_slice(overrides);
    FakeApi::start(&routes)
}

fn plan(api: &FakeApi) -> assert_cmd::Command {
    let mut cmd = hoist();
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc");
    cmd
}

#[test]
fn plan_site_json_snapshot() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[
        (
            "GET /user/tokens",
            200,
            r#"{"success":true,"errors":[],"messages":[],"result":[]}"#,
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/empty_list.json",
            &[],
        ),
        (
            "GET /user/installations/501/repositories",
            200,
            "github/installation_repos_other.json",
            &[],
        ),
    ]);
    let out = plan(&api)
        .current_dir(repo.path())
        .args(["plan", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    api.assert_only_get();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("test-token"));
    let report: Value = serde_json::from_str(&text).expect("one JSON object");
    let actions: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    for want in ["ok", "create", "pending"] {
        assert!(actions.contains(&want), "no {want} in {actions:?}");
    }
    assert_eq!(report["human_steps"].as_array().unwrap().len(), 1);
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["area"] == "cloudflare")
    );
    insta::assert_json_snapshot!(report);
}

#[test]
fn area_filter_skips_cloudflare() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    plan(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .current_dir(repo.path())
        .args(["plan", "--area", "github"])
        .assert()
        .code(0);
    api.assert_only_get();
    let requests = api.requests();
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|r| !r.path.contains("/accounts/")),
        "{requests:?}"
    );
}

#[test]
fn bare_repo_needs_no_cloudflare_token() {
    let repo = fixture_repo("bare", SITE);
    let api = fake(&[]);
    plan(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .arg("plan")
        .assert()
        .code(0);
    api.assert_only_get();
    assert!(
        api.requests()
            .iter()
            .all(|r| !r.path.contains("/accounts/"))
    );
}

#[test]
fn missing_github_token_exits_3() {
    let repo = fixture_repo("site", SITE);
    let real_git = String::from_utf8(
        std::process::Command::new("which")
            .arg("git")
            .output()
            .expect("which git")
            .stdout,
    )
    .unwrap();
    let bin = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(real_git.trim(), bin.path().join("git")).unwrap();
    hoist()
        .env("PATH", bin.path())
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .current_dir(repo.path())
        .arg("plan")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("no GitHub token"))
        .stderr(predicate::str::contains("Administration"))
        .stderr(predicate::str::contains(
            "Administration, Secrets, Environments, Actions, Metadata: read",
        ))
        .stderr(predicate::str::contains("write permissions come with M2"))
        .stderr(predicate::str::contains("(write)").not());
}

#[test]
fn missing_cloudflare_token_exits_3_when_targets_exist() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    plan(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .current_dir(repo.path())
        .arg("plan")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("CLOUDFLARE_API_TOKEN"));
}

fn stderr_of(cmd: &mut assert_cmd::Command, dir: &std::path::Path, code: i32) -> String {
    let out = cmd
        .current_dir(dir)
        .args(["plan", "--json"])
        .assert()
        .code(code)
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("test-token"), "{all}");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn github_401_exits_3_and_names_the_token_vars() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /repos/acme/x/rulesets",
        401,
        "github/bad_credentials.json",
        &[],
    )]);
    let err = stderr_of(&mut plan(&api), repo.path(), 3);
    api.assert_only_get();
    assert!(
        err.contains("GitHub rejected the token (401); check GITHUB_TOKEN/GH_TOKEN"),
        "{err}"
    );
}

#[test]
fn cloudflare_401_exits_3() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /accounts/acc/pages/projects/keelstack-landing",
        401,
        "cloudflare/forbidden.json",
        &[],
    )]);
    let err = stderr_of(&mut plan(&api), repo.path(), 3);
    api.assert_only_get();
    assert!(
        err.contains("Cloudflare rejected the token (401); check CLOUDFLARE_API_TOKEN"),
        "{err}"
    );
}

#[test]
fn repo_404_exits_3() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[("GET /repos/acme/x", 404, "github/forbidden.json", &[])]);
    let err = stderr_of(&mut plan(&api), repo.path(), 3);
    api.assert_only_get();
    assert!(
        err.contains("token cannot see acme/x (404); grant it access or check the remote"),
        "{err}"
    );
}

#[test]
fn area_github_needs_no_account_id() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    plan(&api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .args(["plan", "--area", "github"])
        .assert()
        .code(0);
    api.assert_only_get();
}

#[test]
fn missing_account_id_still_exits_3_when_cloudflare_is_planned() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    plan(&api)
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .arg("plan")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("CLOUDFLARE_ACCOUNT_ID"));
}
