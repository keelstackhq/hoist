mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, fixture_repo};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use hoist::auth::Secret;
use hoist::change::Change;
use hoist::desired::{Desired, build_desired};
use hoist::github_write::execute_github;
use hoist::http::{Http, Service};
use hoist::seal::{put_secret, seal};
use serde_json::Value;

const CANARY: &str = "hoist-canary-seal";
const KEY_PATH: &str = "GET /repos/acme/x/actions/secrets/public-key";
const PUT_PATH: &str = "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN";

fn desired(toml: &str) -> (tempfile::TempDir, Desired) {
    let dir = fixture_repo("site", "https://github.com/acme/x.git");
    std::fs::write(dir.path().join("hoist.toml"), toml).unwrap();
    let d = build_desired(dir.path()).expect("desired");
    (dir, d)
}

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, Secret::from("dummy"), &api.url()).max_sleep_ms(0)
}

/// A keypair and the fake's JSON body for its public key.
fn keypair() -> (SecretKey, String, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    let json = format!(r#"{{"key_id":"kid-1","key":"{b64}"}}"#);
    (sk, b64, json)
}

fn unseal(sk: &SecretKey, b64: &str) -> String {
    let bytes = STANDARD.decode(b64).expect("base64");
    String::from_utf8(sk.unseal(&bytes).expect("unseal")).unwrap()
}

#[test]
fn seal_roundtrips_with_test_keypair() {
    let (sk, key_b64, _) = keypair();
    let sealed = seal(&key_b64, &Secret::from(CANARY)).unwrap();
    assert!(!sealed.contains(CANARY));
    assert_eq!(unseal(&sk, &sealed), CANARY);
    // A sealed box is randomised.
    assert_ne!(sealed, seal(&key_b64, &Secret::from(CANARY)).unwrap());
}

#[test]
fn invalid_public_keys_are_rejected_without_echoing_them() {
    for bad in ["not base64 !!", "AAAA", ""] {
        let err = seal(bad, &Secret::from(CANARY)).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        let text = err.to_string();
        assert_eq!(text, "GitHub returned an invalid repository public key");
        assert!(!text.contains(CANARY));
    }
}

#[test]
fn put_secret_body_has_no_plaintext() {
    let (_dir, d) = desired("");
    let (sk, _, key_json) = keypair();
    let api = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 201, "empty.json", &[]),
    ]);
    put_secret(
        &http(&api),
        &d,
        "CLOUDFLARE_API_TOKEN",
        &Secret::from(CANARY),
    )
    .unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 2);
    for r in &requests {
        assert!(!r.path.contains(CANARY));
    }
    let raw = requests[1].body.as_deref().expect("body");
    assert!(!raw.contains(CANARY), "plaintext in body: {raw}");
    let body: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(body["key_id"], "kid-1");
    assert_eq!(body.as_object().unwrap().len(), 2);
    let sealed = body["encrypted_value"].as_str().expect("encrypted_value");
    assert_eq!(unseal(&sk, sealed), CANARY);
}

#[test]
fn stale_key_is_refetched_once() {
    let (_dir, d) = desired("");
    let (sk, _, key_json) = keypair();
    let api = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 422, "unprocessable.json", &[]),
        (PUT_PATH, 201, "empty.json", &[]),
    ]);
    put_secret(
        &http(&api),
        &d,
        "CLOUDFLARE_API_TOKEN",
        &Secret::from(CANARY),
    )
    .unwrap();
    let requests = api.requests();
    let keys = requests
        .iter()
        .filter(|r| r.path.ends_with("public-key"))
        .count();
    let puts: Vec<_> = requests.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!((keys, puts.len()), (2, 2));
    for put in puts {
        assert!(!put.body.as_deref().unwrap().contains(CANARY));
        let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            unseal(&sk, body["encrypted_value"].as_str().unwrap()),
            CANARY
        );
    }
}

#[test]
fn second_422_is_an_error_naming_the_secret() {
    let (_dir, d) = desired("");
    let (_, _, key_json) = keypair();
    let api = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 422, "unprocessable.json", &[]),
    ]);
    let err = put_secret(
        &http(&api),
        &d,
        "CLOUDFLARE_API_TOKEN",
        &Secret::from(CANARY),
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 3);
    let text = err.to_string();
    assert!(text.contains("CLOUDFLARE_API_TOKEN"), "{text}");
    assert!(!text.contains(CANARY), "{text}");
    let requests = api.requests();
    assert_eq!(requests.iter().filter(|r| r.method == "PUT").count(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path.ends_with("public-key"))
            .count(),
        2
    );
}

#[test]
fn invalid_key_from_github_is_an_error_and_nothing_is_put() {
    let (_dir, d) = desired("");
    let api = FakeApi::start(&[(KEY_PATH, 200, r#"{"key_id":"k","key":"AAAA"}"#, &[])]);
    let err = put_secret(
        &http(&api),
        &d,
        "CLOUDFLARE_API_TOKEN",
        &Secret::from(CANARY),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "GitHub returned an invalid repository public key"
    );
    assert!(api.requests().iter().all(|r| r.method == "GET"));
}

#[test]
fn account_id_secret_seals_the_desired_account_id() {
    let (_dir, d) = desired("[cloudflare]\naccount_id = \"acc-canary-123\"\n");
    let (sk, _, key_json) = keypair();
    let api = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (
            "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_ACCOUNT_ID",
            201,
            "empty.json",
            &[],
        ),
    ]);
    execute_github(&http(&api), &d, &Change::AccountIdSecret).unwrap();
    let requests = api.requests();
    let raw = requests[1].body.as_deref().unwrap();
    assert!(!raw.contains("acc-canary-123"));
    let body: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(
        unseal(&sk, body["encrypted_value"].as_str().unwrap()),
        "acc-canary-123"
    );
}

#[test]
fn account_id_secret_without_account_id_is_a_usage_error() {
    let (_dir, mut d) = desired("");
    d.cloudflare.account_id = None;
    let api = FakeApi::start(&[]);
    let err = execute_github(&http(&api), &d, &Change::AccountIdSecret).unwrap_err();
    assert_eq!(err.exit_code(), 3);
    assert!(err.to_string().contains("account id"), "{err}");
    assert!(api.requests().is_empty());
}

#[test]
fn secret_debug_still_redacted() {
    let s = Secret::from(CANARY);
    assert_eq!(format!("{s:?}"), "***");
    assert_eq!(format!("{s}"), "***");
}
