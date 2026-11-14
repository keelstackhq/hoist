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
    assert!(ci.contains("  ci-ok:\n") && ci.contains("run: cargo test\n"));
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

#[test]
fn a_managed_ci_keeps_its_own_ci_ok_when_other_workflows_appear() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    init(d, &["--sha", SHA]).code(0);
    std::fs::write(
        d.join(".github/workflows/deploy.yml"),
        "name: d\non: push\njobs:\n  d:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    )
    .unwrap();
    let a = init(d, &["--sha", SHA]).code(0);
    assert_eq!(items(&a)[1]["action"], "ok");
}

#[test]
fn empty_required_checks_opts_out_of_the_gate() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    std::fs::write(d.join("hoist.toml"), "[github]\nrequired_checks = []\n").unwrap();
    let a = init(d, &["--sha", SHA]).code(0);
    assert_eq!(items(&a).len(), 1);
    assert!(!d.join(".github/workflows/ci.yml").exists());
}

#[test]
fn the_gate_runs_cargo_test_without_locked_when_there_is_no_lockfile() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    init(d, &["--sha", SHA]).code(0);
    let ci = std::fs::read_to_string(d.join(".github/workflows/ci.yml")).unwrap();
    assert!(ci.contains("run: cargo test\n"), "{ci}");
}

fn add_npm(d: &Path, test_script: &str, lock: bool) {
    std::fs::write(
        d.join("package.json"),
        format!(
            "{{\"name\":\"solo-js\",\"private\":false,\"version\":\"0.1.0\",\"scripts\":{{\"test\":{}}}}}",
            serde_json::to_string(test_script).unwrap()
        ),
    )
    .unwrap();
    if lock {
        std::fs::write(d.join("package-lock.json"), "{}").unwrap();
    }
}

fn ci_text(d: &Path) -> String {
    std::fs::read_to_string(d.join(".github/workflows/ci.yml")).unwrap_or_default()
}

#[test]
fn the_npm_placeholder_test_script_is_a_human_step_not_a_failing_gate() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    std::fs::remove_file(d.join("Cargo.toml")).unwrap();
    add_npm(d, "echo \"Error: no test specified\" && exit 1", true);
    let a = init(d, &["--sha", SHA]);
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    assert_eq!(v["exit_code"], 2, "{v}");
    assert_eq!(v["human_steps"][0]["id"], "ci-ok");
    assert!(
        v["human_steps"][0]["title"]
            .as_str()
            .unwrap()
            .contains("placeholder")
    );
    assert!(ci_text(d).is_empty());
}

#[test]
fn cargo_plus_npm_placeholder_gates_cargo_and_asks_only_for_the_npm_script() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    add_npm(d, "echo \"Error: no test specified\" && exit 1", true);
    let a = init(d, &["--sha", SHA]);
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    assert_eq!(v["exit_code"], 2, "{v}");
    assert_eq!(v["human_steps"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["human_steps"][0]["id"], "ci-ok-npm-test");
    assert!(
        v["human_steps"][0]["title"]
            .as_str()
            .unwrap()
            .contains("placeholder")
    );
    let ci = ci_text(d);
    assert!(ci.contains("run: cargo test\n"), "{ci}");
    assert!(!ci.contains("npm"), "{ci}");
}

#[test]
fn cargo_and_npm_are_both_in_the_gate() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    add_npm(d, "node --test", true);
    init(d, &["--sha", SHA]).code(0);
    let ci = ci_text(d);
    assert!(ci.contains("run: cargo test\n"), "{ci}");
    assert!(ci.contains("run: npm ci && npm test\n"), "{ci}");
}

#[test]
fn cargo_plus_npm_without_a_lockfile_is_a_human_step() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    add_npm(d, "node --test", false);
    init(d, &["--sha", SHA]).code(2);
    assert!(ci_text(d).is_empty());
}

#[test]
fn a_second_ci_ok_next_to_the_managed_ci_is_reported_and_left_alone() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    init(d, &["--sha", SHA]).code(0);
    let before = ci_text(d);
    std::fs::write(
        d.join(".github/workflows/own.yml"),
        "name: o\non: pull_request\njobs:\n  ci-ok:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    )
    .unwrap();
    let a = init(d, &["--sha", SHA]).code(2);
    let v: Value = serde_json::from_slice(&a.get_output().stdout).unwrap();
    let step = &v["human_steps"][0];
    assert_eq!(step["id"], "ci-ok-duplicate");
    assert!(step["title"].as_str().unwrap().contains("ci.yml"));
    assert_eq!(ci_text(d), before, "never rewritten");
    // --force does not make two checks named ci-ok either.
    init(d, &["--sha", SHA, "--force"]).code(2);
    assert_eq!(ci_text(d), before);
}

#[test]
fn the_table_has_no_trailing_colon_for_a_step_without_a_url() {
    let repo = fixture_repo("release-single", "https://github.com/acme/x.git");
    let d = repo.path();
    std::fs::create_dir_all(d.join(".github/workflows")).unwrap();
    std::fs::write(
        d.join(".github/workflows/test.yml"),
        "name: t\non: pull_request\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n",
    )
    .unwrap();
    let out = hoist()
        .args(["-C", d.to_str().unwrap(), "init", "--sha", SHA])
        .assert()
        .code(2);
    let text = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let line = text.lines().find(|l| l.contains("(ci-ok)")).unwrap();
    assert!(line.ends_with("(ci-ok)"), "{line:?}");
}
