mod common;

use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;

const KEELSTACK: &str = "https://github.com/keelstackhq/keelstack.git";

fn explain_json(dir: &Path) -> Value {
    let out = common::hoist()
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .args(["explain", "--json", "-C"])
        .arg(dir)
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("one JSON object")
}

fn find<'a>(report: &'a Value, area: &str, kind: &str, key: &str) -> Option<&'a Value> {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["area"] == area && i["kind"] == kind && i["key"] == key)
}

#[test]
fn explain_site_snapshot() {
    let repo = common::fixture_repo("site", KEELSTACK);
    let report = explain_json(repo.path());
    let check = find(&report, "github", "required_check", "ci-ok").expect("ci-ok check");
    assert_eq!(check["action"], "ok");
    let pages =
        find(&report, "cloudflare", "pages_project", "keelstack-landing").expect("pages project");
    assert_eq!(pages["source"], "apps/site/hoist.toml:deploy.project");
    let mq = find(&report, "github", "merge_queue", "default").expect("merge queue");
    assert_eq!(mq["reason"], "true");
    assert_eq!(mq["source"], ".github/workflows/ci.yml:5");
    // Same source and reason as `plan` reports for an in-sync ruleset.
    let rs = find(&report, "github", "ruleset", "hoist: default branch").expect("ruleset");
    assert_eq!(rs["source"], ".github/workflows/ci.yml:98");
    assert_eq!(rs["reason"], "required checks `ci-ok`");
    insta::assert_json_snapshot!(report);
}

#[test]
fn pages_project_defaults_to_repo_dash_target() {
    let repo = common::fixture_repo("site", "https://github.com/acme/site.git");
    std::fs::remove_file(repo.path().join("apps/site/hoist.toml")).unwrap();
    let report = explain_json(repo.path());
    let pages = find(&report, "cloudflare", "pages_project", "site-site").expect("default project");
    assert_eq!(pages["source"], "default");
}

#[test]
fn missing_account_id_exits_3() {
    let repo = common::fixture_repo("site", KEELSTACK);
    common::hoist()
        .args(["explain", "-C"])
        .arg(repo.path())
        .assert()
        .code(3)
        .stderr(predicate::str::contains("CLOUDFLARE_ACCOUNT_ID"))
        .stderr(predicate::str::contains("[cloudflare] account_id"));
}

#[test]
fn explain_table_lists_sources() {
    let repo = common::fixture_repo("site", KEELSTACK);
    common::hoist()
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .args(["explain", "-C"])
        .arg(repo.path())
        .assert()
        .code(0)
        .stdout(predicate::str::contains(".github/workflows/ci.yml:"));
}

#[test]
fn merge_queue_from_config() {
    let repo = common::fixture_repo("site", KEELSTACK);
    std::fs::write(
        repo.path().join("hoist.toml"),
        "[github]\nmerge_queue = false\n",
    )
    .unwrap();
    let report = explain_json(repo.path());
    let mq = find(&report, "github", "merge_queue", "default").expect("merge queue");
    assert_eq!(mq["reason"], "false");
    assert_eq!(mq["source"], "hoist.toml:github.merge_queue");
}
