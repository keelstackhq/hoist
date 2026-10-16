mod common;

use common::{FakeApi, fixture_repo};
use hoist::Action;
use hoist::auth::Secret;
use hoist::cloudflare::read_cloudflare;
use hoist::desired::{Desired, build_desired};
use hoist::diff::diff_cloudflare;
use hoist::http::{Http, Service};
use hoist::report::{HumanStep, Item};

const PROJECT: &str = "GET /accounts/acc/pages/projects/keelstack-landing";
const WORKER: &str = "GET /accounts/acc/workers/scripts/api/settings";

fn desired(fixture: &str, site_toml: &str) -> (tempfile::TempDir, Desired) {
    let dir = fixture_repo(fixture, "https://github.com/acme/x.git");
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[cloudflare]\naccount_id = \"acc\"\n",
    )
    .unwrap();
    if !site_toml.is_empty() {
        std::fs::write(dir.path().join("apps/site/hoist.toml"), site_toml).unwrap();
    }
    let d = build_desired(dir.path()).expect("desired");
    (dir, d)
}

fn run(api: &FakeApi, d: &Desired) -> (Vec<Item>, Vec<HumanStep>) {
    let http =
        Http::with_base(Service::Cloudflare, Secret::from("dummy"), &api.url()).max_sleep_ms(0);
    let live = read_cloudflare(&http, d).expect("read_cloudflare");
    api.assert_only_get();
    diff_cloudflare(d, &live)
}

fn find<'a>(items: &'a [Item], kind: &str, key: &str) -> &'a Item {
    items
        .iter()
        .find(|i| i.kind == kind && i.key == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {items:#?}"))
}

const SITE_TOML: &str =
    "[deploy]\nprovider = \"cloudflare-pages\"\nproject = \"keelstack-landing\"\n";

#[test]
fn existing_project_is_ok() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[(PROJECT, 200, "cloudflare/pages_project.json", &[])]);
    let (items, steps) = run(&api, &d);
    assert!(steps.is_empty());
    // The unrouted `/user/tokens` is a 404: the token item is unknown, not a failure.
    assert_eq!(items.len(), 2, "{items:#?}");
    assert_eq!(
        find(&items, "api_token", "hoist:acme/x:deploy").action,
        Action::Unknown
    );
    let item = find(&items, "pages_project", "keelstack-landing");
    assert_eq!(item.area, "cloudflare");
    assert_eq!(item.action, Action::Ok);
    assert_eq!(item.source, "apps/site/hoist.toml:deploy.project");
}

#[test]
fn missing_project_is_create() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[(PROJECT, 404, "cloudflare/not_found.json", &[])]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "pages_project", "keelstack-landing");
    assert_eq!(item.action, Action::Create);
    assert!(item.reason.contains("missing"), "{}", item.reason);
}

#[test]
fn wrong_production_branch_is_update() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[(PROJECT, 200, "cloudflare/pages_project_develop.json", &[])]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "pages_project", "keelstack-landing");
    assert_eq!(item.action, Action::Update);
    assert!(
        item.reason.contains("develop") && item.reason.contains("main"),
        "{}",
        item.reason
    );
}

#[test]
fn configured_domain_missing_is_create() {
    let toml = format!("{SITE_TOML}domain = \"keelstack.dev\"\n");
    let (_dir, d) = desired("site", &toml);
    let api = FakeApi::start(&[(PROJECT, 200, "cloudflare/pages_project.json", &[])]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "custom_domain", "keelstack.dev");
    assert_eq!(item.action, Action::Create);
    assert_eq!(item.source, "apps/site/hoist.toml:deploy.domain");
    assert_eq!(
        find(&items, "pages_project", "keelstack-landing").action,
        Action::Ok
    );
}

#[test]
fn configured_domain_present_is_ok() {
    let toml = format!("{SITE_TOML}domain = \"keelstack.dev\"\n");
    let (_dir, d) = desired("site", &toml);
    let api = FakeApi::start(&[(PROJECT, 200, "cloudflare/pages_project_domain.json", &[])]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "custom_domain", "keelstack.dev").action,
        Action::Ok
    );
}

#[test]
fn external_turns_create_into_drift() {
    let toml = format!("{SITE_TOML}domain = \"keelstack.dev\"\n");
    let (dir, _) = desired("site", &toml);
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[cloudflare]\naccount_id = \"acc\"\nmanaged = \"external\"\n",
    )
    .unwrap();
    let d = build_desired(dir.path()).expect("desired");
    let api = FakeApi::start(&[(PROJECT, 404, "cloudflare/not_found.json", &[])]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "pages_project", "keelstack-landing").action,
        Action::Drift
    );
    assert_eq!(
        find(&items, "custom_domain", "keelstack.dev").action,
        Action::Drift
    );
}

#[test]
fn missing_worker_is_pending() {
    let (_dir, d) = desired("web", "");
    let api = FakeApi::start(&[(WORKER, 404, "cloudflare/not_found.json", &[])]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "workers_script", "api");
    assert_eq!(item.action, Action::Pending);
    assert_eq!(item.reason, "created by the first deploy");
}

#[test]
fn existing_worker_is_ok() {
    let (_dir, d) = desired("web", "");
    let api = FakeApi::start(&[(WORKER, 200, "cloudflare/worker_settings.json", &[])]);
    let (items, _) = run(&api, &d);
    assert_eq!(find(&items, "workers_script", "api").action, Action::Ok);
}

#[test]
fn forbidden_is_unknown() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[(PROJECT, 403, "cloudflare/forbidden.json", &[])]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "pages_project", "keelstack-landing");
    assert_eq!(item.action, Action::Unknown);
    assert!(
        item.reason.contains("Authentication error"),
        "{}",
        item.reason
    );
}

#[test]
fn forbidden_project_makes_domain_unknown() {
    let toml = format!("{SITE_TOML}domain = \"keelstack.dev\"\n");
    let (_dir, d) = desired("site", &toml);
    let api = FakeApi::start(&[(PROJECT, 403, "cloudflare/forbidden.json", &[])]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "custom_domain", "keelstack.dev").action,
        Action::Unknown
    );
}

// ---- minted deploy token ----

use hoist::timeutil::{format_rfc3339, now_secs};

const TOKENS: &str = "GET /user/tokens";
const TOKEN_KEY: &str = "hoist:acme/x:deploy";

fn tokens_json(expires: Option<i64>) -> String {
    let exp = expires
        .map(|s| format!(r#","expires_on":"{}""#, format_rfc3339(s)))
        .unwrap_or_default();
    format!(
        r#"{{"success":true,"errors":[],"messages":[],"result":[
        {{"id":"t1","name":"{TOKEN_KEY}","status":"active","issued_on":"2026-01-01T00:00:00Z"{exp}}},
        {{"id":"t2","name":"hoist:other/repo:deploy","status":"active"}},
        {{"id":"t3","name":"unrelated","status":"active"}}],
        "result_info":{{"page":1,"total_pages":1}}}}"#
    )
}

fn token_run(api: &FakeApi, d: &Desired) -> (Vec<Item>, hoist::cloudflare::LiveCloudflare) {
    let http =
        Http::with_base(Service::Cloudflare, Secret::from("dummy"), &api.url()).max_sleep_ms(0);
    let live = read_cloudflare(&http, d).expect("read_cloudflare");
    api.assert_only_get();
    let (items, _) = diff_cloudflare(d, &live);
    (items, live)
}

#[test]
fn missing_token_is_create() {
    let (_dir, d) = desired("site", "");
    let list = r#"{"success":true,"errors":[],"messages":[],"result":[{"id":"t3","name":"unrelated","status":"active"}]}"#;
    let api = FakeApi::start(&[
        (PROJECT, 200, "cloudflare/pages_project.json", &[]),
        (TOKENS, 200, list, &[]),
    ]);
    let (items, live) = token_run(&api, &d);
    assert!(live.tokens.as_got().unwrap().is_empty());
    let item = find(&items, "api_token", TOKEN_KEY);
    assert_eq!(item.action, Action::Create);
    assert_eq!(item.area, "cloudflare");
}

#[test]
fn only_this_repos_tokens_are_kept() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[
        (PROJECT, 200, "cloudflare/pages_project.json", &[]),
        (TOKENS, 200, &tokens_json(None), &[]),
    ]);
    let (_, live) = token_run(&api, &d);
    let tokens = live.tokens.as_got().unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].id, "t1");
    assert_eq!(tokens[0].status, "active");
    assert_eq!(tokens[0].issued_on.as_deref(), Some("2026-01-01T00:00:00Z"));
}

#[test]
fn expiring_token_is_update() {
    let (_dir, d) = desired("site", "");
    let soon = now_secs() + 10 * 86400 + 3600;
    let api = FakeApi::start(&[
        (PROJECT, 200, "cloudflare/pages_project.json", &[]),
        (TOKENS, 200, &tokens_json(Some(soon)), &[]),
    ]);
    let (items, _) = token_run(&api, &d);
    let item = find(&items, "api_token", TOKEN_KEY);
    assert_eq!(item.action, Action::Update);
    assert_eq!(
        item.reason,
        "expires in 10 days; run `hoist secrets rotate`"
    );
}

#[test]
fn far_expiry_is_ok() {
    let (_dir, d) = desired("site", "");
    let later = now_secs() + 200 * 86400;
    let api = FakeApi::start(&[
        (PROJECT, 200, "cloudflare/pages_project.json", &[]),
        (TOKENS, 200, &tokens_json(Some(later)), &[]),
    ]);
    let (items, _) = token_run(&api, &d);
    assert_eq!(find(&items, "api_token", TOKEN_KEY).action, Action::Ok);
}

#[test]
fn tokens_forbidden_is_unknown() {
    let (_dir, d) = desired("site", "");
    let api = FakeApi::start(&[
        (PROJECT, 200, "cloudflare/pages_project.json", &[]),
        (TOKENS, 403, "cloudflare/forbidden.json", &[]),
    ]);
    let (items, live) = token_run(&api, &d);
    assert!(live.tokens.as_got().is_err());
    let item = find(&items, "api_token", TOKEN_KEY);
    assert_eq!(item.action, Action::Unknown);
    assert!(
        item.reason.contains("Authentication error"),
        "{}",
        item.reason
    );
}
