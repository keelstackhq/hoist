//! `hoist release prepare` in changesets mode (changesets spec §8).

mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, git_wrapper, hoist, set_origin_head, stub};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const PULLS_GET: &str = "GET /repos/acme/x/pulls?head=acme:hoist/release&state=open";

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

fn commit(d: &Path, msg: &str) -> String {
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", msg]);
    out(d, &["rev-parse", "--short=7", "HEAD"])
        .trim()
        .to_string()
}

struct Env {
    repo: TempDir,
    bare: TempDir,
    bin: TempDir,
}

/// release-ws in changesets mode: a@2.0.0 (`b: ~1.0.0`), b@1.0.0, c@1.0.0 (dev `b: ~1.0.0`),
/// tool@0.3.0, all tagged; `b_version` is b's manifest version (above its tag = awaiting publish).
fn setup(config: &str, b_version: &str) -> Env {
    let repo = fixture_repo("release-ws", "https://github.com/acme/x.git");
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    let pkg = |name: &str, version: &str, deps: &str| {
        format!(
            "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"private\": false{deps}\n}}\n"
        )
    };
    let a_deps = ",\n  \"dependencies\": {\n    \"b\": \"~1.0.0\"\n  }";
    let c_deps = ",\n  \"devDependencies\": {\n    \"b\": \"~1.0.0\"\n  }";
    write(d, "packages/a/package.json", &pkg("a", "2.0.0", a_deps));
    write(d, "packages/b/package.json", &pkg("b", "1.0.0", ""));
    write(d, "packages/c/package.json", &pkg("c", "1.0.0", c_deps));
    write(d, ".changeset/config.json", config);
    write(d, ".changeset/README.md", "# Changesets\n");
    commit(d, "chore: init");
    for t in ["a@2.0.0", "b@1.0.0", "c@1.0.0", "tool@0.3.0"] {
        git(d, &["tag", t]);
    }
    if b_version != "1.0.0" {
        write(d, "packages/b/package.json", &pkg("b", b_version, ""));
        commit(d, "release: b");
    }
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare"]);
    let push = bare.path().to_str().unwrap();
    git(d, &["remote", "set-url", "--push", "origin", push]);
    git(d, &["push", "-q", "origin", "main"]);
    set_origin_head(d, "main");
    let bin = tempfile::tempdir().unwrap();
    stub(bin.path(), "pnpm", "echo pnpm-stdout");
    stub(bin.path(), "cargo", "echo cargo-stdout");
    stub(bin.path(), "git", &git_wrapper());
    Env { repo, bare, bin }
}

fn prepare(env: &Env) -> (Value, Option<Value>) {
    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, "[]", &[]),
        ("POST /repos/acme/x/pulls", 201, r#"{"number":12}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    let o = hoist()
        .arg("-C")
        .arg(env.repo.path())
        .args(["release", "prepare", "--json"])
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .env("HOIST_PNPM", env.bin.path().join("pnpm"))
        .env("HOIST_CARGO", env.bin.path().join("cargo"))
        .env("HOIST_NOW", "2026-10-17T12:00:00Z")
        .env("HOIST_GIT", env.bin.path().join("git"))
        .env("STUB_ORIGIN", env.bare.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .assert();
    let report: Value = serde_json::from_slice(&o.get_output().stdout).expect("one JSON object");
    let post = api
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .map(|q| serde_json::from_str(q.body.as_deref().unwrap()).unwrap());
    (report, post)
}

/// Files of `hoist/release` in the bare origin.
fn tree(env: &Env) -> String {
    out(
        env.bare.path(),
        &["ls-tree", "-r", "--name-only", "hoist/release"],
    )
}

fn show(env: &Env, rel: &str) -> String {
    out(env.bare.path(), &["show", &format!("hoist/release:{rel}")])
}

#[test]
fn release_commit_deletes_consumed_changesets() {
    let env = setup("{}\n", "1.0.0");
    let d = env.repo.path();
    write(
        d,
        ".changeset/brave-fox.md",
        "---\n\"b\": minor\n---\n\nAdd thing\nsecond line\n",
    );
    let sha = commit(d, "feat: changeset");
    write(d, ".changeset/calm-owl.md", "---\n---\n\nDocs only\n");
    commit(d, "docs: empty changeset");
    let (r, post) = prepare(&env);
    assert_eq!(r["exit_code"], 0, "{r}");
    let files = tree(&env);
    for gone in [".changeset/brave-fox.md", ".changeset/calm-owl.md"] {
        assert!(!files.contains(gone), "{gone} deleted: {files}");
    }
    assert!(files.contains(".changeset/README.md"), "{files}");
    assert!(files.contains(".changeset/config.json"), "{files}");
    // In the default branch's checkout they stay until the release PR merges.
    assert!(d.join(".changeset/brave-fox.md").exists());

    // changelog_layout: changesets lines with hoist's dated heading (O1).
    let b_log = show(&env, "packages/b/CHANGELOG.md");
    assert_eq!(
        b_log,
        format!(
            "# Changelog\n\n## 1.1.0 (2026-10-17)\n\n### Minor Changes\n\n\
             - {sha}: Add thing\n  second line\n"
        )
    );
    // a's `~1.0.0` no longer admits 1.1.0: a patch with the dependency line.
    let a_log = show(&env, "packages/a/CHANGELOG.md");
    assert_eq!(
        a_log,
        format!(
            "# Changelog\n\n## 2.0.1 (2026-10-17)\n\n### Patch Changes\n\n\
             - Updated dependencies [{sha}]\n  - b@1.1.0\n"
        )
    );
    let a_pkg = show(&env, "packages/a/package.json");
    assert!(a_pkg.contains("\"b\": \"~1.1.0\""), "{a_pkg}");
    // c only has a dev edge: its range is rewritten, its version and changelog stay.
    let c_pkg = show(&env, "packages/c/package.json");
    assert!(c_pkg.contains("\"b\": \"~1.1.0\""), "{c_pkg}");
    assert!(c_pkg.contains("\"version\": \"1.0.0\""), "{c_pkg}");
    assert!(!files.contains("packages/c/CHANGELOG.md"), "{files}");

    // pr_body_lists_changesets
    let post = post.expect("POST /pulls");
    assert_eq!(post["title"], "release: b@1.1.0, a@2.0.1");
    let body = post["body"].as_str().unwrap();
    assert!(
        body.contains("## Changesets\n\n- `b`: brave-fox\n- no package: calm-owl\n"),
        "{body}"
    );
    assert!(body.contains("### Minor Changes"), "{body}");
}

#[test]
fn awaiting_publish_keeps_its_changeset() {
    // b@1.1.0 is merged but not tagged yet; its changeset waits for the next release.
    let env = setup("{}\n", "1.1.0");
    let d = env.repo.path();
    write(
        d,
        ".changeset/b-fix.md",
        "---\n\"b\": patch\n---\n\nFix b\n",
    );
    write(
        d,
        ".changeset/c-fix.md",
        "---\n\"c\": patch\n---\n\nFix c\n",
    );
    write(
        d,
        ".changeset/both.md",
        "---\n\"b\": patch\n\"c\": patch\n---\n\nBoth\n",
    );
    commit(d, "chore: changesets");
    let (r, _) = prepare(&env);
    assert_eq!(r["exit_code"], 0, "{r}");
    let files = tree(&env);
    assert!(files.contains(".changeset/b-fix.md"), "{files}");
    assert!(
        files.contains(".changeset/both.md"),
        "b does not release: {files}"
    );
    assert!(!files.contains(".changeset/c-fix.md"), "{files}");
    let c_log = show(&env, "packages/c/CHANGELOG.md");
    assert!(
        c_log.contains("Fix c") && !c_log.contains("Both"),
        "{c_log}"
    );
}

#[test]
fn changelog_false_writes_none() {
    let env = setup("{\"changelog\": false}\n", "1.0.0");
    let d = env.repo.path();
    write(d, ".changeset/x.md", "---\n\"b\": patch\n---\n\nFix\n");
    commit(d, "chore: changeset");
    let (r, post) = prepare(&env);
    assert_eq!(r["exit_code"], 0, "{r}");
    let files = tree(&env);
    assert!(!files.contains("CHANGELOG.md"), "{files}");
    assert!(!files.contains(".changeset/x.md"), "{files}");
    let pkg = show(&env, "packages/b/package.json");
    assert!(pkg.contains("\"version\": \"1.0.1\""), "{pkg}");
    assert!(post.is_some());
}

#[test]
fn unreleased_moves_under_version() {
    let env = setup("{}\n", "1.0.0");
    let d = env.repo.path();
    write(
        d,
        "packages/b/CHANGELOG.md",
        "# Changelog\n\n## Unreleased\n\n- folded fragment\n\n## 1.0.0 (2026-01-01)\n\n- old\n",
    );
    write(d, ".changeset/x.md", "---\n\"b\": patch\n---\n\nFix\n");
    let sha = commit(d, "chore: changeset");
    let (r, _) = prepare(&env);
    assert_eq!(r["exit_code"], 0, "{r}");
    assert_eq!(
        show(&env, "packages/b/CHANGELOG.md"),
        format!(
            "# Changelog\n\n## 1.0.1 (2026-10-17)\n\n### Patch Changes\n\n- {sha}: Fix\n\n\
             - folded fragment\n\n## 1.0.0 (2026-01-01)\n\n- old\n"
        )
    );
}
