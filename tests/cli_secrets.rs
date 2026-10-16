mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, RouteSpec, fixture_repo, hoist};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use serde_json::Value;

const SITE: &str = "https://github.com/acme/x.git";
const CANARY: &str = "hoist-canary-secrets-set";
const PUT: &str = "PUT /repos/acme/x/actions/secrets/DEPLOY_KEY";

fn keypair() -> (SecretKey, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    (sk, format!(r#"{{"key_id":"kid-1","key":"{b64}"}}"#))
}

fn api(repo: &str, put_status: u16, key_json: &str) -> FakeApi {
    let routes: Vec<RouteSpec> = vec![
        ("GET /repos/acme/x", 200, repo, &[]),
        (
            "GET /repos/acme/x/actions/secrets/public-key",
            200,
            key_json,
            &[],
        ),
        (PUT, put_status, "empty.json", &[]),
    ];
    FakeApi::start(&routes)
}

fn set(api: &FakeApi, dir: &std::path::Path, args: &[&str]) -> assert_cmd::Command {
    let mut c = hoist();
    c.current_dir(dir)
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_MAX_SLEEP_MS", "0")
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("DEPLOY_KEY", CANARY)
        .args(args);
    c
}

fn writes(api: &FakeApi) -> Vec<String> {
    api.requests()
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

#[test]
fn set_from_env_seals_value() {
    let dir = fixture_repo("site", SITE);
    let (sk, key_json) = keypair();
    let api = api("github/repo_admin.json", 201, &key_json);
    let out = set(
        &api,
        dir.path(),
        &["secrets", "set", "DEPLOY_KEY", "-v", "--json"],
    )
    .assert()
    .code(0)
    .get_output()
    .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stdout.contains(CANARY) && !stderr.contains(CANARY));
    let report: Value = serde_json::from_str(&stdout).expect("one JSON object");
    assert_eq!(report["command"], "secrets set");
    let item = &report["items"][0];
    assert_eq!(item["area"], "github");
    assert_eq!(item["kind"], "secret");
    assert_eq!(item["key"], "DEPLOY_KEY");
    assert_eq!(item["action"], "ok");
    assert_eq!(item["reason"], "set");
    assert_eq!(item["source"], "env DEPLOY_KEY");
    assert_eq!(writes(&api), [PUT.to_string()]);
    let put = api
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    let body = put.body.unwrap();
    assert!(!body.contains(CANARY));
    let b: Value = serde_json::from_str(&body).unwrap();
    let bytes = STANDARD
        .decode(b["encrypted_value"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        String::from_utf8(sk.unseal(&bytes).unwrap()).unwrap(),
        CANARY
    );
}

#[test]
fn no_env_no_tty_exits_3() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 201, &key_json);
    let mut c = set(&api, dir.path(), &["secrets", "set", "DEPLOY_KEY"]);
    c.env_remove("DEPLOY_KEY");
    c.assert().code(3).stderr(predicates::str::contains(
        "set DEPLOY_KEY in the environment or run in a terminal",
    ));
    assert!(writes(&api).is_empty());
}

#[test]
fn invalid_names_exit_3() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 201, &key_json);
    for name in ["lower", "1ABC", "A-B", "GITHUB_TOKEN"] {
        let out = set(&api, dir.path(), &["secrets", "set", name])
            .env(name, CANARY)
            .assert()
            .code(3)
            .get_output()
            .clone();
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(!err.contains(CANARY), "{err}");
        if name.starts_with("GITHUB_") {
            assert!(err.contains("GITHUB_"), "{err}");
        }
    }
    assert!(
        api.requests().is_empty(),
        "name is validated before any request"
    );
}

/// R30: a value typed on the command line is refused without being echoed anywhere.
#[test]
fn argv_value_is_refused_and_never_echoed() {
    const ARGV_CANARY: &str = "s3cr3t-argv-canary";
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 201, &key_json);
    let joined = format!("DEPLOY_KEY={ARGV_CANARY}");
    let forms: [Vec<&str>; 5] = [
        vec!["secrets", "set", &joined],
        vec!["secrets", "set", "DEPLOY_KEY", ARGV_CANARY],
        vec!["secrets", "set", "--json", "DEPLOY_KEY", ARGV_CANARY],
        vec!["secrets", "set", "DEPLOY_KEY", ARGV_CANARY, "more"],
        vec!["secrets", "set", "DEPLOY_KEY", "-s3cr3t-argv-canary"],
    ];
    for args in forms {
        let out = set(&api, dir.path(), &args)
            .assert()
            .code(3)
            .get_output()
            .clone();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(!stdout.contains(ARGV_CANARY), "{args:?} stdout: {stdout}");
        assert!(!stderr.contains(ARGV_CANARY), "{args:?} stderr: {stderr}");
        assert!(stderr.contains("DEPLOY_KEY"), "{args:?}: {stderr}");
    }
    assert!(api.requests().is_empty(), "refused before any request");
}

#[test]
fn value_never_in_argv_path() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 201, &key_json);
    set(
        &api,
        dir.path(),
        &["secrets", "set", "DEPLOY_KEY", "--value", "y"],
    )
    .assert()
    .code(3)
    .stderr(predicates::str::contains("takes only NAME"));
    assert!(api.requests().is_empty());
}

#[test]
fn own_credentials_never_come_from_env() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    for name in ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID", "GH_TOKEN"] {
        let api = api("github/repo_admin.json", 201, &key_json);
        let out = set(&api, dir.path(), &["secrets", "set", name])
            .env(name, CANARY)
            .assert()
            .code(3)
            .get_output()
            .clone();
        let all = format!(
            "{}{}",
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap()
        );
        assert!(!all.contains(CANARY), "{all}");
        let why = format!(
            "{name} is also hoist's own credential; enter the value at the prompt in a terminal"
        );
        assert!(all.contains(&why), "{all}");
        if name == "CLOUDFLARE_API_TOKEN" {
            assert!(
                all.contains("or let `hoist apply` mint a scoped deploy token"),
                "{all}"
            );
        }
        assert!(
            api.requests().is_empty(),
            "{name}: no request before exit 3"
        );
    }
}

#[test]
fn failed_put_keeps_create_for_an_absent_secret() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 500, &key_json);
    let out = set(
        &api,
        dir.path(),
        &["secrets", "set", "DEPLOY_KEY", "--json"],
    )
    .assert()
    .code(1)
    .get_output()
    .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["items"][0]["action"], "create");
}

#[test]
fn non_admin_exits_3_before_write() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo.json", 201, &key_json);
    set(&api, dir.path(), &["secrets", "set", "DEPLOY_KEY"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("admin"));
    assert!(writes(&api).is_empty());
}

#[test]
fn failed_put_exits_1_with_reason() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = api("github/repo_admin.json", 500, &key_json);
    let out = set(
        &api,
        dir.path(),
        &["secrets", "set", "DEPLOY_KEY", "--json"],
    )
    .assert()
    .code(1)
    .get_output()
    .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let reason = report["items"][0]["reason"].as_str().unwrap();
    assert!(reason.starts_with("failed: "), "{reason}");
    assert!(!String::from_utf8(out.stdout).unwrap().contains(CANARY));
}

// ---- `hoist secrets rotate` (spec §8.3) ----

const MINTED: &str = "hoist-canary-mint-rotate";
const VERIFY: &str = "GET /user/tokens/verify";
const PUT_CF: &str = "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN";
const DEL_OLD: &str = "DELETE /user/tokens/tok-1";
const DEL_NEW: &str = "DELETE /user/tokens/new-id";
const TOKENS_ROTATE: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"tok-1","name":"hoist:acme/x:deploy","status":"active"},{"id":"tok-2","name":"hoist:acme/x:preview","status":"active"},{"id":"tok-3","name":"hoist:other/y:deploy","status":"active"}]}"#;
const GROUPS: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"g-pages","name":"Pages Write"}]}"#;
const DELETED: &str = r#"{"success":true,"errors":[],"messages":[],"result":{"id":"x"}}"#;

fn minted_body() -> String {
    format!(
        r#"{{"success":true,"errors":[],"messages":[],"result":{{"id":"new-id","name":"hoist:acme/x:deploy","value":"{MINTED}"}}}}"#
    )
}

/// Routes for a successful rotation; `over` replaces every response of a key, `extra`
/// appends responses (several routes with one key answer in order).
fn rotate_api(key_json: &str, over: &[(&str, u16, &str)], extra: &[(&str, u16, &str)]) -> FakeApi {
    let minted = minted_body();
    let base: Vec<(&str, u16, &str)> = vec![
        ("GET /repos/acme/x", 200, "github/repo_admin.json"),
        (
            "GET /repos/acme/x/actions/secrets/public-key",
            200,
            key_json,
        ),
        (PUT_CF, 201, "empty.json"),
        (VERIFY, 200, "cloudflare/token_verify_active.json"),
        ("GET /user/tokens", 200, TOKENS_ROTATE),
        (
            "GET /accounts/acc/pages/projects/keelstack-landing",
            200,
            "cloudflare/pages_project.json",
        ),
        ("GET /user/tokens/permission_groups", 200, GROUPS),
        ("POST /user/tokens", 200, &minted),
        (DEL_OLD, 200, DELETED),
        (DEL_NEW, 200, DELETED),
    ];
    let routes: Vec<RouteSpec> = base
        .into_iter()
        .filter(|(k, ..)| !over.iter().any(|(o, ..)| o == k))
        .chain(over.iter().copied())
        .chain(extra.iter().copied())
        .map(|(k, s, b)| (k, s, b, &[][..]))
        .collect();
    FakeApi::start(&routes)
}

fn rotate(api: &FakeApi, dir: &std::path::Path, args: &[&str], code: i32) -> (Value, String) {
    let mut c = hoist();
    let out = c
        .current_dir(dir)
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_MAX_SLEEP_MS", "0")
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .args(args)
        .assert()
        .code(code)
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let all = format!("{stdout}{stderr}");
    assert!(!all.contains(MINTED), "minted value leaked: {all}");
    assert!(!all.contains("test-token"), "credential leaked: {all}");
    let report = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (report, stderr)
}

fn item<'a>(report: &'a Value, kind: &str, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {report}"))
}

#[test]
fn rotate_mints_sets_and_revokes_old() {
    let dir = fixture_repo("site", SITE);
    let (sk, key_json) = keypair();
    let api = rotate_api(&key_json, &[], &[]);
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 0);
    assert_eq!(report["command"], "secrets rotate");
    assert_eq!(report["exit_code"], 0);
    let secret = item(&report, "secret", "CLOUDFLARE_API_TOKEN");
    assert_eq!(secret["area"], "github");
    assert_eq!(secret["action"], "ok");
    assert_eq!(secret["reason"], "rotated");
    // R31: revoked tokens are keyed by name like plan's api_token item; the id is in the reason.
    let old = item(&report, "api_token", "hoist:acme/x:deploy");
    assert_eq!(old["area"], "cloudflare");
    assert_eq!(old["action"], "ok");
    assert_eq!(old["reason"], "revoked token tok-1");
    assert_eq!(report["items"].as_array().unwrap().len(), 2, "{report}");
    // Mint, then seal, then revoke only this repo's deploy token.
    assert_eq!(
        writes(&api),
        ["POST /user/tokens", PUT_CF, DEL_OLD].map(String::from)
    );
    // The new value was verified with its own Authorization before the seal.
    let reqs = api.requests();
    let verify_new = reqs
        .iter()
        .position(|r| {
            r.path == "/user/tokens/verify"
                && r.auth.as_deref() == Some(&format!("Bearer {MINTED}"))
        })
        .expect("new token verified");
    let put = reqs.iter().position(|r| r.method == "PUT").unwrap();
    assert!(verify_new < put);
    let body: Value = serde_json::from_str(reqs[put].body.as_ref().unwrap()).unwrap();
    let bytes = STANDARD
        .decode(body["encrypted_value"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        String::from_utf8(sk.unseal(&bytes).unwrap()).unwrap(),
        MINTED
    );
}

#[test]
fn rotate_unknown_name_exits_3() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(&key_json, &[], &[]);
    for name in ["DEPLOY_KEY", "CLOUDFLARE_ACCOUNT_ID"] {
        let (_, stderr) = rotate(&api, dir.path(), &["secrets", "rotate", name], 3);
        assert!(
            stderr.contains("only minted secrets can be rotated: CLOUDFLARE_API_TOKEN"),
            "{stderr}"
        );
    }
    assert!(api.requests().is_empty(), "name checked before any request");
    // Naming the rotatable secret works like no name.
    rotate(
        &api,
        dir.path(),
        &["secrets", "rotate", "CLOUDFLARE_API_TOKEN"],
        0,
    );
}

#[test]
fn verify_failure_revokes_new_token() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    // First verify (preflight, old token) is active; the second (new token) is not.
    let api = rotate_api(
        &key_json,
        &[],
        &[(VERIFY, 200, "cloudflare/token_verify_inactive.json")],
    );
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    let secret = item(&report, "secret", "CLOUDFLARE_API_TOKEN");
    assert!(
        secret["reason"].as_str().unwrap().starts_with("failed: "),
        "{report}"
    );
    assert_eq!(
        writes(&api),
        ["POST /user/tokens", DEL_NEW].map(String::from)
    );
}

#[test]
fn put_failure_revokes_new_token() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(&key_json, &[(PUT_CF, 500, "empty.json")], &[]);
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    let reason = item(&report, "secret", "CLOUDFLARE_API_TOKEN")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.starts_with("failed: "), "{reason}");
    let w = writes(&api);
    assert_eq!(w.first().map(String::as_str), Some("POST /user/tokens"));
    assert_eq!(w.last().map(String::as_str), Some(DEL_NEW));
    assert!(!w.iter().any(|x| x == DEL_OLD), "{w:?}");
}

#[test]
fn old_revoke_failure_exits_1_with_human_step() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(&key_json, &[(DEL_OLD, 500, "empty.json")], &[]);
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    assert_eq!(
        item(&report, "secret", "CLOUDFLARE_API_TOKEN")["reason"],
        "rotated"
    );
    let old = item(&report, "api_token", "hoist:acme/x:deploy");
    let reason = old["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("failed: ") && reason.contains("tok-1"),
        "{report}"
    );
    let steps = report["human_steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["title"] == "revoke Cloudflare token tok-1 in the dashboard"),
        "{report}"
    );
}

#[test]
fn rotate_refuses_blind_token_list() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[("GET /user/tokens", 403, "cloudflare/forbidden.json")],
        &[],
    );
    let (_, stderr) = rotate(&api, dir.path(), &["secrets", "rotate"], 3);
    assert!(
        stderr.contains("cannot list Cloudflare tokens; hoist will not rotate blind"),
        "{stderr}"
    );
    assert!(writes(&api).is_empty());
}

#[test]
fn canary_new_value_never_printed() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    for args in [
        &["secrets", "rotate", "-v", "--json"][..],
        &["secrets", "rotate", "-v"][..],
    ] {
        let api = rotate_api(&key_json, &[], &[]);
        // `rotate` asserts the minted value is absent from stdout and stderr.
        let (_, stderr) = rotate(&api, dir.path(), args, 0);
        assert!(
            stderr.contains("/user/tokens/verify"),
            "verbose logs: {stderr}"
        );
    }
    // Failure paths carry no value either.
    let api = rotate_api(
        &key_json,
        &[],
        &[(VERIFY, 200, "cloudflare/token_verify_inactive.json")],
    );
    rotate(&api, dir.path(), &["secrets", "rotate", "-v", "--json"], 1);
}

#[test]
fn rotate_needs_hoist_managed_cloudflare() {
    let (_, key_json) = keypair();
    let api = rotate_api(&key_json, &[], &[]);
    // No deploy target.
    let bare = fixture_repo("bare", SITE);
    // Cloudflare managed elsewhere.
    let external = fixture_repo("site", SITE);
    std::fs::write(
        external.path().join("hoist.toml"),
        "[cloudflare]\nmanaged = \"external\"\n",
    )
    .unwrap();
    for dir in [bare.path(), external.path()] {
        let (_, stderr) = rotate(&api, dir, &["secrets", "rotate"], 3);
        assert!(
            stderr.contains("rotate needs hoist-managed Cloudflare (hoist.toml / inference)"),
            "{stderr}"
        );
    }
    assert!(writes(&api).is_empty());
}

/// Verify requests sent with the newly minted token.
fn new_token_verifies(api: &FakeApi) -> usize {
    let bearer = format!("Bearer {MINTED}");
    api.requests()
        .iter()
        .filter(|r| r.path == "/user/tokens/verify" && r.auth.as_deref() == Some(bearer.as_str()))
        .count()
}

#[test]
fn verify_retries_until_active() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[],
        &[
            (VERIFY, 200, "cloudflare/token_verify_inactive.json"),
            (VERIFY, 200, "cloudflare/token_verify_active.json"),
        ],
    );
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 0);
    assert_eq!(
        item(&report, "secret", "CLOUDFLARE_API_TOKEN")["reason"],
        "rotated"
    );
    assert_eq!(new_token_verifies(&api), 2);
}

#[test]
fn verify_inactive_three_times_revokes_new_token() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[],
        &[(VERIFY, 200, "cloudflare/token_verify_inactive.json")],
    );
    rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    assert_eq!(new_token_verifies(&api), 3);
    assert_eq!(
        writes(&api),
        ["POST /user/tokens", DEL_NEW].map(String::from)
    );
}

#[test]
fn verify_401_names_the_new_token() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[],
        &[(VERIFY, 401, "cloudflare/forbidden.json")],
    );
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    let reason = item(&report, "secret", "CLOUDFLARE_API_TOKEN")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("the newly minted token did not verify"),
        "{reason}"
    );
    assert!(!reason.contains("check CLOUDFLARE_API_TOKEN"), "{reason}");
    assert_eq!(new_token_verifies(&api), 3);
    assert_eq!(
        writes(&api),
        ["POST /user/tokens", DEL_NEW].map(String::from)
    );
}

#[test]
fn permission_groups_403_exits_3_before_writing() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[(
            "GET /user/tokens/permission_groups",
            403,
            "cloudflare/forbidden.json",
        )],
        &[],
    );
    rotate(&api, dir.path(), &["secrets", "rotate"], 3);
    api.assert_only_get();
}

#[test]
fn verify_and_cleanup_revoke_failure_names_new_token() {
    let dir = fixture_repo("site", SITE);
    let (_, key_json) = keypair();
    let api = rotate_api(
        &key_json,
        &[(DEL_NEW, 500, "empty.json")],
        &[(VERIFY, 200, "cloudflare/token_verify_inactive.json")],
    );
    let (report, _) = rotate(&api, dir.path(), &["secrets", "rotate", "--json"], 1);
    let new = item(&report, "api_token", "hoist:acme/x:deploy");
    let reason = new["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("failed: ") && reason.contains("new-id"),
        "{report}"
    );
    let steps = report["human_steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["title"] == "revoke Cloudflare token new-id in the dashboard"),
        "{report}"
    );
    assert!(!writes(&api).iter().any(|w| w == DEL_OLD));
}
