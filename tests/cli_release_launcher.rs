mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, git_wrapper, hoist, set_origin_head, stub};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const REMOTE: &str = "https://github.com/acme/x.git";
const PULLS_GET: &str = "GET /repos/acme/x/pulls?head=acme:hoist/release&state=open";
const TOOL_REF: &str = "GET /repos/acme/x/git/ref/tags/tool%400.3.0";
const TOOL_REL: &str = "GET /repos/acme/x/releases/tags/tool%400.3.0";
const TOOL_V: &str = "GET /api/v1/crates/tool/0.3.0";
const LINUX_V: &str = "GET /@acme%2Ftool-linux-x64/0.3.0";
const LINUX_PKG: &str = "GET /@acme%2Ftool-linux-x64";
const DARWIN_V: &str = "GET /@acme%2Ftool-darwin-arm64/0.3.0";
const DARWIN_PKG: &str = "GET /@acme%2Ftool-darwin-arm64";
const LAUNCHER_V: &str = "GET /@acme%2Ftool/0.3.0";
const LAUNCHER_PKG: &str = "GET /@acme%2Ftool";
const LINUX: &str = "tool-x86_64-unknown-linux-musl";
const DARWIN: &str = "tool-aarch64-apple-darwin";
const LINUX_BYTES: &[u8] = b"\x7fELF linux tool \x00\xff\x01";
const DARWIN_BYTES: &[u8] = b"\xcf\xfa\xed\xfe darwin tool \x00\x02";

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

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn item<'a>(v: &'a Value, kind: &str, key: &str) -> &'a Value {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no item {kind}/{key} in {v:#}"))
}

/// The release-launcher fixture on `main`, committed: crate `tool@0.3.0` (two targets), the
/// launcher `@acme/tool@0.3.0` in npm/tool and its platform packages linux-x64, darwin-arm64.
fn repo() -> TempDir {
    let repo = fixture_repo("release-launcher", REMOTE);
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    set_origin_head(d, "main");
    repo
}

// ---- prepare ----

struct Prep {
    repo: TempDir,
    bare: TempDir,
    bin: TempDir,
}

/// The fixture tagged `tool@0.3.0`, `edit` applied, then a `feat:` commit in the crate; the
/// push URL of `origin` is a local bare repo; stub pnpm/cargo and the git wrapper.
fn prep(edit: impl FnOnce(&Path)) -> Prep {
    let repo = repo();
    let d = repo.path();
    git(d, &["tag", "tool@0.3.0"]);
    edit(d);
    write(d, "crates/tool/src/lib.rs", b"// faster\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "feat: faster cache"]);
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
    stub(bin.path(), "git", &git_wrapper());
    Prep { repo, bare, bin }
}

fn prepare_out(p: &Prep, api: &FakeApi) -> std::process::Output {
    hoist()
        .arg("-C")
        .arg(p.repo.path())
        .args(["release", "prepare", "--json"])
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .env("HOIST_PNPM", p.bin.path().join("pnpm"))
        .env("HOIST_CARGO", p.bin.path().join("cargo"))
        .env("HOIST_NOW", "2026-10-17T12:00:00Z")
        .env("HOIST_GIT", p.bin.path().join("git"))
        .env("STUB_ORIGIN", p.bare.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
}

#[test]
fn prepare_bumps_platform_packages_and_optional_deps() {
    let env = prep(|_| {});
    let (d, bare, bin) = (env.repo.path(), &env.bare, &env.bin);

    // `release plan`: the launcher follows the crate; platform packages are not packages.
    let plan = hoist()
        .arg("-C")
        .arg(d)
        .args(["release", "plan", "--json"])
        .env("HOIST_GIT", bin.path().join("git"))
        .env("STUB_ORIGIN", bare.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let p = json(&plan);
    assert_eq!(plan.status.code(), Some(0), "{p:#}\n{}", stderr(&plan));
    let keys: Vec<&str> = p["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["@acme/tool", "tool"], "{p:#}");
    let launcher = item(&p, "version", "@acme/tool");
    assert_eq!(launcher["action"], "create");
    assert!(
        launcher["reason"]
            .as_str()
            .unwrap()
            .starts_with("0.3.0 -> 0.4.0 (follows tool"),
        "{launcher:#}"
    );

    let routes: &[RouteSpec] = &[
        (PULLS_GET, 200, "[]", &[]),
        ("POST /repos/acme/x/pulls", 201, r#"{"number":5}"#, &[]),
    ];
    let api = FakeApi::start(routes);
    let output = prepare_out(&env, &api);
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert_eq!(item(&v, "version", "@acme/tool")["action"], "update");
    assert_eq!(item(&v, "version", "tool")["action"], "update");

    let b = bare.path();
    assert_eq!(
        out(b, &["log", "-1", "--format=%s", "hoist/release"]).trim(),
        "release: tool@0.4.0, @acme/tool@0.4.0"
    );
    let launcher = out(b, &["show", "hoist/release:npm/tool/package.json"]);
    assert!(launcher.contains("\"version\": \"0.4.0\""), "{launcher}");
    assert!(
        launcher.contains("\"@acme/tool-linux-x64\": \"0.4.0\""),
        "{launcher}"
    );
    assert!(
        launcher.contains("\"@acme/tool-darwin-arm64\": \"0.4.0\""),
        "{launcher}"
    );
    for platform in ["linux-x64", "darwin-arm64"] {
        let text = out(
            b,
            &[
                "show",
                &format!("hoist/release:npm/tool-{platform}/package.json"),
            ],
        );
        assert!(text.contains("\"version\": \"0.4.0\""), "{text}");
        // Nothing else changes.
        assert_eq!(
            text.replace("0.4.0", "0.3.0"),
            std::fs::read_to_string(d.join(format!("npm/tool-{platform}/package.json"))).unwrap()
        );
    }
    let crate_log = out(b, &["show", "hoist/release:crates/tool/CHANGELOG.md"]);
    assert!(crate_log.contains("## 0.4.0 (2026-10-17)"), "{crate_log}");
    // The launcher rides on the crate's tag: no changelog and no tag of its own.
    let files = out(b, &["ls-tree", "-r", "--name-only", "hoist/release"]);
    assert!(!files.contains("npm/tool/CHANGELOG.md"), "{files}");
    let post = api
        .requests()
        .into_iter()
        .find(|q| q.method == "POST")
        .expect("POST /pulls");
    let body: Value = serde_json::from_str(post.body.as_deref().unwrap()).unwrap();
    let text = body["body"].as_str().unwrap();
    assert!(text.contains("- `tool@0.4.0`"), "{text}");
    assert!(!text.contains("- `@acme/tool@0.4.0`"), "{text}");
}

fn launcher_json(deps: &str) -> String {
    format!(
        "{{\n  \"name\": \"@acme/tool\",\n  \"version\": \"0.3.0\",\n  \"private\": false,\n  \
         \"bin\": {{ \"tool\": \"bin/tool.js\" }},\n  \"optionalDependencies\": {{\n    {deps}\n  }}\n}}\n"
    )
}

/// R23: prepare refuses, before any write, a launcher whose optionalDependencies use a
/// `workspace:` spec or name something that is not one of its platform packages.
#[test]
fn prepare_refuses_bad_optional_dependencies() {
    let cases = [
        (
            "\"@acme/tool-linux-x64\": \"workspace:*\",\n    \"@acme/tool-darwin-arm64\": \"0.3.0\"",
            "workspace:",
        ),
        (
            "\"@acme/tool-linux-x64\": \"0.3.0\",\n    \"@acme/tool-darwin-arm64\": \"0.3.0\",\n    \"@acme/tool-win32-x64\": \"0.3.0\"",
            "@acme/tool-win32-x64",
        ),
    ];
    for (deps, needle) in cases {
        let p = prep(|d| write(d, "npm/tool/package.json", launcher_json(deps).as_bytes()));
        let api = FakeApi::start(&[(PULLS_GET, 200, "[]", &[])]);
        let output = prepare_out(&p, &api);
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(3), "{needle}: {err}");
        assert!(
            err.contains(needle) && err.contains("optionalDependencies"),
            "{err}"
        );
        assert!(
            api.requests().iter().all(|r| r.method == "GET"),
            "{:?}",
            api.requests()
        );
        let branches = out(p.bare.path(), &["branch", "--list", "hoist/release"]);
        assert!(branches.trim().is_empty(), "{branches}");
        assert_eq!(out(p.repo.path(), &["status", "--porcelain"]), "");
    }
}

// ---- model errors ----

fn plan_exit(d: &Path) -> (Option<i32>, String) {
    let output = hoist()
        .arg("-C")
        .arg(d)
        .args(["release", "plan"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    (output.status.code(), stderr(&output))
}

#[test]
fn unknown_target_exits_3() {
    let repo = repo();
    let d = repo.path();
    write(
        d,
        "crates/tool/hoist.toml",
        b"[publish]\ngithub_release = { targets = [\"x86_64-unknown-linux-musl\", \"riscv64gc-unknown-linux-gnu\"] }\n",
    );
    let (code, err) = plan_exit(d);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("riscv64gc-unknown-linux-gnu"), "{err}");
    assert!(err.contains("@acme/tool"), "{err}");
}

#[test]
fn missing_platform_dir_exits_3() {
    let repo = repo();
    let d = repo.path();
    std::fs::remove_dir_all(d.join("npm/tool-darwin-arm64")).unwrap();
    let (code, err) = plan_exit(d);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("npm/tool-darwin-arm64"), "{err}");
}

#[test]
fn launcher_without_crate_exits_3() {
    let repo = repo();
    let d = repo.path();
    write(
        d,
        "npm/tool/package.json",
        b"{\n  \"name\": \"@acme/tool\",\n  \"version\": \"0.3.0\",\n  \"private\": false,\n  \"bin\": { \"other\": \"bin/other.js\" }\n}\n",
    );
    let (code, err) = plan_exit(d);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("`other`"), "{err}");
}

// ---- publish ----

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn asset(id: u64, name: &str, bytes: &[u8]) -> String {
    format!(
        r#"{{"id":{id},"name":"{name}","state":"uploaded","size":{},"digest":"sha256:{}"}}"#,
        bytes.len(),
        hex(bytes)
    )
}

struct Pub {
    repo: TempDir,
    assets: TempDir,
    tools: TempDir,
    head: String,
}

/// The fixture committed on `main`, the two binaries in an assets dir, a stub npm that logs
/// the directory of each `publish`.
fn pub_setup() -> Pub {
    let repo = repo();
    let head = out(repo.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let assets = tempfile::tempdir().unwrap();
    write(assets.path(), LINUX, LINUX_BYTES);
    write(assets.path(), DARWIN, DARWIN_BYTES);
    let tools = tempfile::tempdir().unwrap();
    let pwd = tools.path().join("npm.pwd");
    stub(
        tools.path(),
        "npm",
        &format!(
            "if [ \"$1\" = --version ]; then echo 12.2.0; exit 0; fi\n\
             pwd >> '{}'\n\
             if [ -f bin/tool ]; then cp -p bin/tool \"{}/$(basename \"$PWD\").bin\"; fi\n\
             if [ -n \"$STUB_NPM_FAILS\" ]; then case \"$PWD\" in *\"$STUB_NPM_FAILS\") echo 'npm ERR! 403' >&2; exit 1;; esac; fi\n\
             echo '+ published'",
            pwd.display(),
            tools.path().display()
        ),
    );
    stub(tools.path(), "cargo", "echo cargo-ran");
    Pub {
        repo,
        assets,
        tools,
        head,
    }
}

/// Tag and GitHub Release of `tool@0.3.0` done at HEAD, the crate already on crates.io;
/// `extra` (the npm routes) comes first.
fn api(head: &str, extra: &[RouteSpec]) -> FakeApi {
    let at_head = format!(r#"{{"ref":"refs/tags/x","object":{{"sha":"{head}","type":"commit"}}}}"#);
    let sums = format!(
        "{}  {DARWIN}\n{}  {LINUX}\n",
        hex(DARWIN_BYTES),
        hex(LINUX_BYTES)
    );
    let release = format!(
        r#"{{"id":2,"draft":false,"assets":[{},{},{}]}}"#,
        asset(11, LINUX, LINUX_BYTES),
        asset(12, DARWIN, DARWIN_BYTES),
        asset(13, "SHA256SUMS", sums.as_bytes())
    );
    let mut routes: Vec<RouteSpec> = extra.to_vec();
    routes.extend_from_slice(&[
        (TOOL_REF, 200, &at_head, &[]),
        (TOOL_REL, 200, &release, &[]),
        (TOOL_V, 200, "{}", &[]),
    ]);
    FakeApi::start(&routes)
}

fn publish(p: &Pub, api: &FakeApi) -> std::process::Output {
    publish_cmd(p, api).output().unwrap()
}

fn publish_cmd(p: &Pub, api: &FakeApi) -> assert_cmd::Command {
    let mut c = hoist();
    c.arg("-C")
        .arg(p.repo.path())
        .args(["release", "publish", "--json", "--assets"])
        .arg(p.assets.path())
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_GITHUB_UPLOADS", api.url())
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM", p.tools.path().join("npm"))
        .env("HOIST_CARGO", p.tools.path().join("cargo"))
        .env("HOIST_MAX_SLEEP_MS", "1")
        .env("GITHUB_TOKEN", "test-token")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_SHA", &p.head)
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

/// The directories npm published in, relative to the repo.
fn published_dirs(p: &Pub) -> Vec<String> {
    let root = p.repo.path().canonicalize().unwrap();
    std::fs::read_to_string(p.tools.path().join("npm.pwd"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            Path::new(l.trim())
                .canonicalize()
                .unwrap()
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn publish_copies_binaries_and_publishes_platforms_first() {
    let p = pub_setup();
    let api = api(
        &p.head,
        &[
            (LINUX_V, 404, "", &[]),
            (LINUX_V, 200, "{}", &[]),
            (LINUX_PKG, 200, "{}", &[]),
            (DARWIN_V, 404, "", &[]),
            (DARWIN_V, 200, "{}", &[]),
            (DARWIN_PKG, 200, "{}", &[]),
            (LAUNCHER_V, 404, "", &[]),
            (LAUNCHER_V, 200, "{}", &[]),
            (LAUNCHER_PKG, 200, "{}", &[]),
        ],
    );
    let output = publish(&p, &api);
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert_eq!(
        published_dirs(&p),
        ["npm/tool-linux-x64", "npm/tool-darwin-arm64", "npm/tool"]
    );
    // npm saw each binary in place, executable; afterwards the checkout is clean again.
    let linux = p.tools.path().join("tool-linux-x64.bin");
    let darwin = p.tools.path().join("tool-darwin-arm64.bin");
    assert_eq!(std::fs::read(&linux).unwrap(), LINUX_BYTES);
    assert_eq!(std::fs::read(&darwin).unwrap(), DARWIN_BYTES);
    #[cfg(unix)]
    {
        assert_eq!(mode(&linux), 0o755);
        assert_eq!(mode(&darwin), 0o755);
    }
    assert_clean(&p);
    for key in [
        "@acme/tool-linux-x64@0.3.0",
        "@acme/tool-darwin-arm64@0.3.0",
        "@acme/tool@0.3.0",
    ] {
        let it = item(&v, "npm", key);
        assert_eq!(it["action"], "ok", "{it:#}");
        assert!(
            it["reason"].as_str().unwrap().starts_with("published"),
            "{it:#}"
        );
        assert_eq!(it["verify"]["ok"], true, "{it:#}");
    }
    // The items come in run order: platforms, then the launcher.
    let npm_keys: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "npm")
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        npm_keys,
        [
            "@acme/tool-linux-x64@0.3.0",
            "@acme/tool-darwin-arm64@0.3.0",
            "@acme/tool@0.3.0"
        ]
    );
    // The launcher has no tag or GitHub Release of its own.
    assert!(
        !v["items"].as_array().unwrap().iter().any(|i| i["key"]
            .as_str()
            .unwrap()
            .starts_with("@acme/tool@")
            && i["kind"] != "npm"),
        "{v:#}"
    );
}

#[test]
fn launcher_rerun_skips_published_platforms() {
    let p = pub_setup();
    // A re-run: this release's tag is on HEAD.
    git(p.repo.path(), &["tag", "tool@0.3.0"]);
    let api = api(
        &p.head,
        &[
            (LINUX_V, 200, "{}", &[]),
            (DARWIN_V, 404, "", &[]),
            (DARWIN_V, 200, "{}", &[]),
            (DARWIN_PKG, 200, "{}", &[]),
            (LAUNCHER_V, 404, "", &[]),
            (LAUNCHER_V, 200, "{}", &[]),
            (LAUNCHER_PKG, 200, "{}", &[]),
        ],
    );
    let output = publish(&p, &api);
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert_eq!(published_dirs(&p), ["npm/tool-darwin-arm64", "npm/tool"]);
    let linux = item(&v, "npm", "@acme/tool-linux-x64@0.3.0");
    assert!(
        linux["reason"].as_str().unwrap().contains("already on npm"),
        "{linux:#}"
    );
    // A skipped platform gets no binary copied.
    assert!(!p.tools.path().join("tool-linux-x64.bin").exists());
    assert!(p.tools.path().join("tool-darwin-arm64.bin").exists());
    assert_clean(&p);
}

/// No copied binary stays behind: `git status` is clean, no `bin/` in platform packages.
fn assert_clean(p: &Pub) {
    let d = p.repo.path();
    assert_eq!(out(d, &["status", "--porcelain", "--ignored"]), "");
    assert!(!d.join("npm/tool-linux-x64/bin").exists());
    assert!(!d.join("npm/tool-darwin-arm64/bin").exists());
}

#[test]
fn rerun_with_only_the_launcher_left() {
    let p = pub_setup();
    git(p.repo.path(), &["tag", "tool@0.3.0"]);
    let api = api(
        &p.head,
        &[
            (LINUX_V, 200, "{}", &[]),
            (DARWIN_V, 200, "{}", &[]),
            (LAUNCHER_V, 404, "", &[]),
            (LAUNCHER_V, 200, "{}", &[]),
            (LAUNCHER_PKG, 200, "{}", &[]),
        ],
    );
    let output = publish(&p, &api);
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert_eq!(published_dirs(&p), ["npm/tool"]);
    for key in [
        "@acme/tool-linux-x64@0.3.0",
        "@acme/tool-darwin-arm64@0.3.0",
    ] {
        let it = item(&v, "npm", key);
        assert!(
            it["reason"].as_str().unwrap().contains("already on npm"),
            "{it:#}"
        );
    }
    let launcher = item(&v, "npm", "@acme/tool@0.3.0");
    assert!(
        launcher["reason"]
            .as_str()
            .unwrap()
            .starts_with("published"),
        "{launcher:#}"
    );
    assert_clean(&p);
}

#[test]
fn failed_platform_publish_removes_its_binary() {
    let p = pub_setup();
    let api = api(
        &p.head,
        &[
            (LINUX_V, 404, "", &[]),
            (LINUX_V, 200, "{}", &[]),
            (LINUX_PKG, 200, "{}", &[]),
            (DARWIN_V, 404, "", &[]),
            (DARWIN_PKG, 200, "{}", &[]),
        ],
    );
    let output = publish_cmd(&p, &api)
        .env("STUB_NPM_FAILS", "tool-darwin-arm64")
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(1), "{v:#}\n{}", stderr(&output));
    let darwin = item(&v, "npm", "@acme/tool-darwin-arm64@0.3.0");
    assert!(
        darwin["reason"].as_str().unwrap().starts_with("failed:"),
        "{darwin:#}"
    );
    assert_eq!(
        item(&v, "npm", "@acme/tool@0.3.0")["reason"],
        "not attempted"
    );
    // npm ran with the binary in place, and it is gone again.
    assert!(p.tools.path().join("tool-darwin-arm64.bin").exists());
    assert_clean(&p);
}

/// R23: before any write, publish checks that the launcher's optionalDependencies name
/// exactly its platform packages, each at the crate's version.
#[test]
fn publish_refuses_stale_optional_dependencies() {
    let cases = [
        (
            "\"@acme/tool-linux-x64\": \"0.2.0\",\n    \"@acme/tool-darwin-arm64\": \"0.3.0\"",
            "0.2.0",
        ),
        (
            "\"@acme/tool-linux-x64\": \"0.3.0\"",
            "@acme/tool-darwin-arm64",
        ),
        (
            "\"@acme/tool-linux-x64\": \"0.3.0\",\n    \"@acme/tool-darwin-arm64\": \"0.3.0\",\n    \"left-pad\": \"1.0.0\"",
            "left-pad",
        ),
    ];
    for (deps, needle) in cases {
        let p = pub_setup();
        let d = p.repo.path();
        write(d, "npm/tool/package.json", launcher_json(deps).as_bytes());
        git(d, &["commit", "-q", "-am", "chore: edit launcher"]);
        let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
        let p = Pub { head, ..p };
        let api = api(&p.head, &[]);
        let output = publish(&p, &api);
        let err = stderr(&output);
        assert_eq!(output.status.code(), Some(3), "{needle}: {err}");
        assert!(
            err.contains(needle) && err.contains("optionalDependencies"),
            "{err}"
        );
        assert!(
            api.requests().iter().all(|r| r.method == "GET"),
            "{:?}",
            api.requests()
        );
        assert!(published_dirs(&p).is_empty());
    }
}

#[test]
fn launcher_version_behind_crate_exits_3_before_writes() {
    let p = pub_setup();
    let d = p.repo.path();
    write(
        d,
        "crates/tool/Cargo.toml",
        b"[package]\nname = \"tool\"\nversion = \"0.4.0\"\nedition = \"2024\"\n",
    );
    git(d, &["commit", "-q", "-am", "release: tool@0.4.0"]);
    let head = out(d, &["rev-parse", "HEAD"]).trim().to_string();
    let p = Pub { head, ..p };
    let api = api(&p.head, &[]);
    let output = publish(&p, &api);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("@acme/tool") && err.contains("0.4.0"), "{err}");
    assert!(
        api.requests().iter().all(|r| r.method == "GET"),
        "{:?}",
        api.requests()
    );
    assert!(published_dirs(&p).is_empty());
}

#[test]
fn launcher_dry_run_reports_platforms_and_copies_nothing() {
    let p = pub_setup();
    let api = api(
        &p.head,
        &[
            (LINUX_V, 200, "{}", &[]),
            (DARWIN_V, 404, "", &[]),
            (DARWIN_PKG, 404, "", &[]),
            (LAUNCHER_V, 404, "", &[]),
            (LAUNCHER_PKG, 200, "{}", &[]),
        ],
    );
    let output = hoist()
        .arg("-C")
        .arg(p.repo.path())
        .args(["release", "publish", "--dry-run", "--json", "--assets"])
        .arg(p.assets.path())
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("GITHUB_TOKEN", "test-token")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let v = json(&output);
    assert_eq!(output.status.code(), Some(0), "{v:#}\n{}", stderr(&output));
    assert_eq!(
        item(&v, "npm", "@acme/tool-linux-x64@0.3.0")["action"],
        "ok"
    );
    let darwin = item(&v, "npm", "@acme/tool-darwin-arm64@0.3.0");
    assert_eq!(darwin["action"], "pending", "{darwin:#}");
    // R27: the launcher waits while a platform package is pending.
    let launcher = item(&v, "npm", "@acme/tool@0.3.0");
    assert_eq!(launcher["action"], "pending", "{launcher:#}");
    assert_eq!(launcher["reason"], "platform packages not published yet");
    assert!(
        v["human_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["id"] == "first-publish:@acme/tool-darwin-arm64"),
        "{v:#}"
    );
    assert!(api.requests().iter().all(|r| r.method == "GET"));
    assert!(!p.repo.path().join("npm/tool-darwin-arm64/bin").exists());
    assert!(published_dirs(&p).is_empty());
}

/// R27: a platform package that was never published leaves the launcher pending: npm does not
/// publish a launcher whose optional dependencies are missing.
#[test]
fn launcher_waits_for_pending_platform_packages() {
    let p = pub_setup();
    let api = api(
        &p.head,
        &[
            (LINUX_V, 404, "", &[]),
            (LINUX_V, 200, "{}", &[]),
            (LINUX_PKG, 200, "{}", &[]),
            (DARWIN_V, 404, "", &[]),
            (DARWIN_PKG, 404, "", &[]),
            (LAUNCHER_V, 404, "", &[]),
            (LAUNCHER_PKG, 200, "{}", &[]),
        ],
    );
    let output = publish(&p, &api);
    let v = json(&output);
    assert_eq!(output.status.code(), Some(2), "{v:#}\n{}", stderr(&output));
    assert_eq!(published_dirs(&p), ["npm/tool-linux-x64"]);
    let darwin = item(&v, "npm", "@acme/tool-darwin-arm64@0.3.0");
    assert_eq!(darwin["action"], "pending", "{darwin:#}");
    let launcher = item(&v, "npm", "@acme/tool@0.3.0");
    assert_eq!(launcher["action"], "pending", "{launcher:#}");
    assert_eq!(launcher["reason"], "platform packages not published yet");
    assert_clean(&p);
}
