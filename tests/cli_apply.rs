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
    let pages = pages();
    let out = cmd(api, &pages, dir)
        .args(args)
        .assert()
        .code(code)
        .get_output()
        .clone();
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
    let r = run(&api, dir.path(), &["apply", "--json"], 3);
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
