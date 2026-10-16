mod common;

use common::{FakeApi, fixture_repo};
use hoist::Action;
use hoist::auth::Secret;
use hoist::desired::{Desired, build_desired};
use hoist::diff::diff_github;
use hoist::github::{Fetch, LiveGithub, read_github};
use hoist::http::{Http, Service};
use hoist::report::{HumanStep, Item};

type Route = (
    &'static str,
    u16,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const SECRETS_LINK: &[(&str, &str)] = &[(
    "Link",
    r#"<{base}/repos/acme/x/actions/secrets?page=2>; rel="next""#,
)];

/// Routes under which the `site` fixture is fully in sync.
fn in_sync_routes() -> Vec<Route> {
    vec![
        ("GET /repos/acme/x", 200, "github/repo.json", &[]),
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets/11",
            200,
            "github/ruleset_ok.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            "github/actions_permissions.json",
            &[],
        ),
        (
            "GET /repos/acme/x/environments",
            200,
            "github/environments_empty.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_page1.json",
            SECRETS_LINK,
        ),
        (
            "GET /repos/acme/x/actions/secrets?page=2",
            200,
            "github/secrets_page2.json",
            &[],
        ),
        (
            "GET /user/installations",
            200,
            "github/installations.json",
            &[],
        ),
        (
            "GET /user/installations/501/repositories",
            200,
            "github/installation_repos.json",
            &[],
        ),
    ]
}

/// Start the fake with the in-sync routes, replacing every route whose key an override names.
fn fake_with(overrides: &[Route]) -> FakeApi {
    let mut routes: Vec<Route> = in_sync_routes()
        .into_iter()
        .filter(|(key, ..)| !overrides.iter().any(|(k, ..)| k == key))
        .collect();
    routes.extend_from_slice(overrides);
    FakeApi::start(&routes)
}

fn desired(extra_toml: &str) -> (tempfile::TempDir, Desired) {
    let dir = fixture_repo("site", "https://github.com/acme/x.git");
    let toml = format!("[cloudflare]\naccount_id = \"acc\"\n{extra_toml}");
    std::fs::write(dir.path().join("hoist.toml"), toml).unwrap();
    let d = build_desired(dir.path()).expect("desired");
    (dir, d)
}

fn http(api: &FakeApi) -> Http {
    Http::with_base(Service::Github, Secret::from("dummy"), &api.url()).max_sleep_ms(0)
}

/// Cloudflare read with a readable (empty) token list, so the token secret is producible.
fn cf_tokens_read() -> hoist::cloudflare::LiveCloudflare {
    hoist::cloudflare::LiveCloudflare {
        pages: Vec::new(),
        workers: Vec::new(),
        tokens: Fetch::Got(Vec::new()),
    }
}

fn run(api: &FakeApi, d: &Desired) -> (Vec<Item>, Vec<HumanStep>) {
    let live = read_github(&http(api), d).expect("read_github");
    api.assert_only_get();
    diff_github(d, &live, Some(&cf_tokens_read()))
}

fn find<'a>(items: &'a [Item], kind: &str, key: &str) -> &'a Item {
    items
        .iter()
        .find(|i| i.kind == kind && i.key == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {items:#?}"))
}

#[test]
fn in_sync_repo_is_all_ok() {
    let (_dir, d) = desired("");
    let api = fake_with(&[]);
    let (items, steps) = run(&api, &d);
    assert!(!items.is_empty());
    for item in &items {
        assert_eq!(item.action, Action::Ok, "{item:#?}");
    }
    assert!(steps.is_empty());
    for (kind, key) in [
        ("repo_setting", "allow_auto_merge"),
        ("repo_setting", "delete_branch_on_merge"),
        ("ruleset", "hoist: default branch"),
        ("actions_permissions", "default"),
        ("secret", "CLOUDFLARE_ACCOUNT_ID"),
        ("secret", "CLOUDFLARE_API_TOKEN"),
        ("app", "renovate"),
    ] {
        let item = find(&items, kind, key);
        assert_eq!(item.area, "github");
        assert!(!item.source.is_empty());
    }
}

#[test]
fn missing_ruleset_is_create() {
    let (_dir, d) = desired("");
    let api = fake_with(&[(
        "GET /repos/acme/x/rulesets",
        200,
        "github/empty_list.json",
        &[],
    )]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Create);
    assert_eq!(item.source, ".github/workflows/ci.yml:98");
}

#[test]
fn ruleset_with_other_check_is_update() {
    let (_dir, d) = desired("");
    let api = fake_with(&[
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_other_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets/12",
            200,
            "github/ruleset_other_check.json",
            &[],
        ),
    ]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Update);
    assert!(item.reason.contains("ci-ok"), "{}", item.reason);
    assert!(item.reason.contains("`ci`"), "{}", item.reason);
}

#[test]
fn merge_queue_config_false_overrides_the_trigger() {
    let (_dir, d) = desired("[github]\nmerge_queue = false\n");
    assert!(!d.github.ruleset.merge_queue.value);
    assert_eq!(
        d.github.ruleset.merge_queue.source,
        "hoist.toml:github.merge_queue"
    );
    let api = fake_with(&[]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Update);
    assert_eq!(item.reason, "merge queue: live on, want off");
}

#[test]
fn merge_queue_inferred_from_merge_group_trigger() {
    let (_dir, d) = desired("");
    assert!(d.github.ruleset.merge_queue.value);
    assert_eq!(
        d.github.ruleset.merge_queue.source,
        ".github/workflows/ci.yml:5"
    );
    // Live ruleset has the merge queue on (ruleset_ok.json): in sync.
    let api = fake_with(&[]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Ok, "{item:#?}");
}

#[test]
fn merge_queue_defaults_off_without_trigger() {
    let (dir, _) = desired("");
    std::fs::write(
        dir.path().join(".github/workflows/ci.yml"),
        "on: [pull_request]\njobs:\n  ci-ok:\n    steps: []\n",
    )
    .unwrap();
    let d = build_desired(dir.path()).unwrap();
    assert!(!d.github.ruleset.merge_queue.value);
    assert_eq!(d.github.ruleset.merge_queue.source, "default");
}

#[test]
fn ruleset_enforcement_other_than_active_is_update() {
    for mode in ["disabled", "evaluate"] {
        let (_dir, d) = desired("");
        let fixture: &'static str = if mode == "disabled" {
            "github/ruleset_disabled.json"
        } else {
            "github/ruleset_evaluate.json"
        };
        let api = fake_with(&[("GET /repos/acme/x/rulesets/11", 200, fixture, &[])]);
        let (items, _) = run(&api, &d);
        let item = find(&items, "ruleset", "hoist: default branch");
        assert_eq!(item.action, Action::Update, "{mode}");
        assert_eq!(item.reason, format!("enforcement `{mode}`, want `active`"));
    }
}

#[test]
fn secret_only_in_org_list_is_present() {
    let (_dir, d) = desired("");
    let api = fake_with(&[
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_only_account.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/organization-secrets",
            200,
            "github/org_secrets.json",
            &[],
        ),
    ]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_API_TOKEN").action,
        Action::Ok
    );
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_ACCOUNT_ID").action,
        Action::Ok
    );
}

#[test]
fn forbidden_org_secrets_keeps_repo_secrets_known() {
    let (_dir, d) = desired("");
    let api = fake_with(&[
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_only_account.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/organization-secrets",
            403,
            "github/forbidden.json",
            &[],
        ),
    ]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_ACCOUNT_ID").action,
        Action::Ok
    );
    let missing = find(&items, "secret", "CLOUDFLARE_API_TOKEN");
    assert_eq!(missing.action, Action::Create);
    assert!(
        missing
            .reason
            .contains("organization secrets not readable: Resource not accessible"),
        "{}",
        missing.reason
    );
}

#[test]
fn repo_not_visible_is_an_error() {
    let (_dir, d) = desired("");
    let api = fake_with(&[("GET /repos/acme/x", 404, "github/forbidden.json", &[])]);
    let err = read_github(&http(&api), &d).unwrap_err();
    assert_eq!(
        err.to_string(),
        "token cannot see acme/x (404); grant it access or check the remote"
    );
    assert_eq!(err.exit_code(), 3);
}

#[test]
fn secret_on_page_two_is_ok() {
    let (_dir, d) = desired("");
    let api = fake_with(&[]);
    let live = read_github(&http(&api), &d).unwrap();
    match &live.secrets {
        Fetch::Got(names) => assert_eq!(names.len(), 31),
        other => panic!("{other:?}"),
    }
    let (items, _) = diff_github(&d, &live, None);
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_API_TOKEN").action,
        Action::Ok
    );
    api.assert_only_get();
}

#[test]
fn missing_secret_is_create_with_hint() {
    let (_dir, d) = desired("");
    let api = fake_with(&[(
        "GET /repos/acme/x/actions/secrets",
        200,
        "github/secrets_only_account.json",
        &[],
    )]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "secret", "CLOUDFLARE_API_TOKEN");
    assert_eq!(item.action, Action::Create);
    assert_eq!(
        item.reason,
        "missing; set with `hoist secrets set CLOUDFLARE_API_TOKEN`"
    );
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_ACCOUNT_ID").action,
        Action::Ok
    );
}

#[test]
fn forbidden_rulesets_is_unknown() {
    let (_dir, d) = desired("");
    let api = fake_with(&[(
        "GET /repos/acme/x/rulesets",
        403,
        "github/forbidden.json",
        &[],
    )]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Unknown);
    assert_eq!(
        item.reason,
        "cannot read rulesets: Resource not accessible by personal access token"
    );
    assert_eq!(
        find(&items, "repo_setting", "allow_auto_merge").action,
        Action::Ok
    );
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_API_TOKEN").action,
        Action::Ok
    );
}

#[test]
fn classic_protection_reason() {
    let (_dir, d) = desired("");
    let api = fake_with(&[
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/empty_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/branches/main/protection",
            200,
            "github/protection.json",
            &[],
        ),
    ]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Create);
    assert!(
        item.reason
            .contains("classic branch protection exists on `main`"),
        "{}",
        item.reason
    );
}

#[test]
fn external_turns_create_into_drift() {
    let (_dir, d) = desired("[github]\nmanaged = \"external\"\n");
    let api = fake_with(&[
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/empty_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            200,
            "github/secrets_only_account.json",
            &[],
        ),
    ]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "ruleset", "hoist: default branch").action,
        Action::Drift
    );
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_API_TOKEN").action,
        Action::Drift
    );
    assert_eq!(
        find(&items, "secret", "CLOUDFLARE_ACCOUNT_ID").action,
        Action::Ok
    );
}

#[test]
fn renovate_missing_is_pending_human_step() {
    let (_dir, d) = desired("");
    let api = fake_with(&[(
        "GET /user/installations/501/repositories",
        200,
        "github/installation_repos_other.json",
        &[],
    )]);
    let (items, steps) = run(&api, &d);
    let item = find(&items, "app", "renovate");
    assert_eq!(item.action, Action::Pending);
    assert_eq!(item.source, "renovate.json:1");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, "app:renovate");
    assert_eq!(steps[0].url, "https://github.com/apps/renovate");
    assert_eq!(steps[0].status, "pending");
}

#[test]
fn forbidden_installations_is_unknown_without_human_step() {
    let (_dir, d) = desired("");
    let api = fake_with(&[("GET /user/installations", 403, "github/forbidden.json", &[])]);
    let (items, steps) = run(&api, &d);
    let item = find(&items, "app", "renovate");
    assert_eq!(item.action, Action::Unknown);
    assert_eq!(item.reason, "cannot list app installations with this token");
    assert!(steps.is_empty());
}

#[test]
fn settings_drift_is_update_and_visibility_checked_only_when_set() {
    let (_dir, d) = desired("[github]\nvisibility = \"private\"\nallow_auto_merge = false\n");
    let api = fake_with(&[]);
    let (items, _) = run(&api, &d);
    let vis = find(&items, "repo_setting", "visibility");
    assert_eq!(vis.action, Action::Update);
    assert!(vis.reason.contains("public") && vis.reason.contains("private"));
    assert_eq!(
        find(&items, "repo_setting", "allow_auto_merge").action,
        Action::Update
    );

    let (_dir, d) = desired("");
    let (items, _) = run(&api, &d);
    assert!(items.iter().all(|i| i.key != "visibility"));
}

#[test]
fn wrong_actions_permissions_and_missing_environment() {
    let (_dir, d) = desired("[github]\nactions_permissions = \"write\"\n");
    let api = fake_with(&[]);
    let (items, _) = run(&api, &d);
    assert_eq!(
        find(&items, "actions_permissions", "default").action,
        Action::Update
    );
}

#[test]
fn environment_missing_is_create_and_present_is_ok() {
    let dir = fixture_repo("site", "https://github.com/acme/x.git");
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[cloudflare]\naccount_id = \"acc\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".github/workflows/release.yml"),
        "name: release\non: workflow_dispatch\njobs:\n  publish:\n    runs-on: ubuntu-latest\n    environment: production\n    steps:\n      - run: echo hi\n",
    )
    .unwrap();
    let d = build_desired(dir.path()).expect("desired");
    let (items, _) = run(&fake_with(&[]), &d);
    let item = find(&items, "environment", "production");
    assert_eq!(item.action, Action::Create);
    assert_eq!(item.source, ".github/workflows/release.yml:6");

    let api = fake_with(&[(
        "GET /repos/acme/x/environments",
        200,
        "github/environments_production.json",
        &[],
    )]);
    let (items, _) = run(&api, &d);
    assert_eq!(find(&items, "environment", "production").action, Action::Ok);
}

#[test]
fn no_required_checks_means_ruleset_not_managed() {
    let (_dir, d) = desired("[github]\nrequired_checks = []\n");
    let api = fake_with(&[(
        "GET /repos/acme/x/rulesets",
        200,
        "github/empty_list.json",
        &[],
    )]);
    let (items, _) = run(&api, &d);
    let item = find(&items, "ruleset", "hoist: default branch");
    assert_eq!(item.action, Action::Ok);
    assert_eq!(
        item.reason,
        "no required checks inferred; ruleset not managed"
    );
}

#[test]
fn live_github_is_constructible_for_unit_use() {
    let live = LiveGithub {
        settings: Fetch::Unknown("x".into()),
        rulesets: Fetch::Unknown("x".into()),
        classic_protection: Fetch::Unknown("x".into()),
        actions_permissions: Fetch::Unknown("x".into()),
        environments: Fetch::Unknown("x".into()),
        secrets: Fetch::Unknown("x".into()),
        org_secrets_note: None,
        apps: Fetch::Unknown("x".into()),
    };
    let (_dir, d) = desired("");
    let (items, _) = diff_github(&d, &live, None);
    assert!(items.iter().all(|i| i.action == Action::Unknown));
}
