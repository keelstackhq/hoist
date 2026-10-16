#![allow(dead_code)]

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as Std;
use tempfile::TempDir;

/// Env vars that could reach a real API; every test child runs without them (ruling R2).
pub const SCRUBBED_ENV: [&str; 4] = [
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_ACCOUNT_ID",
];

/// `hoist` command with real credentials removed and API bases pointed at a closed port.
pub fn hoist() -> Command {
    let mut cmd = Command::cargo_bin("hoist").expect("hoist binary");
    for name in SCRUBBED_ENV {
        cmd.env_remove(name);
    }
    cmd.env("HOIST_GITHUB_API", "http://127.0.0.1:9");
    cmd.env("HOIST_CLOUDFLARE_API", "http://127.0.0.1:9");
    cmd
}

pub fn git(dir: &Path, args: &[&str]) {
    let status = Std::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .expect("git on PATH");
    assert!(status.success(), "git {args:?} failed");
}

/// Empty git repo whose `origin` is https://github.com/acme/x.git.
pub fn git_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    git(
        dir.path(),
        &["remote", "add", "origin", "https://github.com/acme/x.git"],
    );
    dir
}

/// Empty git repo with no remote.
pub fn git_repo_no_origin() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    dir
}
