//! The GitHub token falls back to `gh auth token` (spec §8.1): the token is read from the
//! child's stdout, never put on a command line, and never printed.
#![cfg(unix)]
mod common;

use common::{FakeApi, RouteSpec, fixture_repo, hoist, write_script};
use serde_json::Value;
use std::path::Path;

const SITE: &str = "https://github.com/acme/x.git";
const TOKEN: &str = "ghp_hoist-canary-from-gh-9f3a";

/// A fake `gh` in `dir`: it logs its arguments to `gh.log` and prints the token only for
/// exactly `auth token`; any other argument list exits 64 with nothing on stdout.
fn fake_gh(dir: &Path, token_script: &str) {
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\nif [ \"$#\" -ne 2 ] || [ \"$1\" != auth ] || [ \"$2\" != token ]; then exit 64; fi\n{token_script}\n",
        log = dir.join("gh.log").display()
    );
    write_script(&dir.join("gh"), body.as_bytes(), 0o755);
}

fn api() -> FakeApi {
    let routes: [RouteSpec; 4] = [
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_page2.json",
            &[],
        ),
        (
            "GET /repos/acme/x/environments",
            200,
            "github/environments_empty.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            "github/actions_permissions.json",
            &[],
        ),
    ];
    FakeApi::start(&routes)
}

fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn gh_auth_token_is_used_when_no_env_token_is_set() {
    let repo = fixture_repo("bare", SITE);
    let bin = tempfile::tempdir().unwrap();
    fake_gh(bin.path(), &format!("printf '%s\\n' '{TOKEN}'"));
    let api = api();
    let out = hoist()
        .current_dir(repo.path())
        .env("PATH", path_with(bin.path()))
        .env("HOIST_GITHUB_API", api.url())
        .args(["auth", "check", "--json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let token = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "token" && i["area"] == "github")
        .expect("github token item");
    assert_eq!(token["action"], "ok", "{token}");
    assert_eq!(token["source"], "gh auth token", "{token}");
    assert_eq!(token["reason"], "gh auth token", "{token}");
    assert!(
        !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
        "token leaked"
    );
    // The token reached the API as a bearer credential only.
    let sent = api.requests();
    assert!(!sent.is_empty());
    assert!(
        sent.iter()
            .all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}")[..])),
        "{sent:?}"
    );
    // `gh` was called with exactly `auth token` (the token is read from stdout, not argv),
    // and its output never appears in an argument list.
    let log = std::fs::read_to_string(bin.path().join("gh.log")).unwrap();
    assert!(log.lines().all(|l| l == "auth token"), "{log}");
    assert!(!log.contains(TOKEN));
}

#[test]
fn an_env_token_wins_over_gh_and_gh_is_not_run() {
    let repo = fixture_repo("bare", SITE);
    let bin = tempfile::tempdir().unwrap();
    fake_gh(bin.path(), &format!("printf '%s\\n' '{TOKEN}'"));
    let api = api();
    let out = hoist()
        .current_dir(repo.path())
        .env("PATH", path_with(bin.path()))
        .env("HOIST_GITHUB_API", api.url())
        .env("GH_TOKEN", "from-gh-token-env")
        .args(["auth", "check", "--json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let token = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "token")
        .unwrap();
    assert_eq!(token["source"], "env GH_TOKEN");
    assert!(!bin.path().join("gh.log").exists());
}

#[test]
fn a_failing_gh_is_no_token_and_nothing_leaks() {
    let repo = fixture_repo("bare", SITE);
    let bin = tempfile::tempdir().unwrap();
    fake_gh(
        bin.path(),
        &format!("echo '{TOKEN}' >&2; echo '{TOKEN}'; exit 1"),
    );
    let api = api();
    let out = hoist()
        .current_dir(repo.path())
        .env("PATH", path_with(bin.path()))
        .env("HOIST_GITHUB_API", api.url())
        .args(["auth", "check", "--json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all.contains("no GitHub token found"), "{all}");
    assert!(!all.contains(TOKEN), "{all}");
    assert!(api.requests().is_empty());
}
