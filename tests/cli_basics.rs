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
