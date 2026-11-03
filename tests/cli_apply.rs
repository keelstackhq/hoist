mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, RouteSpec, fixture_repo, hoist};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use serde_json::Value;
use std::path::Path;

const SITE: &str = "https://github.com/acme/x.git";
const NOW: &str = "2026-10-16T00:00:00Z";
const MINTED: &str = "hoist-canary-mint-apply";

const OK: &str = r#"{"success":true,"errors":[],"messages":[],"result":{}}"#;
const SECRETS_NONE: &str = r#"{"total_count":0,"secrets":[]}"#;
const SECRETS_BOTH: &str = r#"{"total_count":2,"secrets":[{"name":"CLOUDFLARE_API_TOKEN"},{"name":"CLOUDFLARE_ACCOUNT_ID"}]}"#;
const SECRETS_ACCOUNT_ONLY: &str =
    r#"{"total_count":1,"secrets":[{"name":"CLOUDFLARE_ACCOUNT_ID"}]}"#;
const TOKENS_NONE: &str = r#"{"success":true,"errors":[],"messages":[],"result":[]}"#;
const TOKENS_DEPLOY: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"tok-1","name":"hoist:acme/x:deploy","status":"active","expires_on":"2027-10-01T00:00:00Z","issued_on":"2026-10-01T00:00:00Z"}]}"#;
const GROUPS: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"g-pages","name":"Pages Write"}]}"#;

const RULESETS: &str = "GET /repos/acme/x/rulesets";
const SECRETS: &str = "GET /repos/acme/x/actions/secrets";
const ENVS: &str = "GET /repos/acme/x/environments";
const PROJECT: &str = "GET /accounts/acc/pages/projects/keelstack-landing";
const TOKENS: &str = "GET /user/tokens";
const REPO: &str = "GET /repos/acme/x";

fn minted_body() -> String {
    format!(
        r#"{{"success":true,"errors":[],"messages":[],"result":{{"id":"new-id","name":"hoist:acme/x:deploy","value":"{MINTED}"}}}}"#
    )
}

fn keypair() -> (SecretKey, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    (sk, format!(r#"{{"key_id":"kid-1","key":"{b64}"}}"#))
}

/// Route list builder: `set` replaces every response for a key, `then` appends the next
/// response for it (route lists answer in order, the last repeats).
struct Routes(Vec<(String, u16, String)>);

impl Routes {
    /// The `site` fixture (with a `production` environment) fully in sync, admin token.
    fn synced() -> Routes {
        let mut r = Routes(Vec::new());
        for (k, s, b) in [
            (REPO, 200, "github/repo_admin.json"),
            (RULESETS, 200, "github/rulesets_list.json"),
            (
                "GET /repos/acme/x/rulesets/11",
                200,
                "github/ruleset_ok.json",
            ),
            (
                "GET /repos/acme/x/actions/permissions/workflow",
                200,
                "github/actions_permissions.json",
            ),
            (ENVS, 200, "github/environments_production.json"),
            (SECRETS, 200, SECRETS_BOTH),
            ("GET /user/installations", 200, "github/installations.json"),
            (
                "GET /user/installations/501/repositories",
                200,
                "github/installation_repos.json",
            ),
            (PROJECT, 200, "cloudflare/pages_project.json"),
            (TOKENS, 200, TOKENS_DEPLOY),
            (
                "GET /user/tokens/verify",
                200,
                "cloudflare/token_verify_active.json",
            ),
            ("GET /user/tokens/permission_groups", 200, GROUPS),
            ("POST /accounts/acc/pages/projects", 200, OK),
            ("POST /user/tokens", 200, &minted_body()),
            (
                "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN",
                201,
                "empty.json",
            ),
            (
                "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_ACCOUNT_ID",
                201,
                "empty.json",
            ),
            (
                "PUT /repos/acme/x/environments/production",
                200,
                "empty.json",
            ),
            (
                "POST /repos/acme/x/environments/production/deployment-branch-policies",
                200,
                "empty.json",
            ),
            ("POST /repos/acme/x/rulesets", 201, "empty.json"),
            ("PATCH /repos/acme/x", 200, "empty.json"),
        ] {
            r.0.push((k.to_string(), s, b.to_string()));
        }
        r
    }

    fn set(mut self, key: &str, status: u16, body: &str) -> Routes {
        self.0.retain(|(k, ..)| k != key);
        self.0.push((key.to_string(), status, body.to_string()));
        self
    }

    fn then(mut self, key: &str, status: u16, body: &str) -> Routes {
        self.0.push((key.to_string(), status, body.to_string()));
        self
    }

    fn start(&self) -> FakeApi {
        let specs: Vec<RouteSpec> = self
            .0
            .iter()
            .map(|(k, s, b)| (k.as_str(), *s, b.as_str(), &[][..]))
            .collect();
        FakeApi::start(&specs)
    }
}

/// The `site` fixture plus a deploy job that uses the `production` environment.
fn repo() -> tempfile::TempDir {
    let dir = fixture_repo("site", SITE);
    let path = dir.path().join(".github/workflows/deploy.yml");
    let text = std::fs::read_to_string(&path).unwrap();
    let marker = "    runs-on: ubuntu-latest\n";
    assert!(text.contains(marker));
    std::fs::write(
        &path,
        text.replacen(marker, &format!("{marker}    environment: production\n"), 1),
    )
    .unwrap();
    dir
}

fn pages() -> FakeApi {
    FakeApi::start(&[("GET /", 200, "", &[])])
}

fn cmd(api: &FakeApi, pages: &FakeApi, dir: &Path) -> assert_cmd::Command {
    let mut cmd = hoist();
    cmd.current_dir(dir)
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_PAGES_BASE", pages.url())
        .env("HOIST_NOW", NOW)
        .env("HOIST_MAX_SLEEP_MS", "0")
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc");
    cmd
}

struct Run {
    report: Option<Value>,
    stdout: String,
    stderr: String,
}

fn run(api: &FakeApi, dir: &Path, args: &[&str], code: i32) -> Run {
    run_with(api, &pages(), dir, args, code, |_| {})
}

fn run_with(
    api: &FakeApi,
    pages: &FakeApi,
    dir: &Path,
    args: &[&str],
    code: i32,
    setup: impl FnOnce(&mut assert_cmd::Command),
) -> Run {
    let mut c = cmd(api, pages, dir);
    setup(&mut c);
    let out = c.args(args).assert().code(code).get_output().clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stdout.contains("test-token") && !stderr.contains("test-token"));
    let report = serde_json::from_str::<Value>(&stdout).ok();
    if let Some(r) = &report {
        assert!(r["command"].as_str().unwrap().starts_with(args[0]), "{r}");
        assert_eq!(r["exit_code"], code);
    }
    Run {
        report,
        stdout,
        stderr,
    }
}

fn apply(api: &FakeApi, dir: &Path, code: i32) -> Value {
    let r = run(api, dir, &["apply", "--yes", "--json"], code);
    r.report
        .unwrap_or_else(|| panic!("one JSON object; stderr: {}", r.stderr))
}

fn writes(api: &FakeApi) -> Vec<String> {
    api.requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

fn item<'a>(report: &'a Value, kind: &str, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {report}"))
}

/// Live state before any apply: no project, no tokens, no secrets, no environment, no
/// ruleset. After the writes the reads return the synced state.
fn empty_then_synced(key_json: &str) -> Routes {
    Routes::synced()
        .set(PROJECT, 404, "cloudflare/not_found.json")
        .then(PROJECT, 200, "cloudflare/pages_project.json")
        .set(TOKENS, 200, TOKENS_NONE)
        .then(TOKENS, 200, TOKENS_DEPLOY)
        .set(SECRETS, 200, SECRETS_NONE)
        .then(SECRETS, 200, SECRETS_BOTH)
        .set(ENVS, 200, "github/environments_empty.json")
        .then(ENVS, 200, "github/environments_production.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .then(RULESETS, 200, "github/rulesets_list.json")
        .set(
            "GET /repos/acme/x/actions/secrets/public-key",
            200,
            key_json,
        )
}

const FULL_WRITES: [&str; 7] = [
    "POST /accounts/acc/pages/projects",
    "POST /user/tokens",
    "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN",
    "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_ACCOUNT_ID",
    "PUT /repos/acme/x/environments/production",
    "POST /repos/acme/x/environments/production/deployment-branch-policies",
    "POST /repos/acme/x/rulesets",
];

#[test]
fn in_sync_apply_makes_no_writes() {
    let dir = repo();
    let api = Routes::synced().start();
    let report = apply(&api, dir.path(), 0);
    api.assert_only_get();
    assert_eq!(item(&report, "environment", "production")["action"], "ok");
    assert!(!api.requests().is_empty());
}

#[test]
fn full_apply_creates_everything() {
    let dir = repo();
    let (_sk, key_json) = keypair();
    let api = empty_then_synced(&key_json).start();
    let report = apply(&api, dir.path(), 0);
    assert_eq!(writes(&api), FULL_WRITES);
    // Preflight verified the Cloudflare token before the first write.
    let requests = api.requests();
    let verify = requests
        .iter()
        .position(|r| r.path == "/user/tokens/verify")
        .expect("token verify");
    let first_write = requests.iter().position(|r| r.method != "GET").unwrap();
    assert!(verify < first_write);
    // The final report is the post-apply check.
    assert_eq!(
        item(&report, "ruleset", "hoist: default branch")["action"],
        "ok"
    );
    assert_eq!(
        item(&report, "secret", "CLOUDFLARE_API_TOKEN")["action"],
        "ok"
    );
    assert_eq!(
        item(&report, "required_check", "ci-ok")["verify"]["ok"],
        true
    );
}

#[test]
fn non_tty_without_yes_exits_3_before_writes() {
    let dir = repo();
    let (_sk, key_json) = keypair();
    let api = empty_then_synced(&key_json).start();
    let r = run_with(&api, &pages(), dir.path(), &["apply", "--json"], 3, |c| {
        c.pipe_stdin("/dev/null").expect("stdin from /dev/null");
    });
    assert!(
        r.stderr.contains("use --yes in non-interactive runs"),
        "{}",
        r.stderr
    );
    assert!(r.stdout.is_empty());
    api.assert_only_get();
}

#[test]
fn failure_stops_and_reports() {
    let dir = repo();
    let api = Routes::synced()
        .set(ENVS, 200, "github/environments_empty.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .set(REPO, 200, &repo_with("allow_auto_merge", false))
        .set("POST /repos/acme/x/rulesets", 403, "github/forbidden.json")
        .start();
    let report = apply(&api, dir.path(), 1);
    let env = item(&report, "environment", "production");
    assert_eq!(env["reason"], "applied");
    assert_eq!(env["action"], "ok");
    let rs = item(&report, "ruleset", "hoist: default branch");
    assert_eq!(rs["action"], "create");
    let reason = rs["reason"].as_str().unwrap();
    assert!(reason.starts_with("failed: "), "{reason}");
    assert!(reason.contains("403"), "{reason}");
    let setting = item(&report, "repo_setting", "allow_auto_merge");
    assert_eq!(setting["reason"], "not attempted");
    assert_eq!(setting["action"], "update");
    let requests = api.requests();
    let last = requests.last().unwrap();
    assert_eq!(
        format!("{} {}", last.method, last.path),
        "POST /repos/acme/x/rulesets",
        "no request after the failed write"
    );
}

fn repo_with(field: &str, value: bool) -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api/github/repo_admin.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v[field] = Value::Bool(value);
    v.to_string()
}

#[test]
fn rerun_after_partial_apply_continues() {
    let dir = repo();
    let api = Routes::synced()
        .set(ENVS, 200, "github/environments_empty.json")
        .then(ENVS, 200, "github/environments_production.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .then(RULESETS, 200, "github/rulesets_list.json")
        .start();
    apply(&api, dir.path(), 0);
    assert_eq!(
        writes(&api),
        [
            "PUT /repos/acme/x/environments/production",
            "POST /repos/acme/x/environments/production/deployment-branch-policies",
            "POST /repos/acme/x/rulesets",
        ]
    );
}

#[test]
fn non_admin_token_exits_3_before_writes() {
    let dir = repo();
    let api = Routes::synced()
        .set(REPO, 200, "github/repo.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .start();
    let r = run(&api, dir.path(), &["apply", "--yes", "--json"], 3);
    assert!(
        r.stderr
            .contains("the token needs admin on acme/x (Administration: write)"),
        "{}",
        r.stderr
    );
    api.assert_only_get();
}

#[test]
fn inactive_cloudflare_token_exits_3_before_writes() {
    let dir = repo();
    let api = Routes::synced()
        .set(PROJECT, 404, "cloudflare/not_found.json")
        .set(
            "GET /user/tokens/verify",
            200,
            "cloudflare/token_verify_inactive.json",
        )
        .start();
    let r = run(&api, dir.path(), &["apply", "--yes"], 3);
    assert!(r.stderr.contains("/user/tokens/verify"), "{}", r.stderr);
    api.assert_only_get();
}

#[test]
fn pending_human_step_exits_2() {
    let dir = repo();
    let api = Routes::synced()
        .set(ENVS, 200, "github/environments_empty.json")
        .then(ENVS, 200, "github/environments_production.json")
        .set(
            "GET /user/installations/501/repositories",
            200,
            "github/installation_repos_other.json",
        )
        .start();
    let report = apply(&api, dir.path(), 2);
    assert_eq!(writes(&api).len(), 2, "environment written");
    assert_eq!(item(&report, "app", "renovate")["action"], "pending");
    assert!(
        report["human_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "app:renovate")
    );
}

#[test]
fn domain_zone_human_step_exits_2() {
    let dir = repo();
    std::fs::write(
        dir.path().join("apps/site/hoist.toml"),
        "[deploy]\nprovider = \"cloudflare-pages\"\nproject = \"keelstack-landing\"\ndomain = \"example.com\"\n",
    )
    .unwrap();
    let zone = r#"{"success":false,"errors":[{"code":8000018,"message":"The zone for example.com is not on this Account"}],"messages":[],"result":null}"#;
    let api = Routes::synced()
        .set(
            "POST /accounts/acc/pages/projects/keelstack-landing/domains",
            400,
            zone,
        )
        .start();
    let report = apply(&api, dir.path(), 2);
    assert_eq!(
        writes(&api),
        ["POST /accounts/acc/pages/projects/keelstack-landing/domains"]
    );
    assert_eq!(
        item(&report, "custom_domain", "example.com")["action"],
        "pending"
    );
    assert!(
        report["human_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "domain:example.com")
    );
}

#[test]
fn canary_mint_never_printed() {
    let dir = repo();
    let (sk, key_json) = keypair();
    let api = empty_then_synced(&key_json).start();
    let r = run(&api, dir.path(), &["apply", "--yes", "-v", "--json"], 0);
    assert!(!r.stdout.contains(MINTED), "stdout leaked the minted token");
    assert!(!r.stderr.contains(MINTED), "stderr leaked the minted token");
    assert!(!r.stdout.contains("hoist-canary") && !r.stderr.contains("hoist-canary"));
    assert!(
        r.stderr.contains("POST /user/tokens"),
        "-v logs writes: {}",
        r.stderr
    );
    let mut sealed = 0;
    for req in api.requests() {
        let body = req.body.clone().unwrap_or_default();
        assert!(!body.contains(MINTED), "{} {}", req.method, req.path);
        if req.path == "/repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN" {
            let b: Value = serde_json::from_str(&body).unwrap();
            let bytes = STANDARD
                .decode(b["encrypted_value"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                String::from_utf8(sk.unseal(&bytes).unwrap()).unwrap(),
                MINTED
            );
            sealed += 1;
        }
    }
    assert_eq!(sealed, 1);
}

#[test]
fn read_commands_still_get_only() {
    let dir = repo();
    let (_sk, key_json) = keypair();
    let api = empty_then_synced(&key_json).start();
    run(&api, dir.path(), &["plan", "--json"], 0);
    // Route lists answer in order: `plan` saw the empty state, `check` the synced one.
    run(&api, dir.path(), &["check", "--json"], 0);
    run(&api, dir.path(), &["auth", "check", "--json"], 0);
    run(&api, dir.path(), &["explain", "--json"], 0);
    assert!(!api.requests().is_empty());
    api.assert_only_get();
}

#[test]
fn created_project_is_not_probed_in_the_same_run() {
    let dir = repo();
    let api = Routes::synced()
        .set(PROJECT, 404, "cloudflare/not_found.json")
        .then(PROJECT, 200, "cloudflare/pages_project.json")
        .start();
    // No deployment yet: the Pages URL answers 404.
    let pages = FakeApi::start(&[("GET /", 404, "", &[])]);
    let r = run_with(
        &api,
        &pages,
        dir.path(),
        &["apply", "--yes", "--json"],
        0,
        |_| {},
    );
    let report = r.report.expect("one JSON object");
    assert_eq!(writes(&api), ["POST /accounts/acc/pages/projects"]);
    let project = item(&report, "pages_project", "keelstack-landing");
    assert_eq!(project["action"], "ok");
    assert!(project["verify"].is_null(), "{project}");
    assert_eq!(
        project["reason"],
        "created in this run; verified by `hoist check` after the first deploy"
    );
    assert!(pages.requests().is_empty(), "not probed");
}

#[test]
fn reread_error_after_writes_exits_1_with_applied_items() {
    let dir = repo();
    let api = Routes::synced()
        .set(ENVS, 200, "github/environments_empty.json")
        .then(REPO, 500, "")
        .start();
    let report = apply(&api, dir.path(), 1);
    assert_eq!(writes(&api).len(), 2, "environment written");
    let env = item(&report, "environment", "production");
    assert_eq!(env["reason"], "applied");
    let late = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| {
            i["reason"]
                .as_str()
                .unwrap_or("")
                .contains("after the writes")
        })
        .unwrap_or_else(|| panic!("no re-read error item in {report}"));
    assert!(late["reason"].as_str().unwrap().contains("500"), "{late}");
}

#[test]
fn partial_report_keeps_non_change_items() {
    let dir = repo();
    let api = Routes::synced()
        .set(RULESETS, 200, "github/empty_list.json")
        .set(
            "GET /user/installations/501/repositories",
            200,
            "github/installation_repos_other.json",
        )
        .set("POST /repos/acme/x/rulesets", 403, "github/forbidden.json")
        .start();
    let report = apply(&api, dir.path(), 1);
    assert_eq!(item(&report, "app", "renovate")["action"], "pending");
    assert_eq!(item(&report, "environment", "production")["action"], "ok");
}

#[test]
fn permission_group_failure_exits_3_before_writes() {
    let dir = repo();
    let (_sk, key_json) = keypair();
    let api = empty_then_synced(&key_json)
        .set(
            "GET /user/tokens/permission_groups",
            403,
            "cloudflare/forbidden.json",
        )
        .start();
    let r = run(&api, dir.path(), &["apply", "--yes"], 3);
    assert!(
        r.stderr.contains("/user/tokens/permission_groups"),
        "{}",
        r.stderr
    );
    api.assert_only_get();
}

const SECRETS_DATED: &str = r#"{"total_count":2,"secrets":[{"name":"CLOUDFLARE_API_TOKEN","updated_at":"2026-10-03T00:00:00Z"},{"name":"CLOUDFLARE_ACCOUNT_ID","updated_at":"2026-01-01T00:00:00Z"}]}"#;
const TOKENS_ORPHAN: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"older","name":"hoist:acme/x:deploy","status":"active","expires_on":"2027-09-01T00:00:00Z","issued_on":"2026-09-01T00:00:00Z"},{"id":"live","name":"hoist:acme/x:deploy","status":"active","expires_on":"2027-10-01T00:00:00Z","issued_on":"2026-10-01T00:00:00Z"},{"id":"orphan","name":"hoist:acme/x:deploy","status":"active","expires_on":"2027-10-05T00:00:00Z","issued_on":"2026-10-05T00:00:00Z"}]}"#;

#[test]
fn prune_keeps_the_live_token_and_a_newer_orphan() {
    let dir = repo();
    let api = Routes::synced()
        .set(SECRETS, 200, SECRETS_DATED)
        .set(TOKENS, 200, TOKENS_ORPHAN)
        .set("DELETE /user/tokens/older", 200, OK)
        .start();
    run(
        &api,
        dir.path(),
        &["apply", "--yes", "--prune", "--json"],
        0,
    );
    assert_eq!(writes(&api), ["DELETE /user/tokens/older"]);
}

const INSTALLATION: &str = "GET /installation/repositories";

/// An App installation token (`ghs_…`, from actions/create-github-app-token) reads the repo
/// without `permissions`: the installation's repository list stands in for the admin check.
fn app_token_run(listed: (u16, &str), code: i32) -> (FakeApi, Run) {
    let dir = repo();
    let api = Routes::synced()
        .set(REPO, 200, "github/repo.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .then(RULESETS, 200, "github/rulesets_list.json")
        .set(INSTALLATION, listed.0, listed.1)
        .start();
    let r = run_with(
        &api,
        &pages(),
        dir.path(),
        &["apply", "--yes", "--json"],
        code,
        |c| {
            c.env("GITHUB_TOKEN", "ghs_app-test-token");
        },
    );
    // run_with already refuses any `test-token` in the output; the App token in full too.
    assert!(!r.stdout.contains("ghs_app") && !r.stderr.contains("ghs_app"));
    (api, r)
}

#[test]
fn an_app_installation_token_applies_when_the_installation_reaches_the_repo() {
    // The shape of GitHub's own example (rest-api-description, `GET /installation/repositories`,
    // example `repository-paginated-2`): no `permissions` on the repositories. Names compare
    // without case.
    let listed = "github/installation_repositories.json";
    let (api, _) = app_token_run((200, listed), 0);
    assert_eq!(writes(&api), ["POST /repos/acme/x/rulesets"]);
}

#[test]
fn an_app_installation_token_that_cannot_reach_the_repo_exits_3_before_writes() {
    let other = r#"{"total_count":1,"repositories":[{"full_name":"acme/other"}]}"#;
    for (status, body, says) in [
        (200, other, "the app installation does not include acme/x"),
        (
            403,
            r#"{"message":"no"}"#,
            "cannot list the app installation's repositories",
        ),
        (
            404,
            "{}",
            "cannot list the app installation's repositories (404)",
        ),
    ] {
        let (api, r) = app_token_run((status, body), 3);
        assert!(r.stderr.contains(says), "{}", r.stderr);
        api.assert_only_get();
    }
}

#[test]
fn a_token_without_admin_never_asks_for_an_installation() {
    let dir = repo();
    let api = Routes::synced()
        .set(REPO, 200, "github/repo.json")
        .set(RULESETS, 200, "github/empty_list.json")
        .start();
    run_with(
        &api,
        &pages(),
        dir.path(),
        &["apply", "--yes", "--json"],
        3,
        |c| {
            c.env("GITHUB_TOKEN", "github_pat_test-token");
        },
    );
    assert!(
        api.requests()
            .iter()
            .all(|r| r.path != "/installation/repositories"),
        "a user token is never treated as an installation"
    );
}

#[test]
fn an_app_token_reported_without_admin_still_takes_the_installation_route() {
    // If GitHub reports `permissions` with `admin: false` for an installation token.
    let dir = repo();
    let no_admin = repo_with_admin(false);
    let api = Routes::synced()
        .set(REPO, 200, &no_admin)
        .set(RULESETS, 200, "github/empty_list.json")
        .then(RULESETS, 200, "github/rulesets_list.json")
        .set(
            INSTALLATION,
            200,
            r#"{"repositories":[{"full_name":"acme/x"}]}"#,
        )
        .start();
    run_with(
        &api,
        &pages(),
        dir.path(),
        &["apply", "--yes", "--json"],
        0,
        |c| {
            c.env("GITHUB_TOKEN", "ghs_app-test-token");
        },
    );
    assert_eq!(writes(&api), ["POST /repos/acme/x/rulesets"]);
}

fn repo_with_admin(admin: bool) -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api/github/repo_admin.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["permissions"]["admin"] = Value::Bool(admin);
    v.to_string()
}

const RULESET_11: &str = "GET /repos/acme/x/rulesets/11";
const PUT_RULESET: &str = "PUT /repos/acme/x/rulesets/11";

/// `hoist: default branch` (id 11) requiring `checks`, plus a `pull_request` rule when asked:
/// a rule hoist does not model, which its PUT would drop.
fn live_ruleset(checks: &[&str], pull_request: bool) -> String {
    let checks: Vec<Value> = checks
        .iter()
        .map(|c| serde_json::json!({ "context": c }))
        .collect();
    let mut rules = vec![
        serde_json::json!({"type": "required_status_checks", "parameters": {
        "strict_required_status_checks_policy": false, "required_status_checks": checks}}),
        // The site fixture wants a merge queue (its CI has a merge_group trigger).
        serde_json::json!({"type": "merge_queue", "parameters": {}}),
    ];
    if pull_request {
        rules.push(serde_json::json!({"type": "pull_request",
            "parameters": {"required_approving_review_count": 1}}));
    }
    serde_json::json!({"id": 11, "name": "hoist: default branch", "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": rules})
    .to_string()
}

fn adopt(ruleset: &str, extra: &[&str], code: i32) -> (FakeApi, Value) {
    adopt_then(ruleset, ruleset, extra, code)
}

/// As `adopt`, with the ruleset read after the writes.
fn adopt_then(ruleset: &str, after: &str, extra: &[&str], code: i32) -> (FakeApi, Value) {
    let dir = repo();
    let api = Routes::synced()
        .set(RULESET_11, 200, ruleset)
        .then(RULESET_11, 200, after)
        .set(PUT_RULESET, 200, "empty.json")
        .start();
    let mut args = vec!["apply", "--yes", "--json", "--adopt"];
    args.extend(extra);
    let r = run(&api, dir.path(), &args, code);
    (api, r.report.unwrap_or(Value::Null))
}

#[test]
fn adopt_keeps_a_stricter_live_ruleset_and_says_so() {
    // Live requires one more check than hoist wants: kept, ok, nothing written.
    let (api, report) = adopt(&live_ruleset(&["ci-ok", "lint"], false), &[], 0);
    api.assert_only_get();
    let rs = item(&report, "ruleset", "hoist: default branch");
    assert_eq!(rs["action"], "ok", "{report:#}");
    assert!(
        rs["reason"]
            .as_str()
            .unwrap()
            .contains("kept stricter live setting"),
        "{rs}"
    );
}

#[test]
fn adopt_adds_its_checks_beside_the_live_ones() {
    let after = live_ruleset(&["ci-ok", "lint"], false);
    let (api, _) = adopt_then(&live_ruleset(&["lint"], false), &after, &[], 0);
    assert_eq!(writes(&api), [PUT_RULESET]);
    let put = api
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    let contexts: Vec<&str> = body["rules"][0]["parameters"]["required_status_checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["context"].as_str().unwrap())
        .collect();
    assert_eq!(contexts, ["ci-ok", "lint"]);
}

#[test]
fn adopt_never_drops_a_rule_hoist_does_not_model() {
    // An update would replace the ruleset's rules and drop `pull_request`: drift, no write.
    let (api, report) = adopt(&live_ruleset(&["lint"], true), &[], 1);
    api.assert_only_get();
    let rs = item(&report, "ruleset", "hoist: default branch");
    assert_eq!(rs["action"], "drift", "{report:#}");
    assert!(
        rs["reason"].as_str().unwrap().contains("pull_request"),
        "{rs}"
    );
}

#[test]
fn adopt_keeps_read_only_actions_permissions() {
    // hoist.toml asks for write; live read is stricter: kept.
    let dir = repo();
    let toml = dir.path().join("hoist.toml");
    let text = std::fs::read_to_string(&toml).unwrap_or_default();
    let text = if text.contains("[github]") {
        text.replacen("[github]", "[github]\nactions_permissions = \"write\"", 1)
    } else {
        format!("{text}\n[github]\nactions_permissions = \"write\"\n")
    };
    std::fs::write(&toml, text).unwrap();
    let api = Routes::synced().start();
    let r = run(
        &api,
        dir.path(),
        &["apply", "--yes", "--json", "--adopt"],
        0,
    );
    api.assert_only_get();
    let report = r.report.unwrap();
    let a = item(&report, "actions_permissions", "default");
    assert_eq!(a["action"], "ok", "{report:#}");
    assert!(
        a["reason"]
            .as_str()
            .unwrap()
            .contains("kept stricter live setting")
    );
}

#[test]
fn adopt_with_prune_is_a_usage_error() {
    let dir = repo();
    let api = Routes::synced().start();
    let r = run(
        &api,
        dir.path(),
        &["apply", "--yes", "--adopt", "--prune"],
        3,
    );
    assert!(
        r.stderr.contains("--adopt") && r.stderr.contains("--prune"),
        "{}",
        r.stderr
    );
    assert!(api.requests().is_empty());
}

#[test]
fn adopt_never_opens_up_visibility() {
    // hoist.toml asks for public; the live repo is private: kept, nothing written.
    let dir = repo();
    let toml = dir.path().join("hoist.toml");
    let text = std::fs::read_to_string(&toml).unwrap_or_default();
    let text = if text.contains("[github]") {
        text.replacen("[github]", "[github]\nvisibility = \"public\"", 1)
    } else {
        format!("{text}\n[github]\nvisibility = \"public\"\n")
    };
    std::fs::write(&toml, text).unwrap();
    let mut private: Value = serde_json::from_str(&repo_with_admin(true)).unwrap();
    private["visibility"] = "private".into();
    private["private"] = true.into();
    let api = Routes::synced()
        .set(REPO, 200, &private.to_string())
        .start();
    let r = run(
        &api,
        dir.path(),
        &["apply", "--yes", "--json", "--adopt"],
        0,
    );
    api.assert_only_get();
    let report = r.report.unwrap();
    let v = item(&report, "repo_setting", "visibility");
    assert_eq!(v["action"], "ok", "{report:#}");
    assert!(
        v["reason"]
            .as_str()
            .unwrap()
            .contains("kept stricter live setting")
    );
}

#[test]
fn adopt_keeps_merge_queue_settings_it_does_not_write() {
    // A live queue with its own settings (SQUASH): an update would replace them, so drift.
    let mut rs: Value = serde_json::from_str(&live_ruleset(&["lint"], false)).unwrap();
    rs["rules"][1]["parameters"] = serde_json::json!({"merge_method": "SQUASH"});
    let (api, report) = adopt(&rs.to_string(), &[], 1);
    api.assert_only_get();
    let r = item(&report, "ruleset", "hoist: default branch");
    assert_eq!(r["action"], "drift", "{report:#}");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .contains("merge queue settings"),
        "{r}"
    );
}

#[test]
fn plan_and_check_preview_adopt_with_gets_only() {
    for cmd in ["plan", "check"] {
        let dir = repo();
        let api = Routes::synced()
            .set(RULESET_11, 200, &live_ruleset(&["ci-ok", "lint"], false))
            .start();
        let r = run(&api, dir.path(), &[cmd, "--json", "--adopt"], 0);
        api.assert_only_get();
        let report = r.report.unwrap();
        let rs = item(&report, "ruleset", "hoist: default branch");
        assert_eq!(rs["action"], "ok", "{cmd}: {report:#}");
        assert!(
            rs["reason"]
                .as_str()
                .unwrap()
                .contains("kept stricter live setting")
        );
    }
}

#[test]
fn adopt_never_rewrites_a_sites_production_branch() {
    let dir = repo();
    let api = Routes::synced()
        .set(PROJECT, 200, "cloudflare/pages_project_develop.json")
        .start();
    let r = run(
        &api,
        dir.path(),
        &["apply", "--yes", "--json", "--adopt"],
        1,
    );
    api.assert_only_get();
    let report = r.report.unwrap();
    let p = item(&report, "pages_project", "keelstack-landing");
    assert_eq!(p["action"], "drift", "{report:#}");
    assert!(p["reason"].as_str().unwrap().contains("--adopt"), "{p}");
}

#[test]
fn adopt_leaves_a_missing_token_to_rotate_when_older_ones_exist() {
    let dir = repo();
    let api = Routes::synced()
        .set(SECRETS, 200, SECRETS_ACCOUNT_ONLY)
        .start();
    let r = run(
        &api,
        dir.path(),
        &["apply", "--yes", "--json", "--adopt"],
        2,
    );
    api.assert_only_get();
    let report = r.report.unwrap();
    let s = item(&report, "secret", "CLOUDFLARE_API_TOKEN");
    assert_eq!(s["action"], "pending", "{report:#}");
    assert!(
        report["human_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["id"] == "rotate:CLOUDFLARE_API_TOKEN")
    );
}
