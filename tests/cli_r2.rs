//! `[cloudflare.r2]` (spec §6.2): config, plan/check items, apply and the credential sinks.
//! Every run uses `--area cloudflare` on the `bare` fixture, so GitHub is not read.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, RouteSpec, fixture_repo, hoist, write_script};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const SITE: &str = "https://github.com/acme/x.git";
const MINTED: &str = "hoist-canary-r2-token-value";
const R2_TOML: &str =
    "[cloudflare.r2]\nbucket = \"app-backups\"\nretention_days = 30\nprefix = \"db/\"\n";

const BUCKET: &str = "GET /accounts/acc/r2/buckets/app-backups";
const LIFECYCLE: &str = "GET /accounts/acc/r2/buckets/app-backups/lifecycle";
const TOKENS: &str = "GET /user/tokens";
const VERIFY: &str = "GET /user/tokens/verify";
const GROUPS: &str = "GET /user/tokens/permission_groups";
const POST_BUCKET: &str = "POST /accounts/acc/r2/buckets";
const PUT_LIFECYCLE: &str = "PUT /accounts/acc/r2/buckets/app-backups/lifecycle";
const POST_TOKEN: &str = "POST /user/tokens";
const REVOKE: &str = "DELETE /user/tokens/new-id";

const NOT_FOUND: &str = r#"{"success":false,"errors":[{"code":10006,"message":"bucket does not exist"}],"result":null}"#;
const FORBIDDEN: &str =
    r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}],"result":null}"#;
const BUCKET_OK: &str = r#"{"success":true,"errors":[],"result":{"name":"app-backups"}}"#;
const VERIFY_OK: &str = r#"{"success":true,"errors":[],"result":{"id":"tok1","status":"active"}}"#;
const TOKENS_NONE: &str = r#"{"success":true,"errors":[],"result":[]}"#;
const TOKENS_R2: &str = r#"{"success":true,"errors":[],"result":[{"id":"new-id","name":"hoist:acme/x:r2:app-backups","status":"active"}]}"#;
const GROUP_LIST: &str = r#"{"success":true,"errors":[],"result":[{"id":"g-other","name":"Pages Write"},{"id":"g-read","name":"Workers R2 Storage Bucket Item Read"},{"id":"g-write","name":"Workers R2 Storage Bucket Item Write"}]}"#;
const OK: &str = r#"{"success":true,"errors":[],"result":{}}"#;

const FOREIGN: &str = r#"{"id":"keep-tmp","enabled":true,"conditions":{"prefix":"tmp/"},"deleteObjectsTransition":{"condition":{"type":"Age","maxAge":86400}}}"#;
const HOIST_RULE: &str = r#"{"id":"hoist: retention","enabled":true,"conditions":{"prefix":"db/"},"deleteObjectsTransition":{"condition":{"type":"Age","maxAge":2592000}},"abortMultipartUploadsTransition":{"condition":{"type":"Age","maxAge":86400}}}"#;

fn rules(list: &[&str]) -> String {
    format!(
        r#"{{"success":true,"errors":[],"result":{{"rules":[{}]}}}}"#,
        list.join(",")
    )
}

fn minted() -> String {
    format!(
        r#"{{"success":true,"errors":[],"result":{{"id":"new-id","name":"hoist:acme/x:r2:app-backups","value":"{MINTED}"}}}}"#
    )
}

fn secret_key_hex() -> String {
    Sha256::digest(MINTED.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Route builder: `then` appends the next response for a key (the last repeats).
#[derive(Default)]
struct Routes(Vec<(String, u16, String)>);

impl Routes {
    fn on(mut self, key: &str, status: u16, body: &str) -> Routes {
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

/// Nothing exists yet; after the writes the re-read shows the synced state.
fn fresh() -> Routes {
    Routes::default()
        .on(BUCKET, 404, NOT_FOUND)
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN]))
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, HOIST_RULE]))
        .on(TOKENS, 200, TOKENS_NONE)
        .on(TOKENS, 200, TOKENS_R2)
        .on(VERIFY, 200, VERIFY_OK)
        .on(GROUPS, 200, GROUP_LIST)
        .on(POST_BUCKET, 200, OK)
        .on(PUT_LIFECYCLE, 200, OK)
        .on(POST_TOKEN, 200, &minted())
        .on(REVOKE, 200, OK)
}

/// Everything exists and is in line.
fn synced() -> Routes {
    Routes::default()
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, HOIST_RULE]))
        .on(TOKENS, 200, TOKENS_R2)
        .on(VERIFY, 200, VERIFY_OK)
        .on(GROUPS, 200, GROUP_LIST)
}

fn repo(toml: &str) -> tempfile::TempDir {
    let dir = fixture_repo("bare", SITE);
    std::fs::write(dir.path().join("hoist.toml"), toml).unwrap();
    dir
}

fn cmd(api: &FakeApi, dir: &Path) -> assert_cmd::Command {
    let mut c = hoist();
    c.current_dir(dir)
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_MAX_SLEEP_MS", "0")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc");
    c
}

struct Out {
    stdout: String,
    stderr: String,
}

fn run(c: &mut assert_cmd::Command, args: &[&str], code: i32) -> Out {
    let out = c.args(args).assert().code(code).get_output().clone();
    Out {
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

fn json(out: &Out) -> Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("one JSON object: {e}\n{}\n{}", out.stdout, out.stderr))
}

fn item<'a>(report: &'a Value, kind: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {report}"))
}

fn writes(api: &FakeApi) -> Vec<String> {
    api.requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

fn body_of(api: &FakeApi, method: &str, path: &str) -> Value {
    let requests = api.requests();
    let r = requests
        .iter()
        .find(|r| r.method == method && r.path == path)
        .unwrap_or_else(|| panic!("no {method} {path}"));
    serde_json::from_str(r.body.as_deref().expect("body")).unwrap()
}

/// No token value, derived secret or access key line in what a run printed.
fn assert_no_leak(out: &Out) {
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains(MINTED), "token value leaked:\n{text}");
        assert!(
            !text.contains(&secret_key_hex()),
            "derived secret leaked:\n{text}"
        );
    }
}

// ---- config ----

fn explain_fails(toml: &str, needle: &str) {
    let dir = repo(toml);
    let api = FakeApi::start(&[]);
    let out = run(&mut cmd(&api, dir.path()), &["explain"], 3);
    assert!(out.stderr.contains(needle), "{needle}: {}", out.stderr);
    api.assert_only_get();
}

#[test]
fn r2_config_parses() {
    let dir = repo(R2_TOML);
    let api = FakeApi::start(&[]);
    let out = run(&mut cmd(&api, dir.path()), &["explain", "--json"], 0);
    let report = json(&out);
    let it = item(&report, "r2_bucket");
    assert_eq!(it["key"], "app-backups");
    assert_eq!(it["area"], "cloudflare");
    assert_eq!(it["source"], "hoist.toml:cloudflare.r2");
}

#[test]
fn unknown_r2_key_exits_3() {
    explain_fails(
        "[cloudflare.r2]\nbucket = \"app-backups\"\nretention_days = 30\nretain = 1\n",
        "retain",
    );
}

#[test]
fn bad_bucket_names_exit_3() {
    for bad in [
        "ab",
        "-abc",
        "abc-",
        "Abc",
        "a_b_c",
        &"a".repeat(64),
        "a.b.c",
    ] {
        explain_fails(
            &format!("[cloudflare.r2]\nbucket = \"{bad}\"\nretention_days = 30\n"),
            "bucket",
        );
    }
}

#[test]
fn retention_out_of_range_exits_3() {
    for bad in ["0", "3651", "-5"] {
        explain_fails(
            &format!("[cloudflare.r2]\nbucket = \"app-backups\"\nretention_days = {bad}\n"),
            "retention_days",
        );
    }
}

#[test]
fn bucket_and_retention_are_required() {
    explain_fails("[cloudflare.r2]\nretention_days = 30\n", "bucket");
    explain_fails(
        "[cloudflare.r2]\nbucket = \"app-backups\"\n",
        "retention_days",
    );
}

#[test]
fn r2_needs_an_account_id() {
    let dir = repo(R2_TOML);
    let api = FakeApi::start(&[]);
    let mut c = cmd(&api, dir.path());
    c.env_remove("CLOUDFLARE_ACCOUNT_ID");
    let out = run(&mut c, &["plan", "--area", "cloudflare"], 3);
    assert!(out.stderr.contains("account"), "{}", out.stderr);
}

// ---- plan and check ----

#[test]
fn plan_of_a_fresh_account_creates_all_three_and_only_reads() {
    let dir = repo(R2_TOML);
    let api = fresh().start();
    let out = run(
        &mut cmd(&api, dir.path()),
        &["plan", "--area", "cloudflare", "--json"],
        0,
    );
    let report = json(&out);
    for kind in ["r2_bucket", "r2_lifecycle"] {
        let it = item(&report, kind);
        assert_eq!(it["action"], "create", "{it}");
        assert_eq!(it["key"], "app-backups");
        assert_eq!(it["area"], "cloudflare");
    }
    let t = item(&report, "r2_token");
    assert_eq!(t["action"], "create", "{t}");
    assert_eq!(t["key"], "hoist:acme/x:r2:app-backups");
    api.assert_only_get();
    assert_no_leak(&out);
}

#[test]
fn plan_and_check_of_a_synced_account_are_ok() {
    let dir = repo(R2_TOML);
    let api = synced().start();
    for command in ["plan", "check"] {
        let out = run(
            &mut cmd(&api, dir.path()),
            &[command, "--area", "cloudflare", "--json"],
            0,
        );
        let report = json(&out);
        for kind in ["r2_bucket", "r2_lifecycle", "r2_token"] {
            assert_eq!(item(&report, kind)["action"], "ok", "{command} {kind}");
        }
        assert!(
            item(&report, "r2_token")["reason"]
                .as_str()
                .unwrap()
                .contains("shown only at creation")
        );
    }
    api.assert_only_get();
}

#[test]
fn check_of_a_fresh_account_exits_1() {
    let dir = repo(R2_TOML);
    let api = fresh().start();
    run(
        &mut cmd(&api, dir.path()),
        &["check", "--area", "cloudflare", "--json"],
        1,
    );
    api.assert_only_get();
}

#[test]
fn a_different_rule_is_an_update_naming_the_field() {
    let dir = repo(R2_TOML);
    let other = HOIST_RULE.replace("2592000", "86400");
    let api = Routes::default()
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, &other]))
        .on(TOKENS, 200, TOKENS_R2)
        .start();
    let out = run(
        &mut cmd(&api, dir.path()),
        &["plan", "--area", "cloudflare", "--json"],
        0,
    );
    let report = json(&out);
    let it = item(&report, "r2_lifecycle");
    assert_eq!(it["action"], "update", "{it}");
    assert!(
        it["reason"].as_str().unwrap().contains("retention_days"),
        "{it}"
    );
    assert_eq!(item(&report, "r2_bucket")["action"], "ok");
    api.assert_only_get();
}

#[test]
fn an_existing_bucket_without_the_rule_creates_the_lifecycle() {
    let dir = repo(R2_TOML);
    let api = Routes::default()
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN]))
        .on(TOKENS, 200, TOKENS_R2)
        .start();
    let out = run(
        &mut cmd(&api, dir.path()),
        &["plan", "--area", "cloudflare", "--json"],
        0,
    );
    let report = json(&out);
    assert_eq!(item(&report, "r2_bucket")["action"], "ok");
    assert_eq!(item(&report, "r2_lifecycle")["action"], "create");
}

#[test]
fn a_403_on_the_bucket_is_unknown() {
    let dir = repo(R2_TOML);
    let api = Routes::default()
        .on(BUCKET, 403, FORBIDDEN)
        .on(TOKENS, 403, FORBIDDEN)
        .start();
    let out = run(
        &mut cmd(&api, dir.path()),
        &["plan", "--area", "cloudflare", "--json"],
        0,
    );
    let report = json(&out);
    for kind in ["r2_bucket", "r2_lifecycle", "r2_token"] {
        assert_eq!(item(&report, kind)["action"], "unknown", "{kind}");
    }
    api.assert_only_get();
}

#[test]
fn prune_does_not_revoke_the_r2_token() {
    let dir = repo(R2_TOML);
    let api = synced().start();
    run(
        &mut cmd(&api, dir.path()),
        &[
            "apply",
            "--yes",
            "--prune",
            "--area",
            "cloudflare",
            "--json",
        ],
        0,
    );
    api.assert_only_get();
}

// ---- apply: refusals ----

fn refused(args: &[&str], setup: impl FnOnce(&mut assert_cmd::Command), needle: &str) {
    let dir = repo(R2_TOML);
    let api = fresh().start();
    let mut c = cmd(&api, dir.path());
    setup(&mut c);
    let mut all = vec!["apply", "--yes", "--area", "cloudflare"];
    all.extend_from_slice(args);
    let out = run(&mut c, &all, 3);
    assert!(out.stderr.contains(needle), "{needle}: {}", out.stderr);
    assert_eq!(writes(&api), Vec::<String>::new());
    assert_no_leak(&out);
}

#[test]
fn apply_without_a_sink_exits_3_before_any_write() {
    refused(&[], |_| {}, "--secrets-to");
}

#[test]
fn apply_with_two_sinks_exits_3_before_any_write() {
    refused(
        &["--secrets-to", "fly:app", "--emit-secrets"],
        |_| {},
        "exactly one",
    );
}

#[test]
fn apply_with_a_bad_sink_exits_3_before_any_write() {
    for bad in ["nope", "fly:", "gh-env:", "fly:-x", "s3:bucket"] {
        refused(&["--secrets-to", bad], |_| {}, "--secrets-to");
    }
}

#[test]
fn emit_secrets_on_a_terminal_exits_3_before_any_write() {
    refused(
        &["--emit-secrets"],
        |c| {
            c.env("HOIST_ASSUME_TTY", "1");
        },
        "terminal",
    );
}

#[test]
fn emit_secrets_with_json_exits_3_before_any_write() {
    refused(&["--emit-secrets", "--json"], |_| {}, "--json");
}

#[test]
fn a_missing_fly_exits_3_before_any_write() {
    refused(
        &["--secrets-to", "fly:app"],
        |c| {
            c.env("HOIST_FLY", "/nonexistent/fly");
        },
        "HOIST_FLY",
    );
}

#[test]
fn a_missing_github_environment_exits_3_before_any_write() {
    refused(
        &["--secrets-to", "gh-env:backups"],
        |c| {
            c.env("GITHUB_TOKEN", "gh-test-token");
        },
        "does not exist",
    );
}

// ---- apply: delivery ----

fn fly_stub(dir: &Path, exit: i32) -> std::path::PathBuf {
    let path = dir.join("fly");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = version ]; then echo 'fly v0.0.0'; exit 0; fi\n\
         echo \"$@\" > '{d}/fly.argv'\n\
         cat > '{d}/fly.stdin'\n\
         exit {exit}\n",
        d = dir.display()
    );
    write_script(&path, script.as_bytes(), 0o755);
    path
}

fn lines_of(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(|l| {
            let (k, v) = l.split_once('=').expect("NAME=value");
            (k.to_string(), v.to_string())
        })
        .collect()
}

fn assert_credentials(creds: &[(String, String)]) {
    let get = |k: &str| creds.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(creds.len(), 4, "{creds:?}");
    assert_eq!(get("R2_ACCESS_KEY_ID"), Some("new-id"));
    assert_eq!(get("R2_SECRET_ACCESS_KEY"), Some(secret_key_hex().as_str()));
    assert_eq!(get("R2_BUCKET"), Some("app-backups"));
    assert_eq!(
        get("R2_ENDPOINT"),
        Some("https://acc.r2.cloudflarestorage.com")
    );
}

fn assert_full_writes(api: &FakeApi) {
    assert_eq!(writes(api), [POST_BUCKET, PUT_LIFECYCLE, POST_TOKEN]);
    // Every other rule is kept as read, and the hoist rule is appended.
    let put = body_of(api, "PUT", "/accounts/acc/r2/buckets/app-backups/lifecycle");
    let list = put["rules"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{put}");
    assert_eq!(list[0], serde_json::from_str::<Value>(FOREIGN).unwrap());
    assert_eq!(list[1], serde_json::from_str::<Value>(HOIST_RULE).unwrap());
    // The token reads and writes that bucket only.
    let post = body_of(api, "POST", "/user/tokens");
    assert_eq!(post["name"], "hoist:acme/x:r2:app-backups");
    // The credentials cannot be rotated by hoist, so the token does not lapse.
    assert!(post.get("expires_on").is_none(), "{post}");
    let policy = &post["policies"][0];
    assert_eq!(policy["effect"], "allow");
    assert_eq!(
        policy["resources"],
        serde_json::json!({"com.cloudflare.edge.r2.bucket.acc_default_app-backups": "*"})
    );
    let groups: Vec<&str> = policy["permission_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(groups, ["g-read", "g-write"]);
    let bucket = body_of(api, "POST", "/accounts/acc/r2/buckets");
    assert_eq!(bucket["name"], "app-backups");
}

#[test]
fn emit_secrets_prints_only_the_lines_on_stdout() {
    let dir = repo(R2_TOML);
    let api = fresh().start();
    let out = run(
        &mut cmd(&api, dir.path()),
        &["apply", "--yes", "--area", "cloudflare", "--emit-secrets"],
        0,
    );
    assert_credentials(&lines_of(&out.stdout));
    // The report went to stderr, without any value.
    assert!(out.stderr.contains("r2_bucket"), "{}", out.stderr);
    assert!(!out.stderr.contains(MINTED));
    assert!(!out.stderr.contains(&secret_key_hex()));
    assert!(!out.stdout.contains(MINTED), "only the derived secret");
    assert_full_writes(&api);
}

#[test]
fn fly_sink_gets_the_lines_on_stdin_not_argv() {
    let dir = repo(R2_TOML);
    let tools = tempfile::tempdir().unwrap();
    let fly = fly_stub(tools.path(), 0);
    let api = fresh().start();
    let mut c = cmd(&api, dir.path());
    c.env("HOIST_FLY", &fly);
    let out = run(
        &mut c,
        &[
            "apply",
            "--yes",
            "--area",
            "cloudflare",
            "--secrets-to",
            "fly:my-app",
            "--json",
        ],
        0,
    );
    assert_no_leak(&out);
    let report = json(&out);
    assert_eq!(item(&report, "r2_token")["action"], "ok");
    assert_full_writes(&api);
    let argv = std::fs::read_to_string(tools.path().join("fly.argv")).unwrap();
    assert_eq!(argv.trim(), "secrets import -a my-app");
    assert!(!argv.contains(&secret_key_hex()) && !argv.contains(MINTED));
    let stdin = std::fs::read_to_string(tools.path().join("fly.stdin")).unwrap();
    assert_credentials(&lines_of(&stdin));
}

#[test]
fn a_failed_fly_delivery_revokes_the_token_and_exits_1() {
    let dir = repo(R2_TOML);
    let tools = tempfile::tempdir().unwrap();
    let fly = fly_stub(tools.path(), 1);
    let api = fresh().start();
    let mut c = cmd(&api, dir.path());
    c.env("HOIST_FLY", &fly);
    let out = run(
        &mut c,
        &[
            "apply",
            "--yes",
            "--area",
            "cloudflare",
            "--secrets-to",
            "fly:my-app",
            "--json",
        ],
        1,
    );
    assert_no_leak(&out);
    assert_eq!(
        writes(&api),
        [POST_BUCKET, PUT_LIFECYCLE, POST_TOKEN, REVOKE]
    );
    let report = json(&out);
    let t = item(&report, "r2_token");
    assert!(t["reason"].as_str().unwrap().starts_with("failed: "), "{t}");
}

#[test]
fn gh_env_sink_seals_the_four_secrets_into_the_environment() {
    let dir = repo(R2_TOML);
    let sk = SecretKey::generate(&mut OsRng);
    let key = format!(
        r#"{{"key_id":"kid-1","key":"{}"}}"#,
        STANDARD.encode(sk.public_key().as_bytes())
    );
    let env = "/repos/acme/x/environments/backups";
    let mut routes = fresh()
        .on(&format!("GET {env}"), 200, r#"{"name":"backups"}"#)
        .on(&format!("GET {env}/secrets/public-key"), 200, &key);
    for name in [
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
        "R2_BUCKET",
        "R2_ENDPOINT",
    ] {
        routes = routes.on(&format!("PUT {env}/secrets/{name}"), 201, "{}");
    }
    let api = routes.start();
    let mut c = cmd(&api, dir.path());
    c.env("GITHUB_TOKEN", "gh-test-token");
    let out = run(
        &mut c,
        &[
            "apply",
            "--yes",
            "--area",
            "cloudflare",
            "--secrets-to",
            "gh-env:backups",
            "--json",
        ],
        0,
    );
    assert_no_leak(&out);
    let puts: Vec<String> = writes(&api)
        .into_iter()
        .filter(|w| w.starts_with("PUT /repos"))
        .collect();
    assert_eq!(puts.len(), 4, "{puts:?}");
    let mut creds = Vec::new();
    for request in api
        .requests()
        .iter()
        .filter(|r| r.path.contains("/secrets/R2_"))
    {
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        let sealed = STANDARD
            .decode(body["encrypted_value"].as_str().unwrap())
            .unwrap();
        let value = String::from_utf8(sk.unseal(&sealed).unwrap()).unwrap();
        let name = request.path.rsplit('/').next().unwrap().to_string();
        creds.push((name, value));
    }
    assert_credentials(&creds);
}

// ---- apply: writes without a token mint ----

#[test]
fn a_lifecycle_update_with_the_token_present_needs_no_sink() {
    let dir = repo(R2_TOML);
    let other = HOIST_RULE.replace("2592000", "86400");
    let api = Routes::default()
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, &other]))
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, HOIST_RULE]))
        .on(TOKENS, 200, TOKENS_R2)
        .on(VERIFY, 200, VERIFY_OK)
        .on(GROUPS, 200, GROUP_LIST)
        .on(PUT_LIFECYCLE, 200, OK)
        .start();
    run(
        &mut cmd(&api, dir.path()),
        &["apply", "--yes", "--area", "cloudflare", "--json"],
        0,
    );
    assert_eq!(writes(&api), [PUT_LIFECYCLE]);
}

#[test]
fn a_missing_bucket_with_the_token_present_is_created_without_a_sink() {
    let dir = repo(R2_TOML);
    let api = Routes::default()
        .on(BUCKET, 404, NOT_FOUND)
        .on(BUCKET, 200, BUCKET_OK)
        .on(LIFECYCLE, 200, &rules(&[FOREIGN]))
        .on(LIFECYCLE, 200, &rules(&[FOREIGN, HOIST_RULE]))
        .on(TOKENS, 200, TOKENS_R2)
        .on(VERIFY, 200, VERIFY_OK)
        .on(GROUPS, 200, GROUP_LIST)
        .on(POST_BUCKET, 200, OK)
        .on(PUT_LIFECYCLE, 200, OK)
        .start();
    run(
        &mut cmd(&api, dir.path()),
        &["apply", "--yes", "--area", "cloudflare", "--json"],
        0,
    );
    assert_eq!(writes(&api), [POST_BUCKET, PUT_LIFECYCLE]);
}
