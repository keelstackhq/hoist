//! `hoist changeset status`: changed packages carry a changeset (changesets spec §6).

mod common;

use common::{fixture_repo, git, hoist};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

const CONFIG: &str = "{\"baseBranch\": \"main\"}\n";

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
}

/// `main` (also `origin/main`) holds the fixture plus `config`; HEAD is branch `feat/x`.
fn repo_with(fixture: &str, config: &str) -> TempDir {
    let repo = fixture_repo(fixture, "https://github.com/acme/x.git");
    let d = repo.path();
    write(d, ".changeset/config.json", config);
    write(d, ".changeset/README.md", "# Changesets\n");
    git(d, &["checkout", "-q", "-b", "main"]);
    commit(d, "chore: init");
    git(d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(d, &["checkout", "-q", "-b", "feat/x"]);
    repo
}

/// release-launcher: crate `tool` in crates/tool, launcher `@acme/tool` in npm/tool.
fn repo() -> TempDir {
    repo_with("release-launcher", CONFIG)
}

/// Write files on the branch and commit them.
fn change(dir: &Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        write(dir, rel, text);
    }
    commit(dir, "feat: change");
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = hoist();
    // CI sets this on pull_request; the release PR's own run must not exempt these tests.
    cmd.env_remove("GITHUB_HEAD_REF");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(["-C", dir.to_str().unwrap(), "--json", "changeset", "status"])
        .args(args)
        .output()
        .expect("run hoist")
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    run_env(dir, args, &[])
}

fn json(o: &std::process::Output, code: i32) -> Value {
    assert_eq!(
        o.status.code(),
        Some(code),
        "stdout {} stderr {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}

fn stderr_3(o: &std::process::Output) -> String {
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn item<'a>(v: &'a Value, key: &str) -> &'a Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("no item {key} in {v}"))
}

fn keys(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn changed_crate_without_changeset_is_drift_exit_1() {
    let r = repo();
    change(r.path(), &[("crates/tool/src/lib.rs", "pub fn x() {}\n")]);
    let v = json(&run(r.path(), &[]), 1);
    assert_eq!(v["version"], 1);
    assert_eq!(v["command"], "changeset status");
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit_code"], 1);
    assert!(v["human_steps"].as_array().unwrap().is_empty());
    assert_eq!(keys(&v), ["tool"]);
    let i = item(&v, "tool");
    assert_eq!(i["area"], "changeset");
    assert_eq!(i["kind"], "package");
    assert_eq!(i["action"], "drift");
    let why = i["reason"].as_str().unwrap();
    assert!(why.contains("changed without a changeset"), "{why}");
    assert!(why.contains("hoist changeset add tool:patch"), "{why}");
    assert!(why.contains("--empty"), "{why}");
}

#[test]
fn named_in_changeset_ok() {
    let r = repo();
    change(
        r.path(),
        &[
            ("crates/tool/src/lib.rs", "pub fn x() {}\n"),
            (".changeset/feat-x.md", "---\n\"tool\": minor\n---\n\nx\n"),
        ],
    );
    let v = json(&run(r.path(), &[]), 0);
    assert_eq!(v["ok"], true);
    let i = item(&v, "tool");
    assert_eq!(i["action"], "ok");
    assert_eq!(i["reason"], "feat-x: minor");
    assert_eq!(i["source"], ".changeset/feat-x.md");
}

#[test]
fn none_type_counts() {
    let r = repo();
    change(
        r.path(),
        &[
            ("crates/tool/src/lib.rs", "pub fn x() {}\n"),
            (".changeset/feat-x.md", "---\ntool: none\n---\n"),
        ],
    );
    let v = json(&run(r.path(), &[]), 0);
    assert_eq!(item(&v, "tool")["reason"], "feat-x: none");
}

#[test]
fn empty_changeset_covers_all() {
    let r = repo();
    change(
        r.path(),
        &[
            ("crates/tool/src/lib.rs", "pub fn x() {}\n"),
            ("npm/tool/bin/tool.js", "x\n"),
            (".changeset/feat-x.md", "---\n---\n"),
        ],
    );
    let v = json(&run(r.path(), &[]), 0);
    let i = item(&v, "tool");
    assert_eq!(i["action"], "ok");
    assert_eq!(i["reason"], "feat-x: empty changeset");
}

#[test]
fn root_crate_excludes_other_package_dirs() {
    // Root crate `solo`; npm package `a` in packages/a.
    let r = repo_with("release-single", CONFIG);
    let d = r.path();
    git(d, &["checkout", "-q", "main"]);
    write(d, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    write(
        d,
        "package.json",
        "{\"name\": \"root\", \"private\": true}\n",
    );
    write(
        d,
        "packages/a/package.json",
        "{\"name\": \"a\", \"version\": \"1.0.0\", \"private\": false}\n",
    );
    commit(d, "chore: add a");
    git(d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(d, &["checkout", "-q", "feat/x"]);
    git(d, &["reset", "-q", "--hard", "main"]);

    change(d, &[("packages/a/index.js", "x\n")]);
    let v = json(&run(d, &[]), 1);
    assert_eq!(keys(&v), ["a"]);

    change(d, &[("src/main.rs", "fn main() { }\n")]);
    let v = json(&run(d, &[]), 1);
    assert_eq!(keys(&v), ["a", "solo"]);
    assert_eq!(item(&v, "solo")["action"], "drift");
}

#[test]
fn launcher_dir_counts_for_crate() {
    let r = repo();
    change(
        r.path(),
        &[
            ("npm/tool/bin/tool.js", "x\n"),
            ("npm/tool-linux-x64/README.md", "x\n"),
        ],
    );
    let v = json(&run(r.path(), &[]), 1);
    assert_eq!(keys(&v), ["tool"]);
    assert_eq!(item(&v, "tool")["action"], "drift");
}

#[test]
fn changed_file_patterns_filter() {
    let cfg = "{\"baseBranch\": \"main\", \"changedFilePatterns\": [\"src/**\", \"npm/**\"]}\n";
    let r = repo_with("release-launcher", cfg);
    change(
        r.path(),
        &[("crates/tool/README.md", "docs\n"), ("docs/x.md", "x\n")],
    );
    let v = json(&run(r.path(), &[]), 0);
    assert_eq!(keys(&v), ["branch"]);
    assert_eq!(item(&v, "branch")["action"], "ok");
    assert_eq!(item(&v, "branch")["reason"], "no package changed");

    // Patterns are relative to the crate dir; a launcher outside it, to the repo root.
    change(r.path(), &[("crates/tool/src/lib.rs", "x\n")]);
    assert_eq!(keys(&json(&run(r.path(), &[]), 1)), ["tool"]);
    git(r.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    change(r.path(), &[("npm/tool/bin/tool.js", "x\n")]);
    assert_eq!(keys(&json(&run(r.path(), &[]), 1)), ["tool"]);
}

#[test]
fn changesets_dir_changes_alone_ok() {
    let r = repo();
    change(r.path(), &[(".changeset/README.md", "# edited\n")]);
    let v = json(&run(r.path(), &[]), 0);
    assert_eq!(keys(&v), ["branch"]);
}

#[test]
fn changeset_on_base_or_only_modified_does_not_count() {
    let r = repo();
    let d = r.path();
    git(d, &["checkout", "-q", "main"]);
    write(d, ".changeset/old.md", "---\n\"tool\": patch\n---\n\nold\n");
    commit(d, "chore: old changeset");
    git(d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(d, &["checkout", "-q", "feat/x"]);
    git(d, &["reset", "-q", "--hard", "main"]);
    change(
        d,
        &[
            ("crates/tool/src/lib.rs", "x\n"),
            (".changeset/old.md", "---\n\"tool\": minor\n---\n\nold\n"),
        ],
    );
    assert_eq!(item(&json(&run(d, &[]), 1), "tool")["action"], "drift");
}

#[test]
fn release_branch_is_exempt() {
    let r = repo();
    // As `release prepare` leaves it: changesets deleted, versions bumped.
    let d = r.path();
    git(d, &["checkout", "-q", "main"]);
    write(
        d,
        ".changeset/feat-x.md",
        "---\n\"tool\": minor\n---\n\nx\n",
    );
    commit(d, "feat: x");
    git(d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(d, &["checkout", "-q", "-b", "hoist/release"]);
    std::fs::remove_file(d.join(".changeset/feat-x.md")).unwrap();
    change(
        d,
        &[(
            "crates/tool/Cargo.toml",
            "[package]\nname = \"tool\"\nversion = \"0.4.0\"\nedition = \"2024\"\n",
        )],
    );
    let v = json(&run(d, &[]), 0);
    assert_eq!(keys(&v), ["branch"]);
    assert_eq!(item(&v, "branch")["reason"], "release PR");

    // On CI the PR checkout is detached; GITHUB_HEAD_REF names the branch.
    git(d, &["checkout", "-q", "--detach"]);
    let v = json(&run_env(d, &[], &[("GITHUB_HEAD_REF", "hoist/release")]), 0);
    assert_eq!(item(&v, "branch")["reason"], "release PR");
    json(&run_env(d, &[], &[("GITHUB_HEAD_REF", "feat/y")]), 1);
}

#[test]
fn bad_changeset_exit_3() {
    let r = repo();
    change(
        r.path(),
        &[(".changeset/feat-x.md", "---\n\"nope\": minor\n---\n")],
    );
    let e = stderr_3(&run(r.path(), &[]));
    assert!(e.contains(".changeset/feat-x.md"), "{e}");
    assert!(e.contains("nope"), "{e}");

    // A launcher name is refused even on a file the diff did not add.
    let r = repo();
    change(
        r.path(),
        &[(".changeset/a.md", "---\n\"@acme/tool\": minor\n---\n")],
    );
    let e = stderr_3(&run(r.path(), &[]));
    assert!(e.contains("name `tool`"), "{e}");

    let r = repo();
    change(r.path(), &[(".changeset/config.json", "{\"bogus\": 1}\n")]);
    let e = stderr_3(&run(r.path(), &[]));
    assert!(e.contains("config.json"), "{e}");
}

#[test]
fn no_config_exit_3() {
    let r = repo();
    std::fs::remove_dir_all(r.path().join(".changeset")).unwrap();
    let e = stderr_3(&run(r.path(), &[]));
    assert!(e.contains("hoist changeset init"), "{e}");
}

#[test]
fn since_head_parent_on_merge_commit() {
    // actions/checkout on pull_request: a detached merge commit of the PR into main.
    let r = repo();
    let d = r.path();
    change(d, &[("crates/tool/src/lib.rs", "x\n")]);
    git(d, &["checkout", "-q", "main"]);
    change(d, &[("other.txt", "main moved on\n")]);
    git(d, &["checkout", "-q", "--detach"]);
    git(d, &["merge", "-q", "--no-ff", "--no-edit", "feat/x"]);
    let v = json(&run(d, &["--since", "HEAD^1"]), 1);
    assert_eq!(keys(&v), ["tool"]);
}

#[test]
fn default_base_is_origin_base_branch() {
    let r = repo_with("release-launcher", "{\"baseBranch\": \"trunk\"}\n");
    let d = r.path();
    change(d, &[("crates/tool/src/lib.rs", "x\n")]);
    // No origin/trunk yet: exit 3 with the fix.
    let e = stderr_3(&run(d, &[]));
    assert!(e.contains("origin/trunk"), "{e}");
    assert!(e.contains("--since"), "{e}");
    git(d, &["update-ref", "refs/remotes/origin/trunk", "main"]);
    assert_eq!(keys(&json(&run(d, &[]), 1)), ["tool"]);
    // Without `baseBranch` the default is `master`, as changesets.
    write(d, ".changeset/config.json", "{}\n");
    let e = stderr_3(&run(d, &[]));
    assert!(e.contains("origin/master"), "{e}");
}

#[test]
fn bad_since_exit_3() {
    let r = repo();
    let e = stderr_3(&run(r.path(), &["--since", "no-such-rev"]));
    assert!(e.contains("no-such-rev"), "{e}");
}

#[test]
fn no_http_requests() {
    // The API bases point at a closed port; a request would fail the command with exit 3.
    let r = repo();
    json(&run(r.path(), &[]), 0);
    change(r.path(), &[("crates/tool/src/lib.rs", "x\n")]);
    json(&run(r.path(), &[]), 1);
}
