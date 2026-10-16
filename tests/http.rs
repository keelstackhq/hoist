mod common;

use common::FakeApi;
use hoist::auth::Secret;
use hoist::http::{Fetched, Http, Service};
use serde_json::json;

fn github(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, Secret::from("tok"), &api.url()).max_sleep_ms(0)
}

#[test]
fn paginates_github_link_header() {
    let api = FakeApi::start(&[
        (
            "GET /repos/acme/x/items",
            200,
            "items_page1.json",
            &[("Link", r#"<{base}/repos/acme/x/items?page=2>; rel="next""#)],
        ),
        (
            "GET /repos/acme/x/items?page=2",
            200,
            "items_page2.json",
            &[],
        ),
    ]);
    let got = github(&api).get_all("/repos/acme/x/items", None).unwrap();
    assert_eq!(got, Fetched::Ok(json!([{"id":1},{"id":2},{"id":3}])));
    assert_eq!(api.requests().len(), 2);
}

#[test]
fn retries_after_429() {
    let api = FakeApi::start(&[
        ("GET /r", 429, "rate_limited.json", &[("Retry-After", "1")]),
        ("GET /r", 200, "empty.json", &[]),
    ]);
    let got = github(&api).get_json("/r").unwrap();
    assert_eq!(got, Fetched::Ok(json!({})));
    assert_eq!(api.requests().len(), 2);
}

#[test]
fn gives_up_after_three_rate_limited_attempts() {
    let api = FakeApi::start(&[("GET /r", 429, "rate_limited.json", &[("Retry-After", "1")])]);
    let got = github(&api).get_json("/r").unwrap();
    assert_eq!(got, Fetched::Forbidden("rate limited".into()));
    assert_eq!(api.requests().len(), 3);
}

#[test]
fn forbidden_carries_hint() {
    let api = FakeApi::start(&[("GET /r", 403, "forbidden_pat.json", &[])]);
    let got = github(&api).get_json("/r").unwrap();
    assert_eq!(
        got,
        Fetched::Forbidden("Resource not accessible by personal access token".into())
    );
    assert_eq!(api.requests().len(), 1);
}

#[test]
fn not_found_and_unauthorized_and_unmatched() {
    let api = FakeApi::start(&[
        ("GET /gone", 404, "empty.json", &[]),
        ("GET /nope", 401, "empty.json", &[]),
    ]);
    let http = github(&api);
    assert_eq!(http.get_json("/gone").unwrap(), Fetched::NotFound);
    assert_eq!(
        http.get_json("/nope").unwrap(),
        Fetched::Forbidden("token rejected (401)".into())
    );
    assert_eq!(http.get_json("/unrouted").unwrap(), Fetched::NotFound);
    assert_eq!(api.requests().len(), 3);
}

#[test]
fn cloudflare_pages_are_concatenated_and_forbidden_hint_read() {
    let api = FakeApi::start(&[
        ("GET /accounts/a/projects", 200, "cf_page1.json", &[]),
        ("GET /accounts/a/projects?page=2", 200, "cf_page2.json", &[]),
        ("GET /bad", 403, "cf_forbidden.json", &[]),
    ]);
    let http = Http::with_base(Service::Cloudflare, Secret::from("cf"), &api.url());
    let got = http.get_all("/accounts/a/projects", None).unwrap();
    assert_eq!(
        got,
        Fetched::Ok(json!([{"name":"a"},{"name":"b"},{"name":"c"}]))
    );
    assert_eq!(
        http.get_json("/bad").unwrap(),
        Fetched::Forbidden("Authentication error".into())
    );
}

#[test]
fn sends_bearer_token_and_only_get_requests() {
    let api = FakeApi::start(&[
        ("GET /a", 200, "empty.json", &[]),
        ("GET /list", 200, "items_page2.json", &[]),
    ]);
    let http = github(&api);
    http.get_json("/a").unwrap();
    http.get_all("/list", None).unwrap();
    assert!(
        api.requests()
            .iter()
            .all(|(_, _, auth)| auth.as_deref() == Some("Bearer tok"))
    );
    api.assert_only_get();
}

#[test]
fn only_get_requests() {
    let api = FakeApi::start(&[
        ("GET /one", 200, "empty.json", &[]),
        ("GET /many", 200, "items_page2.json", &[]),
        ("GET /secret", 403, "forbidden_pat.json", &[]),
    ]);
    let http = github(&api);
    http.get_json("/one").unwrap();
    http.get_all("/many", None).unwrap();
    http.get_json("/secret").unwrap();
    http.get_json("/missing").unwrap();
    let requests = api.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|(method, _, _)| method == "GET"));
}

#[test]
fn transport_error_names_service_and_path_only() {
    let http = Http::with_base(
        Service::Github,
        Secret::from("hoist-canary-7f3a"),
        "http://127.0.0.1:9",
    );
    let err = http.get_json("/repos/acme/x").unwrap_err().to_string();
    assert!(
        err.contains("GitHub") && err.contains("/repos/acme/x"),
        "{err}"
    );
    assert!(!err.contains("hoist-canary"), "{err}");
}

#[test]
fn secret_debug_is_redacted() {
    let secret = Secret::from("s3cr3t");
    assert_eq!(format!("{secret:?}"), "***");
    assert_eq!(format!("{secret}"), "***");
}
