mod common;

use common::{fixture_repo, git, hoist, set_origin_head};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

const REMOTE: &str = "https://github.com/acme/x.git";

/// The release-launcher fixture (crate `tool`, launcher `@acme/tool`) on branch `feat/cache`.
fn repo() -> TempDir {
    let repo = fixture_repo("release-launcher", REMOTE);
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    git(d, &["checkout", "-q", "-b", "feat/cache"]);
    repo
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    hoist()
        .args(["-C", dir.to_str().unwrap(), "--json", "changeset"])
        .args(args)
        .output()
        .expect("run hoist")
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn add_writes_changesets_bytes() {
    let r = repo();
    let o = run(r.path(), &["add", "tool:minor", "-m", "faster cache"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        read(r.path(), ".changeset/feat-cache.md"),
        "---\n\"tool\": minor\n---\n\nfaster cache\n"
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["command"], "changeset add");
    let i = &v["items"][0];
    assert_eq!(i["kind"], "changeset");
    assert_eq!(i["key"], "feat-cache");
    assert_eq!(i["action"], "create");
    assert_eq!(i["source"], ".changeset/feat-cache.md");
}

#[test]
fn add_several_packages_in_order_and_explicit_id() {
    let r = repo();
    let o = run(
        r.path(),
        &[
            "add",
            "@acme/none:none",
            "tool:patch",
            "-m",
            "x",
            "--id",
            "my-id",
        ],
    );
    // `@acme/none` is not a package: nothing written.
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("@acme/none"), "{}", stderr(&o));
    assert!(!r.path().join(".changeset").exists());
    let o = run(r.path(), &["add", "tool:patch", "-m", "x", "--id", "my-id"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(r.path().join(".changeset/my-id.md").exists());
}

#[test]
fn add_empty() {
    let r = repo();
    let o = run(r.path(), &["add", "--empty"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(read(r.path(), ".changeset/feat-cache.md"), "---\n---\n");
    let r = repo();
    let o = run(r.path(), &["add", "--empty", "-m", "docs only"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        read(r.path(), ".changeset/feat-cache.md"),
        "---\n---\n\ndocs only\n"
    );
}

#[test]
fn add_reads_summary_from_stdin() {
    let r = repo();
    let o = hoist()
        .args([
            "-C",
            r.path().to_str().unwrap(),
            "changeset",
            "add",
            "tool:major",
            "-m",
            "-",
        ])
        .write_stdin("line1\nline2\n\n")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        read(r.path(), ".changeset/feat-cache.md"),
        "---\n\"tool\": major\n---\n\nline1\nline2\n"
    );
}

#[test]
fn add_usage_errors_exit_3() {
    let r = repo();
    for (args, needle) in [
        (vec!["add"], "--empty"),
        (vec!["add", "tool:minor"], "-m"),
        (vec!["add", "tool:minor", "-m", "  "], "summary"),
        (vec!["add", "tool", "-m", "x"], "<name>:<type>"),
        (vec!["add", "tool:huge", "-m", "x"], "huge"),
        (vec!["add", "tool:minor", "tool:patch", "-m", "x"], "twice"),
        (vec!["add", "--empty", "tool:minor"], "--empty"),
        (vec!["add", "tool:minor", "-m", "x", "--id", "a/b"], "--id"),
        (vec!["add", "tool:minor", "-m", "x", "--id", ".x"], "--id"),
    ] {
        let o = run(r.path(), &args);
        assert_eq!(o.status.code(), Some(3), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).contains(needle), "{args:?}: {}", stderr(&o));
    }
    assert!(!r.path().join(".changeset").exists());
}

#[test]
fn add_unknown_package_exit_3() {
    let r = repo();
    let o = run(r.path(), &["add", "nope:minor", "-m", "x"]);
    assert_eq!(o.status.code(), Some(3));
    let e = stderr(&o);
    assert!(
        e.contains("nope") && e.contains("not a releasable package"),
        "{e}"
    );
    assert!(e.contains("tool"), "lists the known names: {e}");
}

#[test]
fn add_launcher_name_exit_3_names_crate() {
    let r = repo();
    for name in ["@acme/tool", "@acme/tool-linux-x64"] {
        let o = run(r.path(), &["add", &format!("{name}:minor"), "-m", "x"]);
        assert_eq!(o.status.code(), Some(3), "{name}");
        let e = stderr(&o);
        assert!(e.contains(name) && e.contains("follows `tool`"), "{e}");
    }
}

#[test]
fn add_existing_id_exit_3() {
    let r = repo();
    assert_eq!(
        run(r.path(), &["add", "tool:patch", "-m", "a"])
            .status
            .code(),
        Some(0)
    );
    let o = run(r.path(), &["add", "tool:minor", "-m", "b"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("feat-cache"), "{}", stderr(&o));
    assert!(read(r.path(), ".changeset/feat-cache.md").contains("patch"));
}

#[test]
fn add_detached_head_uses_words() {
    let r = repo();
    git(r.path(), &["checkout", "-q", "--detach"]);
    let o = run(r.path(), &["add", "--empty"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let id = v["items"][0]["key"].as_str().unwrap();
    let parts: Vec<&str> = id.split('-').collect();
    assert_eq!(parts.len(), 3, "{id}");
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()))
    );
    assert!(r.path().join(format!(".changeset/{id}.md")).exists());
}

#[test]
fn init_writes_config_and_readme() {
    let r = repo();
    git(
        r.path(),
        &["update-ref", "refs/remotes/origin/trunk", "HEAD"],
    );
    set_origin_head(r.path(), "trunk");
    let o = run(r.path(), &["init"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        read(r.path(), ".changeset/config.json"),
        "{\n  \"$schema\": \"https://unpkg.com/@changesets/config@3.1.4/schema.json\",\n  \
         \"changelog\": \"@changesets/cli/changelog\",\n  \"commit\": false,\n  \"fixed\": [],\n  \
         \"linked\": [],\n  \"access\": \"public\",\n  \"baseBranch\": \"trunk\",\n  \
         \"updateInternalDependencies\": \"patch\",\n  \"ignore\": []\n}\n"
    );
    assert!(read(r.path(), ".changeset/README.md").starts_with("# Changesets\n"));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["command"], "changeset init");
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    // hoist reads what it wrote.
    let cfg = hoist::changeset::read_config(r.path()).unwrap().unwrap();
    assert_eq!(cfg.base_branch, "trunk");
}

#[test]
fn init_defaults_main_and_restricted() {
    let r = fixture_repo("release-single", REMOTE);
    let o = run(r.path(), &["init"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let c = read(r.path(), ".changeset/config.json");
    assert!(c.contains("\"baseBranch\": \"main\""), "{c}");
    assert!(c.contains("\"access\": \"restricted\""), "{c}");
}

#[test]
fn init_twice_exit_3() {
    let r = repo();
    assert_eq!(run(r.path(), &["init"]).status.code(), Some(0));
    let before = read(r.path(), ".changeset/config.json");
    let o = run(r.path(), &["init"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("config.json"), "{}", stderr(&o));
    assert_eq!(read(r.path(), ".changeset/config.json"), before);
}

#[test]
fn status_is_not_yet() {
    let r = repo();
    let o = run(r.path(), &["status"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("not yet"), "{}", stderr(&o));
}

#[test]
fn no_http_requests() {
    // The API bases point at a closed port; a request would fail the command with exit 3.
    let r = repo();
    for args in [vec!["init"], vec!["add", "tool:patch", "-m", "x"]] {
        let o = run(r.path(), &args);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", stderr(&o));
    }
}
