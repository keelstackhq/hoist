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
        err.contains("hoist release bootstrap will publish 2 package(s):"),
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
