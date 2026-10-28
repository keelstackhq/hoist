//! `hoist secrets sync [--rotate]` in a secrets repo (spec §15.4, §15.5): `.sops.yaml` from
//! `access.toml`, then `sops updatekeys -y` on every file whose recipients differ from its rule
//! (`--rotate`: a new data key for every file). Real sops; age keys made at runtime only.
#![cfg(unix)]
mod common;

use common::hoist;
use hoist::secrets::decrypt::resolve;
use hoist::secrets::sopsfile;
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

const FILE: &str = "prod/app.sops.yaml";

/// The stdout of `program args` in `dir` with only PATH (and `key` as SOPS_AGE_KEY), which must
/// succeed.
fn tool(dir: &Path, program: &str, args: &[&str], key: Option<&str>) -> String {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).env_clear();
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    if let Some(key) = key {
        cmd.env("SOPS_AGE_KEY", key);
    }
    let out = cmd.output().expect(program);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{program}: {err}");
    String::from_utf8(out.stdout).unwrap()
}

/// A throwaway age identity in `dir`: (private, public).
fn identity(dir: &Path, name: &str) -> (String, String) {
    tool(dir, "age-keygen", &["-o", name], None);
    let text = std::fs::read_to_string(dir.join(name)).unwrap();
    let private = text.lines().find(|l| l.starts_with("AGE-SECRET-KEY-"));
    let public = tool(dir, "age-keygen", &["-y", name], None);
    (private.unwrap().to_string(), public.trim().to_string())
}

struct Repo {
    dir: tempfile::TempDir,
    keys: tempfile::TempDir,
    owner: (String, String),
    ci: (String, String),
}

/// `None` (skip, with a message) without sops or age-keygen locally; CI must have them. A secrets
/// repo whose `prod/app.sops.yaml` is encrypted for the owner only, while its rule also names `ci`.
fn repo() -> Option<Repo> {
    let path = std::env::var_os("PATH");
    let on_path = |p: &str| resolve(OsStr::new(p), path.as_deref()).is_some();
    if !on_path("sops") || !on_path("age-keygen") {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must install sops and age-keygen"
        );
        eprintln!("skipped: sops or age-keygen is not on PATH (spec §15.9.1)");
        return None;
    }
    let (dir, keys) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (owner, ci) = (
        identity(keys.path(), "owner.txt"),
        identity(keys.path(), "ci.txt"),
    );
    let put = |at: &Path, name: &str, text: &str| std::fs::write(at.join(name), text).unwrap();
    let (o, c) = (&owner.1, &ci.1);
    put(
        dir.path(),
        "access.toml",
        &format!(
            r#"version = 1
recipient = [{{ name = "owner-2026", kind = "human", age = "{o}", expires = "2027-10-27" }},
  {{ name = "site-prod", kind = "ci", repo = "keelstackhq/site", env = "prod", provider = "github", age = "{c}", expires = "2027-10-27" }}]
rule = [{{ path = "prod/**", recipients = ["owner-2026", "site-prod"] }}]
"#
        ),
    );
    // Encrypted outside the repo, for the owner only.
    let rules = format!("creation_rules:\n  - path_regex: '.*'\n    age: {o}\n");
    put(keys.path(), "only-owner.yaml", &rules);
    put(keys.path(), "plain.yaml", "API_TOKEN: hoist-test-value-1\n");
    let args = "--config only-owner.yaml encrypt --unencrypted-suffix _unencrypted plain.yaml";
    let enc = tool(
        keys.path(),
        "sops",
        &args.split(' ').collect::<Vec<_>>(),
        None,
    );
    std::fs::create_dir(dir.path().join("prod")).unwrap();
    put(dir.path(), FILE, &enc);
    Some(Repo {
        dir,
        keys,
        owner,
        ci,
    })
}

/// `hoist secrets sync --json [args]` with `key` as the owner's SOPS_AGE_KEY, outside an agent
/// session, and no other key file (HOME is a temp dir).
fn sync(repo: &Repo, key: &str, args: &[&str], extra: &[(&str, &str)]) -> (i32, Value, String) {
    let mut cmd = hoist();
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_REMOTE");
    cmd.env("HOME", repo.keys.path()).env("SOPS_AGE_KEY", key);
    cmd.env("HOIST_NOW", "2026-10-27T00:00:00Z");
    cmd.envs(extra.iter().copied());
    cmd.args(["secrets", "sync", "--json", "-C"])
        .arg(repo.dir.path())
        .args(args);
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.code().unwrap(), json, err)
}

/// The item for `key` has `want` as its action.
fn is(report: &Value, key: &str, want: &str) {
    let items = report["items"].as_array().expect("items");
    let item = items.iter().find(|i| i["key"] == key);
    assert_eq!(
        item.map(|i| &i["action"]),
        Some(&Value::from(want)),
        "{key}: {report:#}"
    );
}

fn recipients(repo: &Repo) -> BTreeSet<String> {
    let text = std::fs::read_to_string(repo.dir.path().join(FILE)).unwrap();
    sopsfile::parse(&text).unwrap().recipients
}

/// The value `key` decrypts from the file.
fn decrypted(repo: &Repo, key: &str) -> String {
    let file = repo.dir.path().join(FILE);
    let args = [
        "decrypt",
        "--extract",
        "[\"API_TOKEN\"]",
        file.to_str().unwrap(),
    ];
    tool(repo.keys.path(), "sops", &args, Some(key))
}

#[test]
fn sync_writes_the_config_adds_missing_recipients_and_rotates() {
    let Some(repo) = repo() else { return };
    let (code, report, err) = sync(&repo, &repo.owner.0, &[], &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    is(&report, ".sops.yaml", "create");
    is(&report, FILE, "update");
    let want: BTreeSet<String> = [repo.owner.1.clone(), repo.ci.1.clone()].into();
    assert_eq!(recipients(&repo), want);
    assert_eq!(decrypted(&repo, &repo.ci.0), "hoist-test-value-1");
    // `secrets check` agrees, and a second sync changes nothing.
    let mut check = hoist();
    check
        .env("HOIST_NOW", "2026-10-27T00:00:00Z")
        .args(["secrets", "check", "-C"]);
    assert_eq!(
        check.arg(repo.dir.path()).output().unwrap().status.code(),
        Some(0)
    );
    let file = || std::fs::read(repo.dir.path().join(FILE)).unwrap();
    let before = file();
    let (code, report, err) = sync(&repo, &repo.owner.0, &[], &[]);
    assert_eq!(code, 0, "{err}");
    is(&report, ".sops.yaml", "ok");
    is(&report, FILE, "ok");
    assert_eq!(file(), before);
    assert!(!(report.to_string() + &err).contains("hoist-test-value"));
    // --rotate: a new data key, still readable by both.
    let (code, report, err) = sync(&repo, &repo.owner.0, &["--rotate"], &[]);
    assert_eq!(code, 0, "{err}");
    is(&report, FILE, "update");
    assert_ne!(file(), before);
    for key in [&repo.owner.0, &repo.ci.0] {
        assert_eq!(decrypted(&repo, key), "hoist-test-value-1");
    }
    // A revoke: `ci` leaves the rule; after `sync --rotate` its key reads nothing.
    let access = std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    let access = access.replace(r#"["owner-2026", "site-prod"]"#, r#"["owner-2026"]"#);
    std::fs::write(repo.dir.path().join("access.toml"), access).unwrap();
    // A sops file no rule covers is drift, not skipped: it would keep the old key.
    std::fs::write(repo.dir.path().join("stray.sops.yaml"), "x: 1\n").unwrap();
    let (code, report, _) = sync(&repo, &repo.owner.0, &["--rotate"], &[]);
    (
        assert_eq!(code, 1),
        is(&report, FILE, "update"),
        is(&report, "stray.sops.yaml", "drift"),
    );
    assert_eq!(recipients(&repo), [repo.owner.1.clone()].into());
    let file = repo.dir.path().join(FILE);
    let mut sops = Command::new("sops");
    sops.args(["decrypt", file.to_str().unwrap()])
        .env_clear()
        .env("SOPS_AGE_KEY", &repo.ci.0);
    let path = std::env::var_os("PATH").unwrap_or_default();
    assert!(
        !sops.env("PATH", path).output().unwrap().status.success(),
        "the revoked key reads"
    );
}

#[test]
fn sync_is_refused_in_an_agent_session_and_fails_without_an_owner_key() {
    let Some(repo) = repo() else { return };
    let before = std::fs::read(repo.dir.path().join(FILE)).unwrap();
    for (name, value) in [("CLAUDECODE", "1"), ("CLAUDE_CODE_REMOTE", "true")] {
        let (code, report, err) = sync(&repo, &repo.owner.0, &[], &[(name, value)]);
        let step = report["human_steps"][0]["title"]
            .as_str()
            .unwrap_or_default();
        assert!(
            code == 2 && step.contains("agent session"),
            "{name}: {err} {report:#}"
        );
        assert!(
            !repo.dir.path().join(".sops.yaml").exists(),
            "{name} wrote .sops.yaml"
        );
    }
    // A key that is not a recipient: sops cannot open the data key, so nothing changes.
    let (stranger, _) = identity(repo.keys.path(), "stranger.txt");
    let (code, report, err) = sync(&repo, &stranger, &[], &[]);
    assert_eq!(code, 1, "{err} {report:#}");
    is(&report, FILE, "drift");
    assert_eq!(std::fs::read(repo.dir.path().join(FILE)).unwrap(), before);
    let all = report.to_string() + &err;
    assert!(
        !all.contains("hoist-test-value") && !all.contains("AGE-SECRET-KEY"),
        "{all}"
    );
}
