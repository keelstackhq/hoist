mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, RouteSpec, fixture_repo};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use hoist::auth::Secret;
use hoist::change::Change;
use hoist::cloudflare_write::{execute_cloudflare, revoke_token};
use hoist::desired::{Desired, build_desired};
use hoist::http::{Http, Service};
use hoist::timeutil::{now_secs, parse_rfc3339};
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
    let step = execute_cloudflare(&http(&api), &http(&api), &d, &project("site", false)).unwrap();
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
    execute_cloudflare(&http(&api), &http(&api), &d, &project("site", true)).unwrap();
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
    execute_cloudflare(&http(&api), &http(&api), &d, &project("a b", true)).unwrap();
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
    let step = execute_cloudflare(&http(&api), &http(&api), &d, &domain()).unwrap();
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
    let step = execute_cloudflare(&http(&api), &http(&api), &d, &domain())
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
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &domain()).unwrap_err();
    assert!(e.to_string().contains("Invalid input"), "{e}");
}

#[test]
fn missing_account_id_is_usage_before_any_request() {
    let (_dir, mut d) = desired();
    d.cloudflare.account_id = None;
    let api = FakeApi::start(&[]);
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &project("site", false)).unwrap_err();
    assert!(matches!(e, hoist::HoistError::Usage(_)), "{e}");
    assert!(api.requests().is_empty());
}

#[test]
fn non_cloudflare_change_is_usage() {
    let (_dir, d) = desired();
    let api = FakeApi::start(&[]);
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &Change::AccountIdSecret).unwrap_err();
    assert_eq!(e.to_string(), "not a Cloudflare change");
}

fn zone_rejection(status: u16, msg: &str) -> FakeApi {
    let err = format!(
        r#"{{"success":false,"errors":[{{"code":1,"message":"{msg}"}}],"messages":[],"result":null}}"#
    );
    FakeApi::start(&[(
        "POST /accounts/acc/pages/projects/site/domains",
        status,
        &err,
        &[],
    )])
}

#[test]
fn forbidden_zone_message_is_an_error_not_a_human_step() {
    let (_dir, d) = desired();
    let api = zone_rejection(403, "Token has no access to the zone");
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &domain()).unwrap_err();
    assert!(e.to_string().contains("(403)"), "{e}");
}

#[test]
fn unauthorized_zone_message_is_an_error() {
    let (_dir, d) = desired();
    let api = zone_rejection(401, "zone");
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &domain());
    assert!(e.is_err());
}

#[test]
fn server_error_zone_message_is_an_error() {
    let (_dir, d) = desired();
    let api = zone_rejection(500, "zone lookup failed");
    let e = execute_cloudflare(&http(&api), &http(&api), &d, &domain()).unwrap_err();
    assert!(e.to_string().contains("(500)"), "{e}");
}

// ---- deploy token ----

const MINTED: &str = "hoist-canary-mint";
const GROUPS: &str = "GET /user/tokens/permission_groups";
const MINT: &str = "POST /user/tokens";
const KEY_PATH: &str = "GET /repos/acme/x/actions/secrets/public-key";
const PUT_PATH: &str = "PUT /repos/acme/x/actions/secrets/CLOUDFLARE_API_TOKEN";
const NAME: &str = "hoist:acme/x:deploy";

const GROUPS_BODY: &str = r#"{"success":true,"errors":[],"messages":[],"result":[
  {"id":"g-read","name":"Pages Read"},
  {"id":"g-pages","name":"Pages Write"},
  {"id":"g-workers","name":"Workers Scripts Write"}]}"#;

fn minted_body() -> String {
    format!(
        r#"{{"success":true,"errors":[],"messages":[],"result":{{"id":"new-id","name":"{NAME}","value":"{MINTED}"}}}}"#
    )
}

fn gh_http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, Secret::from("dummy"), &api.url()).max_sleep_ms(0)
}

fn keypair() -> (SecretKey, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    (sk, format!(r#"{{"key_id":"kid-1","key":"{b64}"}}"#))
}

fn deploy(replace: &[&str]) -> Change {
    Change::DeployToken {
        name: NAME.into(),
        replace_ids: replace.iter().map(|s| s.to_string()).collect(),
    }
}

fn cf_api(extra: &[RouteSpec]) -> FakeApi {
    let minted = minted_body();
    let mut routes: Vec<RouteSpec> =
        vec![(GROUPS, 200, GROUPS_BODY, &[]), (MINT, 200, &minted, &[])];
    routes.extend_from_slice(extra);
    FakeApi::start(&routes)
}

#[test]
fn mint_body_and_scope() {
    let (_dir, d) = desired();
    let cf = cf_api(&[]);
    let (id, value) = hoist::cloudflare_write::mint_deploy_token(&http(&cf), &d, NAME).unwrap();
    assert_eq!(id, "new-id");
    assert_eq!(format!("{value:?}"), "***");
    let requests = cf.requests();
    assert_eq!(requests[0].path, "/user/tokens/permission_groups");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/user/tokens");
    let b = body(&cf, 1);
    assert_eq!(b["name"], NAME);
    let policy = &b["policies"][0];
    assert_eq!(policy["effect"], "allow");
    assert_eq!(policy["resources"]["com.cloudflare.api.account.acc"], "*");
    assert_eq!(policy["permission_groups"], json!([{"id": "g-pages"}]));
    let expires = parse_rfc3339(b["expires_on"].as_str().unwrap()).unwrap();
    let want = now_secs() + 365 * 86400;
    assert!((expires - want).abs() < 120, "{expires} vs {want}");
    for r in &requests {
        assert!(!r.body.clone().unwrap_or_default().contains(MINTED));
    }
}

#[test]
fn missing_permission_group_names_it() {
    let (_dir, d) = desired();
    let groups =
        r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"g","name":"Pages Read"}]}"#;
    let cf = FakeApi::start(&[(GROUPS, 200, groups, &[])]);
    let e = hoist::cloudflare_write::mint_deploy_token(&http(&cf), &d, NAME).unwrap_err();
    assert!(e.to_string().contains("Pages Write"), "{e}");
    assert_eq!(cf.requests().len(), 1, "no POST without the group");
}

#[test]
fn minted_value_only_reaches_github_sealed() {
    let (_dir, d) = desired();
    let (sk, key_json) = keypair();
    let cf = cf_api(&[("DELETE /user/tokens/old-1", 200, OK, &[])]);
    let gh = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 201, "empty.json", &[]),
    ]);
    let step = execute_cloudflare(&http(&cf), &gh_http(&gh), &d, &deploy(&["old-1"])).unwrap();
    assert!(step.is_none());
    let put = gh
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    let b: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    let sealed = STANDARD
        .decode(b["encrypted_value"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        String::from_utf8(sk.unseal(&sealed).unwrap()).unwrap(),
        MINTED
    );
    for r in cf.requests().iter().chain(gh.requests().iter()) {
        let text = r.body.clone().unwrap_or_default();
        assert!(!text.contains(MINTED), "{} {}", r.method, r.path);
        assert!(!r.path.contains(MINTED));
    }
}

#[test]
fn old_tokens_revoked_after_success() {
    let (_dir, d) = desired();
    let (_sk, key_json) = keypair();
    let cf = cf_api(&[
        ("DELETE /user/tokens/old-1", 200, OK, &[]),
        (
            "DELETE /user/tokens/old%202",
            404,
            "cloudflare/not_found.json",
            &[],
        ),
    ]);
    let gh = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 201, "empty.json", &[]),
    ]);
    execute_cloudflare(&http(&cf), &gh_http(&gh), &d, &deploy(&["old-1", "old 2"])).unwrap();
    let seq: Vec<String> = cf
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        seq,
        [
            "GET /user/tokens/permission_groups",
            "POST /user/tokens",
            "DELETE /user/tokens/old-1",
            "DELETE /user/tokens/old%202",
        ]
    );
}

#[test]
fn secret_write_failure_revokes_new_token() {
    let (_dir, d) = desired();
    let (_sk, key_json) = keypair();
    let cf = cf_api(&[("DELETE /user/tokens/new-id", 200, OK, &[])]);
    let gh = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 403, "github/forbidden.json", &[]),
        (PUT_PATH, 403, "github/forbidden.json", &[]),
    ]);
    let e = execute_cloudflare(&http(&cf), &gh_http(&gh), &d, &deploy(&["old-1"])).unwrap_err();
    assert!(e.to_string().contains("403"), "{e}");
    assert!(!e.to_string().contains(MINTED));
    let deletes: Vec<String> = cf
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(deletes, ["/user/tokens/new-id"], "old tokens stay");
}

#[test]
fn failed_revoke_names_the_new_token_id_never_the_value() {
    let (_dir, d) = desired();
    let (_sk, key_json) = keypair();
    let cf = cf_api(&[(
        "DELETE /user/tokens/new-id",
        500,
        "cloudflare/forbidden.json",
        &[],
    )]);
    let gh = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 403, "github/forbidden.json", &[]),
    ]);
    let e = execute_cloudflare(&http(&cf), &gh_http(&gh), &d, &deploy(&[])).unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("new-id") && text.contains("by hand"),
        "{text}"
    );
    assert!(!text.contains(MINTED));
}

#[test]
fn revoke_change_deletes_and_404_is_success() {
    let (_dir, d) = desired();
    let cf = FakeApi::start(&[(
        "DELETE /user/tokens/gone",
        404,
        "cloudflare/not_found.json",
        &[],
    )]);
    let change = Change::RevokeToken {
        id: "gone".into(),
        name: "hoist:acme/x:old".into(),
    };
    assert!(
        execute_cloudflare(&http(&cf), &http(&cf), &d, &change)
            .unwrap()
            .is_none()
    );
    revoke_token(&http(&cf), "gone").unwrap();
}

#[test]
fn revoke_other_failure_is_an_error() {
    let cf = FakeApi::start(&[(
        "DELETE /user/tokens/x",
        500,
        "cloudflare/forbidden.json",
        &[],
    )]);
    assert!(revoke_token(&http(&cf), "x").is_err());
}

fn mint_response(result: &str) -> String {
    format!(r#"{{"success":true,"errors":[],"messages":[],"result":{result}}}"#)
}

#[test]
fn mint_without_value_revokes_the_returned_id() {
    let (_dir, d) = desired();
    let minted = mint_response(r#"{"id":"new-id"}"#);
    let cf = FakeApi::start(&[
        (GROUPS, 200, GROUPS_BODY, &[]),
        (MINT, 200, &minted, &[]),
        ("DELETE /user/tokens/new-id", 200, OK, &[]),
    ]);
    let e = hoist::cloudflare_write::mint_deploy_token(&http(&cf), &d, NAME).unwrap_err();
    assert!(e.to_string().contains("no token value"), "{e}");
    assert!(
        cf.requests()
            .iter()
            .any(|r| r.method == "DELETE" && r.path == "/user/tokens/new-id")
    );
}

#[test]
fn mint_without_id_says_a_token_may_exist() {
    let (_dir, d) = desired();
    let minted = mint_response(r#"{"value":"hoist-canary-mint"}"#);
    let cf = FakeApi::start(&[(GROUPS, 200, GROUPS_BODY, &[]), (MINT, 200, &minted, &[])]);
    let e = hoist::cloudflare_write::mint_deploy_token(&http(&cf), &d, NAME).unwrap_err();
    let text = e.to_string();
    assert!(text.contains("may exist") && text.contains(NAME), "{text}");
    assert!(!text.contains(MINTED));
    assert!(cf.requests().iter().all(|r| r.method != "DELETE"));
}

#[test]
fn mint_transport_error_says_a_token_may_have_been_created() {
    use std::io::{Read, Write};
    let (_dir, d) = desired();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap();
            if buf[..n].starts_with(b"GET") {
                let body = GROUPS_BODY;
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            } // a POST: close without answering
        }
    });
    let cf = Http::with_base(Service::Cloudflare, Secret::from("dummy"), &url).max_sleep_ms(0);
    let e = hoist::cloudflare_write::mint_deploy_token(&cf, &d, NAME).unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("may have been created") && text.contains("next `apply` replaces it"),
        "{text}"
    );
    server.join().unwrap();
}

#[test]
fn old_revoke_failure_after_seal_says_the_token_was_written() {
    let (_dir, d) = desired();
    let (_sk, key_json) = keypair();
    let cf = cf_api(&[(
        "DELETE /user/tokens/old-1",
        500,
        "cloudflare/forbidden.json",
        &[],
    )]);
    let gh = FakeApi::start(&[
        (KEY_PATH, 200, &key_json, &[]),
        (PUT_PATH, 201, "empty.json", &[]),
    ]);
    let e = execute_cloudflare(&http(&cf), &gh_http(&gh), &d, &deploy(&["old-1"])).unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("written and sealed") && text.contains("only the revocation of old-1 failed"),
        "{text}"
    );
    assert!(!text.contains(MINTED));
}
