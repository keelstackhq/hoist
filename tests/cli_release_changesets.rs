//! `hoist release plan` in changesets mode (`.changeset/config.json` exists).

mod common;

use common::{FakeApi, fixture_repo, git, hoist};
use serde_json::Value;
use std::path::Path;

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// release-ws (a depends on b `workspace:^1.0.0`), tagged a@1.2.0, b@1.0.1, tool@0.3.0.
fn tagged_repo(changesets: bool) -> tempfile::TempDir {
    let repo = fixture_repo("release-ws", "https://github.com/acme/x.git");
    let d = repo.path();
    if changesets {
        write(d, ".changeset/config.json", "{\"baseBranch\": \"main\"}\n");
        write(d, ".changeset/README.md", "# Changesets\n");
    }
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    for t in ["a@1.2.0", "b@1.0.1", "tool@0.3.0"] {
        git(d, &["tag", t]);
    }
    repo
}

fn plan_json(d: &Path, code: i32) -> Value {
    let o = hoist()
        .arg("-C")
        .arg(d)
        .args(["release", "plan", "--json"])
        .assert()
        .code(code);
    serde_json::from_slice(&o.get_output().stdout).unwrap()
}

fn item<'a>(r: &'a Value, kind: &str, key: &str) -> &'a Value {
    r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no item {kind} {key} in {r}"))
}

#[test]
fn plan_reads_changesets_not_commits() {
    let repo = tagged_repo(true);
    let d = repo.path();
    write(
        d,
        ".changeset/brave-cat.md",
        "---\n\"b\": major\n---\n\nBig.\n",
    );
    write(d, "packages/a/x.ts", "x\n");
    git(d, &["add", "-A"]);
    git(
        d,
        &["commit", "-q", "-m", "feat: ignored in changesets mode"],
    );
    let r = plan_json(d, 0);
    let src = item(&r, "source", "changesets");
    assert_eq!(src["action"], "ok");
    assert_eq!(src["reason"], "changesets: brave-cat");
    let b = item(&r, "version", "b");
    assert_eq!(b["action"], "create");
    assert_eq!(b["reason"], "1.0.1 -> 2.0.0 (changesets: brave-cat)");
    // a's `workspace:^1.0.0` no longer covers 2.0.0: a gets a patch, not the feat commit's minor.
    let a = item(&r, "version", "a");
    assert!(
        a["reason"].as_str().unwrap().starts_with("1.2.0 -> 1.2.1"),
        "{a}"
    );
    assert_eq!(item(&r, "version", "tool")["action"], "ok");
}

#[test]
fn commits_mode_has_a_source_item() {
    let repo = tagged_repo(false);
    let r = plan_json(repo.path(), 0);
    assert_eq!(item(&r, "source", "commits")["action"], "ok");
}

#[test]
fn bad_changeset_name_exits_3() {
    let repo = tagged_repo(true);
    write(
        repo.path(),
        ".changeset/oops.md",
        "---\nnope: patch\n---\n\nx\n",
    );
    hoist()
        .arg("-C")
        .arg(repo.path())
        .args(["release", "plan"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("oops.md"))
        .stderr(predicates::str::contains("not a releasable package"));
}

#[test]
fn plan_sends_only_get() {
    let repo = tagged_repo(true);
    write(
        repo.path(),
        ".changeset/x.md",
        "---\n\"a\": patch\n---\n\nx\n",
    );
    let api = FakeApi::start(&[]);
    hoist()
        .arg("-C")
        .arg(repo.path())
        .args(["release", "plan"])
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .assert()
        .code(0);
    api.assert_only_get();
}
