mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, hoist, stub};
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

fn has_ref(dir: &Path, r: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "-q", r])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git")
        .status
        .success()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit_in(d: &Path, rel: &str, msg: &str) {
    write(d, rel, &format!("{msg}\n"));
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", msg]);
}

struct Env {
    repo: TempDir,
    bare: TempDir,
    bin: TempDir,
}

/// release-ws on `main` with a@2.0.0 (depends on b), b@1.0.0 and tool@0.3.0, all tagged; the
/// push URL of `origin` (https://github.com/acme/x.git) is a local bare repo.
fn setup() -> Env {
    let repo = fixture_repo("release-ws", "https://github.com/acme/x.git");
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    write(
        d,
        "packages/a/package.json",
        "{\n  \"name\": \"a\",\n  \"version\": \"2.0.0\",\n  \"private\": false,\n  \
         \"dependencies\": {\n    \"b\": \"^1.0.0\"\n  }\n}\n",
    );
    write(
        d,
        "packages/b/package.json",
        "{\n  \"name\": \"b\",\n  \"version\": \"1.0.0\",\n  \"private\": false\n}\n",
    );
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    for t in ["a@2.0.0", "b@1.0.0", "tool@0.3.0"] {
        git(d, &["tag", t]);
    }
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare"]);
    git(
        d,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            bare.path().to_str().unwrap(),
        ],
    );
    git(d, &["push", "-q", "origin", "main"]);
    let bin = tempfile::tempdir().unwrap();
    stub(bin.path(), "pnpm", "echo pnpm-stdout");
    stub(bin.path(), "cargo", "echo cargo-stdout");
    Env { repo, bare, bin }
}

fn prepare(env: &Env, api: &FakeApi) -> assert_cmd::Command {
    let mut c = hoist();
    c.arg("-C")
        .arg(env.repo.path())
        .args(["release", "prepare", "--json"])
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .env("HOIST_PNPM", env.bin.path().join("pnpm"))
        .env("HOIST_CARGO", env.bin.path().join("cargo"))
        .env("HOIST_NOW", "2026-10-17T12:00:00Z")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    c
}

fn report(o: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&o.get_output().stdout).expect("one JSON object")
}

fn item<'a>(report: &'a Value, kind: &str, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no item {kind}/{key} in {report}"))
}

fn assert_restored(d: &Path, head: &str) {
    assert_eq!(out(d, &["symbolic-ref", "--short", "HEAD"]).trim(), "main");
    assert_eq!(out(d, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(out(d, &["status", "--porcelain"]), "");
}

#[test]
fn prepare_opens_pr_with_bumps() {
    let env = setup();
    let d = env.repo.path();
    commit_in(d, "packages/b/src.ts", "feat: add thing");
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, "[]", &[]),
        ("POST /repos/acme/x/pulls", 201, r#"{"number":12}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(0);
    let r = report(&o);
    assert_eq!(r["command"], "release prepare");
    assert_eq!(item(&r, "pr", "hoist/release")["action"], "create");
    for key in ["a", "b"] {
        let v = item(&r, "version", key);
        assert_eq!(v["action"], "update");
        assert_eq!(v["reason"], "in release PR #12");
    }

    let bare = env.bare.path();
    assert_eq!(
        out(bare, &["log", "-1", "--format=%s", "hoist/release"]).trim(),
        "release: b@1.1.0, a@2.0.1"
    );
    let b_pkg = out(bare, &["show", "hoist/release:packages/b/package.json"]);
    assert!(b_pkg.contains("\"version\": \"1.1.0\""), "{b_pkg}");
    let a_pkg = out(bare, &["show", "hoist/release:packages/a/package.json"]);
    assert!(a_pkg.contains("\"version\": \"2.0.1\""), "{a_pkg}");
    assert!(a_pkg.contains("\"b\": \"^1.1.0\""), "{a_pkg}");
    let log = out(bare, &["show", "hoist/release:packages/b/CHANGELOG.md"]);
    assert!(log.contains("## 1.1.0 (2026-10-17)"), "{log}");
    assert!(log.contains("add thing"), "{log}");

    let pnpm = std::fs::read_to_string(env.bin.path().join("pnpm.log")).unwrap();
    assert!(pnpm.contains("install --lockfile-only"), "{pnpm}");
    assert!(!env.bin.path().join("cargo.log").exists());
    let stderr = String::from_utf8_lossy(&o.get_output().stderr);
    assert!(stderr.contains("pnpm-stdout"), "{stderr}");

    let reqs = api.requests();
    let post = reqs
        .iter()
        .find(|q| q.method == "POST" && q.path == "/repos/acme/x/pulls")
        .expect("POST /pulls");
    let body: Value = serde_json::from_str(post.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["title"], "release: b@1.1.0, a@2.0.1");
    assert_eq!(body["head"], "hoist/release");
    assert_eq!(body["base"], "main");
    let text = body["body"].as_str().unwrap();
    assert!(text.contains("1.0.0 → 1.1.0"), "{text}");
    assert!(text.contains("b@1.1.0"), "{text}");
    assert!(text.contains("add thing"), "{text}");
    assert_eq!(post.auth.as_deref(), Some("Bearer test-token"));

    assert_restored(d, &head);
}

#[test]
fn prepare_updates_existing_pr() {
    let env = setup();
    let d = env.repo.path();
    commit_in(d, "packages/b/src.ts", "fix: repair");
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, r#"[{"number":7}]"#, &[]),
        ("PATCH /repos/acme/x/pulls/7", 200, r#"{"number":7}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(0);
    let r = report(&o);
    assert_eq!(item(&r, "pr", "hoist/release")["action"], "update");
    assert_eq!(item(&r, "version", "b")["reason"], "in release PR #7");
    let reqs = api.requests();
    assert!(!reqs.iter().any(|q| q.method == "POST"), "{reqs:?}");
    let patch = reqs
        .iter()
        .find(|q| q.method == "PATCH" && q.path == "/repos/acme/x/pulls/7")
        .expect("PATCH");
    let body: Value = serde_json::from_str(patch.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["title"], "release: b@1.0.1, a@2.0.1");
    assert_eq!(
        out(
            env.bare.path(),
            &["log", "-1", "--format=%s", "hoist/release"]
        )
        .trim(),
        "release: b@1.0.1, a@2.0.1"
    );
    assert_restored(d, &head);
}

#[test]
fn prepare_closes_pr_when_nothing_to_release() {
    let env = setup();
    let d = env.repo.path();
    git(
        d,
        &["push", "-q", "origin", "main:refs/heads/hoist/release"],
    );
    commit_in(d, "packages/b/notes.md", "docs: notes");
    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, r#"[{"number":7}]"#, &[]),
        ("PATCH /repos/acme/x/pulls/7", 200, r#"{"number":7}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(0);
    let r = report(&o);
    assert_eq!(item(&r, "pr", "hoist/release")["action"], "ok");
    let patch = api
        .requests()
        .into_iter()
        .find(|q| q.method == "PATCH")
        .expect("PATCH");
    let body: Value = serde_json::from_str(patch.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["state"], "closed");
    assert!(!has_ref(env.bare.path(), "refs/heads/hoist/release"));
    assert!(!env.bin.path().join("pnpm.log").exists());
}

#[test]
fn dirty_tree_exits_3_before_writes() {
    let env = setup();
    let d = env.repo.path();
    commit_in(d, "packages/b/src.ts", "feat: add thing");
    write(d, "packages/b/wip.ts", "wip\n");
    let routes: &[RouteSpec] = &[(PULLS_GET, 200, "[]", &[])];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(3);
    let stderr = String::from_utf8_lossy(&o.get_output().stderr);
    assert!(stderr.contains("clean"), "{stderr}");
    api.assert_only_get();
    assert!(!has_ref(d, "refs/heads/hoist/release"));
    assert!(!has_ref(env.bare.path(), "refs/heads/hoist/release"));
    assert_eq!(out(d, &["status", "--porcelain"]), "?? packages/b/wip.ts\n");
}

#[test]
fn push_failure_exits_1_and_reports() {
    let env = setup();
    let d = env.repo.path();
    commit_in(d, "packages/b/src.ts", "feat: add thing");
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    git(
        d,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "/nonexistent/hoist-origin.git",
        ],
    );
    let routes: &[RouteSpec] = &[(PULLS_GET, 200, "[]", &[])];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(1);
    let r = report(&o);
    assert_eq!(r["exit_code"], 1);
    assert_eq!(item(&r, "step", "commit")["action"], "ok");
    let push = item(&r, "step", "push");
    assert!(
        push["reason"].as_str().unwrap().starts_with("failed: "),
        "{push}"
    );
    assert_eq!(item(&r, "step", "restore")["action"], "ok");
    assert_eq!(item(&r, "pr", "hoist/release")["reason"], "not attempted");
    api.assert_only_get();
    assert_restored(d, &head);
}

/// `tool` (and `tool2` when `two`) inherit `[workspace.package] version = "0.3.0"`.
fn inherit(env: &Env, two: bool) {
    let d = env.repo.path();
    write(
        d,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.3.0\" # shared\n",
    );
    let krate = |name: &str| {
        format!("[package]\nname = \"{name}\"\nversion.workspace = true\nedition = \"2024\"\n")
    };
    write(d, "crates/tool/Cargo.toml", &krate("tool"));
    if two {
        write(d, "crates/tool2/Cargo.toml", &krate("tool2"));
        write(d, "crates/tool2/src/lib.rs", "\n");
    }
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: inherit version"]);
    if two {
        git(d, &["tag", "tool2@0.3.0"]);
    }
    git(d, &["tag", "-f", "tool@0.3.0"]);
}

#[test]
fn inherited_version_bumps_workspace_package_version() {
    let env = setup();
    inherit(&env, false);
    let d = env.repo.path();
    commit_in(d, "crates/tool/src/lib.rs", "fix: tool");
    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, "[]", &[]),
        ("POST /repos/acme/x/pulls", 201, r#"{"number":3}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    prepare(&env, &api).assert().code(0);
    let root = out(env.bare.path(), &["show", "hoist/release:Cargo.toml"]);
    assert!(root.contains("version = \"0.3.1\" # shared"), "{root}");
    let krate = out(
        env.bare.path(),
        &["show", "hoist/release:crates/tool/Cargo.toml"],
    );
    assert!(krate.contains("version.workspace = true"), "{krate}");
    let cargo = std::fs::read_to_string(env.bin.path().join("cargo.log")).unwrap();
    assert!(cargo.starts_with("update -w"), "{cargo}");
}

#[test]
fn inherited_version_with_different_targets_exits_3() {
    let env = setup();
    inherit(&env, true);
    let d = env.repo.path();
    commit_in(d, "crates/tool/src/lib.rs", "fix: tool");
    let routes: &[RouteSpec] = &[(PULLS_GET, 200, "[]", &[])];
    let api = FakeApi::start(routes);
    let o = prepare(&env, &api).assert().code(3);
    let stderr = String::from_utf8_lossy(&o.get_output().stderr);
    assert!(stderr.contains("tool 0.3.1, tool2 unchanged"), "{stderr}");
    assert!(api.requests().is_empty());
    assert!(!has_ref(d, "refs/heads/hoist/release"));
}
