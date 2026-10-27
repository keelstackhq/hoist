//! `hoist secrets check` in a secrets repo (spec §15.4): no network, exit 0/1/3.
#![cfg(unix)]
mod common;

use common::{FakeApi, hoist};
use hoist::secrets::access::{Access, sops_yaml};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const NOW: &str = "2026-10-27T00:00:00Z";
fn file(name: &str) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/secrets/sopsfile");
    std::fs::read_to_string(dir.join(name)).unwrap()
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
    write(dir.path(), "access.toml", access);
    write(dir.path(), ".sops.yaml", &sops_yaml(&parsed));
    write(dir.path(), "prod/app.sops.yaml", &file("ok.sops.yaml"));
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
    // A `.git` file (worktree, submodule) is skipped like the `.git` directory.
    for own in "README.md CODEOWNERS .gitignore LICENSE .github/workflows/check.yml .git".split(' ')
    {
        write(dir.path(), own, "MARK");
    }
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 0, "{report:#}");
    assert!(reason(&report, "file", "prod/app.sops.yaml").starts_with("ok "));
    assert!(reason(&report, "sops-config", ".sops.yaml").starts_with("ok "));
    assert!(reason(&report, "rule", "prod/**").contains("warning: only one human"));
    let dir = repo(&access("2026-11-16", ""));
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 0, "{report:#}");
    assert!(reason(&report, "recipient", "owner").contains("ok warning: expires in 20 days"));
}

const APP: &str = "prod/app.sops.yaml";
const CFG: &str = ".sops.yaml";

/// After `change`, check exits 1 and the item for `key` reads `drift <want>…`, with no value.
fn drifts(change: impl FnOnce(&Path), key: &str, want: &str) {
    let dir = repo(&access("2027-10-27", ""));
    change(dir.path());
    let (code, report) = check(dir.path(), NOW);
    assert_eq!(code, 1, "{want}: {report:#}");
    let got = reason(
        &report,
        if key == CFG { "sops-config" } else { "file" },
        key,
    );
    assert!(got.starts_with(&format!("drift {want}")), "{got}");
    assert!(
        !report.to_string().contains("MARK"),
        "{want} leaked a value"
    );
}

fn put(name: &'static str, text: String) -> impl FnOnce(&Path) {
    move |d| write(d, name, &text)
}

#[test]
fn drift_exits_1() {
    let ok = file("ok.sops.yaml");
    let token = ok.lines().find(|l| l.starts_with("API_TOKEN:")).unwrap();
    let plain = ok.replace(token, "API_TOKEN: MARK");
    drifts(put(APP, plain), APP, "plaintext: API_TOKEN");
    let age = ok.find("        - enc:").unwrap();
    let rec = ok[age..].find("          recipient:").unwrap() + age;
    let entry = &ok[age..ok[rec..].find('\n').unwrap() + rec];
    let twice = ok.replacen(entry, &format!("{entry}\n{entry}"), 1);
    drifts(put(APP, twice), APP, "`sops.age` entry 2 repeats");
    let one = access("2027-10-27", "").replace("[\"owner\", \"site-prod\"]", "[\"owner\"]");
    drifts(put("access.toml", one), APP, "recipients differ");
    drifts(put(CFG, "x".into()), CFG, "differs");
    let rm = |d: &Path| std::fs::remove_file(d.join(CFG)).unwrap();
    drifts(rm, CFG, "missing");
    let (other, notes, nested) = ("staging/a.sops.yaml", "prod/notes.txt", "prod/.sops.yaml");
    drifts(put(other, ok.clone()), other, "no rule");
    drifts(put(notes, "MARK".into()), notes, "not a .sops.yaml");
    drifts(put(nested, "x".into()), nested, "nested sops config");
    let (env_file, stray) = ("prod.env", "staging/creds.txt");
    drifts(put(env_file, "MARK".into()), env_file, "not part of a");
    drifts(put(stray, "MARK".into()), stray, "not part of a");
    let (evil, shown) = (
        "prod/\u{1b}[31m::error::x.sops.yaml",
        "prod/??31m??error??x.sops.yaml",
    );
    drifts(put(evil, ok.clone()), shown, "name has characters");
    drifts(
        put("prod/a?.sops.yaml", ok.clone()),
        "prod/a?.sops.yaml",
        "name has characters",
    );
    let link = |to: &'static str, at: &'static str| move |d: &Path| symlink(to, &d.join(at));
    let (file_link, dir_link) = ("prod/l.sops.yaml", "preview");
    drifts(link("app.sops.yaml", file_link), file_link, "symlink");
    drifts(link("prod", dir_link), dir_link, "symlink");
    let fifo = |d: &Path| drop(Command::new("mkfifo").arg(d.join("prod/p")).status());
    drifts(fifo, "prod/p", "not a regular file");
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
fn non_utf8_name_is_drift_even_next_to_its_lossy_twin() {
    use std::os::unix::ffi::OsStrExt;
    let dir = repo(&access("2027-10-27", ""));
    write(dir.path(), "prod/\u{FFFD}.sops.yaml", &file("ok.sops.yaml"));
    let name = std::ffi::OsStr::from_bytes(b"\xFF.sops.yaml");
    std::fs::write(dir.path().join("prod").join(name), "API_TOKEN: MARK\n").unwrap();
    let (code, report) = check(dir.path(), NOW);
    let text = report.to_string();
    let shown_raw = text.contains('\u{FFFD}') || text.contains("\\u001b");
    assert!(
        code == 1 && !shown_raw && text.contains("name has characters"),
        "{report:#}"
    );
}

#[test]
fn config_errors_exit_3() {
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(check(empty.path(), NOW).0, 3);
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
