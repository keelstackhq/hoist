mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, hoist};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const REMOTE: &str = "https://github.com/acme/x.git";
const LIB_REF: &str = "GET /repos/acme/x/git/ref/tags/%40acme/lib%401.0.0";
const TOOL_REF: &str = "GET /repos/acme/x/git/ref/tags/tool%400.3.0";
const LIB_REL: &str = "GET /repos/acme/x/releases/tags/%40acme/lib%401.0.0";
const TOOL_REL: &str = "GET /repos/acme/x/releases/tags/tool%400.3.0";
const LIST: &str = "GET /repos/acme/x/releases";
const LINUX: &str = "tool-x86_64-unknown-linux-musl";
const WINDOWS: &str = "tool-x86_64-pc-windows-msvc.exe";
const LINUX_BYTES: &[u8] = b"\x7fELF linux binary \x00\xff\x01";
const WINDOWS_BYTES: &[u8] = b"MZ windows binary \x00\xfe\x02";
const ACTIONS_VARS: [&str; 5] = [
    "GITHUB_ACTIONS",
    "GITHUB_REF",
    "GITHUB_SHA",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_JOB",
];

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

fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

struct Env {
    repo: TempDir,
    assets: TempDir,
    head: String,
}

/// The `publish` fixture on `main`: `@acme/lib@1.0.0` (npm, no changelog) and `tool@0.3.0`
/// (crate with a CHANGELOG.md and two release targets), committed, no tags. The assets dir
/// holds both binaries.
fn setup() -> Env {
    let repo = fixture_repo("publish", REMOTE);
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    write(
        d,
        "crates/tool/hoist.toml",
        b"[publish]\ngithub_release = { targets = [\"x86_64-unknown-linux-musl\", \"x86_64-pc-windows-msvc\"] }\n",
    );
    write(
        d,
        "crates/tool/CHANGELOG.md",
        b"# Changelog\n\n## 0.3.0 (2026-10-17)\n\n### Features\n\n- faster cache (abc1234)\n\n## 0.2.0 (2026-10-01)\n\n- First release.\n",
    );
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let assets = tempfile::tempdir().unwrap();
    write(assets.path(), LINUX, LINUX_BYTES);
    write(assets.path(), WINDOWS, WINDOWS_BYTES);
    Env { repo, assets, head }
}

/// `release publish --json --assets <dir>` with no Actions env at all.
fn publish(env: &Env, api: &FakeApi) -> assert_cmd::Command {
    let mut c = hoist();
    for v in ACTIONS_VARS {
        c.env_remove(v);
    }
    c.arg("-C")
        .arg(env.repo.path())
        .args(["release", "publish", "--json", "--assets"])
        .arg(env.assets.path())
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_GITHUB_UPLOADS", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    c
}

/// Like `publish`, with the env of the `publish` job of release.yml on main at HEAD.
fn in_actions(env: &Env, api: &FakeApi) -> assert_cmd::Command {
    let mut c = publish(env, api);
    c.env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_SHA", &env.head)
        .env(
            "GITHUB_WORKFLOW_REF",
            "acme/x/.github/workflows/release.yml@refs/heads/main",
        )
        .env("GITHUB_JOB", "publish");
    c
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn item<'a>(v: &'a Value, kind: &str, key: &str) -> &'a Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no item {kind}/{key} in {v:#}"))
}

fn writes(api: &FakeApi) -> Vec<(String, String)> {
    api.requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .map(|r| (r.method, r.url))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ref_json(sha: &str) -> String {
    format!(r#"{{"ref":"refs/tags/x","object":{{"sha":"{sha}","type":"commit"}}}}"#)
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn preflight_not_in_actions_exits_3() {
    let env = setup();
    let api = FakeApi::start(&[]);
    let output = publish(&env, &api).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("GITHUB_ACTIONS"),
        "{}",
        stderr(&output)
    );
    assert!(api.requests().is_empty(), "{:?}", api.requests());
}

#[test]
fn preflight_off_default_branch_exits_3() {
    let env = setup();
    let api = FakeApi::start(&[]);
    let output = in_actions(&env, &api)
        .env("GITHUB_REF", "refs/heads/feature")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains("refs/heads/feature") && err.contains("default branch"),
        "{err}"
    );
    assert!(api.requests().is_empty(), "{:?}", api.requests());
}

#[test]
fn preflight_head_not_github_sha_exits_3() {
    let env = setup();
    let api = FakeApi::start(&[]);
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", "0000000000000000000000000000000000000001")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("GITHUB_SHA"),
        "{}",
        stderr(&output)
    );
    assert!(api.requests().is_empty(), "{:?}", api.requests());
}

#[test]
fn preflight_wrong_environment_exits_3() {
    let env = setup();
    let d = env.repo.path();
    let wf = d.join(".github/workflows/release.yml");
    let text = std::fs::read_to_string(&wf).unwrap();
    std::fs::write(
        &wf,
        text.replace("environment: release", "environment: staging"),
    )
    .unwrap();
    git(d, &["commit", "-q", "-am", "ci: staging"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let api = FakeApi::start(&[]);
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("staging") && err.contains("release"), "{err}");
    assert!(api.requests().is_empty(), "{:?}", api.requests());
    // A job that is not in the workflow at all.
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .env("GITHUB_JOB", "build")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains("build"), "{}", stderr(&output));
    assert!(api.requests().is_empty(), "{:?}", api.requests());
}

#[test]
fn publish_creates_tag_draft_assets_then_publishes() {
    let env = setup();
    let at_head = ref_json(&env.head);
    let lib_published = r#"{"id":1,"draft":false,"assets":[]}"#;
    let tool_published = format!(
        r#"{{"id":2,"draft":false,"assets":[{{"name":"{LINUX}"}},{{"name":"{WINDOWS}"}},{{"name":"SHA256SUMS"}}]}}"#
    );
    let routes: Vec<RouteSpec> = vec![
        (LIB_REF, 404, "", &[]),
        (LIB_REF, 200, &at_head, &[]),
        (TOOL_REF, 404, "", &[]),
        (TOOL_REF, 200, &at_head, &[]),
        (LIB_REL, 404, "", &[]),
        (LIB_REL, 200, lib_published, &[]),
        (TOOL_REL, 404, "", &[]),
        (TOOL_REL, 200, &tool_published, &[]),
        (LIST, 200, "[]", &[]),
        ("POST /repos/acme/x/git/refs", 201, r#"{"ref":"x"}"#, &[]),
        (
            "POST /repos/acme/x/releases",
            201,
            r#"{"id":1,"draft":true,"assets":[]}"#,
            &[],
        ),
        (
            "POST /repos/acme/x/releases",
            201,
            r#"{"id":2,"draft":true,"assets":[]}"#,
            &[],
        ),
        ("PATCH /repos/acme/x/releases/1", 200, r#"{"id":1}"#, &[]),
        ("PATCH /repos/acme/x/releases/2", 200, r#"{"id":2}"#, &[]),
        (
            "POST /repos/acme/x/releases/2/assets",
            201,
            r#"{"id":9}"#,
            &[],
        ),
    ];
    let api = FakeApi::start(&routes);
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));

    let w = |m: &str, u: &str| (m.to_string(), u.to_string());
    assert_eq!(
        writes(&api),
        vec![
            w("POST", "/repos/acme/x/git/refs"),
            w("POST", "/repos/acme/x/releases"),
            w("PATCH", "/repos/acme/x/releases/1"),
            w("POST", "/repos/acme/x/git/refs"),
            w("POST", "/repos/acme/x/releases"),
            w(
                "POST",
                "/repos/acme/x/releases/2/assets?name=tool-x86_64-pc-windows-msvc.exe"
            ),
            w(
                "POST",
                "/repos/acme/x/releases/2/assets?name=tool-x86_64-unknown-linux-musl"
            ),
            w("POST", "/repos/acme/x/releases/2/assets?name=SHA256SUMS"),
            w("PATCH", "/repos/acme/x/releases/2"),
        ]
    );

    let reqs: Vec<_> = api
        .requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .collect();
    let body = |i: usize| -> Value { serde_json::from_slice(&reqs[i].raw).unwrap() };
    assert_eq!(
        body(0),
        serde_json::json!({"ref": "refs/tags/@acme/lib@1.0.0", "sha": env.head})
    );
    assert_eq!(
        body(1),
        serde_json::json!({"tag_name": "@acme/lib@1.0.0", "name": "@acme/lib@1.0.0",
            "body": "Release @acme/lib@1.0.0.", "draft": true})
    );
    assert_eq!(body(2), serde_json::json!({"draft": false}));
    assert_eq!(
        body(3),
        serde_json::json!({"ref": "refs/tags/tool@0.3.0", "sha": env.head})
    );
    assert_eq!(
        body(4),
        serde_json::json!({"tag_name": "tool@0.3.0", "name": "tool@0.3.0",
            "body": "### Features\n\n- faster cache (abc1234)", "draft": true})
    );
    for r in &reqs[5..8] {
        assert_eq!(r.content_type.as_deref(), Some("application/octet-stream"));
        assert_eq!(r.auth.as_deref(), Some("Bearer test-token"));
    }
    assert_eq!(reqs[5].raw, WINDOWS_BYTES);
    assert_eq!(reqs[6].raw, LINUX_BYTES);
    let sums = format!(
        "{}  {WINDOWS}\n{}  {LINUX}\n",
        hex(WINDOWS_BYTES),
        hex(LINUX_BYTES)
    );
    assert_eq!(String::from_utf8(reqs[7].raw.clone()).unwrap(), sums);
    assert_eq!(body(8), serde_json::json!({"draft": false}));

    for (kind, key) in [
        ("tag", "@acme/lib@1.0.0"),
        ("github_release", "@acme/lib@1.0.0"),
        ("tag", "tool@0.3.0"),
        ("github_release", "tool@0.3.0"),
    ] {
        let i = item(&v, kind, key);
        assert_eq!(i["action"], "ok", "{i:#}");
        assert_eq!(i["verify"]["ok"], true, "{i:#}");
    }
}

#[test]
fn rerun_skips_existing_tag_and_release() {
    let env = setup();
    let d = env.repo.path();
    git(d, &["tag", "@acme/lib@1.0.0"]);
    git(d, &["tag", "tool@0.3.0"]);
    let at_head = ref_json(&env.head);
    let tool_published = format!(
        r#"{{"id":2,"draft":false,"assets":[{{"name":"{LINUX}"}},{{"name":"{WINDOWS}"}},{{"name":"SHA256SUMS"}}]}}"#
    );
    let routes: Vec<RouteSpec> = vec![
        (LIB_REF, 200, &at_head, &[]),
        (TOOL_REF, 200, &at_head, &[]),
        (LIB_REL, 200, r#"{"id":1,"draft":false,"assets":[]}"#, &[]),
        (TOOL_REL, 200, &tool_published, &[]),
    ];
    let api = FakeApi::start(&routes);
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    for (kind, key) in [("tag", "tool@0.3.0"), ("github_release", "tool@0.3.0")] {
        let i = item(&v, kind, key);
        assert_eq!(i["action"], "ok", "{i:#}");
        assert!(i["reason"].as_str().unwrap().contains("already"), "{i:#}");
    }
}

#[test]
fn released_tags_on_older_commits_are_not_pending() {
    let env = setup();
    let d = env.repo.path();
    git(d, &["tag", "@acme/lib@1.0.0"]);
    git(d, &["tag", "tool@0.3.0"]);
    git(d, &["commit", "-q", "--allow-empty", "-m", "docs: later"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let api = FakeApi::start(&[]);
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}");
    assert!(api.requests().is_empty(), "{:?}", api.requests());
    let i = item(&v, "publish", "pending");
    assert_eq!(i["action"], "ok", "{i:#}");
}

#[test]
fn tag_at_other_commit_exits_1() {
    let env = setup();
    let elsewhere = ref_json("1111111111111111111111111111111111111111");
    let routes: Vec<RouteSpec> = vec![(LIB_REF, 200, &elsewhere, &[])];
    let api = FakeApi::start(&routes);
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    let lib = item(&v, "tag", "@acme/lib@1.0.0");
    let reason = lib["reason"].as_str().unwrap();
    assert!(
        reason.contains("tag exists at another commit") && reason.contains("1111111"),
        "{lib:#}"
    );
    assert_eq!(
        item(&v, "github_release", "tool@0.3.0")["reason"],
        "not attempted"
    );
}

#[test]
fn missing_asset_exits_3_before_writes() {
    let env = setup();
    std::fs::remove_file(env.assets.path().join(WINDOWS)).unwrap();
    let api = FakeApi::start(&[]);
    let output = in_actions(&env, &api).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains(WINDOWS), "{}", stderr(&output));
    assert!(api.requests().is_empty(), "{:?}", api.requests());
    // Without --assets the expected files are missing too.
    let mut c = in_actions(&env, &api);
    let output = c
        .args(["--assets", "/nonexistent-hoist-assets"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(api.requests().is_empty());
}

#[test]
fn verification_names_missing_asset_and_exits_1() {
    let env = setup();
    let d = env.repo.path();
    git(d, &["tag", "@acme/lib@1.0.0"]);
    git(d, &["commit", "-q", "--allow-empty", "-m", "docs: later"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let at_head = ref_json(&head);
    let incomplete = format!(r#"{{"id":2,"draft":false,"assets":[{{"name":"{LINUX}"}}]}}"#);
    let draft = format!(
        r#"[{{"id":2,"tag_name":"tool@0.3.0","draft":true,"assets":[{{"name":"{LINUX}"}},{{"name":"{WINDOWS}"}}]}}]"#
    );
    let routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &at_head, &[]),
        (TOOL_REL, 404, "", &[]),
        (TOOL_REL, 200, &incomplete, &[]),
        (LIST, 200, &draft, &[]),
        (
            "POST /repos/acme/x/releases/2/assets",
            201,
            r#"{"id":9}"#,
            &[],
        ),
        ("PATCH /repos/acme/x/releases/2", 200, r#"{"id":2}"#, &[]),
    ];
    let api = FakeApi::start(&routes);
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    // The existing draft is reused: only SHA256SUMS is uploaded, then it is published.
    let w = |m: &str, u: &str| (m.to_string(), u.to_string());
    assert_eq!(
        writes(&api),
        vec![
            w("POST", "/repos/acme/x/releases/2/assets?name=SHA256SUMS"),
            w("PATCH", "/repos/acme/x/releases/2"),
        ]
    );
    let rel = item(&v, "github_release", "tool@0.3.0");
    assert_eq!(rel["verify"]["ok"], false, "{rel:#}");
    let detail = rel["verify"]["detail"].as_str().unwrap();
    assert!(
        detail.contains(WINDOWS) && detail.contains("SHA256SUMS"),
        "{rel:#}"
    );
}

#[test]
fn dry_run_is_get_only() {
    let env = setup();
    let d = env.repo.path();
    let status_before = out(d, &["status", "--porcelain"]);
    let tags_before = out(d, &["tag"]);
    let api = FakeApi::start(&[(LIST, 200, "[]", &[])]);
    let output = publish(&env, &api).arg("--dry-run").output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    assert!(!api.requests().is_empty());
    for (kind, key) in [
        ("tag", "@acme/lib@1.0.0"),
        ("github_release", "@acme/lib@1.0.0"),
        ("tag", "tool@0.3.0"),
        ("github_release", "tool@0.3.0"),
    ] {
        assert_eq!(item(&v, kind, key)["action"], "create");
    }
    assert_eq!(out(d, &["status", "--porcelain"]), status_before);
    assert_eq!(out(d, &["tag"]), tags_before);
}
