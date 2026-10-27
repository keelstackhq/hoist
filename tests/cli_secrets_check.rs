//! `hoist secrets check` in a secrets repo (spec §15.4): no network, exit 0/1/3.
#![cfg(unix)]
mod common;

use common::{FakeApi, hoist};
use hoist::secrets::access::{Access, sops_yaml};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

const NOW: &str = "2026-10-27T00:00:00Z";
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/secrets/sopsfile"
);

fn file(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

/// access.toml: human `owner` and ci `site-prod` (the fixture's two keys), rule `prod/**`.
fn access(owner_expires: &str, extra: &str) -> String {
    let keys: Vec<String> = file("recipients.txt").lines().map(String::from).collect();
    format!(
        "version = 1\n\n[[recipient]]\nname = \"owner\"\nkind = \"human\"\nage = \"{}\"\n\
         expires = \"{owner_expires}\"\n\n[[recipient]]\nname = \"site-prod\"\nkind = \"ci\"\n\
         repo = \"acme/site\"\nenv = \"prod\"\nprovider = \"github\"\nage = \"{}\"\n\
         expires = \"2027-10-27\"\n\n[[rule]]\npath = \"prod/**\"\n\
         recipients = [\"owner\", \"site-prod\"]\n{extra}",
        keys[0], keys[1]
    )
}

/// A secrets repo with `access`, its generated `.sops.yaml` and `prod/app.sops.yaml`.
fn repo(access: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let parsed = Access::parse(access, "access.toml", 1_793_059_200).unwrap();
    std::fs::write(dir.path().join("access.toml"), access).unwrap();
    std::fs::write(dir.path().join(".sops.yaml"), sops_yaml(&parsed)).unwrap();
    std::fs::create_dir(dir.path().join("prod")).unwrap();
    std::fs::write(dir.path().join("prod/app.sops.yaml"), file("ok.sops.yaml")).unwrap();
    dir
}

fn check(dir: &Path, now: &str) -> (i32, Value) {
    let api = FakeApi::start(&[]);
    let out = hoist()
        .args(["secrets", "check", "--json", "-C"])
        .arg(dir)
        .env("HOIST_NOW", now)
        .env("HOIST_GITHUB_API", api.url())
        .output()
        .unwrap();
    assert!(api.requests().is_empty(), "secrets check sent a request");
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code().unwrap(), json)
}

/// "<action> <reason>" of the item `kind`/`key`.
fn reason(report: &Value, kind: &str, key: &str) -> String {
    let items = report["items"].as_array().unwrap();
    let item = items.iter().find(|i| i["kind"] == kind && i["key"] == key);
    let item = item.unwrap_or_else(|| panic!("no item {key} in {report:#}"));
    let field = |name: &str| item[name].as_str().unwrap().to_string();
    format!("{} {}", field("action"), field("reason"))
}

#[test]
fn in_sync_repo_exits_0_with_warnings() {
    let dir = repo(&access("2027-10-27", ""));
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 0, "{report:#}");
    assert!(reason(&report, "file", "prod/app.sops.yaml").starts_with("ok "));
    assert!(reason(&report, "sops-config", ".sops.yaml").starts_with("ok "));
    assert!(reason(&report, "rule", "prod/**").contains("warning: only one human"));
    // Expires in 20 days: still ok, with a warning.
    let dir = repo(&access("2026-11-16", ""));
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 0, "{report:#}");
    assert!(reason(&report, "recipient", "owner").contains("ok warning: expires in 20 days"));
}

const APP: &str = "prod/app.sops.yaml";

/// After `change`, check exits 1 and the item for `key` reads `want…`, with no value shown.
fn drifts(change: impl FnOnce(&Path), key: &str, want: &str) {
    let dir = repo(&access("2027-10-27", ""));
    change(dir.path());
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 1, "{want}: {report:#}");
    let kind = if key == ".sops.yaml" {
        "sops-config"
    } else {
        "file"
    };
    let got = reason(&report, kind, key);
    assert!(got.starts_with(want), "{got}");
    assert!(
        !report.to_string().contains("MARK"),
        "{want} leaked a value"
    );
}

#[test]
fn drift_exits_1() {
    let ok = file("ok.sops.yaml");
    let app = |text: String| move |d: &Path| write(d, APP, &text);
    let token = ok.lines().find(|l| l.starts_with("API_TOKEN:")).unwrap();
    drifts(
        app(ok.replace(token, "API_TOKEN: MARK")),
        APP,
        "drift plaintext: API_TOKEN",
    );
    let age = ok.find("        - enc:").unwrap();
    let rec = ok[age..].find("          recipient:").unwrap() + age;
    let entry = &ok[age..ok[rec..].find('\n').unwrap() + rec];
    drifts(
        app(ok.replacen(entry, &format!("{entry}\n{entry}"), 1)),
        APP,
        "drift `sops.age`",
    );
    let one = access("2027-10-27", "").replace("[\"owner\", \"site-prod\"]", "[\"owner\"]");
    drifts(
        |d| write(d, "access.toml", &one),
        APP,
        "drift recipients differ",
    );
    drifts(
        |d| write(d, ".sops.yaml", "creation_rules: []\n"),
        ".sops.yaml",
        "drift differs",
    );
    drifts(
        |d| std::fs::remove_file(d.join(".sops.yaml")).unwrap(),
        ".sops.yaml",
        "drift missing",
    );
    drifts(
        |d| write(d, "staging/app.sops.yaml", &ok),
        "staging/app.sops.yaml",
        "drift no rule",
    );
    drifts(
        |d| write(d, "prod/notes.txt", "MARK"),
        "prod/notes.txt",
        "drift not a .sops.yaml",
    );
    let link =
        |target: &'static str, at: &'static str| move |d: &Path| symlink(target, &d.join(at));
    drifts(
        link("app.sops.yaml", "prod/l.sops.yaml"),
        "prod/l.sops.yaml",
        "drift symlink",
    );
    drifts(link("prod", "preview"), "preview", "drift symlink");
}

#[test]
fn expired_recipient_is_drift_and_shadowed_rule_warns() {
    let extra = "\n[[rule]]\npath = \"prod/app.sops.yaml\"\nrecipients = [\"owner\"]\n";
    let dir = repo(&access("2027-10-27", extra));
    let (code, report) = check(dir.path(), "2027-11-01T00:00:00Z");
    assert_eq!(code, 1, "{report:#}");
    assert!(reason(&report, "recipient", "owner").starts_with("drift expired"));
    assert!(reason(&report, "rule", "prod/app.sops.yaml").contains("warning: unreachable"));
}

#[test]
fn config_errors_exit_3() {
    let empty = tempfile::tempdir().unwrap();
    let (code, _) = check(empty.path(), NOW);
    assert_eq!(code, 3);
    let dir = repo(&access("2027-10-27", ""));
    write(dir.path(), "access.toml", "version = 2\n");
    assert_eq!(check(dir.path(), NOW).0, 3);
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn symlink(target: &str, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}
