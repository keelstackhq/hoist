mod common;

use common::{FakeApi, RouteSpec};
use hoist::auth::Secret;
use hoist::http::{Http, Service};
use hoist::release::oidc::{crates_token_with_jwt, id_token_from};

const JWT: &str = "canary-jwt-7f3a";
const TOKEN: &str = "canary-crates-token-91b2";

#[test]
fn id_token_appends_audience_and_sends_request_token() {
    let body = format!(r#"{{"value":"{JWT}"}}"#);
    let routes: Vec<RouteSpec> = vec![("GET /oidc", 200, &body, &[])];
    let api = FakeApi::start(&routes);
    let url = format!("{}/oidc?api-version=2.0", api.url());
    let jwt = id_token_from(&url, &Secret::from("req-token"), "crates.io").unwrap();
    assert_eq!(format!("{jwt:?}"), "***");
    let reqs = api.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url, "/oidc?api-version=2.0&audience=crates.io");
    assert_eq!(reqs[0].auth.as_deref(), Some("Bearer req-token"));
}

#[test]
fn id_token_without_value_is_an_error_without_secrets() {
    let routes: Vec<RouteSpec> = vec![("GET /oidc", 200, r#"{"other":1}"#, &[])];
    let api = FakeApi::start(&routes);
    let url = format!("{}/oidc?x=1", api.url());
    let err = id_token_from(&url, &Secret::from("req-token"), "crates.io").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("value"), "{text}");
    assert!(!text.contains("req-token"), "{text}");
}

#[test]
fn crates_token_exchange_then_revoke() {
    let body = format!(r#"{{"token":"{TOKEN}"}}"#);
    let routes: Vec<RouteSpec> = vec![
        ("POST /api/v1/trusted_publishing/tokens", 200, &body, &[]),
        ("DELETE /api/v1/trusted_publishing/tokens", 204, "", &[]),
    ];
    let api = FakeApi::start(&routes);
    let http = Http::with_base(Service::Crates, Secret::from(""), &api.url());
    let (token, guard) = crates_token_with_jwt(&http, &Secret::from(JWT)).unwrap();
    assert_eq!(format!("{token:?}"), "***");
    guard.revoke().unwrap();
    let reqs = api.requests();
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].auth, None);
    let sent: serde_json::Value = serde_json::from_slice(&reqs[0].raw).unwrap();
    assert_eq!(sent, serde_json::json!({ "jwt": JWT }));
    assert!(
        reqs[0]
            .user_agent
            .as_deref()
            .unwrap()
            .contains("keelstackhq/hoist")
    );
    assert_eq!(reqs[1].method, "DELETE");
    assert_eq!(reqs[1].path, "/api/v1/trusted_publishing/tokens");
    assert_eq!(reqs[1].auth.as_deref(), Some(&*format!("Bearer {TOKEN}")));
}

#[test]
fn crates_token_guard_revokes_on_drop() {
    let body = format!(r#"{{"token":"{TOKEN}"}}"#);
    let routes: Vec<RouteSpec> = vec![
        ("POST /api/v1/trusted_publishing/tokens", 200, &body, &[]),
        ("DELETE /api/v1/trusted_publishing/tokens", 500, "", &[]),
    ];
    let api = FakeApi::start(&routes);
    let http = Http::with_base(Service::Crates, Secret::from(""), &api.url());
    let (_token, guard) = crates_token_with_jwt(&http, &Secret::from(JWT)).unwrap();
    drop(guard); // a failed revoke on drop must not panic
    let methods: Vec<String> = api.requests().into_iter().map(|r| r.method).collect();
    assert_eq!(methods, vec!["POST", "DELETE"]);
}

#[test]
fn crates_token_exchange_rejected_names_crates_io() {
    let routes: Vec<RouteSpec> = vec![(
        "POST /api/v1/trusted_publishing/tokens",
        400,
        r#"{"errors":[{"detail":"No matching Trusted Publishing config found"}]}"#,
        &[],
    )];
    let api = FakeApi::start(&routes);
    let http = Http::with_base(Service::Crates, Secret::from(""), &api.url());
    let err = crates_token_with_jwt(&http, &Secret::from(JWT))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("crates.io"), "{err}");
    assert!(
        err.contains("No matching Trusted Publishing config"),
        "{err}"
    );
    assert!(!err.contains(JWT), "{err}");
}
