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
    let content = wrapped.join("\n");
    serde_json::json!({ "type": "file", "encoding": "base64", "content": content }).to_string()
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
    let item = items.iter().find(|i| i["key"] == key);
    let item = item.unwrap_or_else(|| panic!("{key}: {report:#}"));
    let field = |name: &str| item[name].as_str().unwrap().to_string();
    format!("{} {}", field("action"), field("reason"))
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
    let plain = contents(&ok.replace(ok.lines().next().unwrap(), "API_TOKEN: MARK"));
    let nope = MANIFEST.replace("#API_TOKEN\"", "#NOPE\"");
    let big = r#"{"type":"file","encoding":"none","content":""}"#;
    let cases = [
        (
            nope.as_str(),
            200,
            contents(&ok),
            "key `NOPE` is not in prod/app.sops.yaml at acme/",
        ),
        (
            MANIFEST,
            404,
            "{}".into(),
            "not found in acme/secrets-shared@main (or the token",
        ),
        (MANIFEST, 200, plain, "not a valid sops file: plaintext"),
        (MANIFEST, 200, big.into(), "too large for the contents API"),
        (MANIFEST, 200, "[]".into(), "not a file"),
        (
            MANIFEST,
            200,
            r#"{"type":"submodule"}"#.into(),
            "not a file",
        ),
    ];
    for (manifest, status, body, want) in cases {
        let site = contents(&ok);
        let api = FakeApi::start(&[(SHARED, status, &body, &[]), (SITE, 200, &site, &[])]);
        let (code, report, _) = run(manifest, &api, &["--remote"], true);
        let got = reason(&report, "prod.API_TOKEN");
        assert!(
            code == 1 && got.starts_with(&format!("drift {want}")),
            "{want}: {got}"
        );
        assert!(!report.to_string().contains("MARK"), "a value leaked");
        api.assert_only_get();
    }
    // No access or a server error: nothing could be verified, so exit 3 with how to fix it.
    for status in [403, 500] {
        let api = FakeApi::start(&[(SHARED, status, "{}", &[]), (SITE, status, "{}", &[])]);
        let (code, _, err) = run(MANIFEST, &api, &["--remote"], true);
        assert_eq!(code, 3, "{status}: {err}");
    }
    let api = FakeApi::start(&[(SHARED, 403, "{}", &[])]);
    assert!(
        run(MANIFEST, &api, &["--remote"], true)
            .2
            .contains("Contents: read")
    );
    let api = FakeApi::start(&[]);
    let (code, _, err) = run(MANIFEST, &api, &["--remote"], false);
    assert!(
        code == 3 && err.contains("Contents: read") && err.contains("GITHUB_TOKEN"),
        "{err}"
    );
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

#[test]
fn remote_refuses_a_redirect_to_another_host_but_follows_one_on_the_same_host() {
    let body = contents(&ok_sops());
    let other = FakeApi::start(&[(SHARED, 200, &body, &[])]);
    let to_other = format!(
        "{}/repos/acme/secrets-shared/contents/prod/app.sops.yaml",
        other.url()
    );
    let api = FakeApi::start(&[
        (SHARED, 302, "{}", &[("Location", &to_other)]),
        (SITE, 200, &body, &[]),
    ]);
    let (code, _, err) = run(MANIFEST, &api, &["--remote"], true);
    assert!(
        code == 3 && err.contains("redirected to another host"),
        "{err}"
    );
    assert!(other.requests().is_empty(), "the other host was contacted");
    let moved = "{base}/moved/prod/app.sops.yaml";
    let routes = [
        (SHARED, 301, "{}", &[("Location", moved)][..]),
        (SITE, 200, &body, &[]),
    ];
    let api = FakeApi::start(&[
        routes[0],
        routes[1],
        ("GET /moved/prod/app.sops.yaml", 200, &body, &[]),
    ]);
    let (code, report, err) = run(MANIFEST, &api, &["--remote"], true);
    assert_eq!(code, 0, "{err} {report:#}");
    let moved = api
        .requests()
        .into_iter()
        .find(|r| r.path.starts_with("/moved/"))
        .unwrap();
    assert_eq!(
        moved.auth.as_deref(),
        Some("Bearer gh-test-token"),
        "same-host keeps the token"
    );
}

/// A `//host/...` Location is another host (a network-path reference), refused as such without
/// blaming the network; `-v` logs each hop by path, never its query.
#[test]
fn remote_refuses_a_network_path_redirect_and_logs_hops() {
    let body = contents(&ok_sops());
    let other = FakeApi::start(&[(SHARED, 200, &body, &[])]);
    let host = other.url().trim_start_matches("http://").to_string();
    let file = "repos/acme/secrets-shared/contents/prod/app.sops.yaml";
    // Another host as a network path, behind userinfo, as `/\` (a browser's `//`), then an
    // invalid URL and an empty one. Only another host gets the "repo moved" hint.
    let locations = [
        (format!("//{host}/{file}"), true),
        (format!("//127.0.0.1@{host}/{file}"), true),
        (format!("/\\{host}/{file}"), true),
        ("\\\\evil.example/x".to_string(), false),
        (String::new(), false),
    ];
    for (to, cross_host) in locations {
        let api = FakeApi::start(&[
            (SHARED, 302, "{}", &[("Location", &to)]),
            (SITE, 200, &body, &[]),
        ]);
        let (code, _, err) = run(MANIFEST, &api, &["--remote"], true);
        let hint = err.contains("update its owner/name");
        let another = err.contains("redirected to another host");
        assert!(
            code == 3 && err.contains("refused") && !err.contains("Check your network"),
            "{to}: {err}"
        );
        assert_eq!((hint, another), (cross_host, cross_host), "{to}: {err}");
    }
    assert!(other.requests().is_empty(), "the other host was contacted");
    let moved = "{base}/moved/prod/app.sops.yaml?ref=main";
    let api = FakeApi::start(&[
        (SHARED, 301, "{}", &[("Location", moved)]),
        ("GET /moved/prod/app.sops.yaml", 200, &body, &[]),
        (SITE, 200, &body, &[]),
    ]);
    let (code, _, err) = run(MANIFEST, &api, &["--remote", "-v"], true);
    assert_eq!(code, 0, "{err}");
    // The hop, then the request's own line, by path.
    let path = "GET /repos/acme/secrets-shared/contents/prod/app.sops.yaml";
    let (hop, done) = (format!("{path} 301"), format!("{path} 200"));
    assert!(err.contains(&hop) && err.contains(&done), "{err}");
    assert!(!err.contains("?ref="), "{err}");
}
