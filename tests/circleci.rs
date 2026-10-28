//! The CircleCI context client (spec §15.8.3) against a fake API: the v3 paths, queries and
//! bodies, cursor pagination, exact name matching, and errors that never show the token or a
//! value.

mod common;

use common::FakeApi;
use hoist::auth::Secret;
use hoist::circleci::{self, Restriction};
use hoist::http::{Http, Service};
use serde_json::{Value, json};

const ORG: &str = "0e1f0000-0000-4000-8000-000000000001";
const PROJECT: &str = "9a1b0000-0000-4000-8000-000000000002";
const CTX: &str = "c0de0000-0000-4000-8000-000000000003";

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Circleci, Secret::from("cci-token-xyz"), &api.url()).max_sleep_ms(0)
}

fn urls(api: &FakeApi) -> Vec<String> {
    api.requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.url))
        .collect()
}

fn body(api: &FakeApi, i: usize) -> Value {
    serde_json::from_str(api.requests()[i].body.as_deref().expect("body")).unwrap()
}

#[test]
fn a_project_slug_resolves_to_its_id_and_org() {
    let found = json!({"data": [{"id": PROJECT, "attributes": {"name": "app"},
        "references": {"org": {"id": ORG}}}], "page": {"next": null}})
    .to_string();
    let api = FakeApi::start(&[("GET /api/v3/projects", 200, &found, &[])]);
    let p = circleci::project(&http(&api), "gh/acme/app")
        .unwrap()
        .unwrap();
    assert_eq!((p.id.as_str(), p.org_id.as_str()), (PROJECT, ORG));
    assert_eq!(
        urls(&api),
        ["GET /api/v3/projects?filter%5Bslug%5D=gh%2Facme%2Fapp&page%5Blimit%5D=100"]
    );
    assert_eq!(
        api.requests()[0].auth.as_deref(),
        Some("Bearer cci-token-xyz")
    );
    // No such project: none, not an error.
    let api = FakeApi::start(&[("GET /api/v3/projects", 200, r#"{"data":[]}"#, &[])]);
    assert!(
        circleci::project(&http(&api), "gh/acme/app")
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_context_is_found_by_its_exact_name_across_pages() {
    // The API matches names as a case-insensitive substring: only the exact name counts.
    let page1 = json!({"data": [{"id": "x1", "attributes": {"name": "app-prod-old"}}],
        "page": {"next": "abc+/="}})
    .to_string();
    let page2 = json!({"data": [{"id": CTX, "attributes": {"name": "app-prod"}}],
        "page": {"next": null}})
    .to_string();
    let api = FakeApi::start(&[
        ("GET /api/v3/contexts", 200, &page1, &[]),
        ("GET /api/v3/contexts", 200, &page2, &[]),
    ]);
    let id = circleci::find_context(&http(&api), ORG, "app-prod").unwrap();
    assert_eq!(id.as_deref(), Some(CTX));
    let first = format!(
        "GET /api/v3/contexts?filter%5Borg_id%5D={ORG}&filter%5Bname%5D=app-prod&page%5Blimit%5D=100"
    );
    assert_eq!(
        urls(&api),
        [
            first.clone(),
            format!("{first}&page%5Bcursor%5D=abc%2B%2F%3D")
        ]
    );
    // Only a longer name or another case: not found.
    let near = json!({"data": [{"id": "x1", "attributes": {"name": "App-Prod"}}],
        "page": {"next": ""}})
    .to_string();
    let api = FakeApi::start(&[("GET /api/v3/contexts", 200, &near, &[])]);
    assert_eq!(
        circleci::find_context(&http(&api), ORG, "app-prod").unwrap(),
        None
    );
    // An empty cursor is the last page.
    assert_eq!(api.requests().len(), 1);
}

#[test]
fn contexts_env_vars_and_restrictions_are_written_as_v3_asks() {
    let created = json!({"data": {"id": CTX, "attributes": {"name": "app-prod"}}}).to_string();
    let restriction = json!({"data": {"id": "r1", "attributes":
        {"restriction_type": "project", "match_pattern": PROJECT}}})
    .to_string();
    let api = FakeApi::start(&[
        ("POST /api/v3/contexts", 201, &created, &[]),
        (
            &format!("POST /api/v3/contexts/{CTX}/env-vars/set"),
            200,
            "{}",
            &[],
        ),
        (
            &format!("DELETE /api/v3/contexts/{CTX}/env-vars"),
            204,
            "",
            &[],
        ),
        ("POST /api/v3/context-restrictions", 201, &restriction, &[]),
    ]);
    let http = http(&api);
    assert_eq!(
        circleci::create_context(&http, ORG, "app-prod").unwrap(),
        CTX
    );
    let value = Secret::from("AGE-SECRET-KEY-1TESTONLY");
    circleci::set_env_var(&http, CTX, "SOPS_AGE_KEY", &value).unwrap();
    circleci::delete_env_var(&http, CTX, "SOPS_AGE_KEY").unwrap();
    let r = circleci::add_restriction(&http, CTX, "project", PROJECT).unwrap();
    assert_eq!(
        r,
        Restriction {
            id: "r1".into(),
            kind: "project".into(),
            pattern: PROJECT.into()
        }
    );
    assert_eq!(
        urls(&api),
        [
            "POST /api/v3/contexts".to_string(),
            format!("POST /api/v3/contexts/{CTX}/env-vars/set"),
            format!("DELETE /api/v3/contexts/{CTX}/env-vars?filter%5Bname%5D=SOPS_AGE_KEY"),
            "POST /api/v3/context-restrictions".to_string(),
        ]
    );
    assert_eq!(
        body(&api, 0),
        json!({"data": {"attributes": {"name": "app-prod"}, "references": {"org": {"id": ORG}}}})
    );
    assert_eq!(
        body(&api, 1),
        json!({"name": "SOPS_AGE_KEY", "value": "AGE-SECRET-KEY-1TESTONLY"})
    );
    assert_eq!(
        body(&api, 3),
        json!({"data": {"attributes": {"restriction_type": "project", "match_pattern": PROJECT},
            "references": {"context": {"id": CTX}}}})
    );
}

#[test]
fn env_var_names_and_restrictions_are_read() {
    let vars = json!({"data": [{"id": "v", "attributes": {"name": "SOPS_AGE_KEY",
        "truncated_value": "xxxx"}}]})
    .to_string();
    let restrictions = json!({"data": [
        {"id": "r1", "attributes": {"restriction_type": "project", "match_pattern": PROJECT}},
        {"id": "r2", "attributes": {"restriction_type": "expression",
            "match_pattern": "pipeline.git.branch == \"main\""}}
    ]})
    .to_string();
    let api = FakeApi::start(&[
        (
            &format!("GET /api/v3/contexts/{CTX}/env-vars"),
            200,
            &vars,
            &[],
        ),
        ("GET /api/v3/context-restrictions", 200, &restrictions, &[]),
    ]);
    let http = http(&api);
    assert_eq!(
        circleci::env_var_names(&http, CTX).unwrap(),
        ["SOPS_AGE_KEY"]
    );
    let got = circleci::restrictions(&http, CTX).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].kind, "expression");
    assert_eq!(got[1].pattern, "pipeline.git.branch == \"main\"");
    assert_eq!(
        urls(&api)[1],
        format!(
            "GET /api/v3/context-restrictions?filter%5Bcontext_id%5D={CTX}&page%5Blimit%5D=100"
        )
    );
}

#[test]
fn failures_say_what_failed_without_the_token_or_a_value() {
    let api = FakeApi::start(&[
        (
            "GET /api/v3/projects",
            403,
            r#"{"message":"Permission denied"}"#,
            &[],
        ),
        (
            &format!("POST /api/v3/contexts/{CTX}/env-vars/set"),
            500,
            "{}",
            &[],
        ),
        ("GET /api/v3/contexts", 200, r#"{"data":[{"id":7}]}"#, &[]),
    ]);
    let client = http(&api);
    let errors = [
        circleci::project(&client, "gh/acme/app")
            .unwrap_err()
            .to_string(),
        circleci::set_env_var(
            &client,
            CTX,
            "SOPS_AGE_KEY",
            &Secret::from("AGE-SECRET-KEY-1VALUE"),
        )
        .unwrap_err()
        .to_string(),
        // A context entry without a string id or name is an error, not a skip.
        circleci::find_context(&client, ORG, "app-prod")
            .unwrap_err()
            .to_string(),
    ];
    assert!(errors[0].contains("Permission denied"), "{errors:?}");
    assert!(errors[1].contains("CircleCI"), "{errors:?}");
    for e in &errors {
        assert!(
            !e.contains("cci-token-xyz") && !e.contains("AGE-SECRET-KEY"),
            "{e}"
        );
    }
    // A 401 names the variable to fix.
    let api = FakeApi::start(&[("GET /api/v3/projects", 401, "{}", &[])]);
    let e = circleci::project(&http(&api), "gh/acme/app").unwrap_err();
    assert!(e.to_string().contains("CIRCLECI_TOKEN"), "{e}");
}
