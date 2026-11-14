mod common;

use predicates::prelude::*;

#[test]
fn version_prints_name() {
    common::hoist()
        .arg("--version")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("hoist"));
}

#[test]
fn unknown_command_exits_3() {
    common::hoist().arg("nope").assert().code(3);
}

#[test]
fn json_is_one_object() {
    let repo = common::git_repo();
    let out = common::hoist()
        .args(["explain", "--json", "-C"])
        .arg(repo.path())
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value =
        serde_json::from_slice(&out).expect("stdout is exactly one JSON value");
    let obj = value.as_object().expect("a JSON object");
    assert_eq!(obj["version"], 1);
    assert_eq!(obj["command"], "explain");
    assert_eq!(obj["ok"], true);
    assert_eq!(obj["exit_code"], 0);
    assert!(obj["items"].is_array());
    assert!(obj["human_steps"].is_array());
}

fn json_error(args: &[&str]) -> (serde_json::Value, String) {
    let out = common::hoist()
        .args(args)
        .assert()
        .code(3)
        .get_output()
        .clone();
    let value = serde_json::from_slice(&out.stdout).expect("stdout is exactly one JSON value");
    (value, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn json_usage_error_prints_error_object() {
    let (v, stderr) = json_error(&["plan", "--json", "--bogus"]);
    assert_eq!(v["version"], 1);
    assert_eq!(v["command"], "plan");
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit_code"], 3);
    assert_eq!(v["items"], serde_json::json!([]));
    assert_eq!(v["human_steps"], serde_json::json!([]));
    assert!(v["error"]["message"].as_str().unwrap().contains("--bogus"));
    assert!(!v["error"]["fix"].as_str().unwrap().is_empty());
    assert!(
        stderr.contains("--bogus"),
        "stderr keeps the text: {stderr}"
    );
}

#[test]
fn json_unknown_command_prints_error_object() {
    let (v, _) = json_error(&["--json", "nope"]);
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit_code"], 3);
    assert!(v["error"]["message"].is_string());
}

#[test]
fn json_runtime_error_prints_error_object() {
    let dir = tempfile::tempdir().unwrap();
    // Not a repo with no token: plan fails with exit 3 before any output.
    let (v, stderr) = json_error(&[
        "release",
        "plan",
        "--json",
        "-C",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(v["command"], "release plan");
    assert_eq!(v["exit_code"], 3);
    assert!(!v["error"]["message"].as_str().unwrap().is_empty());
    assert!(stderr.contains("error:"));
}

#[test]
fn table_error_prints_nothing_on_stdout() {
    let out = common::hoist()
        .arg("nope")
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
}

#[test]
fn json_unknown_command_has_no_bogus_command_name() {
    let (v, _) = json_error(&["--json", "bogus"]);
    assert_eq!(v["command"], "");
    assert_eq!(v["error"]["fix"], "run `hoist --help`");
}

#[test]
fn json_error_of_init_names_the_command() {
    let (v, _) = json_error(&["--json", "-C", "/nonexistent-hoist-dir", "init"]);
    assert_eq!(v["command"], "init");
}

#[test]
fn non_utf8_argument_is_exit_3_not_a_panic() {
    use std::os::unix::ffi::OsStrExt;
    let bad = std::ffi::OsStr::from_bytes(b"/tmp/d\xff");
    common::hoist()
        .args(["--json", "-C"])
        .arg(bad)
        .arg("explain")
        .assert()
        .code(3);
}

#[test]
fn json_after_double_dash_is_not_the_flag() {
    let out = common::hoist()
        .args(["secrets", "exec", "--bogus", "--", "tool", "--json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
}
