//! `[cloudflare.dns]` (spec §6.3): config, plan/check items, apply writes, no-token human steps,
//! `targets = "fly"`.

mod common;

use common::{FakeApi, RouteSpec, git_repo, hoist, stub};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

const ZONE: &str = "GET /zones?name=example.com";
const HOST: &str = "GET /zones/z1/dns_records?name=app.example.com&page=1";
const DMARC: &str = "GET /zones/z1/dns_records?name=_dmarc.app.example.com&page=1";

const BASE_TOML: &str = r#"[cloudflare.dns]
zone = "example.com"
host = "app.example.com"
targets = ["203.0.113.7", "2001:db8::7"]
"#;

fn repo(toml: &str) -> TempDir {
    let dir = git_repo();
    std::fs::write(dir.path().join("hoist.toml"), toml).unwrap();
    dir
}

fn list(records: Value) -> String {
    json!({"success": true, "errors": [], "messages": [], "result": records,
           "result_info": {"page": 1, "total_pages": 1}})
    .to_string()
}

fn rec(id: &str, ty: &str, name: &str, content: &str, proxied: bool) -> Value {
    json!({"id": id, "type": ty, "name": name, "content": content, "proxied": proxied, "ttl": 1})
}

fn zone_found() -> String {
    list(json!([{"id": "z1", "name": "example.com"}]))
}

fn start(routes: &[(&str, u16, &str)]) -> FakeApi {
    let specs: Vec<RouteSpec> = routes
        .iter()
        .map(|(k, s, b)| (*k, *s, *b, &[][..]))
        .collect();
    FakeApi::start(&specs)
}

fn cmd(api: &FakeApi, token: bool) -> assert_cmd::Command {
    let mut c = hoist();
    c.env("HOIST_CLOUDFLARE_API", api.url());
    if token {
        c.env("CLOUDFLARE_API_TOKEN", "cf-test-token");
    }
    c
}

fn run(c: &mut assert_cmd::Command, dir: &Path, args: &[&str], code: i32) -> Value {
    let out = c
        .current_dir(dir)
        .args(args)
        .assert()
        .code(code)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("cf-test-token"));
    serde_json::from_str(&text).expect("one JSON object")
}

fn item<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "dns_record" && i["key"] == key)
        .unwrap_or_else(|| panic!("no dns_record {key} in {report:#}"))
}

fn dns_keys(report: &Value) -> Vec<String> {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "dns_record")
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

fn writes(api: &FakeApi) -> Vec<(String, String, Option<Value>)> {
    api.requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .map(|r| {
            (
                r.method,
                r.path,
                r.body.map(|b| serde_json::from_str(&b).unwrap()),
            )
        })
        .collect()
}

// ---- config ----

fn config_error(toml: &str, needle: &str) {
    let dir = repo(toml);
    let api = start(&[]);
    let out = cmd(&api, true)
        .current_dir(dir.path())
        .args(["plan", "--area", "cloudflare"])
        .assert()
        .code(3)
        .get_output()
        .stderr
        .clone();
    let err = String::from_utf8(out).unwrap();
    assert!(err.contains(needle), "{needle} not in: {err}");
    assert!(api.requests().is_empty());
}

#[test]
fn unknown_dns_key_is_exit_3() {
    config_error(&format!("{BASE_TOML}ttl = 60\n"), "ttl");
}

#[test]
fn proxied_true_is_exit_3() {
    config_error(&format!("{BASE_TOML}proxied = true\n"), "proxied");
}

#[test]
fn host_outside_the_zone_is_exit_3() {
    config_error(
        &BASE_TOML.replace("app.example.com", "app.other.org"),
        "app.other.org",
    );
}

#[test]
fn host_sharing_only_a_suffix_with_the_zone_is_exit_3() {
    config_error(
        &BASE_TOML.replace("app.example.com", "badexample.com"),
        "badexample.com",
    );
}

#[test]
fn a_bad_ip_is_exit_3() {
    config_error(
        &BASE_TOML.replace("203.0.113.7", "203.0.113.999"),
        "203.0.113.999",
    );
}

#[test]
fn a_target_string_other_than_fly_is_exit_3() {
    config_error(
        &BASE_TOML.replace(r#"["203.0.113.7", "2001:db8::7"]"#, r#""cloud""#),
        "fly",
    );
}

#[test]
fn empty_targets_and_stray_fly_app_are_exit_3() {
    config_error(
        &BASE_TOML.replace(r#""203.0.113.7", "2001:db8::7""#, ""),
        "targets",
    );
    config_error(&format!("{BASE_TOML}fly_app = \"app\"\n"), "fly_app");
}

#[test]
fn zone_itself_as_host_is_accepted() {
    let dir = repo(&BASE_TOML.replace("app.example.com", "example.com"));
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (
            "GET /zones/z1/dns_records?name=example.com&page=1",
            200,
            &list(json!([])),
        ),
    ]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    assert_eq!(item(&r, "example.com A 203.0.113.7")["action"], "create");
}

// ---- plan / check ----

#[test]
fn plan_reports_missing_records_as_create_and_sends_only_get() {
    let dir = repo(BASE_TOML);
    let api = start(&[(ZONE, 200, &zone_found()), (HOST, 200, &list(json!([])))]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    api.assert_only_get();
    assert_eq!(
        dns_keys(&r),
        [
            "app.example.com A 203.0.113.7",
            "app.example.com AAAA 2001:db8::7"
        ]
    );
    for key in dns_keys(&r) {
        let i = item(&r, &key);
        assert_eq!(i["action"], "create");
        assert_eq!(i["area"], "cloudflare");
        assert_eq!(i["source"], "hoist.toml:cloudflare.dns");
    }
    assert!(r["human_steps"].as_array().unwrap().is_empty());
}

#[test]
fn check_is_clean_when_records_match_and_exits_1_on_drift() {
    let dir = repo(BASE_TOML);
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (
            HOST,
            200,
            &list(json!([
                rec("r1", "A", "app.example.com", "203.0.113.7", false),
                // The compressed or expanded spelling of a v6 address is the same record.
                rec(
                    "r2",
                    "AAAA",
                    "app.example.com",
                    "2001:0db8:0:0:0:0:0:7",
                    false
                ),
            ])),
        ),
    ]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        0,
    );
    api.assert_only_get();
    assert_eq!(item(&r, "app.example.com A 203.0.113.7")["action"], "ok");
    assert_eq!(item(&r, "app.example.com AAAA 2001:db8::7")["action"], "ok");
}

const MIXED_TOML: &str = r#"[cloudflare.dns]
zone = "example.com"
host = "app.example.com"
targets = ["203.0.113.7", "203.0.113.8"]
no_mail = true
"#;

fn mixed_routes() -> Vec<(&'static str, u16, String)> {
    vec![
        (ZONE, 200, zone_found()),
        (
            HOST,
            200,
            list(json!([
                rec("r1", "A", "app.example.com", "203.0.113.7", true),
                rec("r2", "A", "app.example.com", "198.51.100.1", false),
                rec(
                    "r3",
                    "TXT",
                    "app.example.com",
                    "\"v=spf1 include:x.example ~all\"",
                    false
                ),
                rec(
                    "r4",
                    "TXT",
                    "app.example.com",
                    "google-site-verification=abc",
                    false
                ),
            ])),
        ),
        (
            DMARC,
            200,
            list(json!([rec(
                "r5",
                "TXT",
                "_dmarc.app.example.com",
                "v=DMARC1; p=none",
                false
            )])),
        ),
    ]
}

fn start_owned(routes: &[(&str, u16, String)]) -> FakeApi {
    let borrowed: Vec<(&str, u16, &str)> = routes
        .iter()
        .map(|(k, s, b)| (*k, *s, b.as_str()))
        .collect();
    start(&borrowed)
}

#[test]
fn plan_classifies_proxied_stray_and_mail_records() {
    let dir = repo(MIXED_TOML);
    let api = start_owned(&mixed_routes());
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        1,
    );
    api.assert_only_get();
    let proxied = item(&r, "app.example.com A 203.0.113.7");
    assert_eq!(proxied["action"], "drift");
    assert!(proxied["reason"].as_str().unwrap().contains("proxied"));
    assert_eq!(
        item(&r, "app.example.com A 203.0.113.8")["action"],
        "create"
    );
    let stray = item(&r, "app.example.com A 198.51.100.1");
    assert_eq!(stray["action"], "drift");
    assert!(stray["reason"].as_str().unwrap().contains("not a target"));
    assert_eq!(
        item(&r, "app.example.com TXT v=spf1 -all")["action"],
        "update"
    );
    assert_eq!(
        item(&r, "_dmarc.app.example.com TXT v=DMARC1; p=reject")["action"],
        "update"
    );
    // The unrelated TXT record is neither listed nor touched.
    assert_eq!(dns_keys(&r).len(), 5, "{:?}", dns_keys(&r));
}

#[test]
fn no_mail_creates_spf_and_dmarc_when_absent() {
    let dir = repo(MIXED_TOML);
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (HOST, 200, &list(json!([]))),
        (DMARC, 200, &list(json!([]))),
    ]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    assert_eq!(
        item(&r, "app.example.com TXT v=spf1 -all")["action"],
        "create"
    );
    assert_eq!(
        item(&r, "_dmarc.app.example.com TXT v=DMARC1; p=reject")["action"],
        "create"
    );
}

#[test]
fn a_cname_at_the_host_is_drift_with_a_human_step() {
    let dir = repo(BASE_TOML);
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (
            HOST,
            200,
            &list(json!([rec(
                "c1",
                "CNAME",
                "app.example.com",
                "other.example.net",
                false
            )])),
        ),
    ]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        1,
    );
    let cname = item(&r, "app.example.com CNAME other.example.net");
    assert_eq!(cname["action"], "drift");
    let steps = r["human_steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["id"], "dns-cname:app.example.com");
    assert_eq!(steps[0]["status"], "pending");
}

// ---- apply ----

#[test]
fn apply_creates_unproxies_deletes_and_updates() {
    let dir = repo(MIXED_TOML);
    let mut routes = mixed_routes();
    routes.extend([
        ("POST /zones/z1/dns_records", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r1", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r3", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r5", 200, list(json!({}))),
        ("DELETE /zones/z1/dns_records/r2", 200, list(json!({}))),
        (
            "GET /user/tokens/verify",
            200,
            r#"{"success":true,"result":{"status":"active"}}"#.to_string(),
        ),
    ]);
    let api = start_owned(&routes);
    run(
        &mut cmd(&api, true),
        dir.path(),
        &["apply", "--yes", "--json", "--area", "cloudflare"],
        1, // the fake keeps answering with the old state, so the re-read still drifts
    );
    let w = writes(&api);
    let has = |m: &str, p: &str, b: Option<Value>| {
        assert!(
            w.iter().any(|(wm, wp, wb)| wm == m && wp == p && *wb == b),
            "missing {m} {p} {b:?} in {w:#?}"
        );
    };
    has(
        "POST",
        "/zones/z1/dns_records",
        Some(
            json!({"type": "A", "name": "app.example.com", "content": "203.0.113.8", "ttl": 1, "proxied": false}),
        ),
    );
    has(
        "PATCH",
        "/zones/z1/dns_records/r1",
        Some(json!({"proxied": false})),
    );
    has("DELETE", "/zones/z1/dns_records/r2", None);
    has(
        "PATCH",
        "/zones/z1/dns_records/r3",
        Some(json!({"content": "v=spf1 -all"})),
    );
    has(
        "PATCH",
        "/zones/z1/dns_records/r5",
        Some(json!({"content": "v=DMARC1; p=reject"})),
    );
    assert_eq!(w.len(), 5, "{w:#?}");
}

#[test]
fn apply_posts_a_missing_txt_without_proxied() {
    let dir = repo(MIXED_TOML);
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (
            HOST,
            200,
            &list(json!([
                rec("r1", "A", "app.example.com", "203.0.113.7", false),
                rec("r2", "A", "app.example.com", "203.0.113.8", false),
            ])),
        ),
        (DMARC, 200, &list(json!([]))),
        ("POST /zones/z1/dns_records", 200, &list(json!({}))),
        (
            "GET /user/tokens/verify",
            200,
            r#"{"success":true,"result":{"status":"active"}}"#,
        ),
    ]);
    run(
        &mut cmd(&api, true),
        dir.path(),
        &["apply", "--yes", "--json", "--area", "cloudflare"],
        1,
    );
    let w = writes(&api);
    assert_eq!(w.len(), 2, "{w:#?}");
    assert!(w.contains(&(
        "POST".into(),
        "/zones/z1/dns_records".into(),
        Some(json!({"type": "TXT", "name": "app.example.com", "content": "v=spf1 -all", "ttl": 1}))
    )));
    assert!(w.contains(&(
        "POST".into(),
        "/zones/z1/dns_records".into(),
        Some(json!({"type": "TXT", "name": "_dmarc.app.example.com", "content": "v=DMARC1; p=reject", "ttl": 1}))
    )));
}

#[test]
fn adopt_keeps_a_stray_record() {
    let dir = repo(MIXED_TOML);
    let mut routes = mixed_routes();
    routes.extend([
        ("POST /zones/z1/dns_records", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r1", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r3", 200, list(json!({}))),
        ("PATCH /zones/z1/dns_records/r5", 200, list(json!({}))),
        (
            "GET /user/tokens/verify",
            200,
            r#"{"success":true,"result":{"status":"active"}}"#.to_string(),
        ),
    ]);
    let api = start_owned(&routes);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &[
            "apply",
            "--yes",
            "--adopt",
            "--json",
            "--area",
            "cloudflare",
        ],
        1,
    );
    assert!(writes(&api).iter().all(|(m, _, _)| m != "DELETE"));
    assert_eq!(
        item(&r, "app.example.com A 198.51.100.1")["action"],
        "drift"
    );
}

#[test]
fn apply_in_sync_writes_nothing() {
    let dir = repo(BASE_TOML);
    let api = start(&[
        (ZONE, 200, &zone_found()),
        (
            HOST,
            200,
            &list(json!([
                rec("r1", "A", "app.example.com", "203.0.113.7", false),
                rec("r2", "AAAA", "app.example.com", "2001:db8::7", false),
            ])),
        ),
    ]);
    run(
        &mut cmd(&api, true),
        dir.path(),
        &["apply", "--yes", "--json", "--area", "cloudflare"],
        0,
    );
    assert!(writes(&api).is_empty());
}

// ---- no token / unreadable zone ----

#[test]
fn without_a_token_records_are_unknown_with_human_steps() {
    let dir = repo(MIXED_TOML);
    let api = start(&[]);
    let r = run(
        &mut cmd(&api, false),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        2,
    );
    assert!(api.requests().is_empty());
    for key in [
        "app.example.com A 203.0.113.7",
        "app.example.com A 203.0.113.8",
        "app.example.com TXT v=spf1 -all",
        "_dmarc.app.example.com TXT v=DMARC1; p=reject",
    ] {
        assert_eq!(item(&r, key)["action"], "unknown", "{key}");
    }
    let steps = r["human_steps"].as_array().unwrap();
    let ids: Vec<&str> = steps.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "dns:app.example.com A",
            "dns:app.example.com A",
            "dns:app.example.com TXT",
            "dns:_dmarc.app.example.com TXT"
        ]
    );
    assert_eq!(
        steps[0]["title"],
        "in zone example.com add A app.example.com 203.0.113.7, DNS only (grey cloud), TTL auto"
    );
    assert_eq!(steps[0]["url"], "https://dash.cloudflare.com/");
    assert_eq!(steps[0]["status"], "pending");
}

#[test]
fn plan_without_a_token_still_succeeds() {
    let dir = repo(BASE_TOML);
    let api = start(&[]);
    let r = run(
        &mut cmd(&api, false),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    assert_eq!(
        item(&r, "app.example.com A 203.0.113.7")["action"],
        "unknown"
    );
    assert_eq!(r["human_steps"].as_array().unwrap().len(), 2);
}

#[test]
fn an_unreadable_zone_makes_every_record_unknown() {
    let dir = repo(BASE_TOML);
    let api = start(&[(
        ZONE,
        403,
        r#"{"success":false,"errors":[{"message":"no zone access"}]}"#,
    )]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        2,
    );
    let i = item(&r, "app.example.com A 203.0.113.7");
    assert_eq!(i["action"], "unknown");
    assert!(i["reason"].as_str().unwrap().contains("no zone access"));
    assert_eq!(r["human_steps"].as_array().unwrap().len(), 2);
}

#[test]
fn a_missing_zone_makes_every_record_unknown() {
    let dir = repo(BASE_TOML);
    let api = start(&[(ZONE, 200, &list(json!([])))]);
    let r = run(
        &mut cmd(&api, true),
        dir.path(),
        &["check", "--json", "--area", "cloudflare"],
        2,
    );
    assert_eq!(
        item(&r, "app.example.com A 203.0.113.7")["action"],
        "unknown"
    );
}

// ---- targets = "fly" ----

const FLY_TOML: &str = r#"[cloudflare.dns]
zone = "example.com"
host = "app.example.com"
targets = "fly"
"#;

const FLY_IPS: &str = r#"[{"Address":"203.0.113.7","Type":"v4","Region":"global"},{"Address":"2a09:8280:1::1","Type":"v6","Region":"global"},{"Address":"fdaa::3","Type":"private_v6","Region":""}]"#;

fn fly_stub(script: &str) -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir_in(common::tmpdir()).unwrap();
    let path = stub(dir.path(), "fly", script);
    (dir, path)
}

#[test]
fn fly_targets_skip_private_addresses() {
    let dir = repo(FLY_TOML);
    let (fly_dir, fly) = fly_stub(&format!("echo '{FLY_IPS}'"));
    let api = start(&[(ZONE, 200, &zone_found()), (HOST, 200, &list(json!([])))]);
    let r = run(
        cmd(&api, true).env("HOIST_FLY", &fly),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    assert_eq!(
        dns_keys(&r),
        [
            "app.example.com A 203.0.113.7",
            "app.example.com AAAA 2a09:8280:1::1"
        ]
    );
    let log = std::fs::read_to_string(fly_dir.path().join("fly.log")).unwrap();
    assert!(log.starts_with("ips list --json "), "{log}");
}

#[test]
fn fly_app_adds_the_app_flag() {
    let dir = repo(&format!("{FLY_TOML}fly_app = \"my-app\"\n"));
    let (fly_dir, fly) = fly_stub(&format!("echo '{FLY_IPS}'"));
    let api = start(&[(ZONE, 200, &zone_found()), (HOST, 200, &list(json!([])))]);
    run(
        cmd(&api, true).env("HOIST_FLY", &fly),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    let log = std::fs::read_to_string(fly_dir.path().join("fly.log")).unwrap();
    assert!(log.starts_with("ips list -a my-app --json "), "{log}");
}

#[test]
fn a_failing_fly_makes_the_records_unknown() {
    let dir = repo(FLY_TOML);
    let (_fly_dir, fly) = fly_stub("echo 'not logged in' >&2; exit 1");
    let api = start(&[(ZONE, 200, &zone_found()), (HOST, 200, &list(json!([])))]);
    let r = run(
        cmd(&api, true).env("HOIST_FLY", &fly),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    let unknown: Vec<&Value> = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "dns_record")
        .collect();
    assert_eq!(unknown.len(), 1, "{r:#}");
    assert_eq!(unknown[0]["action"], "unknown");
    assert!(unknown[0]["reason"].as_str().unwrap().contains("fly"));
    api.assert_only_get();
}

// ---- auth check ----

#[test]
fn auth_check_lists_the_zone_read_probe() {
    let dir = repo(BASE_TOML);
    let api = start(&[
        (
            "GET /user/tokens/verify",
            200,
            r#"{"success":true,"result":{"status":"active"}}"#,
        ),
        (ZONE, 200, &zone_found()),
    ]);
    let out = cmd(&api, true)
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .current_dir(dir.path())
        .args(["auth", "check", "--json"])
        .output()
        .unwrap();
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    let zone = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "cloudflare:zone:read")
        .unwrap_or_else(|| panic!("no zone probe in {r:#}"));
    assert!(zone["reason"].as_str().unwrap().contains("DNS Write"));
}

#[test]
fn auth_check_zone_probe_needs_a_visible_zone() {
    let dir = repo(BASE_TOML);
    let api = start(&[
        (
            "GET /user/tokens/verify",
            200,
            r#"{"success":true,"result":{"status":"active"}}"#,
        ),
        (ZONE, 200, &list(json!([]))),
    ]);
    let out = cmd(&api, true)
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .current_dir(dir.path())
        .args(["auth", "check", "--json"])
        .output()
        .unwrap();
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    let zone = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "cloudflare:zone:read")
        .unwrap();
    assert_eq!(zone["action"], "unknown", "{zone}");
    assert!(
        zone["reason"]
            .as_str()
            .unwrap()
            .contains("zone not visible to this token"),
        "{zone}"
    );
}

#[test]
fn fly_never_sees_hoists_tokens() {
    let dir = repo(FLY_TOML);
    let (fly_dir, fly) = fly_stub(&format!("env > \"$0.env\"; echo '{FLY_IPS}'"));
    let api = start(&[(ZONE, 200, &zone_found()), (HOST, 200, &list(json!([])))]);
    run(
        cmd(&api, true)
            .env("HOIST_FLY", &fly)
            .env("GITHUB_TOKEN", "gh-secret")
            .env("GH_TOKEN", "gh-secret2"),
        dir.path(),
        &["plan", "--json", "--area", "cloudflare"],
        0,
    );
    let env = std::fs::read_to_string(fly_dir.path().join("fly.env")).unwrap();
    for name in ["CLOUDFLARE_API_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        assert!(!env.contains(name), "{name} leaked to fly: {env}");
    }
}

#[test]
fn a_bad_fly_app_exits_3() {
    let dir = repo(&format!("{FLY_TOML}fly_app = \"--foo\"\n"));
    let api = start(&[]);
    let mut c = cmd(&api, true);
    let out = c
        .current_dir(dir.path())
        .args(["plan", "--area", "cloudflare"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fly_app"));
}
