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
        .stderr(predicate::str::contains("`hoist apply` also needs"))
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

// --- release areas: npm, crates, release (M3 Task 6) ---

const ENVS_RELEASE: &str = r#"{"total_count":1,"environments":[{"name":"release"}]}"#;
const NPM_LIB: Route = ("GET /@acme%2Flib", 200, r#"{"name":"@acme/lib"}"#, &[]);
const CRATE_TOOL: Route = ("GET /api/v1/crates/tool", 200, r#"{"crate":{}}"#, &[]);
const TRUST_NPM: &str =
    "npm trust github @acme/lib --file release.yml --repo acme/x --env release --allow-publish";

/// `plan` (or another command) on the `publish` fixture, every API and registry on `api`.
fn release_cmd(api: &FakeApi) -> assert_cmd::Command {
    let mut cmd = plan(api);
    cmd.env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url());
    cmd
}

fn json_of(
    cmd: &mut assert_cmd::Command,
    dir: &std::path::Path,
    args: &[&str],
    code: i32,
) -> Value {
    let out = cmd
        .current_dir(dir)
        .args(args)
        .assert()
        .code(code)
        .get_output()
        .stdout
        .clone();
    serde_json::from_str(&String::from_utf8(out).unwrap()).expect("one JSON object")
}

fn item<'a>(report: &'a Value, area: &str, kind: &str, key: &str) -> Option<&'a Value> {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["area"] == area && i["kind"] == kind && i["key"] == key)
}

fn step<'a>(report: &'a Value, id: &str) -> Option<&'a Value> {
    report["human_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
}

#[test]
fn plan_lists_trusted_publisher_steps() {
    let repo = fixture_repo("publish", SITE);
    let api = fake(&[NPM_LIB, CRATE_TOOL]);
    let report = json_of(&mut release_cmd(&api), repo.path(), &["plan", "--json"], 0);
    let tp = item(&report, "npm", "trusted_publisher", "@acme/lib").expect("npm publisher");
    assert_eq!(tp["action"], "unknown");
    assert_eq!(
        tp["reason"],
        "registered by a person; hoist cannot read it with a CI token"
    );
    assert!(tp["verify"].is_null());
    let tp = item(&report, "crates", "trusted_publisher", "tool").expect("crates publisher");
    assert_eq!(tp["action"], "unknown");

    let npm = step(&report, "npm-trust:@acme/lib").expect("npm step");
    assert_eq!(npm["title"], TRUST_NPM);
    assert_eq!(npm["url"], "https://www.npmjs.com/package/@acme/lib/access");
    let crates = step(&report, "crates-trust:tool").expect("crates step");
    assert_eq!(crates["url"], "https://crates.io/crates/tool/settings");
    assert!(crates["title"].as_str().unwrap().contains("release.yml"));
    assert!(step(&report, "first-publish:@acme/lib").is_none());

    let wf = item(&report, "release", "workflow", "release").expect("workflow item");
    assert_eq!(wf["action"], "ok");
    assert!(wf["reason"].as_str().unwrap().contains("release.yml"));
}

#[test]
fn check_is_clean_when_only_publishers_are_unread() {
    // R6: an unreadable trusted publisher keeps its human step but does not hold check at 2.
    let repo = fixture_repo("publish", SITE);
    let api = fake(&[
        NPM_LIB,
        CRATE_TOOL,
        ("GET /repos/acme/x/environments", 200, ENVS_RELEASE, &[]),
    ]);
    let args = [
        "check", "--json", "--area", "npm", "--area", "crates", "--area", "release",
    ];
    let report = json_of(&mut release_cmd(&api), repo.path(), &args, 0);
    assert!(step(&report, "npm-trust:@acme/lib").is_some());
    let env = item(&report, "release", "environment", "release").expect("env item");
    assert_eq!(env["action"], "ok");
    api.assert_only_get();
}

#[test]
fn plan_adds_release_environment() {
    let repo = fixture_repo("publish", SITE);
    let api = fake(&[NPM_LIB, CRATE_TOOL]);
    let report = json_of(&mut release_cmd(&api), repo.path(), &["plan", "--json"], 0);
    let env = item(&report, "release", "environment", "release").expect("release env");
    assert_eq!(env["action"], "create");
    // Merged with the workflow-inferred environment: listed once, not again under github.
    assert!(item(&report, "github", "environment", "release").is_none());

    // Without a workflow job in that environment the release workflow item is pending.
    std::fs::remove_file(repo.path().join(".github/workflows/release.yml")).unwrap();
    let report = json_of(&mut release_cmd(&api), repo.path(), &["plan", "--json"], 0);
    let env = item(&report, "release", "environment", "release").expect("release env");
    assert_eq!(env["action"], "create");
    let wf = item(&report, "release", "workflow", "release").expect("workflow item");
    assert_eq!(wf["action"], "pending");
    assert_eq!(wf["reason"], "no workflow job uses environment `release`");
}

#[test]
fn apply_provisions_release_environment() {
    let repo = fixture_repo("publish", SITE);
    let api = fake(&[
        ("GET /repos/acme/x", 200, "github/repo_admin.json", &[]),
        (
            "GET /repos/acme/x/environments",
            200,
            "github/environments_empty.json",
            &[],
        ),
        ("GET /repos/acme/x/environments", 200, ENVS_RELEASE, &[]),
        ("PUT /repos/acme/x/environments/release", 200, "{}", &[]),
        (
            "POST /repos/acme/x/environments/release/deployment-branch-policies",
            200,
            "{}",
            &[],
        ),
    ]);
    let args = ["apply", "--yes", "--json", "--area", "release"];
    let report = json_of(&mut release_cmd(&api), repo.path(), &args, 0);
    let env = item(&report, "release", "environment", "release").expect("env item");
    assert_eq!(env["action"], "ok");
    let writes: Vec<String> = api
        .requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        writes,
        [
            "PUT /repos/acme/x/environments/release",
            "POST /repos/acme/x/environments/release/deployment-branch-policies"
        ]
    );
    assert!(
        !api.requests()
            .iter()
            .any(|r| r.path.starts_with("/@") || r.path.starts_with("/api/v1/crates"))
    );
}

#[test]
fn new_package_gets_first_publish_step() {
    let repo = fixture_repo("publish", SITE);
    // No npm route: the registry answers 404 for @acme/lib.
    let api = fake(&[
        CRATE_TOOL,
        ("GET /repos/acme/x/environments", 200, ENVS_RELEASE, &[]),
    ]);
    let report = json_of(&mut release_cmd(&api), repo.path(), &["plan", "--json"], 0);
    let fp = item(&report, "npm", "first_publish", "@acme/lib").expect("first publish");
    assert_eq!(fp["action"], "pending");
    let s = step(&report, "first-publish:@acme/lib").expect("step");
    assert_eq!(
        s["title"],
        "publish the first version by hand; trusted publishing needs an existing package"
    );
    assert_eq!(s["url"], "https://docs.npmjs.com/cli/commands/npm-publish");
    let tool = item(&report, "crates", "first_publish", "tool").expect("tool");
    assert_eq!(tool["action"], "ok");

    let args = ["check", "--json", "--area", "npm"];
    let report = json_of(&mut release_cmd(&api), repo.path(), &args, 2);
    assert!(step(&report, "first-publish:@acme/lib").is_some());
}

#[test]
fn registry_error_is_unknown_not_exit_3() {
    let repo = fixture_repo("publish", SITE);
    let api = fake(&[("GET /api/v1/crates/tool", 503, "{}", &[])]);
    let args = ["plan", "--json", "--area", "crates"];
    let report = json_of(&mut release_cmd(&api), repo.path(), &args, 0);
    let fp = item(&report, "crates", "first_publish", "tool").expect("first publish");
    assert_eq!(fp["action"], "unknown");
}

#[test]
fn area_npm_reads_only_npm() {
    let repo = fixture_repo("publish", SITE);
    let api = FakeApi::start(&[NPM_LIB]);
    let mut cmd = hoist();
    // Every base points at the fake, but no token is set: npm reads need none.
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM_REGISTRY", api.url());
    let report = json_of(
        &mut cmd,
        repo.path(),
        &["plan", "--json", "--area", "npm"],
        0,
    );
    let paths: Vec<String> = api.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/@acme%2Flib"]);
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["area"] == "npm")
    );
}

#[test]
fn plan_still_get_only() {
    let repo = fixture_repo("publish", SITE);
    common::git(repo.path(), &["add", "-A"]);
    common::git(repo.path(), &["commit", "-qm", "init"]);
    let porcelain = || {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let before = porcelain();
    let api = fake(&[]);
    for args in [&["plan", "--json"][..], &["check", "--json"][..]] {
        let _ = release_cmd(&api)
            .current_dir(repo.path())
            .args(args)
            .assert();
    }
    api.assert_only_get();
    assert!(api.requests().iter().any(|r| r.path == "/@acme%2Flib"));
    assert!(
        api.requests()
            .iter()
            .any(|r| r.path == "/api/v1/crates/tool")
    );
    assert_eq!(porcelain(), before);
}

/// The `publish` fixture with a pre-release crate: an invalid release model (R18).
fn prerelease_repo() -> tempfile::TempDir {
    let repo = fixture_repo("publish", SITE);
    std::fs::write(
        repo.path().join("crates/tool/Cargo.toml"),
        "[package]\nname = \"tool\"\nversion = \"0.1.0-alpha.1\"\nedition = \"2024\"\n",
    )
    .unwrap();
    repo
}

#[test]
fn release_model_error_spares_area_github() {
    let repo = prerelease_repo();
    let api = fake(&[]);
    let args = ["plan", "--json", "--area", "github"];
    let report = json_of(&mut release_cmd(&api), repo.path(), &args, 0);
    assert!(item(&report, "release", "model", "release").is_none());
}

#[test]
fn release_model_error_is_one_unknown_item_without_area() {
    let repo = prerelease_repo();
    let api = fake(&[]);
    let report = json_of(&mut release_cmd(&api), repo.path(), &["plan", "--json"], 0);
    let model = item(&report, "release", "model", "release").expect("model item");
    assert_eq!(model["action"], "unknown");
    assert!(
        model["reason"].as_str().unwrap().contains("0.1.0-alpha.1"),
        "{model}"
    );
    let items = report["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["area"] == "github"));
    assert!(
        !items
            .iter()
            .any(|i| i["area"] == "npm" || i["area"] == "crates")
    );
}

#[test]
fn release_model_error_exits_3_for_release_areas() {
    let repo = prerelease_repo();
    let api = fake(&[]);
    for area in ["release", "npm", "crates"] {
        release_cmd(&api)
            .current_dir(repo.path())
            .args(["plan", "--json", "--area", area])
            .assert()
            .code(3);
    }
}

#[test]
fn no_publishing_packages_no_registry_requests() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let registry = FakeApi::start(&[]);
    plan(&api)
        .env("HOIST_NPM_REGISTRY", registry.url())
        .env("HOIST_CRATES_API", registry.url())
        .current_dir(repo.path())
        .args(["plan", "--json"])
        .assert()
        .code(0);
    assert!(registry.requests().is_empty(), "{:?}", registry.requests());
}
