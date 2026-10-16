//! `plan_changes` on constructed inputs (pure, no I/O).

use hoist::Area;
use hoist::change::{Change, ChangeOpts, plan_changes};
use hoist::cloudflare::{CfToken, LiveCloudflare, PagesProject};
use hoist::config::Managed;
use hoist::desired::{
    Desired, DesiredCloudflare, DesiredGithub, DesiredPages, DesiredRuleset, DesiredSettings,
};
use hoist::diff::{diff_cloudflare, diff_github};
use hoist::github::{Fetch, LiveGithub, RepoSettings, Ruleset};
use hoist::repo::Sourced;
use hoist::repo::git::GithubRepo;
use hoist::repo::targets::Packages;
use hoist::{Action, HumanStep};

const TOKEN_NAME: &str = "hoist:o/r:deploy";
/// When `gh_in_sync` last wrote `CLOUDFLARE_API_TOKEN`.
const SECRET_UPDATED: &str = "2026-10-03T00:00:00Z";

fn s<T>(value: T) -> Sourced<T> {
    Sourced {
        value,
        source: "test".to_string(),
    }
}

fn desired() -> Desired {
    Desired {
        repo: GithubRepo {
            owner: "o".into(),
            name: "r".into(),
        },
        default_branch: "main".into(),
        github: DesiredGithub {
            settings: DesiredSettings {
                visibility: None,
                allow_auto_merge: s(true),
                delete_branch_on_merge: s(true),
            },
            ruleset: DesiredRuleset {
                name: "hoist: default branch".into(),
                branch: "main".into(),
                required_checks: vec![s("ci-ok".to_string())],
                merge_queue: s(false),
            },
            actions_permissions: s("read".to_string()),
            environments: vec![s("production".to_string())],
            secrets: vec![
                s("CLOUDFLARE_API_TOKEN".to_string()),
                s("CLOUDFLARE_ACCOUNT_ID".to_string()),
            ],
            apps: Vec::new(),
            managed: Managed::Hoist,
        },
        cloudflare: DesiredCloudflare {
            account_id: Some(s("acc".to_string())),
            pages: vec![DesiredPages {
                target: "site".into(),
                dir: "apps/site".into(),
                project: s("proj".to_string()),
                domain: Some(s("example.com".to_string())),
            }],
            workers: Vec::new(),
            managed: Managed::Hoist,
        },
        packages: Packages::default(),
    }
}

fn gh_in_sync() -> LiveGithub {
    LiveGithub {
        settings: Fetch::Got(RepoSettings {
            visibility: "private".into(),
            allow_auto_merge: Some(true),
            delete_branch_on_merge: Some(true),
            admin: Some(true),
        }),
        rulesets: Fetch::Got(vec![Ruleset {
            id: 42,
            name: "hoist: default branch".into(),
            include: vec!["~DEFAULT_BRANCH".into()],
            required_checks: vec!["ci-ok".into()],
            merge_queue: false,
            enforcement: Some("active".into()),
        }]),
        classic_protection: Fetch::Got(false),
        actions_permissions: Fetch::Got("read".into()),
        environments: Fetch::Got(vec!["production".into()]),
        secrets: Fetch::Got(vec![
            "CLOUDFLARE_API_TOKEN".into(),
            "CLOUDFLARE_ACCOUNT_ID".into(),
        ]),
        org_secrets_note: None,
        api_token_updated_at: Some(SECRET_UPDATED.into()),
        apps: Fetch::Got(Vec::new()),
    }
}

fn tok(id: &str, name: &str, issued: Option<&str>) -> CfToken {
    CfToken {
        id: id.into(),
        name: name.into(),
        expires_on: None,
        status: "active".into(),
        issued_on: issued.map(str::to_string),
    }
}

fn cf_in_sync() -> LiveCloudflare {
    LiveCloudflare {
        pages: vec![(
            "proj".into(),
            Fetch::Got(Some(PagesProject {
                subdomain: "proj.pages.dev".into(),
                production_branch: "main".into(),
                domains: vec!["example.com".into()],
            })),
        )],
        workers: Vec::new(),
        tokens: Fetch::Got(vec![tok("t1", TOKEN_NAME, None)]),
    }
}

fn gh_all_missing() -> LiveGithub {
    LiveGithub {
        settings: Fetch::Got(RepoSettings {
            visibility: "private".into(),
            allow_auto_merge: Some(false),
            delete_branch_on_merge: Some(false),
            admin: Some(true),
        }),
        rulesets: Fetch::Got(Vec::new()),
        classic_protection: Fetch::Got(false),
        actions_permissions: Fetch::Got("write".into()),
        environments: Fetch::Got(Vec::new()),
        secrets: Fetch::Got(Vec::new()),
        org_secrets_note: None,
        api_token_updated_at: None,
        apps: Fetch::Got(Vec::new()),
    }
}

fn cf_all_missing() -> LiveCloudflare {
    LiveCloudflare {
        pages: vec![("proj".into(), Fetch::Got(None))],
        workers: Vec::new(),
        tokens: Fetch::Got(Vec::new()),
    }
}

fn opts(prune: bool) -> ChangeOpts {
    ChangeOpts {
        prune,
        areas: Vec::new(),
    }
}

fn why<T>() -> Fetch<T> {
    Fetch::Unknown("no access".to_string())
}

fn kinds(changes: &[Change]) -> Vec<&'static str> {
    changes.iter().map(|c| c.key().1).collect()
}

#[test]
fn in_sync_has_no_changes() {
    let d = desired();
    let changes = plan_changes(&d, Some(&gh_in_sync()), Some(&cf_in_sync()), &opts(true));
    assert!(changes.is_empty(), "{changes:#?}");
}

#[test]
fn order_is_dependency_order() {
    let d = desired();
    let mut cf = cf_all_missing();
    let changes = plan_changes(&d, Some(&gh_all_missing()), Some(&cf), &opts(false));
    assert_eq!(
        kinds(&changes),
        [
            "pages_project",
            "custom_domain",
            "secret",
            "secret",
            "environment",
            "ruleset",
            "repo_setting",
            "actions_permissions"
        ]
    );
    assert!(matches!(
        changes[0],
        Change::PagesProject { exists: false, .. }
    ));
    assert!(matches!(changes[1], Change::Domain { .. }));
    assert!(matches!(changes[2], Change::DeployToken { .. }));
    assert!(matches!(changes[3], Change::AccountIdSecret));
    assert!(matches!(changes[4], Change::Environment { .. }));
    assert!(matches!(
        changes[5],
        Change::Ruleset {
            existing_id: None,
            ..
        }
    ));
    assert!(matches!(changes[6], Change::RepoSettings { .. }));
    assert!(matches!(changes[7], Change::ActionsPermissions { .. }));

    // With prune, revocations come last.
    cf.tokens = Fetch::Got(vec![tok("old", "hoist:o/r:old", None)]);
    let changes = plan_changes(&d, Some(&gh_all_missing()), Some(&cf), &opts(true));
    assert!(matches!(
        changes.last(),
        Some(Change::RevokeToken { id, .. }) if id == "old"
    ));
}

#[test]
fn repo_settings_patch_has_only_differing_fields() {
    let d = desired();
    let mut gh = gh_in_sync();
    gh.settings = Fetch::Got(RepoSettings {
        visibility: "private".into(),
        allow_auto_merge: Some(true),
        delete_branch_on_merge: Some(false),
        admin: Some(true),
    });
    let changes = plan_changes(&d, Some(&gh), Some(&cf_in_sync()), &opts(false));
    assert_eq!(changes.len(), 1, "{changes:#?}");
    match &changes[0] {
        Change::RepoSettings { patch } => {
            assert_eq!(patch.len(), 1);
            assert_eq!(patch["delete_branch_on_merge"], serde_json::json!(true));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn existing_ruleset_is_updated_by_id() {
    let d = desired();
    let mut gh = gh_in_sync();
    if let Fetch::Got(list) = &mut gh.rulesets {
        list[0].required_checks = vec!["other".into()];
    }
    let changes = plan_changes(&d, Some(&gh), Some(&cf_in_sync()), &opts(false));
    assert_eq!(
        changes,
        vec![Change::Ruleset {
            existing_id: Some(42),
            branch: "main".into(),
            required_checks: vec!["ci-ok".into()],
            merge_queue: false,
        }]
    );
}

#[test]
fn external_area_yields_no_changes() {
    let mut d = desired();
    d.github.managed = Managed::External;
    d.cloudflare.managed = Managed::External;
    let changes = plan_changes(
        &d,
        Some(&gh_all_missing()),
        Some(&cf_all_missing()),
        &opts(true),
    );
    assert!(changes.is_empty(), "{changes:#?}");
}

#[test]
fn unknown_yields_no_changes() {
    let d = desired();
    let gh = LiveGithub {
        settings: why(),
        rulesets: why(),
        classic_protection: why(),
        actions_permissions: why(),
        environments: why(),
        secrets: why(),
        org_secrets_note: None,
        api_token_updated_at: None,
        apps: why(),
    };
    let cf = LiveCloudflare {
        pages: vec![("proj".into(), why())],
        workers: Vec::new(),
        tokens: why(),
    };
    let changes = plan_changes(&d, Some(&gh), Some(&cf), &opts(true));
    assert!(changes.is_empty(), "{changes:#?}");
}

#[test]
fn token_present_secret_missing_mints_and_replaces() {
    let d = desired();
    let mut gh = gh_in_sync();
    gh.secrets = Fetch::Got(vec!["CLOUDFLARE_ACCOUNT_ID".into()]);
    let changes = plan_changes(&d, Some(&gh), Some(&cf_in_sync()), &opts(false));
    assert_eq!(
        changes,
        vec![Change::DeployToken {
            name: TOKEN_NAME.into(),
            replace_ids: vec!["t1".into()],
        }]
    );
}

#[test]
fn prune_revokes_leftover_tokens_only_with_flag() {
    let d = desired();
    let mut cf = cf_in_sync();
    cf.tokens = Fetch::Got(vec![
        tok("new", TOKEN_NAME, Some("2026-10-02T00:00:00Z")),
        tok("old1", TOKEN_NAME, Some("2026-10-01T00:00:00Z")),
        tok("stale", "hoist:o/r:old", None),
        tok("other", "other", None),
    ]);
    let none = plan_changes(&d, Some(&gh_in_sync()), Some(&cf), &opts(false));
    assert!(none.is_empty(), "{none:#?}");
    let changes = plan_changes(&d, Some(&gh_in_sync()), Some(&cf), &opts(true));
    let mut ids: Vec<_> = changes
        .iter()
        .map(|c| match c {
            Change::RevokeToken { id, .. } => id.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    ids.sort();
    assert_eq!(ids, ["old1", "stale"]);
}

// ---- R29: the live token is the newest one issued before the secret was written ----

fn revoked_ids(changes: &[Change]) -> Vec<&str> {
    let mut ids: Vec<&str> = changes
        .iter()
        .map(|c| match c {
            Change::RevokeToken { id, .. } => id.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    ids.sort();
    ids
}

#[test]
fn orphan_newer_than_the_secret_survives_prune_and_the_live_token_is_kept() {
    let d = desired();
    let mut cf = cf_in_sync();
    cf.tokens = Fetch::Got(vec![
        tok("older", TOKEN_NAME, Some("2026-09-01T00:00:00Z")),
        tok("live", TOKEN_NAME, Some("2026-10-01T00:00:00Z")),
        // Minted after the secret was last written (e.g. a mint transport error): not live.
        tok("orphan", TOKEN_NAME, Some("2026-10-05T00:00:00Z")),
        tok("stale", "hoist:o/r:old", None),
    ]);
    let changes = plan_changes(&d, Some(&gh_in_sync()), Some(&cf), &opts(true));
    assert_eq!(revoked_ids(&changes), ["older", "stale"]);
    let (items, steps) = api_token_items(&d, &gh_in_sync(), &cf);
    assert_eq!(items[0].action, Action::Ok, "{items:#?}");
    assert!(items[0].reason.contains("orphan"), "{}", items[0].reason);
    assert!(steps.is_empty(), "{steps:#?}");
}

fn assert_ambiguous(d: &Desired, gh: &LiveGithub, cf: &LiveCloudflare, ids: &[&str]) {
    let changes = plan_changes(d, Some(gh), Some(cf), &opts(true));
    assert!(changes.is_empty(), "no DELETE when ambiguous: {changes:#?}");
    let (items, steps) = api_token_items(d, gh, cf);
    assert_eq!(items[0].action, Action::Pending, "{items:#?}");
    let step = steps
        .iter()
        .find(|s| s.id == "rotate:CLOUDFLARE_API_TOKEN")
        .expect("human step");
    for id in ids {
        assert!(step.title.contains(id), "{}", step.title);
        assert!(items[0].reason.contains(id), "{}", items[0].reason);
    }
}

#[test]
fn ambiguous_live_token_means_no_revocation() {
    let d = desired();
    let mut cf = cf_in_sync();
    let stale = tok("stale", "hoist:o/r:old", None);
    // No updated_at on the secret: two deploy tokens cannot be told apart.
    let mut gh = gh_in_sync();
    gh.api_token_updated_at = None;
    cf.tokens = Fetch::Got(vec![
        tok("a", TOKEN_NAME, Some("2026-10-01T00:00:00Z")),
        tok("b", TOKEN_NAME, Some("2026-10-02T00:00:00Z")),
        stale.clone(),
    ]);
    assert_ambiguous(&d, &gh, &cf, &["a", "b"]);
    // Every deploy token was issued after the secret was written.
    cf.tokens = Fetch::Got(vec![
        tok("a", TOKEN_NAME, Some("2026-10-04T00:00:00Z")),
        tok("b", TOKEN_NAME, Some("2026-10-05T00:00:00Z")),
        stale.clone(),
    ]);
    assert_ambiguous(&d, &gh_in_sync(), &cf, &["a", "b"]);
    // An issue date is missing.
    cf.tokens = Fetch::Got(vec![
        tok("a", TOKEN_NAME, None),
        tok("b", TOKEN_NAME, Some("2026-10-01T00:00:00Z")),
        stale,
    ]);
    assert_ambiguous(&d, &gh_in_sync(), &cf, &["a", "b"]);
}

#[test]
fn plan_reports_the_live_tokens_status_not_the_newest() {
    let d = desired();
    let soon = hoist::timeutil::format_rfc3339(hoist::timeutil::now_secs() + 10 * 86_400 + 3600);
    let far = hoist::timeutil::format_rfc3339(hoist::timeutil::now_secs() + 200 * 86_400);
    let mut live = tok("live", TOKEN_NAME, Some("2026-10-01T00:00:00Z"));
    live.expires_on = Some(soon);
    let mut orphan = tok("orphan", TOKEN_NAME, Some("2026-10-05T00:00:00Z"));
    orphan.expires_on = Some(far);
    let mut cf = cf_in_sync();
    cf.tokens = Fetch::Got(vec![live, orphan]);
    let (items, _) = api_token_items(&d, &gh_in_sync(), &cf);
    assert_eq!(items[0].action, Action::Pending, "{items:#?}");
    assert!(
        items[0].reason.starts_with("expires in 10 days"),
        "{}",
        items[0].reason
    );
}

#[test]
fn areas_limit_the_plan() {
    let d = desired();
    let changes = plan_changes(
        &d,
        Some(&gh_all_missing()),
        Some(&cf_all_missing()),
        &ChangeOpts {
            prune: false,
            areas: vec![Area::Cloudflare],
        },
    );
    assert_eq!(kinds(&changes), ["pages_project", "custom_domain"]);
}

#[test]
fn user_secret_is_pending_human_step() {
    let mut d = desired();
    d.github.secrets.push(s("SENTRY_DSN".to_string()));
    let (items, steps) = diff_github(&d, &gh_in_sync(), Some(&cf_in_sync()));
    let item = items
        .iter()
        .find(|i| i.kind == "secret" && i.key == "SENTRY_DSN")
        .unwrap();
    assert_eq!(item.action, Action::Pending);
    assert_eq!(
        steps,
        vec![HumanStep {
            id: "secret:SENTRY_DSN".into(),
            title: "set secret SENTRY_DSN".into(),
            url: "hoist secrets set SENTRY_DSN".into(),
            status: "pending".into(),
        }]
    );
    let changes = plan_changes(&d, Some(&gh_in_sync()), Some(&cf_in_sync()), &opts(false));
    assert!(changes.is_empty());
}

#[test]
fn cloudflare_secrets_without_target_or_account_are_pending() {
    let mut d = desired();
    d.cloudflare.account_id = None;
    let mut gh = gh_in_sync();
    gh.secrets = Fetch::Got(Vec::new());
    let (items, steps) = diff_github(&d, &gh, Some(&cf_in_sync()));
    for key in ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"] {
        let item = items.iter().find(|i| i.key == key).unwrap();
        assert_eq!(item.action, Action::Pending, "{key}");
    }
    assert_eq!(steps.len(), 2);
}

/// Every create/update item has a matching change and vice versa (revocations have no item).
#[test]
fn items_and_changes_match_one_to_one() {
    let d = desired();
    let (gh, cf) = (gh_all_missing(), cf_all_missing());
    let mut items = diff_github(&d, &gh, Some(&cf)).0;
    items.extend(diff_cloudflare(&d, &cf).0);
    let mut want: Vec<(String, String, String)> = items
        .iter()
        .filter(|i| matches!(i.action, Action::Create | Action::Update))
        // Checked separately below: an `api_token` create pairs with the `DeployToken` change.
        .filter(|i| i.kind != "api_token")
        .map(|i| (i.area.clone(), i.kind.clone(), i.key.clone()))
        .collect();
    // Repo settings are one change keyed by the first differing setting; compare per change.
    let changes = plan_changes(&d, Some(&gh), Some(&cf), &opts(false));
    let mut got: Vec<(String, String, String)> = Vec::new();
    for c in &changes {
        match c {
            Change::RepoSettings { patch } => {
                for k in patch.keys() {
                    got.push(("github".into(), "repo_setting".into(), k.clone()));
                }
            }
            other => {
                let (a, k, key) = other.key();
                got.push((a.into(), k.into(), key));
            }
        }
    }
    want.sort();
    got.sort();
    assert_eq!(got, want);
    // R16: an api_token create implies a DeployToken change, and none is ever `update`.
    let token_items: Vec<_> = items.iter().filter(|i| i.kind == "api_token").collect();
    assert!(!token_items.is_empty());
    assert!(token_items.iter().all(|i| i.action != Action::Update));
    let creates = token_items.iter().any(|i| i.action == Action::Create);
    assert_eq!(
        creates,
        changes
            .iter()
            .any(|c| matches!(c, Change::DeployToken { .. }))
    );
}

fn missing_token_secret() -> LiveGithub {
    let mut gh = gh_in_sync();
    gh.secrets = Fetch::Got(vec!["CLOUDFLARE_ACCOUNT_ID".into()]);
    gh
}

fn token_item(d: &Desired, gh: &LiveGithub, cf: Option<&LiveCloudflare>) -> hoist::Item {
    let (items, _) = diff_github(d, gh, cf);
    items
        .into_iter()
        .find(|i| i.key == "CLOUDFLARE_API_TOKEN")
        .unwrap()
}

#[test]
fn unknown_token_list_never_mints() {
    let d = desired();
    let mut cf = cf_in_sync();
    cf.tokens = why();
    let gh = missing_token_secret();
    let item = token_item(&d, &gh, Some(&cf));
    assert_eq!(item.action, Action::Unknown);
    assert_eq!(
        item.reason,
        "cannot list Cloudflare tokens: no access; hoist will not mint blind"
    );
    assert!(plan_changes(&d, Some(&gh), Some(&cf), &opts(true)).is_empty());
}

#[test]
fn cloudflare_not_read_never_mints() {
    let d = desired();
    let gh = missing_token_secret();
    assert_eq!(token_item(&d, &gh, None).action, Action::Unknown);
    let o = ChangeOpts {
        prune: false,
        areas: vec![Area::Github],
    };
    assert!(plan_changes(&d, Some(&gh), None, &o).is_empty());
    assert!(plan_changes(&d, Some(&gh), None, &opts(false)).is_empty());
}

#[test]
fn external_cloudflare_makes_secrets_pending() {
    let mut d = desired();
    d.cloudflare.managed = Managed::External;
    let mut gh = gh_in_sync();
    gh.secrets = Fetch::Got(Vec::new());
    let (items, steps) = diff_github(&d, &gh, Some(&cf_in_sync()));
    for key in ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"] {
        let item = items.iter().find(|i| i.key == key).unwrap();
        assert_eq!(item.action, Action::Pending, "{key}");
    }
    assert_eq!(steps.len(), 2);
    let changes = plan_changes(&d, Some(&gh), Some(&cf_in_sync()), &opts(false));
    assert!(changes.is_empty(), "{changes:#?}");
}

#[test]
fn account_id_secret_needs_no_token_list() {
    let d = desired();
    let mut gh = gh_in_sync();
    gh.secrets = Fetch::Got(vec!["CLOUDFLARE_API_TOKEN".into()]);
    let mut cf = cf_in_sync();
    cf.tokens = why();
    let changes = plan_changes(&d, Some(&gh), Some(&cf), &opts(false));
    assert_eq!(changes, vec![Change::AccountIdSecret]);
}

#[test]
fn visibility_patch_and_existing_project_update_match_items() {
    let mut d = desired();
    d.github.settings.visibility = Some(s("public".to_string()));
    let gh = gh_in_sync();
    let mut cf = cf_in_sync();
    cf.pages[0].1 = Fetch::Got(Some(PagesProject {
        subdomain: "proj.pages.dev".into(),
        production_branch: "develop".into(),
        domains: vec!["example.com".into()],
    }));
    let changes = plan_changes(&d, Some(&gh), Some(&cf), &opts(false));
    assert_eq!(changes.len(), 2, "{changes:#?}");
    assert_eq!(
        changes[0],
        Change::PagesProject {
            name: "proj".into(),
            production_branch: "main".into(),
            exists: true
        }
    );
    match &changes[1] {
        Change::RepoSettings { patch } => {
            assert_eq!(patch["visibility"], serde_json::json!("public"))
        }
        other => panic!("{other:?}"),
    }
    let mut want: Vec<_> = diff_github(&d, &gh, Some(&cf))
        .0
        .into_iter()
        .chain(diff_cloudflare(&d, &cf).0)
        .filter(|i| matches!(i.action, Action::Create | Action::Update))
        .map(|i| (i.kind, i.key))
        .collect();
    want.sort();
    assert_eq!(
        want,
        [
            ("pages_project".to_string(), "proj".to_string()),
            ("repo_setting".to_string(), "visibility".to_string())
        ]
    );
}

// ---- R16: the api_token item agrees with what apply can do ----

fn api_token_items(
    d: &Desired,
    gh: &LiveGithub,
    cf: &LiveCloudflare,
) -> (Vec<hoist::Item>, Vec<hoist::HumanStep>) {
    let (items, steps) = diff_github(d, gh, Some(cf));
    (
        items
            .into_iter()
            .filter(|i| i.kind == "api_token")
            .collect(),
        steps,
    )
}

fn cf_with_token(expires: Option<String>, status: &str) -> LiveCloudflare {
    let mut cf = cf_in_sync();
    let mut t = tok("t1", TOKEN_NAME, Some("2026-01-01T00:00:00Z"));
    t.expires_on = expires;
    t.status = status.into();
    cf.tokens = Fetch::Got(vec![t]);
    cf
}

#[test]
fn hand_set_secret_without_hoist_token_is_ok_and_plans_nothing() {
    let d = desired();
    let mut cf = cf_in_sync();
    cf.tokens = Fetch::Got(Vec::new());
    let (items, steps) = api_token_items(&d, &gh_in_sync(), &cf);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].action, Action::Ok);
    assert_eq!(
        items[0].reason,
        "CLOUDFLARE_API_TOKEN is set outside hoist; run `hoist secrets rotate` to let hoist mint and manage it"
    );
    assert!(steps.is_empty());
    assert!(plan_changes(&d, Some(&gh_in_sync()), Some(&cf), &opts(false)).is_empty());
}

#[test]
fn missing_secret_with_readable_list_is_create_with_a_change() {
    let d = desired();
    let mut cf = cf_in_sync();
    cf.tokens = Fetch::Got(Vec::new());
    let gh = missing_token_secret();
    let (items, _) = api_token_items(&d, &gh, &cf);
    assert_eq!(items[0].action, Action::Create);
    let changes = plan_changes(&d, Some(&gh), Some(&cf), &opts(false));
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::DeployToken { .. }))
    );
}

#[test]
fn expiring_token_is_pending_with_a_rotate_step() {
    let d = desired();
    let soon = hoist::timeutil::format_rfc3339(hoist::timeutil::now_secs() + 10 * 86_400 + 3600);
    let cf = cf_with_token(Some(soon), "active");
    let (items, steps) = api_token_items(&d, &gh_in_sync(), &cf);
    assert_eq!(items[0].action, Action::Pending);
    assert_eq!(
        items[0].reason,
        "expires in 10 days; run `hoist secrets rotate`"
    );
    let step = steps
        .iter()
        .find(|s| s.id == "rotate:CLOUDFLARE_API_TOKEN")
        .expect("step");
    assert_eq!(step.title, "rotate the Cloudflare deploy token");
    assert_eq!(step.url, "hoist secrets rotate");
    assert_eq!(step.status, "pending");
    assert!(plan_changes(&d, Some(&gh_in_sync()), Some(&cf), &opts(false)).is_empty());
}

#[test]
fn disabled_token_is_pending() {
    let d = desired();
    let cf = cf_with_token(None, "disabled");
    let (items, steps) = api_token_items(&d, &gh_in_sync(), &cf);
    assert_eq!(items[0].action, Action::Pending);
    assert!(items[0].reason.contains("disabled"), "{}", items[0].reason);
    assert!(steps.iter().any(|s| s.id == "rotate:CLOUDFLARE_API_TOKEN"));
}

#[test]
fn healthy_token_is_ok() {
    let d = desired();
    let far = hoist::timeutil::format_rfc3339(hoist::timeutil::now_secs() + 200 * 86_400);
    let (items, steps) = api_token_items(&d, &gh_in_sync(), &cf_with_token(Some(far), "active"));
    assert_eq!(items[0].action, Action::Ok);
    assert!(steps.is_empty());
}

#[test]
fn rotation_boundary_is_30_days() {
    let now = 1_800_000_000;
    let at = |secs: i64| {
        let mut t = tok("t", TOKEN_NAME, None);
        t.expires_on = Some(hoist::timeutil::format_rfc3339(now + secs));
        t
    };
    let day = 86_400;
    assert_eq!(
        hoist::diff::rotation_reason(&at(30 * day), now).as_deref(),
        Some("expires in 30 days")
    );
    assert_eq!(hoist::diff::rotation_reason(&at(31 * day), now), None);
    assert!(
        hoist::diff::rotation_reason(&at(-day), now)
            .unwrap()
            .contains("expired 1 days ago")
    );
    assert_eq!(
        hoist::diff::rotation_reason(&tok("t", TOKEN_NAME, None), now),
        None
    );
}

// R33: equal issued_on, or a second candidate close to the secret's updated_at, is unclear.
#[test]
fn live_token_tie_is_unclear() {
    use hoist::change::{LiveToken, live_deploy_token};
    let t = [
        tok("a", "n", Some("2026-10-01T00:00:00Z")),
        tok("b", "n", Some("2026-10-01T00:00:00Z")),
    ];
    let got = live_deploy_token("n", &t, Some("2026-10-02T00:00:00Z"));
    assert!(matches!(got, LiveToken::Unclear(_)), "{got:?}");
}

#[test]
fn live_token_candidate_within_skew_of_secret_is_unclear() {
    use hoist::change::{LiveToken, live_deploy_token};
    let t = [
        tok("old", "n", Some("2026-10-01T00:00:00Z")),
        tok("late", "n", Some("2026-10-02T00:00:30Z")),
    ];
    let got = live_deploy_token("n", &t, Some("2026-10-02T00:00:40Z"));
    assert!(matches!(got, LiveToken::Unclear(_)), "{got:?}");
}

#[test]
fn live_token_single_recent_candidate_is_live() {
    use hoist::change::{LiveToken, live_deploy_token};
    let t = [tok("new", "n", Some("2026-10-02T00:00:30Z"))];
    let got = live_deploy_token("n", &t, Some("2026-10-02T00:00:32Z"));
    assert!(
        matches!(got, LiveToken::Live(tk, _) if tk.id == "new"),
        "{got:?}"
    );
}
