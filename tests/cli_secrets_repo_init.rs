//! `hoist secrets repo init OWNER/NAME [--require-check C]… [--dry-run]` (spec §15.4): a private
//! secrets repo with the default-branch ruleset; the fake GitHub API only.
mod common;

use common::{FakeApi, hoist};
use serde_json::Value;

const REPO: &str = "GET /repos/acme/secrets";
const USER: &str = "GET /user";
const CREATE: &str = "POST /orgs/acme/repos";
const RULESETS: &str = "GET /repos/acme/secrets/rulesets";
const ADD: &str = "POST /repos/acme/secrets/rulesets";

fn init(api: &FakeApi, args: &[&str], extra: &[(&str, &str)]) -> (i32, Value, String) {
    let mut cmd = hoist();
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_REMOTE");
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token");
    cmd.envs(extra.iter().copied());
    let dir = tempfile::tempdir().unwrap();
    cmd.args(["secrets", "repo", "init", "--json", "-C"])
        .arg(dir.path())
        .args(args);
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (
        out.status.code().unwrap(),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sent(api: &FakeApi) -> Vec<String> {
    let reqs = api.requests().into_iter();
    reqs.map(|r| format!("{} {}", r.method, r.path)).collect()
}

fn body(api: &FakeApi, key: &str) -> Value {
    let (method, path) = key.split_once(' ').unwrap();
    let req = api
        .requests()
        .into_iter()
        .find(|r| r.method == method && r.path == path);
    serde_json::from_str(req.expect(key).body.as_deref().unwrap()).unwrap()
}

fn new_repo() -> FakeApi {
    FakeApi::start(&[
        (REPO, 404, "{}", &[]),
        (USER, 200, r#"{"login":"someone"}"#, &[]),
        (
            CREATE,
            201,
            r#"{"full_name":"acme/secrets","private":true}"#,
            &[],
        ),
        (RULESETS, 200, "[]", &[]),
        (ADD, 201, r#"{"id":7}"#, &[]),
    ])
}

#[test]
fn repo_init_creates_a_private_repo_and_its_ruleset() {
    let api = new_repo();
    let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    // A repo made just now has no ruleset to look for.
    assert_eq!(sent(&api), [REPO, USER, CREATE, ADD]);
    let repo = body(&api, CREATE);
    assert_eq!(
        (&repo["name"], &repo["private"]),
        (&"secrets".into(), &true.into())
    );
    let ruleset = body(&api, ADD);
    assert_eq!(ruleset["name"], "hoist: default branch");
    let rules = ruleset["rules"].as_array().unwrap();
    let kinds: Vec<&str> = rules.iter().map(|r| r["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "pull_request",
            "non_fast_forward",
            "deletion",
            "required_status_checks"
        ]
    );
    assert_eq!(rules[0]["parameters"]["required_approving_review_count"], 1);
    assert_eq!(
        rules[3]["parameters"]["required_status_checks"][0]["context"],
        "ci-ok"
    );
    assert!(
        report["human_steps"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
}

#[test]
fn repo_init_without_a_check_says_to_add_one_and_prints_the_template() {
    let api = new_repo();
    let mut cmd = hoist();
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_REMOTE");
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token");
    cmd.args(["secrets", "repo", "init", "acme/secrets"]);
    let out = cmd.output().unwrap();
    let (stdout, err) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(2), "{stdout}{err}");
    assert!(stdout.contains("required check"), "{stdout}");
    assert!(
        err.contains("splice new gh:keelstackhq/hull//template-secrets"),
        "{err}"
    );
    let ruleset = body(&api, ADD);
    let kinds: Vec<&str> = ruleset["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["pull_request", "non_fast_forward", "deletion"]);
}

#[test]
fn repo_init_for_the_tokens_own_account_and_an_existing_repo() {
    // The token's own login: /user/repos.
    let api = FakeApi::start(&[
        (REPO, 404, "{}", &[]),
        (USER, 200, r#"{"login":"Acme"}"#, &[]),
        ("POST /user/repos", 201, r#"{"private":true}"#, &[]),
        (RULESETS, 200, "[]", &[]),
        (ADD, 201, r#"{"id":7}"#, &[]),
    ]);
    let (code, _, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(sent(&api).contains(&"POST /user/repos".to_string()));
    // Already private with the ruleset: GETs only, nothing to do.
    let api = FakeApi::start(&[
        (REPO, 200, r#"{"private":true}"#, &[]),
        (
            RULESETS,
            200,
            r#"[{"id":7,"name":"hoist: default branch"}]"#,
            &[],
        ),
    ]);
    let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    assert_eq!(sent(&api), [REPO, RULESETS]);
}

#[test]
fn repo_init_refuses_a_public_repo_and_dry_run_sends_only_gets() {
    let api = FakeApi::start(&[(REPO, 200, r#"{"private":false}"#, &[])]);
    let (code, _, err) = init(&api, &["acme/secrets"], &[]);
    assert!(code == 3 && err.contains("public"), "{err}");
    assert_eq!(sent(&api), [REPO]);
    // --dry-run: the plan (exit 1, something to create), GETs only.
    let api = FakeApi::start(&[(REPO, 404, "{}", &[]), (USER, 200, r#"{"login":"x"}"#, &[])]);
    let (code, report, err) = init(&api, &["acme/secrets", "--dry-run"], &[]);
    assert_eq!(code, 1, "{err} {report:#}");
    assert!(api.requests().iter().all(|r| r.method == "GET"));
    let actions: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["create", "create"], "{report:#}");
    // Agent session and a bad name: refused before any request.
    for (args, extra, want) in [
        (&["acme/secrets"][..], &[("CLAUDECODE", "1")][..], 2),
        (&["acme"][..], &[][..], 3),
    ] {
        let api = FakeApi::start(&[]);
        assert_eq!(init(&api, args, extra).0, want, "{args:?}");
        assert!(api.requests().is_empty());
    }
}
