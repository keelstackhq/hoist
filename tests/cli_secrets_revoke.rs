//! `hoist secrets revoke <recipient> [--delete-secret]` in a secrets repo (spec §15.4, §15.5): the
//! recipient leaves access.toml, every file gets a new data key, and the values it could read are
//! listed for rotation. A shared `ci` key's secret stays unless `--delete-secret`. Real sops; keys
//! made at runtime only.
#![cfg(unix)]
mod common;

use common::{FakeApi, hoist};
use hoist::secrets::decrypt::resolve;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

const FILE: &str = "prod/app.sops.yaml";
const DELETE: &str = "DELETE /repos/acme/site/environments/prod/secrets/SOPS_AGE_KEY";

/// The stdout of `program args` in `dir` with only PATH (and `key` as SOPS_AGE_KEY), or `None`.
fn tool(dir: &Path, program: &str, args: &[&str], key: Option<&str>) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).env_clear();
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    key.map(|k| cmd.env("SOPS_AGE_KEY", k));
    let out = cmd.output().expect(program);
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap())
}

/// A throwaway age identity in `dir`: (private, public).
fn identity(dir: &Path, name: &str) -> (String, String) {
    tool(dir, "age-keygen", &["-o", name], None).unwrap();
    let text = std::fs::read_to_string(dir.join(name)).unwrap();
    let private = text.lines().find(|l| l.starts_with("AGE-SECRET-KEY-"));
    let public = tool(dir, "age-keygen", &["-y", name], None).unwrap();
    (private.unwrap().to_string(), public.trim().to_string())
}

struct Repo {
    dir: tempfile::TempDir,
    keys: tempfile::TempDir,
    owner: (String, String),
    ci: (String, String),
}

/// `None` without sops or age-keygen locally (CI must have them): a secrets repo whose
/// `prod/app.sops.yaml` is encrypted for the owner and `site-prod`, the ci key of acme/site.
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
    let owner = identity(keys.path(), "owner.txt");
    let ci = identity(keys.path(), "ci.txt");
    let put = |at: &Path, name: &str, text: &str| std::fs::write(at.join(name), text).unwrap();
    let (o, c) = (&owner.1, &ci.1);
    put(
        dir.path(),
        "access.toml",
        &format!(
            "version = 1\n\n[[recipient]]\nname = \"owner-2026\"\nkind = \"human\"\nage = \"{o}\"\nexpires = \"2027-10-27\"\n\n[[recipient]]\nname = \"site-prod\"\nkind = \"ci\"\nage = \"{c}\"\nrepo = \"acme/site\"\nenv = \"prod\"\nprovider = \"github\"\nexpires = \"2027-10-27\"\n\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\", \"site-prod\"]\n\n[[rule]]\npath = \"preview/**\"\nrecipients = [\"owner-2026\"]\n"
        ),
    );
    put(
        keys.path(),
        "both.yaml",
        &format!("creation_rules:\n  - path_regex: '.*'\n    age: {o},{c}\n"),
    );
    put(
        keys.path(),
        "plain.yaml",
        "API_TOKEN: hoist-test-value-1\nDB_URL: hoist-test-value-2\n",
    );
    let args = "--config both.yaml encrypt plain.yaml";
    let enc = tool(
        keys.path(),
        "sops",
        &args.split(' ').collect::<Vec<_>>(),
        None,
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("prod")).unwrap();
    put(dir.path(), FILE, &enc);
    Some(Repo {
        dir,
        keys,
        owner,
        ci,
    })
}

/// `hoist secrets revoke <args>` outside an agent session, the owner's key as SOPS_AGE_KEY;
/// `--json` unless `json` is false (then stderr has the hint).
fn revoke(
    repo: &Repo,
    api: &FakeApi,
    args: &[&str],
    extra: &[(&str, &str)],
    json: bool,
) -> (i32, Value, String) {
    let mut cmd = hoist();
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_REMOTE");
    cmd.env("HOME", repo.keys.path())
        .env("SOPS_AGE_KEY", &repo.owner.0);
    cmd.env("HOIST_NOW", "2026-10-27T00:00:00Z")
        .env("HOIST_GITHUB_API", api.url());
    cmd.env("GITHUB_TOKEN", "gh-test-token")
        .envs(extra.iter().copied());
    cmd.args(["secrets", "revoke", "-C"])
        .arg(repo.dir.path())
        .args(args);
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let all = format!("{json}{err}");
    assert!(
        !all.contains("AGE-SECRET-KEY") && !all.contains("hoist-test-value"),
        "{all}"
    );
    (out.status.code().unwrap(), json, err)
}

fn decrypts(repo: &Repo, key: &str) -> bool {
    let file = repo.dir.path().join(FILE);
    tool(
        repo.keys.path(),
        "sops",
        &["decrypt", file.to_str().unwrap()],
        Some(key),
    )
    .is_some()
}

fn step<'a>(report: &'a Value, id: &str) -> &'a str {
    let steps = report["human_steps"].as_array().unwrap();
    let found = steps.iter().find(|s| s["id"] == id);
    found.and_then(|s| s["title"].as_str()).unwrap_or_default()
}

#[test]
fn revoke_removes_a_ci_key_gives_new_data_keys_and_lists_what_it_could_read() {
    let Some(repo) = repo() else { return };
    // A preview file only the owner reads: not among the values to rotate.
    let o = &repo.owner.1;
    let owner_only = format!("creation_rules:\n  - path_regex: '.*'\n    age: {o}\n");
    std::fs::write(repo.keys.path().join("owner.yaml"), owner_only).unwrap();
    std::fs::write(
        repo.keys.path().join("web.yaml"),
        "WEB_TOKEN: hoist-test-value-3\n",
    )
    .unwrap();
    let args = ["--config", "owner.yaml", "encrypt", "web.yaml"];
    let web = tool(repo.keys.path(), "sops", &args, None).unwrap();
    std::fs::create_dir(repo.dir.path().join("preview")).unwrap();
    std::fs::write(repo.dir.path().join("preview/web.sops.yaml"), web).unwrap();
    let token = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("API_TOKEN:"))
            .unwrap()
            .to_string()
    };
    let before = token(&std::fs::read_to_string(repo.dir.path().join(FILE)).unwrap());
    let api = FakeApi::start(&[]);
    let (code, report, err) = revoke(&repo, &api, &["site-prod"], &[], true);
    assert_eq!(code, 2, "{err} {report:#}");
    // Gone from access.toml, from the file, with a new data key; nothing sent to GitHub.
    let access = std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    assert!(
        !access.contains("site-prod") && !access.contains(&repo.ci.1),
        "{access}"
    );
    assert!(access.contains("recipients = [\"owner-2026\"]\n\n[[rule]]\npath = \"preview/**\""));
    assert!(!decrypts(&repo, &repo.ci.0) && decrypts(&repo, &repo.owner.0));
    // A new data key: the value's ciphertext changed, not only the recipients.
    let after = token(&std::fs::read_to_string(repo.dir.path().join(FILE)).unwrap());
    assert_ne!(after, before);
    assert!(api.requests().is_empty());
    // The values it could read, to rotate at their source; its secret stays (shared).
    let values = step(&report, "rotate-values:site-prod");
    assert!(
        values.contains(&format!("{FILE}: API_TOKEN, DB_URL")),
        "{report:#}"
    );
    assert!(
        !values.contains("preview/") && !values.contains("WEB_TOKEN"),
        "{values}"
    );
    let secret = step(&report, "delete-secret:site-prod");
    assert!(
        secret.contains("environment prod of acme/site")
            && secret.contains("Settings → Environments"),
        "{report:#}"
    );
}

#[test]
fn revoke_prints_the_commands_to_commit() {
    let Some(repo) = repo() else { return };
    let api = FakeApi::start(&[]);
    let (code, _, err) = revoke(&repo, &api, &["site-prod"], &[], false);
    assert_eq!(code, 2, "{err}");
    let dir = repo.dir.path().display();
    for line in [
        format!("git -C '{dir}' switch -c hoist/revoke-site-prod"),
        format!("git -C '{dir}' add access.toml .sops.yaml '{FILE}'"),
        format!("git -C '{dir}' commit -m 'secrets: revoke site-prod'"),
        format!("git -C '{dir}' push -u origin hoist/revoke-site-prod"),
    ] {
        assert!(err.contains(&line), "{line}\n{err}");
    }
}

/// Arguments, extra environment, the expected exit code and a text the output must contain.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], i32, &'a str);

#[test]
fn revoke_refuses_and_changes_nothing() {
    let Some(repo) = repo() else { return };
    let read = |name: &str| std::fs::read_to_string(repo.dir.path().join(name)).unwrap();
    let (access, file) = (read("access.toml"), read(FILE));
    let only = tempfile::tempdir().unwrap();
    let path = only.path().to_str().unwrap();
    let cases: [Case; 5] = [
        (&["site-prod"], &[("CLAUDECODE", "1")], 2, "agent session"),
        (&["nobody"], &[], 3, "no recipient `nobody`"),
        (&["owner-2026"], &[], 3, "no human recipient"),
        (
            &["owner-2026", "--delete-secret"],
            &[],
            3,
            "only a ci recipient",
        ),
        (&["site-prod"], &[("PATH", path)], 3, "sops"),
    ];
    for (args, extra, want, text) in cases {
        let api = FakeApi::start(&[]);
        let (code, report, err) = revoke(&repo, &api, args, extra, true);
        assert!(
            code == want && format!("{report}{err}").contains(text),
            "{args:?}: {code} {err} {report:#}"
        );
        assert!(api.requests().is_empty(), "{args:?}");
        assert_eq!(
            (read("access.toml"), read(FILE)),
            (access.clone(), file.clone())
        );
    }
}

#[test]
fn revoke_delete_secret_deletes_it_and_says_what_hoist_cannot_see() {
    let Some(repo) = repo() else { return };
    let api = FakeApi::start(&[(DELETE, 204, "", &[])]);
    let (code, report, err) = revoke(&repo, &api, &["site-prod", "--delete-secret"], &[], true);
    assert_eq!(code, 2, "{err} {report:#}");
    let sent: Vec<String> = api
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(sent, [DELETE]);
    let items = report["items"].as_array().unwrap();
    let secret = items
        .iter()
        .find(|i| i["kind"] == "environment-secret")
        .unwrap();
    assert_eq!(
        (&secret["kind"], &secret["action"]),
        (&"environment-secret".into(), &"update".into()),
        "{report:#}"
    );
    let other = step(&report, "regrant:site-prod");
    assert!(other.contains("other secrets repos"), "{report:#}");
    assert!(step(&report, "delete-secret:site-prod").is_empty());
}

#[test]
fn revoke_that_cannot_delete_the_secret_is_drift_after_the_local_revoke() {
    let Some(repo) = repo() else { return };
    let api = FakeApi::start(&[(DELETE, 403, r#"{"message":"nope"}"#, &[])]);
    let (code, report, err) = revoke(&repo, &api, &["site-prod", "--delete-secret"], &[], true);
    assert_eq!(code, 1, "{err} {report:#}");
    // The local revoke stands: access.toml and the file no longer have the key.
    assert!(!decrypts(&repo, &repo.ci.0));
    let items = report["items"].as_array().unwrap();
    let secret = items
        .iter()
        .find(|i| i["kind"] == "environment-secret")
        .unwrap();
    assert_eq!(secret["action"], "drift", "{report:#}");
}
