mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, hoist, stub};
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
    /// Stub `npm`, `cargo` and `pnpm` (and their `.log` files).
    tools: TempDir,
}

const LIB_V: &str = "GET /@acme%2Flib/1.0.0";
const LIB_PKG: &str = "GET /@acme%2Flib";
const TOOL_V: &str = "GET /api/v1/crates/tool/0.3.0";
const TOOL_PKG: &str = "GET /api/v1/crates/tool";
const TOKENS: &str = "/api/v1/trusted_publishing/tokens";
const JWT: &str = "canary-jwt-5d1e";
const CRATES_TOKEN: &str = "canary-crates-token-c0ff";
const NPM_OK: &str =
    "if [ \"$1\" = --version ]; then echo 12.2.0; else echo '+ @acme/lib@1.0.0'; fi";

/// Both versions already on their registries: the npm and crates steps are skipped.
const ON_REGISTRIES: [RouteSpec<'static>; 2] = [(LIB_V, 200, "{}", &[]), (TOOL_V, 200, "{}", &[])];

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
    let tools = tempfile::tempdir().unwrap();
    stub(tools.path(), "npm", NPM_OK);
    stub(tools.path(), "cargo", "echo '   Uploading tool v0.3.0'");
    stub(tools.path(), "pnpm", "echo deployed");
    Env {
        repo,
        assets,
        head,
        tools,
    }
}

/// `routes` plus [`ON_REGISTRIES`] (used after them, so a test's own registry routes win).
fn with_registries<'a>(routes: &[RouteSpec<'a>]) -> Vec<RouteSpec<'a>> {
    [routes, &ON_REGISTRIES[..]].concat()
}

/// The lines a stub tool logged (arguments, then ` env:` and variable names); empty if never run.
fn tool_log(env: &Env, name: &str) -> Vec<String> {
    std::fs::read_to_string(env.tools.path().join(format!("{name}.log")))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
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
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM", env.tools.path().join("npm"))
        .env("HOIST_CARGO", env.tools.path().join("cargo"))
        .env("HOIST_PNPM", env.tools.path().join("pnpm"))
        .env("HOIST_MAX_SLEEP_MS", "1")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
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
        .env("GITHUB_JOB", "publish")
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("{}/oidc?api-version=2.0", api.url()),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req-token");
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

/// A release asset as GitHub lists it: uploaded, with the sha256 digest and size of `bytes`.
fn asset(id: u64, name: &str, bytes: &[u8]) -> String {
    format!(
        r#"{{"id":{id},"name":"{name}","state":"uploaded","size":{},"digest":"sha256:{}"}}"#,
        bytes.len(),
        hex(bytes)
    )
}

fn sums() -> String {
    format!(
        "{}  {WINDOWS}\n{}  {LINUX}\n",
        hex(WINDOWS_BYTES),
        hex(LINUX_BYTES)
    )
}

/// Both binaries and SHA256SUMS, all matching the fixture files.
fn all_assets() -> String {
    [
        asset(11, LINUX, LINUX_BYTES),
        asset(12, WINDOWS, WINDOWS_BYTES),
        asset(13, "SHA256SUMS", sums().as_bytes()),
    ]
    .join(",")
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
    let tool_published = format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets());
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
    let api = FakeApi::start(&with_registries(&routes));
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
    let tool_published = format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets());
    let routes: Vec<RouteSpec> = vec![
        (LIB_REF, 200, &at_head, &[]),
        (TOOL_REF, 200, &at_head, &[]),
        (LIB_REL, 200, r#"{"id":1,"draft":false,"assets":[]}"#, &[]),
        (TOOL_REL, 200, &tool_published, &[]),
    ];
    let api = FakeApi::start(&with_registries(&routes));
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
    let api = FakeApi::start(&with_registries(&routes));
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
    let incomplete = format!(
        r#"{{"id":2,"draft":false,"assets":[{}]}}"#,
        asset(11, LINUX, LINUX_BYTES)
    );
    let draft = format!(
        r#"[{{"id":2,"tag_name":"tool@0.3.0","draft":true,"assets":[{},{}]}}]"#,
        asset(11, LINUX, LINUX_BYTES),
        asset(12, WINDOWS, WINDOWS_BYTES)
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
    let api = FakeApi::start(&with_registries(&routes));
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
    // Registry steps read only: neither package is on its registry yet (fake 404s).
    assert_eq!(item(&v, "npm", "@acme/lib@1.0.0")["action"], "pending");
    assert_eq!(item(&v, "crates", "tool@0.3.0")["action"], "pending");
    assert!(tool_log(&env, "npm").is_empty() && tool_log(&env, "cargo").is_empty());
    assert_eq!(out(d, &["status", "--porcelain"]), status_before);
    assert_eq!(out(d, &["tag"]), tags_before);
}

/// `tool@0.3.0` pending at a new HEAD (`@acme/lib` released on an older commit).
fn setup_tool_only() -> (Env, String) {
    let env = setup();
    let d = env.repo.path();
    git(d, &["tag", "@acme/lib@1.0.0"]);
    git(d, &["commit", "-q", "--allow-empty", "-m", "docs: later"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    (env, head)
}

fn annotated(tag_object: &str) -> String {
    format!(r#"{{"ref":"refs/tags/tool@0.3.0","object":{{"sha":"{tag_object}","type":"tag"}}}}"#)
}

#[test]
fn annotated_tag_on_head_is_peeled_and_counts() {
    let (env, head) = setup_tool_only();
    let tag_obj = format!(r#"{{"sha":"aaaa","object":{{"sha":"{head}","type":"commit"}}}}"#);
    let published = format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets());
    let ann = annotated("aaaa");
    let routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &ann, &[]),
        ("GET /repos/acme/x/git/tags/aaaa", 200, &tag_obj, &[]),
        (TOOL_REL, 200, &published, &[]),
    ];
    let api = FakeApi::start(&with_registries(&routes));
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    let tag = item(&v, "tag", "tool@0.3.0");
    assert!(
        tag["reason"].as_str().unwrap().contains("already"),
        "{tag:#}"
    );
    assert_eq!(tag["verify"]["ok"], true, "{tag:#}");
}

#[test]
fn annotated_tag_elsewhere_exits_1() {
    let (env, head) = setup_tool_only();
    let tag_obj = r#"{"sha":"aaaa","object":{"sha":"2222222222222222222222222222222222222222","type":"commit"}}"#;
    let ann = annotated("aaaa");
    let routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &ann, &[]),
        ("GET /repos/acme/x/git/tags/aaaa", 200, tag_obj, &[]),
    ];
    let api = FakeApi::start(&with_registries(&routes));
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    let reason = item(&v, "tag", "tool@0.3.0")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("tag exists at another commit") && reason.contains("2222222"),
        "{reason}"
    );
}

/// Routes for `tool@0.3.0` with its tag on HEAD and the draft `draft_assets` (id 2).
fn draft_routes(head: &str, draft_assets: &str) -> (String, String, String) {
    (
        ref_json(head),
        format!(r#"[{{"id":2,"tag_name":"tool@0.3.0","draft":true,"assets":[{draft_assets}]}}]"#),
        format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets()),
    )
}

fn run_draft(extra: &[RouteSpec], draft_assets: &str) -> (Value, Option<i32>, FakeApi) {
    let (env, head) = setup_tool_only();
    let (at_head, list, published) = draft_routes(&head, draft_assets);
    let mut routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &at_head, &[]),
        (TOOL_REL, 404, "", &[]),
        (TOOL_REL, 200, &published, &[]),
        (LIST, 200, &list, &[]),
        (
            "POST /repos/acme/x/releases/2/assets",
            201,
            r#"{"id":9}"#,
            &[],
        ),
        ("PATCH /repos/acme/x/releases/2", 200, r#"{"id":2}"#, &[]),
        ("DELETE /repos/acme/x/releases/assets/5", 204, "", &[]),
    ];
    routes.extend_from_slice(extra);
    let api = FakeApi::start(&with_registries(&routes));
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    (v, output.status.code(), api)
}

fn w(m: &str, u: &str) -> (String, String) {
    (m.to_string(), u.to_string())
}

#[test]
fn draft_starter_asset_is_deleted_and_uploaded_again() {
    let starter = format!(
        r#"{{"id":5,"name":"{LINUX}","state":"starter","size":{},"digest":null}}"#,
        LINUX_BYTES.len()
    );
    let assets = format!("{},{starter}", asset(12, WINDOWS, WINDOWS_BYTES));
    let (v, code, api) = run_draft(&[], &assets);
    assert_eq!(code, Some(0), "{v:#}");
    assert_eq!(
        writes(&api),
        vec![
            w("DELETE", "/repos/acme/x/releases/assets/5"),
            w(
                "POST",
                &format!("/repos/acme/x/releases/2/assets?name={LINUX}")
            ),
            w("POST", "/repos/acme/x/releases/2/assets?name=SHA256SUMS"),
            w("PATCH", "/repos/acme/x/releases/2"),
        ]
    );
}

#[test]
fn draft_asset_with_other_digest_is_replaced() {
    let wrong = format!(
        r#"{{"id":5,"name":"{WINDOWS}","state":"uploaded","size":{},"digest":"sha256:{}"}}"#,
        WINDOWS_BYTES.len(),
        "0".repeat(64)
    );
    let assets = format!("{wrong},{}", asset(11, LINUX, LINUX_BYTES));
    let (v, code, api) = run_draft(&[], &assets);
    assert_eq!(code, Some(0), "{v:#}");
    let reqs: Vec<_> = api
        .requests()
        .into_iter()
        .filter(|r| r.method != "GET")
        .collect();
    assert_eq!(
        writes(&api),
        vec![
            w("DELETE", "/repos/acme/x/releases/assets/5"),
            w(
                "POST",
                &format!("/repos/acme/x/releases/2/assets?name={WINDOWS}")
            ),
            w("POST", "/repos/acme/x/releases/2/assets?name=SHA256SUMS"),
            w("PATCH", "/repos/acme/x/releases/2"),
        ]
    );
    assert_eq!(reqs[1].raw, WINDOWS_BYTES);
}

#[test]
fn upload_already_exists_relists_and_accepts_a_matching_asset() {
    // The re-listed asset has no digest: the size decides.
    let relisted = format!(
        r#"[{{"id":7,"name":"{LINUX}","state":"uploaded","size":{}}}]"#,
        LINUX_BYTES.len()
    );
    let linux_post = format!("POST /repos/acme/x/releases/2/assets?name={LINUX}");
    let extra: Vec<RouteSpec> = vec![
        (
            &linux_post,
            422,
            r#"{"message":"Validation Failed","errors":[{"resource":"ReleaseAsset","code":"already_exists","field":"name"}]}"#,
            &[],
        ),
        ("GET /repos/acme/x/releases/2/assets", 200, &relisted, &[]),
    ];
    let (v, code, api) = run_draft(&extra, "");
    assert_eq!(code, Some(0), "{v:#}");
    assert_eq!(
        writes(&api),
        vec![
            w(
                "POST",
                &format!("/repos/acme/x/releases/2/assets?name={WINDOWS}")
            ),
            w(
                "POST",
                &format!("/repos/acme/x/releases/2/assets?name={LINUX}")
            ),
            w("POST", "/repos/acme/x/releases/2/assets?name=SHA256SUMS"),
            w("PATCH", "/repos/acme/x/releases/2"),
        ]
    );
}

#[test]
fn published_release_with_missing_asset_exits_1_without_writes() {
    let (env, head) = setup_tool_only();
    let at_head = ref_json(&head);
    let published = format!(
        r#"{{"id":2,"draft":false,"assets":[{},{}]}}"#,
        asset(11, LINUX, LINUX_BYTES),
        asset(12, WINDOWS, WINDOWS_BYTES)
    );
    let routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &at_head, &[]),
        (TOOL_REL, 200, &published, &[]),
    ];
    let api = FakeApi::start(&with_registries(&routes));
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    api.assert_only_get();
    let reason = item(&v, "github_release", "tool@0.3.0")["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("published release tool@0.3.0 lacks SHA256SUMS")
            && reason.contains("releases are immutable"),
        "{reason}"
    );
}

#[test]
fn draft_search_stops_at_the_first_matching_draft() {
    let (env, head) = setup_tool_only();
    let at_head = ref_json(&head);
    let page1 = format!(
        r#"[{{"id":2,"tag_name":"tool@0.3.0","draft":true,"assets":[{}]}}]"#,
        all_assets()
    );
    let published = format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets());
    let routes: Vec<RouteSpec> = vec![
        (TOOL_REF, 200, &at_head, &[]),
        (TOOL_REL, 404, "", &[]),
        (TOOL_REL, 200, &published, &[]),
        (
            LIST,
            200,
            &page1,
            &[(
                "Link",
                "<{base}/repos/acme/x/releases?per_page=100&page=2>; rel=\"next\"",
            )],
        ),
        ("PATCH /repos/acme/x/releases/2", 200, r#"{"id":2}"#, &[]),
    ];
    let api = FakeApi::start(&with_registries(&routes));
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let lists = api
        .requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path == "/repos/acme/x/releases")
        .count();
    assert_eq!(lists, 1, "{:?}", api.requests());
    assert_eq!(writes(&api), vec![w("PATCH", "/repos/acme/x/releases/2")]);
}

// ---- Task 8: npm, crates.io and `on = "release"` deploys ----

/// A fake API where both tags and GitHub Releases are done at `head`, plus the Actions OIDC
/// endpoint and the crates.io token exchange. `extra` comes first; a registry version that
/// `extra` does not route is already published ([`ON_REGISTRIES`]).
fn api_done(head: &str, extra: &[RouteSpec]) -> FakeApi {
    let at_head = ref_json(head);
    let tool_published = format!(r#"{{"id":2,"draft":false,"assets":[{}]}}"#, all_assets());
    let oidc = format!(r#"{{"value":"{JWT}"}}"#);
    let token = format!(r#"{{"token":"{CRATES_TOKEN}"}}"#);
    let post = format!("POST {TOKENS}");
    let delete = format!("DELETE {TOKENS}");
    let mut routes: Vec<RouteSpec> = extra.to_vec();
    for r in ON_REGISTRIES {
        if !extra.iter().any(|e| e.0 == r.0) {
            routes.push(r);
        }
    }
    routes.extend_from_slice(&[
        (LIB_REF, 200, &at_head, &[]),
        (TOOL_REF, 200, &at_head, &[]),
        (LIB_REL, 200, r#"{"id":1,"draft":false,"assets":[]}"#, &[]),
        (TOOL_REL, 200, &tool_published, &[]),
        ("GET /oidc", 200, &oidc, &[]),
        (&post, 200, &token, &[]),
        (&delete, 204, "", &[]),
        ("GET /cargo-ran", 200, "{}", &[]),
    ]);
    FakeApi::start(&routes)
}

fn has_line(log: &[String], start: &str) -> bool {
    log.iter().any(|l| l.starts_with(start))
}

/// Non-GET requests plus the cargo stub's marker GET, in order.
fn writes_and_cargo(api: &FakeApi) -> Vec<(String, String)> {
    api.requests()
        .into_iter()
        .filter(|r| r.method != "GET" || r.path == "/cargo-ran")
        .map(|r| (r.method, r.path))
        .collect()
}

/// The cargo stub: records (via the fake API) that it ran, and whether it received the token.
fn cargo_stub(env: &Env, tail: &str) {
    let ok = env.tools.path().join("cargo.token-ok");
    stub(
        env.tools.path(),
        "cargo",
        &format!(
            "curl -s --noproxy '*' \"$HOIST_CRATES_API/cargo-ran\" >/dev/null\n\
             if [ \"$CARGO_REGISTRY_TOKEN\" = '{CRATES_TOKEN}' ]; then echo yes > '{}'; fi\n\
             echo '   Uploading tool v0.3.0'\n{tail}",
            ok.display()
        ),
    );
}

#[test]
fn npm_publish_runs_in_package_dir() {
    let env = setup();
    let pwd = env.tools.path().join("npm.pwd");
    stub(
        env.tools.path(),
        "npm",
        &format!(
            "if [ \"$1\" = --version ]; then echo 12.2.0; else pwd > '{}'; echo '+ @acme/lib@1.0.0'; fi",
            pwd.display()
        ),
    );
    let api = api_done(
        &env.head,
        &[
            (LIB_V, 404, "", &[]),
            (LIB_V, 200, "{}", &[]),
            (LIB_PKG, 200, "{}", &[]),
            (TOOL_V, 200, "{}", &[]),
        ],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let log = tool_log(&env, "npm");
    assert!(has_line(&log, "--version env:"), "{log:?}");
    assert!(has_line(&log, "publish --access public env:"), "{log:?}");
    assert!(
        !log.iter().any(|l| l.contains("CARGO_REGISTRY_TOKEN")),
        "{log:?}"
    );
    let ran_in = std::fs::read_to_string(&pwd).unwrap();
    let want = env.repo.path().join("packages/lib").canonicalize().unwrap();
    assert_eq!(Path::new(ran_in.trim()).canonicalize().unwrap(), want);
    // npm does the OIDC exchange itself: hoist sends no write and no OIDC request.
    assert!(writes(&api).is_empty(), "{:?}", writes(&api));
    assert!(!api.requests().iter().any(|r| r.path == "/oidc"));
    // Tool output goes to stderr; stdout stays one JSON object.
    assert!(
        stderr(&output).contains("+ @acme/lib@1.0.0"),
        "{}",
        stderr(&output)
    );
    let npm = item(&v, "npm", "@acme/lib@1.0.0");
    assert_eq!(npm["action"], "ok", "{npm:#}");
    assert_eq!(npm["verify"]["ok"], true, "{npm:#}");
    let crates = item(&v, "crates", "tool@0.3.0");
    assert!(
        crates["reason"].as_str().unwrap().contains("already"),
        "{crates:#}"
    );
    assert!(tool_log(&env, "cargo").is_empty());
}

#[test]
fn old_npm_fails_that_package() {
    let env = setup();
    stub(
        env.tools.path(),
        "npm",
        "if [ \"$1\" = --version ]; then echo 11.4.2; else echo published; fi",
    );
    let api = api_done(
        &env.head,
        &[(LIB_V, 404, "", &[]), (LIB_PKG, 200, "{}", &[])],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    let npm = item(&v, "npm", "@acme/lib@1.0.0");
    assert_eq!(npm["action"], "update", "{npm:#}");
    let reason = npm["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("failed:") && reason.contains("11.4.2") && reason.contains("11.5.1"),
        "{npm:#}"
    );
    assert!(!has_line(&tool_log(&env, "npm"), "publish"));
    // The first failure stops the remaining packages.
    assert_eq!(item(&v, "crates", "tool@0.3.0")["reason"], "not attempted");
    assert_eq!(item(&v, "tag", "tool@0.3.0")["reason"], "not attempted");
    assert!(tool_log(&env, "cargo").is_empty());
}

#[test]
fn crates_exchange_publish_and_revoke() {
    let env = setup();
    cargo_stub(&env, "");
    let api = api_done(
        &env.head,
        &[
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 200, "{}", &[]),
            (TOOL_PKG, 200, "{}", &[]),
        ],
    );
    let output = in_actions(&env, &api).arg("-v").output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let w = |m: &str, p: &str| (m.to_string(), p.to_string());
    assert_eq!(
        writes_and_cargo(&api),
        vec![
            w("POST", TOKENS),
            w("GET", "/cargo-ran"),
            w("DELETE", TOKENS)
        ]
    );
    let reqs = api.requests();
    let oidc = reqs.iter().find(|r| r.path == "/oidc").unwrap();
    assert_eq!(oidc.url, "/oidc?api-version=2.0&audience=crates.io");
    assert_eq!(oidc.auth.as_deref(), Some("Bearer req-token"));
    let post = reqs.iter().find(|r| r.method == "POST").unwrap();
    let sent: Value = serde_json::from_slice(&post.raw).unwrap();
    assert_eq!(sent, serde_json::json!({ "jwt": JWT }));
    let delete = reqs.iter().find(|r| r.method == "DELETE").unwrap();
    assert_eq!(
        delete.auth.as_deref(),
        Some(&*format!("Bearer {CRATES_TOKEN}"))
    );
    // The token reaches cargo through CARGO_REGISTRY_TOKEN only.
    let log = tool_log(&env, "cargo");
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].starts_with("publish -p tool --locked env:"),
        "{log:?}"
    );
    assert!(log[0].contains("CARGO_REGISTRY_TOKEN"), "{log:?}");
    assert!(
        !log[0].contains(CRATES_TOKEN) && !log[0].contains(JWT),
        "{log:?}"
    );
    assert!(env.tools.path().join("cargo.token-ok").is_file());
    // Neither secret appears in any output, even with -v.
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(!all.contains(JWT) && !all.contains(CRATES_TOKEN), "{all}");
    assert!(
        stderr(&output).contains("Uploading tool"),
        "{}",
        stderr(&output)
    );
    let crates = item(&v, "crates", "tool@0.3.0");
    assert_eq!(crates["action"], "ok", "{crates:#}");
    assert_eq!(crates["verify"]["ok"], true, "{crates:#}");
}

#[test]
fn crates_revoke_runs_when_cargo_fails() {
    let env = setup();
    cargo_stub(&env, "exit 101");
    let api = api_done(
        &env.head,
        &[(TOOL_V, 404, "", &[]), (TOOL_PKG, 200, "{}", &[])],
    );
    let output = in_actions(&env, &api).arg("-v").output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    let w = |m: &str, p: &str| (m.to_string(), p.to_string());
    assert_eq!(
        writes_and_cargo(&api),
        vec![
            w("POST", TOKENS),
            w("GET", "/cargo-ran"),
            w("DELETE", TOKENS)
        ]
    );
    let crates = item(&v, "crates", "tool@0.3.0");
    let reason = crates["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("failed:") && reason.contains("cargo publish"),
        "{crates:#}"
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(!all.contains(JWT) && !all.contains(CRATES_TOKEN), "{all}");
}

#[test]
fn failed_revoke_is_reported_but_publish_succeeds() {
    let env = setup();
    cargo_stub(&env, "");
    let delete = format!("DELETE {TOKENS}");
    let api = api_done(
        &env.head,
        &[
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 200, "{}", &[]),
            (TOOL_PKG, 200, "{}", &[]),
            (&delete, 500, r#"{"message":"boom"}"#, &[]),
        ],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let revoke = item(&v, "crates_token", "tool@0.3.0");
    assert_eq!(revoke["action"], "unknown", "{revoke:#}");
    assert!(
        revoke["reason"].as_str().unwrap().contains("revoke"),
        "{revoke:#}"
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(!all.contains(CRATES_TOKEN), "{all}");
}

#[test]
fn rerun_after_partial_failure_publishes_only_missing() {
    // A previous run tagged both, released both and published npm; crates.io failed.
    let env = setup();
    let d = env.repo.path();
    git(d, &["tag", "@acme/lib@1.0.0"]);
    git(d, &["tag", "tool@0.3.0"]);
    cargo_stub(&env, "");
    let api = api_done(
        &env.head,
        &[
            (LIB_V, 200, "{}", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 200, "{}", &[]),
            (TOOL_PKG, 200, "{}", &[]),
        ],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let w = |m: &str, p: &str| (m.to_string(), p.to_string());
    assert_eq!(
        writes_and_cargo(&api),
        vec![
            w("POST", TOKENS),
            w("GET", "/cargo-ran"),
            w("DELETE", TOKENS)
        ]
    );
    assert!(
        tool_log(&env, "npm").is_empty(),
        "{:?}",
        tool_log(&env, "npm")
    );
    assert!(has_line(
        &tool_log(&env, "cargo"),
        "publish -p tool --locked"
    ));
    for (kind, key) in [
        ("tag", "@acme/lib@1.0.0"),
        ("github_release", "@acme/lib@1.0.0"),
        ("npm", "@acme/lib@1.0.0"),
        ("tag", "tool@0.3.0"),
        ("github_release", "tool@0.3.0"),
    ] {
        let i = item(&v, kind, key);
        assert_eq!(i["action"], "ok", "{i:#}");
        assert!(i["reason"].as_str().unwrap().contains("already"), "{i:#}");
    }
    assert_eq!(item(&v, "crates", "tool@0.3.0")["action"], "ok");
}

#[test]
fn never_published_package_is_pending_and_others_continue() {
    let env = setup();
    cargo_stub(&env, "");
    let api = api_done(
        &env.head,
        &[
            (LIB_V, 404, "", &[]),
            (LIB_PKG, 404, "", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 200, "{}", &[]),
            (TOOL_PKG, 200, "{}", &[]),
        ],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(2), "{v:#}\n{}", stderr(&output));
    let npm = item(&v, "npm", "@acme/lib@1.0.0");
    assert_eq!(npm["action"], "pending", "{npm:#}");
    let steps = v["human_steps"].as_array().unwrap();
    assert!(
        steps.iter().any(|s| s["id"] == "first-publish:@acme/lib"),
        "{v:#}"
    );
    // npm was never asked to publish (no OIDC attempt for it); the crate still published.
    assert!(!has_line(&tool_log(&env, "npm"), "publish"));
    assert!(has_line(
        &tool_log(&env, "cargo"),
        "publish -p tool --locked"
    ));
    assert_eq!(item(&v, "crates", "tool@0.3.0")["action"], "ok");
}

#[test]
fn missing_oidc_env_exits_3_before_writes() {
    let env = setup();
    let api = api_done(&env.head, &[]);
    let output = in_actions(&env, &api)
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("permissions: id-token: write"),
        "{}",
        stderr(&output)
    );
    assert!(api.requests().is_empty(), "{:?}", api.requests());
    assert!(tool_log(&env, "npm").is_empty() && tool_log(&env, "cargo").is_empty());
}

#[test]
fn release_deploy_runs_pnpm_filter() {
    let env = setup();
    let d = env.repo.path();
    write(
        d,
        "packages/lib/hoist.toml",
        b"[deploy]\nprovider = \"cloudflare-pages\"\non = \"release\"\n",
    );
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "ci: deploy on release"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let api = api_done(&head, &ON_REGISTRIES);
    let output = in_actions(&env, &api)
        .env("GITHUB_SHA", &head)
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    let log = tool_log(&env, "pnpm");
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].starts_with("--filter @acme/lib run deploy env:"),
        "{log:?}"
    );
    assert!(stderr(&output).contains("deployed"), "{}", stderr(&output));
    let deploy = item(&v, "deploy", "@acme/lib@1.0.0");
    assert_eq!(deploy["action"], "ok", "{deploy:#}");
    assert!(
        !v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "deploy" && i["key"] == "tool@0.3.0")
    );
}

#[test]
fn registry_lag_retries_then_ok() {
    let env = setup();
    cargo_stub(&env, "");
    let api = api_done(
        &env.head,
        &[
            (LIB_V, 200, "{}", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_V, 200, "{}", &[]),
            (TOOL_PKG, 200, "{}", &[]),
        ],
    );
    let started = std::time::Instant::now();
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert!(started.elapsed().as_secs() < 3, "{:?}", started.elapsed());
    let polls = api
        .requests()
        .iter()
        .filter(|r| r.path == "/api/v1/crates/tool/0.3.0")
        .count();
    assert_eq!(polls, 4, "{:?}", api.requests());
    let crates = item(&v, "crates", "tool@0.3.0");
    assert_eq!(crates["verify"]["ok"], true, "{crates:#}");
}

#[test]
fn registry_never_visible_exits_1() {
    let env = setup();
    cargo_stub(&env, "");
    let api = api_done(
        &env.head,
        &[
            (LIB_V, 200, "{}", &[]),
            (TOOL_V, 404, "", &[]),
            (TOOL_PKG, 200, "{}", &[]),
        ],
    );
    let output = in_actions(&env, &api).output().unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    let polls = api
        .requests()
        .iter()
        .filter(|r| r.path == "/api/v1/crates/tool/0.3.0")
        .count();
    assert_eq!(polls, 6, "one check before publish, then 5 polls");
    let crates = item(&v, "crates", "tool@0.3.0");
    assert_eq!(crates["verify"]["ok"], false, "{crates:#}");
    assert!(
        crates["verify"]["detail"]
            .as_str()
            .unwrap()
            .contains("0.3.0"),
        "{crates:#}"
    );
}
