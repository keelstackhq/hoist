mod common;

use common::{FakeApi, fixture_repo, git, hoist, stub};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn out(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(o.status.success(), "git {args:?}");
    String::from_utf8(o.stdout).unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// release-ws with b at 1.0.0, committed and tagged a@1.2.0, b@1.0.0, tool@0.3.0.
fn tagged_repo() -> tempfile::TempDir {
    let repo = fixture_repo("release-ws", "https://github.com/acme/x.git");
    let d = repo.path();
    write(
        d,
        "packages/b/package.json",
        "{\n  \"name\": \"b\",\n  \"version\": \"1.0.0\",\n  \"private\": false\n}\n",
    );
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    for t in ["a@1.2.0", "b@1.0.0", "tool@0.3.0"] {
        git(d, &["tag", t]);
    }
    repo
}

fn commit_in(d: &Path, rel: &str, msg: &str) {
    write(d, rel, &format!("{msg}\n"));
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", msg]);
}

fn plan(d: &Path) -> assert_cmd::Command {
    let mut c = hoist();
    c.arg("-C").arg(d).args(["release", "plan"]);
    c
}

fn item<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("no item {key}"))
}

#[test]
fn plan_after_feat_shows_minor() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/src.ts", "feat: add thing");
    let o = plan(repo.path()).arg("--json").assert().code(1);
    let report: Value = serde_json::from_slice(&o.get_output().stdout).unwrap();
    let b = item(&report, "b");
    assert_eq!(b["action"], "create");
    assert_eq!(b["area"], "release");
    assert_eq!(b["kind"], "version");
    assert!(b["reason"].as_str().unwrap().starts_with("1.0.0 -> 1.1.0"));
    assert!(b["reason"].as_str().unwrap().contains("feat: add thing"));
    assert_eq!(b["source"], "packages/b/package.json:3");
    let a = item(&report, "a");
    assert_eq!(a["action"], "create");
    assert!(a["reason"].as_str().unwrap().starts_with("1.2.0 -> 1.2.1"));
    assert_eq!(item(&report, "tool")["action"], "ok");
}

#[test]
fn nothing_releasable_exits_0() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/notes.md", "docs: notes");
    let o = plan(repo.path()).arg("--json").assert().code(0);
    let report: Value = serde_json::from_slice(&o.get_output().stdout).unwrap();
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["action"] == "ok")
    );
}

#[test]
fn untagged_package_releases_its_manifest_version() {
    let repo = tagged_repo();
    git(repo.path(), &["tag", "-d", "tool@0.3.0"]);
    let o = plan(repo.path()).arg("--json").assert().code(1);
    let report: Value = serde_json::from_slice(&o.get_output().stdout).unwrap();
    assert_eq!(item(&report, "tool")["reason"], "first release 0.3.0");
}

#[test]
fn highest_tag_wins_not_the_latest_created() {
    let repo = tagged_repo();
    git(repo.path(), &["tag", "b@0.9.0"]);
    commit_in(repo.path(), "packages/b/src.ts", "fix: x");
    let o = plan(repo.path()).arg("--json").assert().code(1);
    let report: Value = serde_json::from_slice(&o.get_output().stdout).unwrap();
    assert!(
        item(&report, "b")["reason"]
            .as_str()
            .unwrap()
            .starts_with("1.0.0 -> 1.0.1")
    );
}

#[test]
fn shallow_clone_exits_3() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/src.ts", "feat: more");
    let clone = tempfile::tempdir().unwrap();
    let url = format!("file://{}", repo.path().display());
    git(
        clone.path(),
        &["clone", "-q", "--depth", "1", &url, "shallow"],
    );
    let o = plan(&clone.path().join("shallow")).assert().code(3);
    let err = String::from_utf8_lossy(&o.get_output().stderr).to_string();
    assert!(err.contains("fetch-depth: 0"), "{err}");
}

#[test]
fn plan_is_read_only() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/src.ts", "feat: x");
    let api = FakeApi::start(&[]);
    let (status, tags) = (
        out(repo.path(), &["status", "--porcelain"]),
        out(repo.path(), &["tag"]),
    );
    let head = out(repo.path(), &["rev-parse", "HEAD"]);
    plan(repo.path())
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .assert()
        .code(1);
    assert!(api.requests().is_empty());
    api.assert_only_get();
    assert_eq!(out(repo.path(), &["status", "--porcelain"]), status);
    assert_eq!(out(repo.path(), &["tag"]), tags);
    assert_eq!(out(repo.path(), &["rev-parse", "HEAD"]), head);
}

#[test]
fn json_is_one_object() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/src.ts", "feat: x");
    let o = plan(repo.path()).arg("--json").assert().code(1);
    let text = String::from_utf8(o.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&text).expect("one JSON object");
    assert_eq!(v["command"], "release plan");
    assert_eq!(v["exit_code"], 1);
    assert_eq!(v["ok"], false);
}

#[test]
fn hoist_git_selects_the_git_binary() {
    let repo = tagged_repo();
    let bin = tempfile::tempdir().unwrap();
    let s = stub(bin.path(), "fakegit", "echo true");
    plan(repo.path()).env("HOIST_GIT", &s).assert().code(3);
    let log = std::fs::read_to_string(bin.path().join("fakegit.log")).unwrap();
    assert!(log.contains("rev-parse --is-shallow-repository"), "{log}");
}
