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
    .stderr(predicates::str::contains("unexpected argument"));
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
