mod common;

use common::{FakeApi, fixture_repo};
use hoist::auth::Secret;
use hoist::change::Change;
use hoist::cloudflare_write::execute_cloudflare;
use hoist::desired::{Desired, build_desired};
use hoist::http::{Http, Service};
use serde_json::{Value, json};

fn desired() -> (tempfile::TempDir, Desired) {
    let dir = fixture_repo("site", "https://github.com/acme/x.git");
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[cloudflare]\naccount_id = \"acc\"\n",
    )
    .unwrap();
    let d = build_desired(dir.path()).expect("desired");
    (dir, d)
}

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Cloudflare, Secret::from("dummy"), &api.url()).max_sleep_ms(0)
}

fn body(api: &FakeApi, index: usize) -> Value {
    let requests = api.requests();
    serde_json::from_str(requests[index].body.as_deref().expect("body")).unwrap()
}

fn project(name: &str, exists: bool) -> Change {
    Change::PagesProject {
        name: name.into(),
        production_branch: "main".into(),
        exists,
    }
}

const OK: &str = r#"{"success":true,"errors":[],"messages":[],"result":{}}"#;

#[test]
fn create_project_post_body() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("POST /accounts/acc/pages/projects", 200, OK, &[])]);
    let step = execute_cloudflare(&http(&api), &d, &project("site", false)).unwrap();
    assert!(step.is_none());
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/accounts/acc/pages/projects");
    assert_eq!(
        body(&api, 0),
        json!({"name": "site", "production_branch": "main"})
    );
}

#[test]
fn update_production_branch_patch() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("PATCH /accounts/acc/pages/projects/site", 200, OK, &[])]);
    execute_cloudflare(&http(&api), &d, &project("site", true)).unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PATCH");
    assert_eq!(requests[0].path, "/accounts/acc/pages/projects/site");
    assert_eq!(body(&api, 0), json!({"production_branch": "main"}));
}

#[test]
fn project_name_is_encoded() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[("PATCH /accounts/acc/pages/projects/a%20b", 200, OK, &[])]);
    execute_cloudflare(&http(&api), &d, &project("a b", true)).unwrap();
    assert_eq!(api.requests()[0].path, "/accounts/acc/pages/projects/a%20b");
}

fn domain() -> Change {
    Change::Domain {
        project: "site".into(),
        host: "example.com".into(),
    }
}

#[test]
fn attach_domain_post() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[(
        "POST /accounts/acc/pages/projects/site/domains",
        200,
        OK,
        &[],
    )]);
    let step = execute_cloudflare(&http(&api), &d, &domain()).unwrap();
    assert!(step.is_none());
    let requests = api.requests();
    assert_eq!(
        requests[0].path,
        "/accounts/acc/pages/projects/site/domains"
    );
    assert_eq!(body(&api, 0), json!({"name": "example.com"}));
}

#[test]
fn domain_without_zone_is_human_step() {
    let (_dir, d) = desired();
    let err = r#"{"success":false,"errors":[{"code":8000018,"message":"The zone for example.com is not on this Account"}],"messages":[],"result":null}"#;
    let api = FakeApi::start(&[(
        "POST /accounts/acc/pages/projects/site/domains",
        400,
        err,
        &[],
    )]);
    let step = execute_cloudflare(&http(&api), &d, &domain())
        .unwrap()
        .expect("human step");
    assert_eq!(step.id, "domain:example.com");
    assert_eq!(
        step.title,
        "add example.com's zone to Cloudflare or point a CNAME to site.pages.dev"
    );
    assert_eq!(step.url, "https://dash.cloudflare.com/");
    assert_eq!(step.status, "pending");
}

#[test]
fn other_domain_rejection_is_an_error() {
    let (_dir, d) = desired();
    let err = r#"{"success":false,"errors":[{"code":1,"message":"Invalid input"}],"messages":[],"result":null}"#;
    let api = FakeApi::start(&[(
        "POST /accounts/acc/pages/projects/site/domains",
        400,
        err,
        &[],
    )]);
    let e = execute_cloudflare(&http(&api), &d, &domain()).unwrap_err();
    assert!(e.to_string().contains("Invalid input"), "{e}");
}

#[test]
fn missing_account_id_is_usage_before_any_request() {
    let (_dir, mut d) = desired();
    d.cloudflare.account_id = None;
    let api = FakeApi::start(&[]);
    let e = execute_cloudflare(&http(&api), &d, &project("site", false)).unwrap_err();
    assert!(matches!(e, hoist::HoistError::Usage(_)), "{e}");
    assert!(api.requests().is_empty());
}

#[test]
fn non_cloudflare_change_is_usage() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[]);
    let e = execute_cloudflare(&http(&api), &d, &Change::AccountIdSecret).unwrap_err();
    assert_eq!(e.to_string(), "not a Cloudflare change");
}
