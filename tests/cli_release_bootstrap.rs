mod common;

use common::{FakeApi, RouteSpec, fixture_repo, git, hoist, stub};
use serde_json::Value;
use tempfile::TempDir;

const REMOTE: &str = "https://github.com/acme/x.git";
const LIB_PKG: &str = "GET /@acme%2Flib";
const TOOL_PKG: &str = "GET /api/v1/crates/tool";
const TAG: &str = "tool@0.3.0";

/// npm 11.15.0, logged in, `trust list` answers an empty list.
const NPM_OK: &str = r#"case "$1" in
  --version) echo 11.15.0;;
  whoami) echo alice;;
  trust) echo '[]';;
  *) echo "unexpected npm call" >&2; exit 9;;
esac"#;
const CARGO_OK: &str = "echo 'cargo 1.97.0'";

struct Env {
    repo: TempDir,
    /// Stub `npm` and `cargo` (and their `.log` files).
    tools: TempDir,
}

/// The `publish` fixture on `main`, committed: `@acme/lib@1.0.0` (npm) and `tool@0.3.0`
/// (crate), with the tag `tool@0.3.0` when `tagged`.
fn setup(npm: &str, tagged: bool) -> Env {
    let repo = fixture_repo("publish", REMOTE);
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    if tagged {
        git(d, &["tag", TAG]);
    }
    let tools = tempfile::tempdir().unwrap();
    stub(tools.path(), "npm", npm);
    stub(tools.path(), "cargo", CARGO_OK);
    Env { repo, tools }
}

/// `release bootstrap --json <args>` outside Actions, stdin not a terminal.
fn bootstrap(env: &Env, api: &FakeApi, args: &[&str]) -> assert_cmd::Command {
    let mut c = hoist();
    c.arg("-C")
        .arg(env.repo.path())
        .args(["release", "bootstrap", "--json"])
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM", env.tools.path().join("npm"))
        .env("HOIST_CARGO", env.tools.path().join("cargo"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .write_stdin("");
    c
}

fn tool_log(env: &Env, name: &str) -> Vec<String> {
    std::fs::read_to_string(env.tools.path().join(format!("{name}.log")))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
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

fn human_step<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["human_steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no human step {id} in {v:#}"))
}

/// [`NPM_OK`] whose `trust list` shows our trusted publisher.
fn trusted_npm() -> String {
    NPM_OK.replace(
        "trust) echo '[]';;",
        r#"trust) echo '[{"type":"github","claims":{"repository":"acme/x","workflow_ref":{"file":"release.yml"},"environment":"release"}}]';;"#,
    )
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn no_publish(env: &Env) {
    for tool in ["npm", "cargo"] {
        let log = tool_log(env, tool);
        assert!(
            !log.iter().any(|l| l.starts_with("publish")),
            "{tool}: {log:?}"
        );
    }
}

/// Review Focus 4: inside GitHub Actions, exit 3 before any request or tool call.
#[test]
fn refuses_in_actions() {
    let env = setup(NPM_OK, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"])
        .env("GITHUB_ACTIONS", "true")
        .assert()
        .code(3);
    let err = stderr(o.get_output());
    assert!(err.contains("release publish"), "{err}");
    assert!(err.contains("own logins"), "{err}");
    assert!(api.requests().is_empty(), "{:?}", api.requests());
    assert!(tool_log(&env, "npm").is_empty());
    assert!(tool_log(&env, "cargo").is_empty());
}

#[test]
fn dry_run_lists_missing_packages() {
    let env = setup(NPM_OK, true);
    // Both packages answer 404 (the fake's default).
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(0);
    let v = json(o.get_output());
    assert_eq!(v["command"], "release bootstrap");
    let npm = item(&v, "bootstrap_npm", "@acme/lib");
    assert_eq!(npm["action"], "create", "{npm}");
    assert_eq!(npm["area"], "release");
    assert_eq!(npm["source"], "packages/lib/package.json:3");
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "create", "{krate}");
    assert!(krate["reason"].as_str().unwrap().contains(TAG), "{krate}");
    assert_eq!(krate["source"], "crates/tool/Cargo.toml:3");
    // R4: without a terminal npm trust is a human step; crate trust is unreadable.
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "pending", "{t}");
    let s = human_step(&v, "npm-trust:@acme/lib");
    assert_eq!(
        s["title"],
        "npm trust github @acme/lib --file release.yml --repo acme/x --env release --allow-publish"
    );
    let t = item(&v, "bootstrap_trust", "tool");
    assert_eq!(t["action"], "unknown", "{t}");
    let s = human_step(&v, "crates-trust:tool");
    assert_eq!(s["url"], "https://crates.io/crates/tool/settings");
    api.assert_only_get();
    let paths: Vec<String> = api.requests().iter().map(|r| r.path.clone()).collect();
    assert!(paths.contains(&"/@acme%2Flib".to_string()), "{paths:?}");
    assert!(
        paths.contains(&"/api/v1/crates/tool".to_string()),
        "{paths:?}"
    );
    let npm_log = tool_log(&env, "npm");
    assert!(!npm_log.is_empty());
    for line in &npm_log {
        assert!(
            ["whoami ", "--version ", "trust list "]
                .iter()
                .any(|p| line.starts_with(p)),
            "not read-only: {line}"
        );
    }
    let cargo_log = tool_log(&env, "cargo");
    assert!(!cargo_log.is_empty());
    assert!(
        cargo_log.iter().all(|l| l.starts_with("--version ")),
        "{cargo_log:?}"
    );
    no_publish(&env);

    // On a terminal, npm trust is a planned write.
    let o = bootstrap(&env, &api, &["--dry-run"])
        .env("HOIST_ASSUME_TTY", "1")
        .assert()
        .code(0);
    let v = json(o.get_output());
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "create", "{t}");
}

#[test]
fn existing_packages_are_ok_and_untouched() {
    let env = setup(NPM_OK, true);
    let routes: [RouteSpec; 2] = [(LIB_PKG, 200, "{}", &[]), (TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(0);
    let v = json(o.get_output());
    let npm = item(&v, "bootstrap_npm", "@acme/lib");
    assert_eq!(npm["action"], "ok", "{npm}");
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "ok", "{krate}");
    // The existing package's trusted publisher is read.
    let log = tool_log(&env, "npm");
    assert!(
        log.iter()
            .any(|l| l.starts_with("trust list @acme/lib --json")),
        "{log:?}"
    );
    // Nothing is missing: cargo is not needed.
    assert!(tool_log(&env, "cargo").is_empty());

    // R4: only human steps remain (npm trust without a terminal, crate trust): no prompt,
    // exit 2, with or without --yes.
    for args in [&[][..], &["--yes"][..]] {
        let o = bootstrap(&env, &api, args).assert().code(2);
        let v = json(o.get_output());
        assert_eq!(item(&v, "bootstrap_npm", "@acme/lib")["action"], "ok");
        assert_eq!(item(&v, "bootstrap_crate", "tool")["action"], "ok");
        assert_eq!(item(&v, "bootstrap_trust", "tool")["action"], "unknown");
        human_step(&v, "npm-trust:@acme/lib");
        human_step(&v, "crates-trust:tool");
    }
    api.assert_only_get();
    no_publish(&env);
}

/// R4: everything exists and npm trust is registered: crate trust alone never needs `--yes`
/// and never sets the exit code.
#[test]
fn everything_in_place_exits_0_without_yes() {
    let env = setup(&trusted_npm(), true);
    let routes: [RouteSpec; 2] = [(LIB_PKG, 200, "{}", &[]), (TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    for args in [&[][..], &["--yes"][..]] {
        let o = bootstrap(&env, &api, args).assert().code(0);
        let v = json(o.get_output());
        assert_eq!(item(&v, "bootstrap_trust", "@acme/lib")["action"], "ok");
        assert_eq!(item(&v, "bootstrap_trust", "tool")["action"], "unknown");
        human_step(&v, "crates-trust:tool");
    }
    no_publish(&env);
}

/// R3: a terminal (simulated) and the answer `n`: exit 1, nothing published.
#[test]
fn declining_on_a_terminal_exits_1() {
    let env = setup(NPM_OK, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("n\n")
        .assert()
        .code(1);
    let v = json(o.get_output());
    let planned: Vec<&Value> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == "create")
        .collect();
    assert_eq!(planned.len(), 3, "{v:#}");
    for i in planned {
        assert_eq!(i["reason"], "not applied: declined", "{i}");
    }
    let err = stderr(o.get_output());
    assert!(
        err.contains(
            "hoist release bootstrap will publish 2 package(s) and register 1 trusted publisher(s):"
        ),
        "{err}"
    );
    api.assert_only_get();
    no_publish(&env);
}

#[test]
fn already_trusted_npm_package_is_ok() {
    let env = setup(&trusted_npm(), true);
    let routes: [RouteSpec; 1] = [(LIB_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(0);
    let v = json(o.get_output());
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "ok", "{t}");
    assert_eq!(t["reason"], "already trusted");
}

#[test]
fn crate_without_tag_is_pending() {
    let env = setup(NPM_OK, false);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(0);
    let v = json(o.get_output());
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "pending", "{krate}");
    let reason = krate["reason"].as_str().unwrap();
    assert!(
        reason.contains("run the release first; bootstrap publishes the tagged version"),
        "{reason}"
    );
    assert!(reason.contains(TAG), "{reason}");
    let t = item(&v, "bootstrap_trust", "tool");
    assert_eq!(t["action"], "pending", "{t}");
    // No crate to publish: cargo is not needed.
    assert!(tool_log(&env, "cargo").is_empty());
}

#[test]
fn npm_not_logged_in_exits_3() {
    let npm = NPM_OK.replace(
        "whoami) echo alice;;",
        "whoami) echo 'ENEEDAUTH' >&2; exit 1;;",
    );
    let env = setup(&npm, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(3);
    let err = stderr(o.get_output());
    assert!(err.contains("npm login"), "{err}");
    no_publish(&env);
}

/// Review Focus 3: npm older than 11.15.0 has no `npm trust`.
#[test]
fn old_npm_exits_3_when_trust_needed() {
    let npm = NPM_OK.replace("--version) echo 11.15.0;;", "--version) echo 11.14.0;;");
    let env = setup(&npm, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(3);
    let err = stderr(o.get_output());
    assert!(err.contains("11.15.0"), "{err}");
    assert!(err.contains("11.14.0"), "{err}");
    no_publish(&env);
}

#[test]
fn missing_cargo_exits_3() {
    let env = setup(NPM_OK, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"])
        .env("HOIST_CARGO", env.tools.path().join("no-such-cargo"))
        .assert()
        .code(3);
    let err = stderr(o.get_output());
    assert!(err.contains("cargo"), "{err}");
    assert!(err.contains("HOIST_CARGO"), "{err}");
}

#[test]
fn non_tty_without_yes_exits_3() {
    let env = setup(NPM_OK, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &[]).assert().code(3);
    let err = stderr(o.get_output());
    assert!(err.contains("use --yes in non-interactive runs"), "{err}");
    api.assert_only_get();
    no_publish(&env);
}

// ---- Task 2: the writes ----

/// [`NPM_OK`] that also publishes: appends the `name` line of the published package.json to
/// `<tools>/npm.published`; `fail_on` (a substring of package.json) makes that publish fail.
fn npm_publishing(tools: &std::path::Path, fail_on: Option<&str>) -> String {
    let fail = fail_on.map_or(String::new(), |s| {
        format!("if grep -q '{s}' package.json; then echo 'E403 forbidden' >&2; exit 1; fi; ")
    });
    NPM_OK.replace(
        "trust) echo '[]';;",
        &format!(
            "trust) echo '[]';;\n  publish) {fail}grep '\"name\"' package.json >> '{}';;",
            tools.join("npm.published").display()
        ),
    )
}

/// cargo that records, on `publish`, `git rev-parse HEAD` and `git status --porcelain` of its
/// working directory in `<tools>/cargo.head`; `fail` makes `publish` exit 101.
fn cargo_publishing(tools: &std::path::Path, fail: bool) -> String {
    let fail = if fail {
        "echo 'error: failed to publish' >&2; exit 101"
    } else {
        "true"
    };
    format!(
        r#"case "$1" in
  --version) echo 'cargo 1.97.0';;
  publish) git rev-parse HEAD > '{h}'; git status --porcelain >> '{h}'; {fail};;
  *) echo "unexpected cargo call" >&2; exit 9;;
esac"#,
        h = tools.join("cargo.head").display()
    )
}

/// `fixture` on `main`, committed; stub npm (publishing, failing on `npm_fail`) and cargo.
fn setup_writes(fixture: &str, tag: Option<&str>, npm_fail: Option<&str>, cargo_fail: bool) -> Env {
    let repo = fixture_repo(fixture, REMOTE);
    let d = repo.path();
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    if let Some(tag) = tag {
        git(d, &["tag", tag]);
    }
    let tools = tempfile::tempdir().unwrap();
    stub(tools.path(), "npm", &npm_publishing(tools.path(), npm_fail));
    stub(
        tools.path(),
        "cargo",
        &cargo_publishing(tools.path(), cargo_fail),
    );
    Env { repo, tools }
}

fn read(path: std::path::PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn published(env: &Env) -> Vec<String> {
    read(env.tools.path().join("npm.published"))
        .lines()
        .map(|l| l.trim().trim_end_matches(',').to_string())
        .collect()
}

fn field<'a>(line: &'a str, name: &str) -> &'a str {
    let start = line
        .find(&format!(" {name}:"))
        .unwrap_or_else(|| panic!("no {name}: in {line}"))
        + name.len()
        + 2;
    line[start..].split(' ').next().unwrap()
}

fn worktrees(dir: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .map(str::to_string)
        .collect()
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Review Focus 5: every platform package and the launcher get a placeholder published from
/// a temp dir holding only package.json and README.md; nothing from `npm/` is published.
#[test]
fn npm_placeholder_published_from_temp_dir() {
    let env = setup_writes("release-launcher", None, None, false);
    // The crate exists: only npm placeholders are written.
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"]).assert();
    let v = json(o.get_output());
    // npm trust without a terminal is a human step: exit 2.
    assert_eq!(v["exit_code"], 2, "{v:#}\n{}", stderr(o.get_output()));
    let names = [
        "@acme/tool-linux-x64",
        "@acme/tool-darwin-arm64",
        "@acme/tool",
    ];
    for name in names {
        let i = item(&v, "bootstrap_npm", name);
        assert_eq!(i["action"], "ok", "{i}");
        assert!(i["reason"].as_str().unwrap().contains("published"), "{i}");
    }
    // Platforms first, then the launcher, each once.
    assert_eq!(
        published(&env),
        names.map(|n| format!("\"name\": \"{n}\"")).to_vec()
    );
    let publishes: Vec<String> = tool_log(&env, "npm")
        .into_iter()
        .filter(|l| l.starts_with("publish"))
        .collect();
    assert_eq!(publishes.len(), 3, "{publishes:?}");
    let repo = env.repo.path().canonicalize().unwrap();
    for line in &publishes {
        assert!(line.starts_with("publish --access public "), "{line}");
        let pwd = std::path::PathBuf::from(field(line, "pwd"));
        assert!(!pwd.starts_with(&repo), "{line}");
        assert!(!pwd.starts_with(env.repo.path()), "{line}");
        assert!(!pwd.exists(), "temp dir left behind: {line}");
        let mut files: Vec<&str> = field(line, "files")
            .split(',')
            .filter(|f| !f.is_empty())
            .collect();
        files.sort_unstable();
        assert_eq!(files, ["README.md", "package.json"], "{line}");
    }
    assert_eq!(git_out(env.repo.path(), &["status", "--porcelain"]), "");
    assert!(tool_log(&env, "cargo").is_empty());
}

/// Review Focus 2: the crate is published from a worktree at its tag, never from the working
/// tree (a later commit and an uncommitted edit), and the worktree is removed.
#[test]
fn crate_published_from_worktree_at_tag() {
    let env = setup_writes("publish", Some(TAG), None, false);
    let d = env.repo.path();
    std::fs::create_dir_all(d.join("crates/tool/src")).unwrap();
    std::fs::write(d.join("crates/tool/src/lib.rs"), "// later\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "feat: later"]);
    let head_before = git_out(d, &["rev-parse", "HEAD"]);
    std::fs::write(d.join("crates/tool/src/lib.rs"), "// uncommitted\n").unwrap();
    stub(env.tools.path(), "npm", &trusted_npm());
    let routes: [RouteSpec; 1] = [(LIB_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"]).assert().code(0);
    let v = json(o.get_output());
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "ok", "{krate}");
    assert!(
        krate["reason"].as_str().unwrap().contains("published"),
        "{krate}"
    );
    let publishes: Vec<String> = tool_log(&env, "cargo")
        .into_iter()
        .filter(|l| l.starts_with("publish"))
        .collect();
    assert_eq!(publishes.len(), 1, "{publishes:?}");
    assert!(
        publishes[0].starts_with("publish -p tool --locked "),
        "{}",
        publishes[0]
    );
    let pwd = std::path::PathBuf::from(field(&publishes[0], "pwd"));
    assert!(!pwd.starts_with(d), "{}", publishes[0]);
    assert!(!pwd.exists(), "worktree left behind: {}", publishes[0]);
    let head = read(env.tools.path().join("cargo.head"));
    let tag_commit = git_out(d, &["rev-parse", &format!("{TAG}^{{commit}}")]);
    assert_eq!(head.trim(), tag_commit, "HEAD and status in the worktree");
    assert_eq!(worktrees(d).len(), 1, "{:?}", worktrees(d));
    // The main checkout is untouched: same commit, same branch.
    assert_eq!(git_out(d, &["rev-parse", "HEAD"]), head_before);
    assert_ne!(head_before, tag_commit);
    assert_eq!(git_out(d, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert_eq!(
        std::fs::read_to_string(d.join("crates/tool/src/lib.rs")).unwrap(),
        "// uncommitted\n"
    );
    api.assert_only_get();
}

/// Review Focus 1: run 1 publishes the placeholder and fails at cargo; run 2 skips the
/// published placeholder (registry 200), publishes only the crate and is not blocked by a
/// dead worktree.
#[test]
fn rerun_skips_done_and_recovers_worktree() {
    let env = setup_writes("publish", Some(TAG), None, true);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--yes"]).assert().code(1);
    let v = json(o.get_output());
    assert_eq!(item(&v, "bootstrap_npm", "@acme/lib")["action"], "ok");
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "create", "{krate}");
    assert!(
        krate["reason"].as_str().unwrap().starts_with("failed: "),
        "{krate}"
    );
    assert_eq!(published(&env).len(), 1);
    let d = env.repo.path();
    assert_eq!(worktrees(d).len(), 1, "{:?}", worktrees(d));

    // An interrupted run left a dead worktree behind.
    let dead = tempfile::tempdir().unwrap();
    let dead_path = dead.path().join("wt");
    git(
        d,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            dead_path.to_str().unwrap(),
            TAG,
        ],
    );
    std::fs::remove_dir_all(&dead_path).unwrap();
    assert_eq!(worktrees(d).len(), 2);

    stub(
        env.tools.path(),
        "cargo",
        &cargo_publishing(env.tools.path(), false),
    );
    let routes: [RouteSpec; 1] = [(LIB_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"]).assert();
    let v = json(o.get_output());
    // npm trust is still a human step (no terminal).
    assert_eq!(v["exit_code"], 2, "{v:#}\n{}", stderr(o.get_output()));
    let npm = item(&v, "bootstrap_npm", "@acme/lib");
    assert_eq!(npm["action"], "ok", "{npm}");
    assert!(
        !npm["reason"].as_str().unwrap().contains("published"),
        "{npm}"
    );
    let krate = item(&v, "bootstrap_crate", "tool");
    assert_eq!(krate["action"], "ok", "{krate}");
    assert_eq!(published(&env).len(), 1, "no second placeholder");
    let cargo_publishes = tool_log(&env, "cargo")
        .iter()
        .filter(|l| l.starts_with("publish"))
        .count();
    assert_eq!(cargo_publishes, 2);
    assert_eq!(worktrees(d).len(), 1, "{:?}", worktrees(d));
}

/// The second placeholder fails: exit 1 with done / failed / not attempted, no crate publish.
#[test]
fn npm_publish_failure_stops_the_run() {
    let env = setup_writes(
        "release-launcher",
        Some(TAG),
        Some("tool-darwin-arm64"),
        false,
    );
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--yes"]).assert().code(1);
    let v = json(o.get_output());
    let linux = item(&v, "bootstrap_npm", "@acme/tool-linux-x64");
    assert_eq!(linux["action"], "ok", "{linux}");
    let darwin = item(&v, "bootstrap_npm", "@acme/tool-darwin-arm64");
    assert_eq!(darwin["action"], "create", "{darwin}");
    assert!(
        darwin["reason"].as_str().unwrap().starts_with("failed: "),
        "{darwin}"
    );
    for (kind, key) in [("bootstrap_npm", "@acme/tool"), ("bootstrap_crate", "tool")] {
        let i = item(&v, kind, key);
        assert_eq!(i["action"], "create", "{i}");
        assert_eq!(i["reason"], "not attempted", "{i}");
    }
    assert_eq!(published(&env), ["\"name\": \"@acme/tool-linux-x64\""]);
    assert!(
        !tool_log(&env, "cargo")
            .iter()
            .any(|l| l.starts_with("publish")),
        "{:?}",
        tool_log(&env, "cargo")
    );
    api.assert_only_get();
}

// ---- Fix round 1 (R5): interactive publishes, trust after a failure ----

/// R5: `npm publish` output is streamed to hoist's stderr (an OTP prompt or auth link reaches
/// the person) and its stdin is the terminal's: after the `y` for the confirmation, the stub
/// reads the OTP line. stdout stays one JSON object.
#[test]
fn npm_publish_is_interactive() {
    let env = setup_writes("publish", Some(TAG), None, false);
    let otp = env.tools.path().join("npm.otp");
    let npm = NPM_OK.replace(
        "trust) echo '[]';;",
        &format!(
            "trust) echo '[]';;\n  publish) echo 'MARKER-enter-OTP'; read line; echo \"$line\" > '{}';;",
            otp.display()
        ),
    );
    stub(env.tools.path(), "npm", &npm);
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n123456\n")
        .assert();
    let out = o.get_output();
    let v = json(out);
    assert_eq!(
        item(&v, "bootstrap_npm", "@acme/lib")["action"],
        "ok",
        "{v:#}\n{}",
        stderr(out)
    );
    assert!(stderr(out).contains("MARKER-enter-OTP"), "{}", stderr(out));
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("MARKER"),
        "stdout must stay JSON"
    );
    assert_eq!(read(otp).trim(), "123456");
}

/// R5: `cargo publish` output is streamed to hoist's stderr too.
#[test]
fn cargo_publish_output_reaches_stderr() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(env.tools.path(), "npm", &trusted_npm());
    stub(
        env.tools.path(),
        "cargo",
        r#"case "$1" in --version) echo 'cargo 1.97.0';; publish) echo 'MARKER-cargo-uploading';; esac"#,
    );
    let routes: [RouteSpec; 1] = [(LIB_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"]).assert().code(0);
    let out = o.get_output();
    json(out);
    assert!(
        stderr(out).contains("MARKER-cargo-uploading"),
        "{}",
        stderr(out)
    );
}

/// After a failed write, planned npm trust steps read `not attempted` too.
#[test]
fn trust_after_a_failure_is_not_attempted() {
    let env = setup_writes("publish", Some(TAG), Some("@acme/lib"), false);
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n")
        .assert()
        .code(1);
    let v = json(o.get_output());
    let npm = item(&v, "bootstrap_npm", "@acme/lib");
    assert!(
        npm["reason"].as_str().unwrap().starts_with("failed: "),
        "{npm}"
    );
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "create", "{t}");
    assert_eq!(t["reason"], "not attempted", "{t}");
}

// ---- Task 3: trusted publishers, hint ----

/// `release bootstrap <args>` in table mode (no `--json`), like [`bootstrap`].
fn bootstrap_table(env: &Env, api: &FakeApi, args: &[&str]) -> assert_cmd::Command {
    let mut c = hoist();
    c.arg("-C")
        .arg(env.repo.path())
        .args(["release", "bootstrap"])
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM", env.tools.path().join("npm"))
        .env("HOIST_CARGO", env.tools.path().join("cargo"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .write_stdin("");
    c
}

const TRUSTED_LIST: &str = r#"[{"type":"github","claims":{"repository":"acme/x","workflow_ref":{"file":"release.yml"},"environment":"release"}}]"#;

/// npm 11.15.0, logged in, publishing. `trust list` shows our publisher once `trust github`
/// ran (or always when `trusted`); `trust github` reads one stdin line (the OTP) into
/// `<tools>/npm.otp`, exiting `github_exit`.
fn npm_trusting(tools: &std::path::Path, trusted: bool, github_exit: i32) -> String {
    let mark = tools.join("npm.trusted");
    let otp = tools.join("npm.otp");
    let list = if trusted {
        format!("echo '{TRUSTED_LIST}'")
    } else {
        format!(
            "if [ -f '{m}' ]; then echo '{TRUSTED_LIST}'; else echo '[]'; fi",
            m = mark.display()
        )
    };
    let github = if github_exit == 0 {
        format!("touch '{}'", mark.display())
    } else {
        format!("echo 'E403 forbidden' >&2; exit {github_exit}")
    };
    format!(
        r#"case "$1 $2" in
  --version*) echo 11.15.0;;
  whoami*) echo alice;;
  "trust list") {list};;
  "trust github") echo 'MARKER-trust-otp'; read line; echo "$line" > '{otp}'; {github};;
  publish*) true;;
  *) echo "unexpected npm call" >&2; exit 9;;
esac"#,
        otp = otp.display()
    )
}

fn trust_lines(env: &Env) -> Vec<String> {
    tool_log(env, "npm")
        .into_iter()
        .filter(|l| l.starts_with("trust "))
        .map(|l| l.split(" env:").next().unwrap().to_string())
        .collect()
}

const TRUST_GITHUB: &str =
    "trust github @acme/lib --file release.yml --repo acme/x --env release --allow-publish";

/// On a terminal: the placeholder is published, then `npm trust list`, `npm trust github`
/// (stdin inherited: it reads the OTP after the confirmation's `y`) and the verifying
/// `npm trust list`. Exit 0 with the final hint on stderr.
#[test]
fn trust_runs_interactively_on_tty() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 0),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap_table(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n654321\n")
        .assert();
    let out = o.get_output();
    let err = stderr(out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{err}\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        trust_lines(&env),
        [
            "trust list @acme/lib --json".to_string(),
            TRUST_GITHUB.to_string(),
            "trust list @acme/lib --json".to_string(),
        ]
    );
    assert!(err.contains("MARKER-trust-otp"), "{err}");
    assert_eq!(read(env.tools.path().join("npm.otp")).trim(), "654321");
    assert!(
        err.contains("next: re-run the failed \"release publish\" job at "),
        "{err}"
    );
    assert!(err.contains("or run the release workflow"), "{err}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("next:"),
        "the hint goes to stderr"
    );

    // The same run in JSON: the trust item is done, stdout is one object without the hint.
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 0),
    );
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n654321\n")
        .assert()
        .code(0);
    let out = o.get_output();
    let v = json(out);
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "ok", "{t}");
    assert!(
        t["reason"].as_str().unwrap().starts_with("registered"),
        "{t}"
    );
    assert!(!stderr(out).contains("next:"), "{}", stderr(out));
    assert!(v.get("hint").is_none() && v.get("text").is_none(), "{v:#}");
}

/// The trusted publisher appears in `npm trust list` before the registration: `ok`
/// "already trusted", no `trust github` call.
#[test]
fn trust_skipped_when_listed() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), true, 0),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n")
        .assert()
        .code(0);
    let v = json(o.get_output());
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "ok", "{t}");
    assert_eq!(t["reason"], "already trusted", "{t}");
    assert_eq!(trust_lines(&env), ["trust list @acme/lib --json"]);
}

/// Without a terminal (`--yes`, stdin not a terminal) the placeholder is published and npm
/// trust stays a human step: exit 2.
#[test]
fn trust_without_tty_is_a_human_step() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 0),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"])
        .pipe_stdin("/dev/null")
        .unwrap()
        .assert()
        .code(2);
    let v = json(o.get_output());
    assert_eq!(item(&v, "bootstrap_npm", "@acme/lib")["action"], "ok");
    assert_eq!(
        item(&v, "bootstrap_trust", "@acme/lib")["action"],
        "pending"
    );
    let s = human_step(&v, "npm-trust:@acme/lib");
    assert_eq!(s["title"], format!("npm {TRUST_GITHUB}"));
    assert!(
        !trust_lines(&env)
            .iter()
            .any(|l| l.starts_with("trust github")),
        "{:?}",
        trust_lines(&env)
    );
}

/// A failed `npm trust github` is a failed item: exit 1, and the run stops there.
#[test]
fn failed_trust_registration_exits_1() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 1),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n1\n")
        .assert()
        .code(1);
    let v = json(o.get_output());
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "create", "{t}");
    assert!(t["reason"].as_str().unwrap().starts_with("failed: "), "{t}");
    // No verifying `trust list` after the failure.
    assert_eq!(
        trust_lines(&env),
        [
            "trust list @acme/lib --json".to_string(),
            TRUST_GITHUB.to_string()
        ]
    );
}

/// `npm trust github` exits 0 but `npm trust list` still lacks the entry: failed, exit 1.
#[test]
fn unverified_trust_registration_exits_1() {
    let env = setup_writes("publish", Some(TAG), None, false);
    let npm = npm_trusting(env.tools.path(), false, 0).replace("touch '", "true '");
    stub(env.tools.path(), "npm", &npm);
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n1\n")
        .assert()
        .code(1);
    let v = json(o.get_output());
    let t = item(&v, "bootstrap_trust", "@acme/lib");
    assert_eq!(t["action"], "create", "{t}");
    let reason = t["reason"].as_str().unwrap();
    assert!(reason.starts_with("failed: "), "{t}");
    assert!(reason.contains("npm trust list"), "{t}");
}

/// The single-crate fixture named `keel-hoist`, tagged `v0.1.0`; stub npm unused.
fn setup_single() -> Env {
    let repo = fixture_repo("release-single", REMOTE);
    let d = repo.path();
    std::fs::write(
        d.join("Cargo.toml"),
        "[package]\nname = \"keel-hoist\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    git(d, &["tag", "v0.1.0"]);
    let tools = tempfile::tempdir().unwrap();
    stub(tools.path(), "npm", NPM_OK);
    stub(
        tools.path(),
        "cargo",
        &cargo_publishing(tools.path(), false),
    );
    Env { repo, tools }
}

#[test]
fn crates_trust_is_a_link() {
    let env = setup_single();
    let api = FakeApi::start(&[]);
    let o = bootstrap(&env, &api, &["--dry-run"]).assert().code(0);
    let v = json(o.get_output());
    let s = human_step(&v, "crates-trust:keel-hoist");
    assert_eq!(s["url"], "https://crates.io/crates/keel-hoist/settings");
    assert_eq!(
        item(&v, "bootstrap_trust", "keel-hoist")["action"],
        "unknown"
    );
}

/// `release bootstrap <args>` in table mode with stdout and stderr in one file, so the test
/// sees their order; `tty` sets the R3 seam. Returns the exit code and the combined output.
fn bootstrap_merged(
    env: &Env,
    api: &FakeApi,
    args: &[&str],
    tty: bool,
    stdin: &str,
) -> (Option<i32>, String) {
    let out_path = env.tools.path().join("merged.out");
    let in_path = env.tools.path().join("merged.in");
    std::fs::write(&in_path, stdin).unwrap();
    let out = std::fs::File::create(&out_path).unwrap();
    let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("hoist"));
    for name in common::SCRUBBED_ENV {
        c.env_remove(name);
    }
    c.arg("-C")
        .arg(env.repo.path())
        .args(["release", "bootstrap"])
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .env("GH_CONFIG_DIR", "/nonexistent-hoist-gh")
        .env("HOIST_GITHUB_API", "http://127.0.0.1:9")
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .env("HOIST_NPM", env.tools.path().join("npm"))
        .env("HOIST_CARGO", env.tools.path().join("cargo"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::fs::File::open(&in_path).unwrap())
        .stdout(out.try_clone().unwrap())
        .stderr(out);
    if tty {
        c.env("HOIST_ASSUME_TTY", "1");
    } else {
        c.env_remove("HOIST_ASSUME_TTY");
    }
    let status = c.status().unwrap();
    (status.code(), read(out_path))
}

/// R6: the hint is the last thing printed, after the table and its human steps.
fn assert_hint_last(out: &str, hint: &str) {
    let table = out
        .find("RESOURCE")
        .unwrap_or_else(|| panic!("no table in {out}"));
    let at = out
        .find(hint)
        .unwrap_or_else(|| panic!("no hint {hint:?} in {out}"));
    assert!(table < at, "hint before the table:\n{out}");
    if let Some(steps) = out.find("human steps:") {
        assert!(steps < at, "hint before the human steps:\n{out}");
    }
    assert_eq!(out[at..].trim_end(), hint, "hint is not last:\n{out}");
}

const HINT_FINISH: &str = "next: finish the steps above, then run bootstrap again";

#[test]
fn hint_after_success() {
    let env = setup_single();
    let api = FakeApi::start(&[]);
    let o = bootstrap_table(&env, &api, &["--yes"]).assert().code(0);
    let err = stderr(o.get_output());
    let hint =
        "next: re-run the failed \"release publish\" job at v0.1.0, or run the release workflow";
    assert!(err.contains(hint), "{err}");
    assert!(
        !String::from_utf8_lossy(&o.get_output().stdout).contains("next:"),
        "the hint goes to stderr"
    );
    assert!(
        tool_log(&env, "cargo")
            .iter()
            .any(|l| l.starts_with("publish -p keel-hoist"))
    );
    // Re-run (crate exists now): the hint follows the table.
    let routes: [RouteSpec; 1] = [("GET /api/v1/crates/keel-hoist", 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let (code, out) = bootstrap_merged(&env, &api, &["--yes"], false, "");
    assert_eq!(code, Some(0), "{out}");
    assert_hint_last(&out, hint);
}

/// A run that leaves a publish undone ends with the other hint, after the table.
#[test]
fn hint_after_failure() {
    let env = setup_writes("publish", Some(TAG), Some("@acme/lib"), false);
    let api = FakeApi::start(&[]);
    let (code, out) = bootstrap_merged(&env, &api, &["--yes"], false, "");
    assert_eq!(code, Some(1), "{out}");
    assert_hint_last(&out, HINT_FINISH);
    assert!(!out.contains("re-run the failed"), "{out}");
}

/// R7: placeholders published but npm trust pending (no terminal, exit 2): the CI publish
/// would fail without the trusted publisher, so the hint says to finish first.
#[test]
fn pending_npm_trust_hint_says_finish() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 0),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let (code, out) = bootstrap_merged(&env, &api, &["--yes"], false, "");
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("npm-trust:@acme/lib"), "{out}");
    assert_hint_last(&out, HINT_FINISH);
    assert!(!out.contains("re-run the failed"), "{out}");
}

/// A failed `npm trust github` on the first of several packages stops the run: the other
/// trust steps are `not attempted` and npm is never asked to register them.
#[test]
fn trust_failure_stops_later_trust_steps() {
    let env = setup_writes("release-launcher", Some(TAG), None, false);
    stub(
        env.tools.path(),
        "npm",
        &npm_trusting(env.tools.path(), false, 1),
    );
    let routes: [RouteSpec; 1] = [(TOOL_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &[])
        .env("HOIST_ASSUME_TTY", "1")
        .write_stdin("y\n1\n")
        .assert()
        .code(1);
    let v = json(o.get_output());
    let trust: Vec<&Value> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "bootstrap_trust" && i["key"] != "tool")
        .collect();
    assert_eq!(trust.len(), 3, "{v:#}");
    assert!(
        trust[0]["reason"].as_str().unwrap().starts_with("failed: "),
        "{}",
        trust[0]
    );
    for t in &trust[1..] {
        assert_eq!(t["action"], "create", "{t}");
        assert_eq!(t["reason"], "not attempted", "{t}");
    }
    let github: Vec<String> = trust_lines(&env)
        .into_iter()
        .filter(|l| l.starts_with("trust github"))
        .collect();
    assert_eq!(github.len(), 1, "{github:?}");
    assert!(
        github[0].starts_with(&format!(
            "trust github {} ",
            trust[0]["key"].as_str().unwrap()
        )),
        "{github:?}"
    );
}

/// A worktree left by an interrupted run whose directory still exists (Ctrl-C during
/// `cargo publish`) is removed before the new one is added; other worktrees stay.
#[test]
fn stale_worktree_of_an_interrupted_run_is_removed() {
    let env = setup_writes("publish", Some(TAG), None, false);
    stub(env.tools.path(), "npm", &trusted_npm());
    let d = env.repo.path();
    let tmp = tempfile::tempdir().unwrap();
    let stale = tmp.path().join("hoist-bootstrap-tool-4242");
    let other = tmp.path().join("hoist-bootstrap-tool-x-4242");
    for wt in [&stale, &other] {
        git(
            d,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().unwrap(),
                TAG,
            ],
        );
    }
    assert_eq!(worktrees(d).len(), 3);
    let routes: [RouteSpec; 1] = [(LIB_PKG, 200, "{}", &[])];
    let api = FakeApi::start(&routes);
    let o = bootstrap(&env, &api, &["--yes"])
        .env("TMPDIR", tmp.path())
        .assert()
        .code(0);
    let v = json(o.get_output());
    assert_eq!(item(&v, "bootstrap_crate", "tool")["action"], "ok", "{v:#}");
    assert!(!stale.exists(), "stale worktree directory left behind");
    let left = worktrees(d);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.iter()
            .any(|l| l.ends_with("hoist-bootstrap-tool-x-4242")),
        "another crate's worktree was removed: {left:?}"
    );
}
