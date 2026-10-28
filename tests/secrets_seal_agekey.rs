//! The two pieces `hoist secrets grant` is built on (spec §15.4): a new age key read from
//! `age-keygen` into memory, and a value sealed into a GitHub environment secret.
#![cfg(unix)]
mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::FakeApi;
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use hoist::auth::Secret;
use hoist::http::{Http, Service};
use hoist::seal::put_env_secret;
use hoist::secrets::{agekey, decrypt::resolve};
use serde_json::Value;
use std::ffi::OsStr;
use std::process::Command;

#[test]
fn a_new_age_key_is_whole_and_its_public_half_matches() {
    if resolve(
        OsStr::new("age-keygen"),
        std::env::var_os("PATH").as_deref(),
    )
    .is_none()
    {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must install age-keygen"
        );
        return eprintln!("skipped: age-keygen is not on PATH (spec §15.9.1)");
    }
    let (a, b) = (agekey::generate().unwrap(), agekey::generate().unwrap());
    assert_ne!(a.public, b.public, "two keys are the same");
    assert_eq!(format!("{:?}", a.private), "***");
    // age-keygen derives the same public key from the private one.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("k.txt");
    std::fs::write(&file, format!("{}\n", expose(&a.private))).unwrap();
    let out = Command::new("age-keygen")
        .arg("-y")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), a.public);
}

/// The private key's text, through the one way a test can see it: sealing it and opening the box.
fn expose(secret: &Secret) -> String {
    let (sk, key) = keypair();
    let api = FakeApi::start(&[(KEY, 200, &key, &[]), (PUT, 201, "{}", &[])]);
    put_env_secret(
        &http(&api),
        ("acme", "site"),
        "prod",
        "SOPS_AGE_KEY",
        secret,
    )
    .unwrap();
    unseal(&sk, &api)
}

const KEY: &str = "GET /repos/acme/site/environments/prod/secrets/public-key";
const PUT: &str = "PUT /repos/acme/site/environments/prod/secrets/SOPS_AGE_KEY";

fn keypair() -> (SecretKey, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let key = STANDARD.encode(sk.public_key().as_bytes());
    (
        sk,
        serde_json::json!({ "key_id": "k1", "key": key }).to_string(),
    )
}

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, "gh-test-token".into(), &api.url()).max_sleep_ms(0)
}

fn unseal(sk: &SecretKey, api: &FakeApi) -> String {
    let put = api
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("a PUT");
    let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["key_id"], "k1");
    let sealed = STANDARD
        .decode(body["encrypted_value"].as_str().unwrap())
        .unwrap();
    String::from_utf8(sk.unseal(&sealed).unwrap()).unwrap()
}

#[test]
fn a_value_is_sealed_into_the_environment_secret() {
    let (sk, key) = keypair();
    let api = FakeApi::start(&[(KEY, 200, &key, &[]), (PUT, 201, "{}", &[])]);
    let value = Secret::from("hoist-test-value-1");
    put_env_secret(
        &http(&api),
        ("acme", "site"),
        "prod",
        "SOPS_AGE_KEY",
        &value,
    )
    .unwrap();
    assert_eq!(unseal(&sk, &api), "hoist-test-value-1");
    let paths: Vec<String> = api
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(paths, [KEY, PUT]);
    // No such environment: the error names it and never the value.
    let api = FakeApi::start(&[(KEY, 404, "{}", &[])]);
    let err = put_env_secret(
        &http(&api),
        ("acme", "site"),
        "prod",
        "SOPS_AGE_KEY",
        &value,
    )
    .unwrap_err();
    let err = err.to_string();
    assert!(
        err.contains("environment prod of acme/site") && !err.contains("hoist-test"),
        "{err}"
    );
}
