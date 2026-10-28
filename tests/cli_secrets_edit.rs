//! `hoist secrets edit <file>` in a secrets repo (spec §15.4): `sops edit` with the owner's key,
//! on a file under a rule whose recipients match access.toml. Real sops; the editor is a script;
//! keys made at runtime only.
#![cfg(unix)]
mod common;

use common::{hoist, write_script};
use hoist::secrets::decrypt::resolve;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

const FILE: &str = "prod/app.sops.yaml";
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

/// `hoist secrets edit <args>` outside an agent session with the owner's key, `editor` as
/// EDITOR; first `hoist secrets sync`, so .sops.yaml matches access.toml.
fn edit(repo: &Repo, args: &[&str], editor: &str, extra: &[(&str, &str)]) -> (i32, String, String) {
    let script = repo.keys.path().join("editor.sh");
    write_script(&script, format!("#!/bin/sh\n{editor}\n").as_bytes(), 0o755);
    let run = |sub: &[&str], extra: &[(&str, &str)]| {
        let mut cmd = hoist();
        cmd.env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_REMOTE");
        cmd.env("HOME", repo.keys.path())
            .env("SOPS_AGE_KEY", &repo.owner.0);
        cmd.env("EDITOR", &script).envs(extra.iter().copied());
        cmd.args(["secrets"])
            .args(sub)
            .arg("-C")
            .arg(repo.dir.path());
        cmd.output().unwrap()
    };
    if !repo.dir.path().join(".sops.yaml").exists() {
        assert!(run(&["sync"], &[]).status.code().is_some());
    }
    let out = run(&[&["edit"][..], args].concat(), extra);
    let (stdout, err) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert!(
        !format!("{stdout}{err}").contains("AGE-SECRET-KEY"),
        "{stdout}{err}"
    );
    (out.status.code().unwrap(), stdout, err)
}

fn value(repo: &Repo, rel: &str, key: &str) -> Option<String> {
    let file = repo.dir.path().join(rel);
    let args = ["decrypt", "--extract", key, file.to_str().unwrap()];
    tool(repo.keys.path(), "sops", &args, Some(&repo.owner.0))
}

#[test]
fn edit_changes_a_value_with_the_owners_key() {
    let Some(repo) = repo() else { return };
    let new = r#"sed -i 's/hoist-test-value-1/hoist-test-value-9/' "$1""#;
    let (code, out, err) = edit(&repo, &[FILE], new, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        value(&repo, FILE, "[\"API_TOKEN\"]").as_deref(),
        Some("hoist-test-value-9")
    );
    assert!(out.contains(FILE) && out.contains("update"), "{out}");
    let dir = repo.dir.path().display();
    assert!(
        err.contains(&format!(
            "git -C '{dir}' add access.toml .sops.yaml '{FILE}'"
        )),
        "{err}"
    );
    // The ci key still reads it: the recipients are the rule's.
    let file = repo.dir.path().join(FILE);
    let read = tool(
        repo.keys.path(),
        "sops",
        &["decrypt", file.to_str().unwrap()],
        Some(&repo.ci.0),
    );
    assert!(read.is_some());
    // No change: ok, exit 0.
    let (code, out, _) = edit(&repo, &[FILE], "true", &[]);
    assert!(code == 0 && out.contains("ok"), "{out}");
}

#[test]
fn edit_creates_a_new_file_for_its_rules_recipients() {
    let Some(repo) = repo() else { return };
    let rel = "prod/new.sops.yaml";
    // A recipient list in the owner's shell would replace the rule's: hoist keeps the rule's.
    let (code, out, err) = edit(
        &repo,
        &[rel],
        r#"printf 'NEW_TOKEN: hoist-test-value-5\n' > "$1""#,
        &[("SOPS_AGE_RECIPIENTS", &repo.owner.1)],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("create"), "{out}");
    assert_eq!(
        value(&repo, rel, "[\"NEW_TOKEN\"]").as_deref(),
        Some("hoist-test-value-5")
    );
    let file = repo.dir.path().join(rel);
    let ci = tool(
        repo.keys.path(),
        "sops",
        &["decrypt", file.to_str().unwrap()],
        Some(&repo.ci.0),
    );
    assert!(ci.is_some(), "the ci key cannot read the new file");
    // An odd name still gives a plain branch name.
    let odd = "prod/a;b.sops.yaml";
    let (code, out, err) = edit(&repo, &[odd], r#"printf 'X: 1\n' > "$1""#, &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        err.contains("switch -c hoist/edit-prod-a-b-sops-yaml\n"),
        "{err}"
    );
}

/// Arguments, extra environment, the expected exit code and a text the output must contain.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], i32, &'a str);

#[test]
fn edit_refuses_and_changes_nothing() {
    let Some(repo) = repo() else { return };
    // .sops.yaml first, as `edit` expects it.
    assert_eq!(edit(&repo, &[FILE], "true", &[]).0, 0);
    let read = |name: &str| std::fs::read_to_string(repo.dir.path().join(name)).unwrap();
    let file = read(FILE);
    let changes = r#"printf 'X: 1\n' > "$1""#;
    let only = tempfile::tempdir().unwrap();
    let path = only.path().to_str().unwrap();
    // A directory that leads out of the repo, and one that does not exist.
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.dir.path().join("preview")).unwrap();
    let cases: [Case; 12] = [
        (&[FILE], &[("CLAUDECODE", "1")], 2, "agent session"),
        (&[FILE], &[("PATH", path)], 3, "sops"),
        (&["staging/x.sops.yaml"], &[], 3, "no rule"),
        (&["prod/../preview/x.sops.yaml"], &[], 3, "no rule"),
        (&["prod/notes.txt"], &[], 3, ".sops.yaml"),
        (&["access.toml"], &[], 3, ".sops.yaml"),
        (&[FILE, "--json"], &[], 3, "--json"),
        (&["/prod/x.sops.yaml"], &[], 3, "no rule"),
        (&["prod/.sops.yaml"], &[], 3, "not a secrets file"),
        (&["preview/x.sops.yaml"], &[], 3, "outside the repo"),
        (&["prod/sub/x.sops.yaml"], &[], 3, "prod/sub does not exist"),
        (&[FILE], &[], 1, "sops edit failed"),
    ];
    for (args, extra, want, text) in cases {
        let editor = if want == 1 { "exit 1" } else { changes };
        let (code, out, err) = edit(&repo, args, editor, extra);
        assert!(
            code == want && format!("{out}{err}").contains(text),
            "{args:?}: {code} {out}{err}"
        );
        assert_eq!(read(FILE), file, "{args:?}");
    }
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    // .sops.yaml was changed by hand: refused, nothing opened.
    let config = read(".sops.yaml");
    std::fs::write(repo.dir.path().join(".sops.yaml"), format!("{config}# x\n")).unwrap();
    let (code, out, err) = edit(&repo, &[FILE], changes, &[]);
    assert!(
        code == 3 && err.contains(".sops.yaml does not match"),
        "{out}{err}"
    );
    std::fs::write(repo.dir.path().join(".sops.yaml"), config).unwrap();
    // The file lacks a recipient its rule lists (sync has not run since): refused.
    let o = &repo.owner.1;
    let owner_only = format!("creation_rules:\n  - path_regex: '.*'\n    age: {o}\n");
    std::fs::write(repo.keys.path().join("owner.yaml"), owner_only).unwrap();
    let args = ["--config", "owner.yaml", "encrypt", "plain.yaml"];
    let narrow = tool(repo.keys.path(), "sops", &args, None).unwrap();
    std::fs::write(repo.dir.path().join(FILE), &narrow).unwrap();
    let (code, out, err) = edit(&repo, &[FILE], changes, &[]);
    assert!(
        code == 3 && err.contains("recipients; run `hoist secrets sync`"),
        "{out}{err}"
    );
    assert_eq!(read(FILE), narrow);
}
