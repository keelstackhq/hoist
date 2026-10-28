//! What `hoist secrets grant` checks on GitHub before it stores a key (spec §15.4, §15.8.2):
//! only `main` (release tags for `release`) may deploy to the consumer's environment, and
//! whether it already holds `SOPS_AGE_KEY`. GET only.
mod common;

use common::FakeApi;
use hoist::http::{Http, Service};
use hoist::secrets::consumer::{Consumer, holds_age_key, require_branch_rules};

const ENV: &str = "GET /repos/acme/site/environments/prod";
const POLICIES: &str = "GET /repos/acme/site/environments/prod/deployment-branch-policies";
const SECRET: &str = "GET /repos/acme/site/environments/prod/secrets/SOPS_AGE_KEY";
const C: Consumer = Consumer {
    owner: "acme",
    name: "site",
    env: "prod",
};

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, "gh-test-token".into(), &api.url()).max_sleep_ms(0)
}

fn env(policy: &str) -> String {
    format!(r#"{{"name":"prod","deployment_branch_policy":{policy}}}"#)
}

/// Routes (key, status, body, headers) and a text the error must contain, or `None` for none.
type Case<'a> = (
    Vec<(&'a str, u16, String, Vec<(&'a str, String)>)>,
    Option<&'a str>,
);

const PAGE1: &str =
    "GET /repos/acme/site/environments/prod/deployment-branch-policies?per_page=100";

/// Deployment branch policies as GitHub lists them: `(type, name)`.
fn policies(list: &[(&str, &str)]) -> String {
    let items: Vec<_> = list
        .iter()
        .map(|(t, n)| serde_json::json!({ "type": t, "name": n }))
        .collect();
    serde_json::json!({ "total_count": items.len(), "branch_policies": items }).to_string()
}

#[test]
fn a_key_goes_only_to_an_environment_that_only_main_can_deploy_to() {
    let custom = env(r#"{"protected_branches":false,"custom_branch_policies":true}"#);
    let with = |list: &[(&str, &str)]| {
        vec![
            (ENV, 200, custom.clone(), vec![]),
            (POLICIES, 200, policies(list), vec![]),
        ]
    };
    let any = Some("only branch `main`");
    let next = format!("<{{base}}{}&page=2>; rel=\"next\"", &PAGE1[4..]);
    let cases: Vec<Case> = vec![
        (with(&[("branch", "main")]), None),
        (with(&[("branch", "main"), ("branch", "*")]), any),
        (with(&[("branch", "feature/*")]), any),
        (with(&[("tag", "*")]), any),
        (with(&[("tag", "main")]), any),
        (with(&[]), any),
        // A wildcard on the second page counts too.
        (
            vec![
                (ENV, 200, custom.clone(), vec![]),
                (
                    PAGE1,
                    200,
                    policies(&[("branch", "main")]),
                    vec![("Link", next)],
                ),
                (
                    "GET /repos/acme/site/environments/prod/deployment-branch-policies?per_page=100&page=2",
                    200,
                    policies(&[("branch", "**")]),
                    vec![],
                ),
            ],
            any,
        ),
        (
            vec![
                (ENV, 200, custom.clone(), vec![]),
                (POLICIES, 404, "{}".into(), vec![]),
            ],
            any,
        ),
        // Protected branches: without a classic protection rule, any branch deploys.
        (
            vec![(
                ENV,
                200,
                env(r#"{"protected_branches":true,"custom_branch_policies":false}"#),
                vec![],
            )],
            any,
        ),
        (
            vec![(
                ENV,
                200,
                env(r#"{"protected_branches":false,"custom_branch_policies":false}"#),
                vec![],
            )],
            any,
        ),
        // Both flags (GitHub never sets both): protected still does not pass.
        (
            vec![
                (
                    ENV,
                    200,
                    env(r#"{"protected_branches":true,"custom_branch_policies":true}"#),
                    vec![],
                ),
                (POLICIES, 200, policies(&[("branch", "main")]), vec![]),
            ],
            any,
        ),
        (vec![(ENV, 200, env("null"), vec![])], any),
        (
            vec![(ENV, 404, "{}".into(), vec![])],
            Some("no environment prod"),
        ),
        (
            vec![(ENV, 403, r#"{"message":"nope"}"#.into(), vec![])],
            Some("403"),
        ),
    ];
    for (routes, want) in cases {
        let specs: Vec<_> = routes
            .iter()
            .map(|(k, s, b, h)| {
                let h: Vec<(&str, &str)> = h.iter().map(|(a, b)| (*a, b.as_str())).collect();
                (*k, *s, b.as_str(), h)
            })
            .collect();
        let specs: Vec<_> = specs
            .iter()
            .map(|(k, s, b, h)| (*k, *s, *b, &h[..]))
            .collect();
        let api = FakeApi::start(&specs);
        let got = require_branch_rules(&http(&api), &C).map_err(|e| e.to_string());
        match want {
            None => assert!(got.is_ok(), "{routes:?}: {got:?}"),
            Some(text) => assert!(
                got.as_ref()
                    .is_err_and(|e| e.contains(text) && e.contains("acme/site")),
                "{routes:?}: {got:?}"
            ),
        }
        assert!(api.requests().iter().all(|r| r.method == "GET"));
    }
    // `release` deploys from release tags only, and only after an owner approves (spec §15.8.2,
    // D-008): anyone who can push a tag `x-v1` chooses the workflow it runs.
    let release = Consumer {
        env: "release",
        ..C
    };
    let base = "GET /repos/acme/site/environments/release";
    let pol = format!("{base}/deployment-branch-policies");
    let owner =
        r#"[{"type":"required_reviewers","reviewers":[{"type":"User","reviewer":{"login":"o"}}]}]"#;
    let nobody = r#"[{"type":"required_reviewers","reviewers":[]}]"#;
    let timer = r#"[{"type":"wait_timer","wait_timer":5,"reviewers":[{"type":"User"}]}]"#;
    let tags = &[("tag", "*-v*")][..];
    for (list, rules, want) in [
        (tags, owner, None),
        // The owner pushes release tags and approves them: self-review stays allowed (D-008).
        (
            tags,
            r#"[{"type":"required_reviewers","prevent_self_review":false,"reviewers":[{"type":"User"}]}]"#,
            None,
        ),
        (tags, nobody, Some("reviewer")),
        (tags, timer, Some("reviewer")),
        (tags, "[]", Some("reviewer")),
        (&[("branch", "main")][..], owner, Some("only tag `*-v*`")),
    ] {
        let body = format!(
            r#"{{"name":"release","protection_rules":{rules},"deployment_branch_policy":{{"protected_branches":false,"custom_branch_policies":true}}}}"#
        );
        let api = FakeApi::start(&[(base, 200, &body, &[]), (&pol, 200, &policies(list), &[])]);
        let got = require_branch_rules(&http(&api), &release).map_err(|e| e.to_string());
        match want {
            None => assert!(got.is_ok(), "{rules}: {got:?}"),
            Some(text) => assert!(
                got.as_ref().is_err_and(|e| e.contains(text)),
                "{rules}: {got:?}"
            ),
        }
    }
}

#[test]
fn whether_the_environment_already_holds_a_key() {
    let api = FakeApi::start(&[(SECRET, 200, r#"{"name":"SOPS_AGE_KEY"}"#, &[])]);
    assert!(holds_age_key(&http(&api), &C).unwrap());
    let api = FakeApi::start(&[(SECRET, 404, "{}", &[])]);
    assert!(!holds_age_key(&http(&api), &C).unwrap());
    let api = FakeApi::start(&[(SECRET, 403, r#"{"message":"nope"}"#, &[])]);
    assert!(holds_age_key(&http(&api), &C).is_err());
}
