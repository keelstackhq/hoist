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
    assert_eq!(ruleset["name"], "hoist: secrets repo");
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
    let api = existing(&full_ruleset("active"));
    let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    assert_eq!(sent(&api), [REPO, RULESETS, ONE]);
    // A ruleset of that name that protects less (here: not enforced) is drift, not ok.
    let api = existing(&full_ruleset("disabled"));
    let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert!(code == 1 && err.is_empty(), "{err} {report:#}");
    let items = report["items"].as_array().unwrap();
    assert_eq!(items[1]["action"], "drift", "{report:#}");
    assert!(api.requests().iter().all(|r| r.method == "GET"));
    // Enforced, but on another branch, or with a bypass: drift too.
    for (from, to) in [
        (r#""~DEFAULT_BRANCH""#, r#""refs/heads/nothing""#),
        (
            r#""bypass_actors":[]"#,
            r#""bypass_actors":[{"actor_type":"RepositoryRole","actor_id":5,"bypass_mode":"always"}]"#,
        ),
    ] {
        let api = existing(&full_ruleset("active").replace(from, to));
        let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
        assert_eq!(code, 1, "{to}: {err} {report:#}");
    }
    // What it targets and excludes: the default branch must stay covered.
    let include = r#""include":["~DEFAULT_BRANCH"]"#;
    let exclude = r#""exclude":[]"#;
    for (from, to, want) in [
        (include, r#""include":["~ALL"]"#, 0),
        (include, r#""include":["refs/heads/main"]"#, 0),
        (exclude, r#""exclude":["~DEFAULT_BRANCH"]"#, 1),
        (exclude, r#""exclude":["refs/heads/main"]"#, 1),
        (exclude, r#""exclude":["refs/heads/m*"]"#, 1),
        (exclude, r#""exclude":["refs/heads/{main,x}"]"#, 1),
        (exclude, r#""exclude":["refs/heads/ma\\in"]"#, 1),
        (exclude, r#""exclude":["refs/heads/other"]"#, 0),
        (r#""target":"branch""#, r#""target":"tag""#, 1),
    ] {
        let ruleset = full_ruleset("active").replace(from, to);
        assert!(ruleset.contains(to), "{ruleset}");
        let api = existing(&ruleset);
        let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
        assert_eq!(code, want, "{to}: {err} {report:#}");
    }
    // With no default branch in the answer, only `~DEFAULT_BRANCH` or `~ALL` cover it, and any
    // exclude may remove it.
    for (from, to) in [
        (exclude, r#""exclude":["refs/heads/other"]"#),
        (include, r#""include":["refs/heads/"]"#),
    ] {
        let ruleset = full_ruleset("active").replace(from, to);
        let api = FakeApi::start(&[
            (REPO, 200, r#"{"visibility":"private"}"#, &[]),
            (
                RULESETS,
                200,
                r#"[{"id":7,"name":"hoist: secrets repo","source_type":"Repository"}]"#,
                &[],
            ),
            (ONE, 200, &ruleset, &[]),
        ]);
        let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
        assert_eq!(code, 1, "{to}: {err} {report:#}");
    }
    // An organization ruleset of the same name is not this repo's: its own is made.
    let api = FakeApi::start(&[
        (REPO, 200, PRIVATE, &[]),
        (
            RULESETS,
            200,
            r#"[{"id":9,"name":"hoist: secrets repo","source_type":"Organization"}]"#,
            &[],
        ),
        (ADD, 201, r#"{"id":8}"#, &[]),
    ]);
    let (code, report, err) = init(&api, &["acme/secrets", "--require-check", "ci-ok"], &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    assert_eq!(sent(&api), [REPO, RULESETS, ADD], "{report:#}");
    let urls: Vec<String> = api.requests().into_iter().map(|r| r.url).collect();
    assert!(urls[1].contains("includes_parents=false"), "{urls:?}");
}

const ONE: &str = "GET /repos/acme/secrets/rulesets/7";
/// A private repo, as GitHub reads it.
const PRIVATE: &str = r#"{"private":true,"visibility":"private","default_branch":"main"}"#;

/// The ruleset `repo init` makes, with `enforcement`.
fn full_ruleset(enforcement: &str) -> String {
    serde_json::json!({
        "id": 7, "name": "hoist: secrets repo", "target": "branch", "enforcement": enforcement,
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "bypass_actors": [],
        "rules": [
            {"type": "pull_request", "parameters": {"required_approving_review_count": 1}},
            {"type": "non_fast_forward"}, {"type": "deletion"},
            {"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "ci-ok"}]}}
        ]
    })
    .to_string()
}

/// A private acme/secrets whose ruleset (id 7) reads `ruleset`.
fn existing(ruleset: &str) -> FakeApi {
    FakeApi::start(&[
        (REPO, 200, PRIVATE, &[]),
        (
            RULESETS,
            200,
            r#"[{"id":7,"name":"hoist: secrets repo","source_type":"Repository"}]"#,
            &[],
        ),
        (ONE, 200, ruleset, &[]),
        (ADD, 201, r#"{"id":8}"#, &[]),
    ])
}

/// Routes, the expected exit code and a text the output must contain.
type Case<'a> = (
    &'a [(&'a str, u16, &'a str, &'a [(&'a str, &'a str)])],
    i32,
    &'a str,
);

#[test]
fn repo_init_says_what_it_cannot_read_or_do() {
    let checks = ["acme/secrets", "--require-check", "ci-ok"];
    let cases: [Case; 6] = [
        // Internal: every member of the enterprise can read it.
        (
            &[(
                REPO,
                200,
                r#"{"private":true,"visibility":"internal"}"#,
                &[],
            )],
            3,
            "internal",
        ),
        // Rulesets it cannot read (a free plan, or not an admin): before any write.
        (
            &[
                (REPO, 200, PRIVATE, &[]),
                (RULESETS, 403, r#"{"message":"Upgrade"}"#, &[]),
            ],
            3,
            "rulesets",
        ),
        // Its visibility is not in the answer.
        (
            &[(REPO, 200, r#"{"private":true}"#, &[])],
            3,
            "cannot tell whether acme/secrets is private",
        ),
        // The token's own account is unknown.
        (
            &[
                (REPO, 404, "{}", &[]),
                (USER, 403, r#"{"message":"no"}"#, &[]),
            ],
            3,
            "GET /user",
        ),
        // It exists where the token cannot see it, or the token may not create repos there.
        (
            &[
                (REPO, 404, "{}", &[]),
                (USER, 200, r#"{"login":"x"}"#, &[]),
                (
                    CREATE,
                    422,
                    r#"{"message":"Repository creation failed."}"#,
                    &[],
                ),
            ],
            3,
            "may already exist",
        ),
        // Made, but its ruleset was refused: drift after the create, not a bare error.
        (
            &[
                (REPO, 404, "{}", &[]),
                (USER, 200, r#"{"login":"x"}"#, &[]),
                (CREATE, 201, "{}", &[]),
                (ADD, 403, r#"{"message":"Upgrade"}"#, &[]),
            ],
            1,
            "",
        ),
    ];
    for (routes, want, text) in cases {
        let api = FakeApi::start(routes);
        let (code, report, err) = init(&api, &checks, &[]);
        assert!(
            code == want && format!("{report}{err}").contains(text),
            "{text}: {code} {err} {report:#}"
        );
        if want == 3 && !text.contains("may already") {
            assert!(api.requests().iter().all(|r| r.method == "GET"), "{text}");
        }
    }
    // Existing repo, missing ruleset, --dry-run: the plan from GETs only.
    let api = FakeApi::start(&[(REPO, 200, PRIVATE, &[]), (RULESETS, 200, "[]", &[])]);
    let (code, report, err) = init(&api, &["acme/secrets", "--dry-run"], &[]);
    assert_eq!(code, 1, "{err} {report:#}");
    assert!(api.requests().iter().all(|r| r.method == "GET"));
}

#[test]
fn repo_init_refuses_a_public_repo_and_dry_run_sends_only_gets() {
    let api = FakeApi::start(&[(REPO, 200, r#"{"private":false,"visibility":"public"}"#, &[])]);
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
