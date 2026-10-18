//! hoist releases itself (M3 Task 10): the repo's own `hoist.toml`, crate and npm launcher
//! form a valid release model. Every test runs on a copy of the checkout in a temp dir with
//! its own `git init`; the real checkout is never touched and no request leaves the machine.

mod common;

use common::{FakeApi, git, git_wrapper, hoist, stub};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const REMOTE: &str = "https://github.com/keelstackhq/hoist.git";
const PLATFORMS: [&str; 5] = [
    "@keelstackhq/hoist-linux-x64",
    "@keelstackhq/hoist-linux-arm64",
    "@keelstackhq/hoist-darwin-x64",
    "@keelstackhq/hoist-darwin-arm64",
    "@keelstackhq/hoist-win32-x64",
];

/// Copy the tracked files of the real checkout into a fresh repo on `main` with one commit
/// and `origin` = keelstackhq/hoist. Untracked files (scratch notes, local tools) stay out.
fn self_copy() -> TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = Command::new("git")
        .args(["ls-files", "-z", "--cached"])
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git ls-files");
    assert!(listed.status.success(), "git ls-files failed");
    let dir = tempfile::tempdir().expect("tempdir");
    for rel in listed.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let rel = std::str::from_utf8(rel).expect("utf-8 path");
        let from = root.join(rel);
        if !from.is_file() {
            continue; // deleted in the work tree but still in the index
        }
        let to = dir.path().join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(&from, &to).unwrap();
    }
    let d = dir.path();
    git(d, &["init", "-q"]);
    git(d, &["checkout", "-q", "-b", "main"]);
    git(d, &["remote", "add", "origin", REMOTE]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    dir
}

/// `hoist` with git wrapped so `ls-remote origin` fails locally instead of reaching GitHub.
fn cmd(repo: &TempDir, tools: &TempDir) -> assert_cmd::Command {
    let git = stub(tools.path(), "git", &git_wrapper());
    let mut c = hoist();
    c.arg("-C")
        .arg(repo.path())
        .env("HOIST_GIT", git)
        .env("STUB_LSREMOTE_FAILS", "1");
    c
}

/// `[package] version` of the real checkout's Cargo.toml.
fn manifest_version() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    cargo["package"]["version"].as_str().unwrap().to_string()
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

fn items<'a>(v: &'a Value, area: &str, kind: &str) -> Vec<&'a Value> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["area"] == area && i["kind"] == kind)
        .collect()
}

fn has_step(v: &Value, id: &str) -> bool {
    v["human_steps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == id)
}

#[test]
fn release_plan_finds_the_crate_and_the_launcher() {
    let (repo, tools) = (self_copy(), tempfile::tempdir().unwrap());
    let out = cmd(&repo, &tools)
        .args(["release", "plan", "--json"])
        .output()
        .unwrap();
    let code = out.status.code();
    assert!(
        matches!(code, Some(0) | Some(1)),
        "exit {code:?}, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    let versions = items(&v, "release", "version");
    let find = |name: &str| {
        versions
            .iter()
            .find(|i| i["key"] == name)
            .unwrap_or_else(|| panic!("no release/version/{name} in {v:#}"))
    };
    // The copy has no tags: both release the manifest version, the launcher at the crate's.
    let version = manifest_version();
    let expected = format!("first release {version}");
    assert_eq!(find("keel-hoist")["reason"], expected.as_str());
    assert_eq!(find("keel-hoist")["source"], "Cargo.toml:3");
    assert_eq!(find("@keelstackhq/hoist")["reason"], expected.as_str());
    // Platform packages leave the model: they are published with the launcher.
    for p in PLATFORMS {
        assert!(
            versions.iter().all(|i| i["key"] != p),
            "{p} is not a package of its own"
        );
    }
    assert_eq!(versions.len(), 2, "only the crate and the launcher: {v:#}");
}

#[test]
fn launcher_has_the_five_platform_packages_at_the_crate_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let version = manifest_version();
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap()
    };
    let launcher = read("npm/hoist/package.json");
    assert_eq!(launcher["name"], "@keelstackhq/hoist");
    assert_eq!(launcher["version"], version.as_str());
    let deps = launcher["optionalDependencies"].as_object().unwrap();
    assert_eq!(deps.len(), 5);
    for (p, dir) in PLATFORMS.iter().zip([
        "linux-x64",
        "linux-arm64",
        "darwin-x64",
        "darwin-arm64",
        "win32-x64",
    ]) {
        assert_eq!(
            deps[*p],
            version.as_str(),
            "{p} pinned at the crate version"
        );
        let pkg = read(&format!("npm/hoist-{dir}/package.json"));
        assert_eq!(pkg["name"], *p);
        assert_eq!(pkg["version"], version.as_str());
        assert!(
            pkg["files"].as_array().unwrap().iter().any(|f| f == "bin/"),
            "{p} ships bin/"
        );
    }
}

#[test]
fn plan_lists_release_environment_and_trusted_publishers() {
    let (repo, tools) = (self_copy(), tempfile::tempdir().unwrap());
    let api = FakeApi::start(&[("GET /repos/keelstackhq/hoist", 200, "github/repo.json", &[])]);
    let out = cmd(&repo, &tools)
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_NPM_REGISTRY", api.url())
        .env("HOIST_CRATES_API", api.url())
        .args([
            "plan", "--json", "--area", "release", "--area", "npm", "--area", "crates",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    let env = items(&v, "release", "environment");
    assert_eq!(env.len(), 1, "{v:#}");
    assert_eq!(env[0]["key"], "release");
    let wf = items(&v, "release", "workflow");
    assert_eq!(wf[0]["action"], "ok", "release.yml has a job in `release`");
    assert!(wf[0]["reason"].as_str().unwrap().contains("release.yml"));

    assert!(has_step(&v, "crates-trust:keel-hoist"), "{v:#}");
    assert!(has_step(&v, "npm-trust:@keelstackhq/hoist"), "{v:#}");
    for p in PLATFORMS {
        assert!(has_step(&v, &format!("npm-trust:{p}")), "{p}: {v:#}");
    }
    api.assert_only_get();
}
