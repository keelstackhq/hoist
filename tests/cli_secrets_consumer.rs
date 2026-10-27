//! `hoist secrets check [--remote]` in a consumer repo (spec §15.3, §15.4): `secrets.toml` is
//! validated; with `--remote` every referenced file and key is looked up, GET-only.
mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, hoist};
use serde_json::Value;
use std::path::Path;

const MANIFEST: &str = r#"version = 1

[sources]
shared = { repo = "acme/secrets-shared" }
site = { repo = "acme/secrets-site", ref = "release/v1" }

[env.prod]
API_TOKEN = "shared:prod/app.sops.yaml#API_TOKEN"
MULTI = "site:prod/app.sops.yaml#multi"

[env.prod.files]
PORT_FILE = "shared:prod/app.sops.yaml#port_unencrypted"
"#;

const SHARED: &str = "GET /repos/acme/secrets-shared/contents/prod/app.sops.yaml";
const SITE: &str = "GET /repos/acme/secrets-site/contents/prod/app.sops.yaml";

fn ok_sops() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/secrets/sopsfile");
    std::fs::read_to_string(dir.join("ok.sops.yaml")).unwrap()
}

/// A contents-API body for `text`, base64 with line breaks as GitHub sends it.
fn contents(text: &str) -> String {
    let b64 = STANDARD.encode(text);
    let wrapped: Vec<&str> = b64
        .as_bytes()
        .chunks(60)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    serde_json::json!({ "encoding": "base64", "content": wrapped.join("\n") }).to_string()
}

fn run(manifest: &str, api: &FakeApi, args: &[&str], token: bool) -> (i32, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secrets.toml"), manifest).unwrap();
    let mut cmd = hoist();
    cmd.args(["secrets", "check", "--json", "-C"])
        .arg(dir.path())
        .args(args);
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_MAX_SLEEP_MS", "0");
    if token {
        cmd.env("GITHUB_TOKEN", "gh-test-token");
    }
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.code().unwrap(), json, stderr)
}

fn reason(report: &Value, key: &str) -> String {
    let items = report["items"].as_array().expect("items");
    let item = items
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("{key}: {report:#}"));
    format!(
        "{} {}",
        item["action"].as_str().unwrap(),
        item["reason"].as_str().unwrap()
    )
}

#[test]
fn without_remote_only_the_manifest_is_checked() {
    let api = FakeApi::start(&[]);
    let (code, report, _) = run(MANIFEST, &api, &[], false);
    assert_eq!(code, 0, "{report:#}");
    assert!(
        reason(&report, "prod.API_TOKEN").starts_with("ok shared:prod/app.sops.yaml#API_TOKEN")
    );
    assert!(reason(&report, "prod.PORT_FILE").starts_with("ok "));
    assert!(api.requests().is_empty());
    let (code, _, err) = run("version = 1\nnope = 1\n", &api, &[], false);
    assert!(code == 3 && err.contains("secrets.toml"), "{err}");
}

#[test]
fn remote_finds_every_file_and_key_with_gets_only() {
    let body = contents(&ok_sops());
    let api = FakeApi::start(&[(SHARED, 200, &body, &[]), (SITE, 200, &body, &[])]);
    let (code, report, _) = run(MANIFEST, &api, &["--remote"], true);
    assert_eq!(code, 0, "{report:#}");
    assert!(reason(&report, "prod.MULTI").starts_with("ok "));
    api.assert_only_get();
    let requests = api.requests();
    assert_eq!(requests.len(), 2, "each file is fetched once");
    assert!(
        requests.iter().any(|r| r.url.ends_with("?ref=main")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.url.ends_with("?ref=release%2Fv1")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|r| r.auth.as_deref() == Some("Bearer gh-test-token"))
    );
}

#[test]
fn remote_drift_and_errors() {
    let ok = ok_sops();
    let plain = ok.replace(ok.lines().next().unwrap(), "API_TOKEN: MARK");
    let cases = [
        (
            MANIFEST.replace("#API_TOKEN\"", "#NOPE\""),
            200,
            ok.clone(),
            "drift key `NOPE` is not in",
        ),
        (
            MANIFEST.to_string(),
            404,
            String::new(),
            "drift not found in acme/secrets-shared@main",
        ),
        (
            MANIFEST.to_string(),
            200,
            plain,
            "drift not a valid sops file: plaintext",
        ),
        (
            MANIFEST.to_string(),
            403,
            String::new(),
            "unknown cannot read",
        ),
    ];
    for (manifest, status, text, want) in cases {
        let body = if text.is_empty() {
            "{}".to_string()
        } else {
            contents(&text)
        };
        let site = contents(&ok);
        let api = FakeApi::start(&[(SHARED, status, &body, &[]), (SITE, 200, &site, &[])]);
        let (code, report, _) = run(&manifest, &api, &["--remote"], true);
        let got = reason(&report, "prod.API_TOKEN");
        assert!(got.starts_with(want), "{want}: {got}");
        assert_eq!(
            code,
            if want.starts_with("drift") { 1 } else { 0 },
            "{report:#}"
        );
        assert!(!report.to_string().contains("MARK"), "a value leaked");
        api.assert_only_get();
    }
    let api = FakeApi::start(&[]);
    let (code, _, err) = run(MANIFEST, &api, &["--remote"], false);
    assert!(code == 3 && err.contains("GITHUB_TOKEN"), "{err}");
}

#[test]
fn remote_is_refused_in_a_secrets_repo_and_nothing_is_an_error() {
    let api = FakeApi::start(&[]);
    let dir = tempfile::tempdir().unwrap();
    let out = hoist()
        .args(["secrets", "check", "-C"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("secrets.toml"));
    std::fs::write(dir.path().join("access.toml"), "version = 1\n").unwrap();
    let mut cmd = hoist();
    cmd.args(["secrets", "check", "--remote", "-C"])
        .arg(dir.path());
    let out = cmd.env("HOIST_GITHUB_API", api.url()).output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--remote"));
}
