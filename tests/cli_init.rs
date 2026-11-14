mod common;

use common::{FakeApi, fixture_repo, hoist};
use serde_json::Value;
use std::path::Path;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn init(dir: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    hoist()
        .arg("--json")
        .args(["-C", dir.to_str().unwrap(), "init"])
        .args(extra)
        .assert()
}

fn action(a: &assert_cmd::assert::Assert) -> String {
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    v["items"][0]["action"].as_str().unwrap().to_string()
}

fn file(dir: &Path) -> std::path::PathBuf {
    dir.join(".github/workflows/release.yml")
}

#[test]
fn writes_a_pinned_thin_workflow_then_is_idempotent() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    let a = init(d, &["--sha", SHA]).code(0);
    assert_eq!(action(&a), "create");
    let text = std::fs::read_to_string(file(d)).unwrap();
    assert!(text.contains(&format!("keelstackhq/hoist/release@{SHA} # v")));
    assert!(text.contains("command: detect") && text.contains("command: publish"));
    assert!(text.contains("environment: \"release\""));
    let b = init(d, &["--sha", SHA]).code(0);
    assert_eq!(action(&b), "ok");
}

#[test]
fn a_hand_edited_file_is_kept_without_force() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    init(d, &["--sha", SHA]).code(0);
    let edited = std::fs::read_to_string(file(d))
        .unwrap()
        .replace("name: release", "name: mine");
    std::fs::write(file(d), &edited).unwrap();
    let a = init(d, &["--sha", SHA]).code(0);
    assert_eq!(action(&a), "ok");
    assert_eq!(std::fs::read_to_string(file(d)).unwrap(), edited);
    let f = init(d, &["--sha", SHA, "--force"]).code(0);
    assert_eq!(action(&f), "update");
    assert!(
        std::fs::read_to_string(file(d))
            .unwrap()
            .contains("name: release")
    );
}

#[test]
fn dry_run_writes_nothing_and_bad_sha_exits_3() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    let a = init(d, &["--sha", SHA, "--dry-run"]).code(0);
    assert_eq!(action(&a), "create");
    assert!(!file(d).exists());
    init(d, &["--sha", "v1"]).code(3);
}

#[test]
fn the_sha_comes_from_the_release_tag_when_not_given() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    let body = format!("{{\"sha\":\"{SHA}\"}}");
    let route = format!(
        "GET /repos/keelstackhq/hoist/commits/v{}",
        env!("CARGO_PKG_VERSION")
    );
    let api = FakeApi::start(&[(route.as_str(), 200, &body, &[])]);
    hoist()
        .args(["-C", d.to_str().unwrap(), "init"])
        .env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "t")
        .assert()
        .code(0);
    assert!(std::fs::read_to_string(file(d)).unwrap().contains(SHA));
}

#[test]
fn a_launcher_repo_is_refused() {
    let repo = fixture_repo("release-launcher", "https://github.com/acme/x.git");
    let a = init(repo.path(), &["--sha", SHA]).code(3);
    assert!(String::from_utf8_lossy(&a.get_output().stdout).contains("launcher"));
}

#[test]
fn an_environment_name_cannot_inject_yaml() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    std::fs::write(
        d.join("hoist.toml"),
        "[release]\nenvironment = \"release\\n    steps: x\"\n",
    )
    .unwrap();
    let a = init(d, &["--sha", SHA]).code(3);
    assert!(String::from_utf8_lossy(&a.get_output().stdout).contains("environment"));
    assert!(!file(d).exists());
}

#[test]
fn dry_run_exits_zero_like_plan() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    init(repo.path(), &["--sha", SHA, "--dry-run"]).code(0);
}

fn items(a: &assert_cmd::assert::Assert) -> Vec<Value> {
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    v["items"].as_array().unwrap().clone()
}

#[test]
fn writes_a_ci_ok_gate_when_the_repo_has_none() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    let a = init(d, &["--sha", SHA]).code(0);
    let it = items(&a);
    assert_eq!(it[1]["kind"], "ci-ok-job");
    assert_eq!(it[1]["action"], "create");
    let ci = std::fs::read_to_string(d.join(".github/workflows/ci.yml")).unwrap();
    assert!(ci.contains("  ci-ok:\n") && ci.contains("cargo test --locked"));
    assert!(ci.contains("pull_request") && ci.contains("persist-credentials: false"));
    // The next run sees the job: idempotent.
    let b = init(d, &["--sha", SHA]).code(0);
    assert_eq!(action(&b), "ok");
    assert_eq!(items(&b)[1]["action"], "ok");
}

#[test]
fn another_workflow_without_ci_ok_is_a_human_step() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    std::fs::create_dir_all(d.join(".github/workflows")).unwrap();
    std::fs::write(
        d.join(".github/workflows/test.yml"),
        "name: t\non: pull_request\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    )
    .unwrap();
    let a = init(d, &["--sha", SHA]).code(2);
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    assert_eq!(v["human_steps"][0]["id"], "ci-ok");
    assert!(!d.join(".github/workflows/ci.yml").exists());
}
