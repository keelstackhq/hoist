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
    assert_eq!(items.len(), 1, "{items:#?}");
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
