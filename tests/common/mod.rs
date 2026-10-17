#![allow(dead_code)]

use assert_cmd::Command;

use std::path::Path;
use std::process::Command as Std;
use tempfile::TempDir;

mod fake;
mod stub_tool;
#[allow(unused_imports)]
pub use fake::{FakeApi, RouteSpec};
#[allow(unused_imports)]
pub use stub_tool::stub;

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
    // `gh auth token` must not find a real login on the dev machine.
    cmd.env("GH_CONFIG_DIR", "/nonexistent-hoist-gh");
    cmd.env("HOIST_GITHUB_API", "http://127.0.0.1:9");
    cmd.env("HOIST_CLOUDFLARE_API", "http://127.0.0.1:9");
    cmd.env("HOIST_GITHUB_UPLOADS", "http://127.0.0.1:9");
    cmd.env("HOIST_NPM_REGISTRY", "http://127.0.0.1:9");
    cmd.env("HOIST_CRATES_API", "http://127.0.0.1:9");
    // `check` probes the Pages URL; it must never leave the machine.
    cmd.env("HOIST_PAGES_BASE", "http://127.0.0.1:9");
    cmd
}

pub fn git(dir: &Path, args: &[&str]) {
    let status = Std::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).expect("copy file");
        }
    }
}

/// Copy `tests/fixtures/repos/<name>` into a fresh git repo whose `origin` is `remote`.
pub fn fixture_repo(name: &str, remote: &str) -> TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/repos")
        .join(name);
    let dir = tempfile::tempdir().expect("tempdir");
    copy_dir(&src, dir.path());
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["remote", "add", "origin", remote]);
    dir
}
