mod common;

use common::FakeApi;
use hoist::auth::Secret;
use hoist::http::{Fetched, Http, HttpError, Method, Service, Written, seg};
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
    // A rejected token is an error, never a degraded "unknown" (it would read as in sync).
    let err = http.get_json("/nope").unwrap_err();
    assert!(matches!(err, HttpError::Unauthorized { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "GitHub rejected the token (401); check GITHUB_TOKEN/GH_TOKEN"
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
fn sends_bearer_token() {
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
            .all(|r| r.auth.as_deref() == Some("Bearer tok"))
    );
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
    assert_eq!(api.requests().len(), 4);
    api.assert_only_get();
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

#[test]
fn cloudflare_stuck_page_terminates_with_error() {
    // The server always answers page 1 of 2, whatever was asked.
    let api = FakeApi::start(&[("GET /cf", 200, "cf_page1.json", &[])]);
    let http = Http::with_base(Service::Cloudflare, Secret::from("cf"), &api.url());
    let err = http.get_all("/cf", None).unwrap_err().to_string();
    assert!(err.contains("Cloudflare") && err.contains("/cf"), "{err}");
    assert!(api.requests().len() <= 3, "{:?}", api.requests());
}

#[test]
fn cloudflare_page_cap_is_an_error() {
    let api = FakeApi::start(&[("GET /cf", 200, "cf_page1.json", &[])]);
    let http = Http::with_base(Service::Cloudflare, Secret::from("cf"), &api.url()).max_pages(1);
    let err = http.get_all("/cf", None).unwrap_err().to_string();
    assert!(err.contains("Cloudflare") && err.contains("/cf"), "{err}");
}

#[test]
fn github_page_cap_is_an_error() {
    let api = FakeApi::start(&[(
        "GET /loop",
        200,
        "items_page2.json",
        &[("Link", r#"</loop?page=2>; rel="next""#)],
    )]);
    let http = github(&api).max_pages(3);
    let err = http.get_all("/loop", None).unwrap_err().to_string();
    assert!(err.contains("GitHub") && err.contains("/loop"), "{err}");
    assert_eq!(api.requests().len(), 3);
}

#[test]
fn github_foreign_link_is_an_error() {
    let api = FakeApi::start(&[(
        "GET /l",
        200,
        "items_page2.json",
        &[(
            "Link",
            r#"<https://elsewhere.example/l?page=2>; rel="next""#,
        )],
    )]);
    let err = github(&api).get_all("/l", None).unwrap_err().to_string();
    assert!(err.contains("GitHub") && err.contains("/l"), "{err}");
    assert_eq!(api.requests().len(), 1);
}

#[test]
fn cloudflare_401_names_the_cloudflare_token() {
    let api = FakeApi::start(&[("GET /x", 401, "cloudflare/forbidden.json", &[])]);
    let http = Http::with_base(Service::Cloudflare, Secret::from("tok"), &api.url());
    let err = http.get_all("/x", None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Cloudflare rejected the token (401); check CLOUDFLARE_API_TOKEN"
    );
}

#[test]
fn put_returns_written_with_body() {
    let api = FakeApi::start(&[("PUT /x", 201, "created.json", &[])]);
    let sent = json!({"enabled": true});
    let got = github(&api)
        .send_json(Method::Put, "/x", Some(&sent))
        .unwrap();
    assert_eq!(
        got,
        Written {
            status: 201,
            body: Some(json!({"id":1}))
        }
    );
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    let logged: serde_json::Value =
        serde_json::from_str(requests[0].body.as_deref().expect("body")).unwrap();
    assert_eq!(logged, sent);
}

#[test]
fn delete_204_has_no_body() {
    let api = FakeApi::start(&[("DELETE /x", 204, "", &[])]);
    let got = github(&api).send_json(Method::Delete, "/x", None).unwrap();
    assert_eq!(
        got,
        Written {
            status: 204,
            body: None
        }
    );
}

#[test]
fn post_is_not_retried() {
    let api = FakeApi::start(&[("POST /x", 429, "rate_limited.json", &[("Retry-After", "0")])]);
    let err = github(&api)
        .send_json(Method::Post, "/x", Some(&json!({})))
        .unwrap_err();
    assert!(
        matches!(err, HttpError::Rejected { status: 429, .. }),
        "{err:?}"
    );
    assert_eq!(api.requests().len(), 1);
}

#[test]
fn put_is_retried_once_after_429() {
    let api = FakeApi::start(&[
        ("PUT /x", 429, "rate_limited.json", &[("Retry-After", "0")]),
        ("PUT /x", 200, "empty.json", &[]),
    ]);
    let got = github(&api)
        .send_json(Method::Put, "/x", Some(&json!({})))
        .unwrap();
    assert_eq!(got.status, 200);
    assert_eq!(api.requests().len(), 2);
}

#[test]
fn rejected_message_never_contains_request_body() {
    let api = FakeApi::start(&[("PUT /x", 422, "unprocessable.json", &[])]);
    let err = github(&api)
        .send_json(
            Method::Put,
            "/x?sig=SECRETQUERY",
            Some(&json!({"value":"hoist-canary-body"})),
        )
        .unwrap_err();
    let text = err.to_string();
    assert!(!text.contains("hoist-canary-body"), "{text}");
    assert!(!text.contains("SECRETQUERY"), "{text}");
    assert!(
        text.contains("GitHub")
            && text.contains("PUT /x")
            && text.contains("422")
            && text.contains("Validation Failed"),
        "{text}"
    );
}

#[test]
fn write_401_is_unauthorized() {
    let api = FakeApi::start(&[("PATCH /x", 401, "empty.json", &[])]);
    let err = github(&api)
        .send_json(Method::Patch, "/x", Some(&json!({})))
        .unwrap_err();
    assert!(matches!(err, HttpError::Unauthorized { .. }), "{err:?}");
}

#[test]
fn seg_encodes_reserved() {
    assert_eq!(seg("a/b c?"), "a%2Fb%20c%3F");
    assert_eq!(seg("keelstack-landing"), "keelstack-landing");
    assert_eq!(seg("A-z_0.9~"), "A-z_0.9~");
    assert_eq!(seg("é"), "%C3%A9");
}

#[test]
fn write_redirect_is_rejected_and_not_followed() {
    let api = FakeApi::start(&[("PATCH /x", 301, "", &[("Location", "/moved")])]);
    let err = github(&api)
        .send_json(Method::Patch, "/x", Some(&json!({"a": 1})))
        .unwrap_err();
    assert!(
        matches!(err, HttpError::Rejected { status: 301, .. }),
        "{err:?}"
    );
    let requests = api.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests.iter().all(|r| r.path != "/moved"));
}
