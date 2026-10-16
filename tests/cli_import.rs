mod common;

use common::{FakeApi, fixture_repo, hoist};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

const SITE: &str = "https://github.com/acme/x.git";

type Route = (
    &'static str,
    u16,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Live state that matches the `site` fixture with the default config (in sync).
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
            "GET /accounts/acc/pages/projects/keelstack-landing",
            200,
            "cloudflare/pages_project.json",
            &[],
        ),
    ]
}

const REPO_DIFFERENT: &str = r#"{"id":1,"name":"x","full_name":"acme/x","private":false,"visibility":"public","default_branch":"main","allow_auto_merge":false,"delete_branch_on_merge":false}"#;
const RULESET_LEGACY: &str = r#"{"id":13,"name":"legacy","target":"branch","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"deletion"}]}"#;
const PERMS_WRITE: &str =
    r#"{"default_workflow_permissions":"write","can_approve_pull_request_reviews":false}"#;
const PROJECTS: &str = r#"{"success":true,"errors":[],"messages":[],"result":[{"name":"other"},{"name":"site-landing"}],"result_info":{"page":1,"per_page":25,"total_pages":1,"count":2,"total_count":2}}"#;
const PROJECT_LANDING: &str = r#"{"success":true,"errors":[],"messages":[],"result":{"id":"1","name":"site-landing","subdomain":"site-landing.pages.dev","production_branch":"main","domains":[]}}"#;

/// Live state that differs from the `site` fixture in every importable field.
fn different_routes() -> Vec<Route> {
    vec![
        ("GET /repos/acme/x", 200, REPO_DIFFERENT, &[]),
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_with_legacy.json",
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets/12",
            200,
            "github/ruleset_other_check.json",
            &[],
        ),
        ("GET /repos/acme/x/rulesets/13", 200, RULESET_LEGACY, &[]),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            PERMS_WRITE,
            &[],
        ),
        ("GET /accounts/acc/pages/projects", 200, PROJECTS, &[]),
        (
            "GET /accounts/acc/pages/projects/site-landing",
            200,
            PROJECT_LANDING,
            &[],
        ),
    ]
}

fn with_tokens(api: &FakeApi) -> assert_cmd::Command {
    let mut cmd = hoist();
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token");
    cmd
}

/// The `site` fixture with `root` as its root hoist.toml. With `infer_project`, the target's
/// hoist.toml is removed so the Pages project name is inferred (`x-site`, missing live).
fn repo(root: &str, infer_project: bool) -> TempDir {
    let repo = fixture_repo("site", SITE);
    std::fs::write(repo.path().join("hoist.toml"), root).unwrap();
    if infer_project {
        std::fs::remove_file(repo.path().join("apps/site/hoist.toml")).unwrap();
    }
    repo
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn items(stdout: &[u8]) -> Vec<Value> {
    let report: Value = serde_json::from_slice(stdout).expect("one JSON object");
    report["items"].as_array().unwrap().clone()
}

const ROOT_PINNED: &str = "# hoist overrides for acme/x\n[github]\nvisibility = \"private\" # pinned by hand\n\n[cloudflare]\naccount_id = \"acc\"  # the account\n";

#[test]
fn import_prints_overrides_for_differences() {
    let repo = repo(ROOT_PINNED, true);
    let api = FakeApi::start(&different_routes());
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .arg("import")
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    for line in [
        "# hoist.toml",
        "[github]",
        "visibility = \"public\"",
        "allow_auto_merge = false",
        "delete_branch_on_merge = false",
        "actions_permissions = \"write\"",
        "required_checks = [\"ci\"]",
        "merge_queue = false",
        "# live ruleset \"legacy\" is not managed by hoist",
        "# apps/site/hoist.toml",
        "[deploy]",
        "project = \"site-landing\"",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "missing line {line:?} in:\n{text}"
        );
    }
    assert!(!text.contains("test-token"));
    // Without --write nothing is touched.
    assert_eq!(
        read(&repo.path().join("hoist.toml")).as_deref(),
        Some(ROOT_PINNED)
    );
    assert!(!repo.path().join("apps/site/hoist.toml").exists());

    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    let overrides: Vec<(&str, &str, &str)> = items
        .iter()
        .filter(|i| i["kind"] == "override")
        .map(|i| {
            (
                i["key"].as_str().unwrap(),
                i["action"].as_str().unwrap(),
                i["reason"].as_str().unwrap(),
            )
        })
        .collect();
    for want in [
        (
            "hoist.toml:github.visibility",
            "drift",
            "visibility = \"public\"",
        ),
        (
            "hoist.toml:github.allow_auto_merge",
            "drift",
            "allow_auto_merge = false",
        ),
        (
            "hoist.toml:github.required_checks",
            "drift",
            "required_checks = [\"ci\"]",
        ),
        (
            "apps/site/hoist.toml:deploy.project",
            "drift",
            "project = \"site-landing\"",
        ),
    ] {
        assert!(overrides.contains(&want), "{want:?} not in {overrides:?}");
    }
    assert_eq!(overrides.len(), 7, "{overrides:?}");
}

#[test]
fn unknown_reads_give_unknown_items() {
    let repo = repo(ROOT_PINNED, false);
    let mut routes = in_sync_routes();
    routes.retain(|(k, ..)| !k.ends_with("/actions/permissions/workflow"));
    routes.push((
        "GET /repos/acme/x/actions/permissions/workflow",
        403,
        "github/forbidden.json",
        &[],
    ));
    let api = FakeApi::start(&routes);
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    let perms = items
        .iter()
        .find(|i| i["key"] == "hoist.toml:github.actions_permissions")
        .expect("actions_permissions item");
    assert_eq!(perms["action"], "unknown");
    assert!(
        perms["reason"]
            .as_str()
            .unwrap()
            .starts_with("cannot read actions permissions"),
        "{perms}"
    );
    // The only override is the pinned visibility (live public).
    let drift: Vec<&Value> = items.iter().filter(|i| i["action"] == "drift").collect();
    assert_eq!(drift.len(), 1, "{items:?}");
    assert_eq!(drift[0]["key"], "hoist.toml:github.visibility");
}

#[test]
fn in_sync_imports_nothing() {
    let root = "[cloudflare]\naccount_id = \"acc\"\n";
    let repo = repo(root, false);
    let api = FakeApi::start(&in_sync_routes());
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    assert!(
        items
            .iter()
            .all(|i| i["action"] != "drift" && i["action"] != "unknown"),
        "{items:?}"
    );
    with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write"])
        .assert()
        .code(0);
    assert_eq!(read(&repo.path().join("hoist.toml")).as_deref(), Some(root));
}

const ROOT_COMMENTED: &str = "# hoist overrides for acme/x\n[github]\nmanaged = \"hoist\" # who manages\n\n[cloudflare]\naccount_id = \"acc\"  # the account\n";

#[test]
fn write_preserves_comments_and_keys() {
    let repo = repo(ROOT_COMMENTED, true);
    let api = FakeApi::start(&different_routes());
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    assert!(items.iter().all(|i| i["action"] != "drift"), "{items:?}");
    let after = read(&repo.path().join("hoist.toml")).unwrap();
    let added = [
        "allow_auto_merge = false",
        "delete_branch_on_merge = false",
        "actions_permissions = \"write\"",
        "required_checks = [\"ci\"]",
        "merge_queue = false",
    ];
    let mut kept = String::new();
    let mut seen = Vec::new();
    for line in after.split_inclusive('\n') {
        if added.contains(&line.trim_end()) {
            seen.push(line.trim_end());
        } else {
            kept.push_str(line);
        }
    }
    assert_eq!(kept, ROOT_COMMENTED, "file after write:\n{after}");
    seen.sort();
    let mut want = added.to_vec();
    want.sort();
    assert_eq!(seen, want, "file after write:\n{after}");
    // A missing target hoist.toml is created.
    assert_eq!(
        read(&repo.path().join("apps/site/hoist.toml")).as_deref(),
        Some("[deploy]\nproject = \"site-landing\"\n")
    );
    // No temp files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(repo.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // A second write finds nothing to import and leaves the files byte-identical.
    with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write"])
        .assert()
        .code(0);
    assert_eq!(read(&repo.path().join("hoist.toml")).unwrap(), after);
}

#[test]
fn import_is_get_only() {
    let repo = repo(ROOT_PINNED, true);
    let api = FakeApi::start(&different_routes());
    with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write"])
        .assert()
        .code(0);
    assert!(!api.requests().is_empty());
    api.assert_only_get();
}

#[test]
fn after_write_plan_shows_no_updates() {
    let repo = repo(ROOT_PINNED, true);
    let api = FakeApi::start(&different_routes());
    with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write"])
        .assert()
        .code(0);
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["plan", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    for kind in [
        "repo_setting",
        "ruleset",
        "actions_permissions",
        "pages_project",
    ] {
        let of_kind: Vec<&Value> = items.iter().filter(|i| i["kind"] == kind).collect();
        assert!(!of_kind.is_empty(), "no {kind} items in {items:?}");
        for item in of_kind {
            assert_eq!(item["action"], "ok", "{item}");
        }
    }
    let project = items.iter().find(|i| i["kind"] == "pages_project").unwrap();
    assert_eq!(project["key"], "site-landing");
}

const RULESET_UNIMPORTABLE: &str = r#"{"id":11,"name":"hoist: default branch","target":"branch","enforcement":"evaluate","conditions":{"ref_name":{"include":["refs/heads/dev"],"exclude":[]}},"rules":[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-ok"}]}},{"type":"merge_queue","parameters":{}}]}"#;

#[test]
fn unimportable_ruleset_state_is_a_comment() {
    let repo = repo("[cloudflare]\naccount_id = \"acc\"\n", false);
    let mut routes = in_sync_routes();
    routes.retain(|(k, ..)| !k.ends_with("/rulesets/11"));
    routes.push((
        "GET /repos/acme/x/rulesets/11",
        200,
        RULESET_UNIMPORTABLE,
        &[],
    ));
    let api = FakeApi::start(&routes);
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--json"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    let comments: Vec<&str> = items
        .iter()
        .filter(|i| i["kind"] == "comment")
        .map(|i| i["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        comments,
        [
            "# live ruleset \"hoist: default branch\" enforcement \"evaluate\" is not importable; apply sets \"active\"",
            "# live ruleset \"hoist: default branch\" target \"refs/heads/dev\" is not importable; apply sets \"~DEFAULT_BRANCH\"",
        ],
        "{items:?}"
    );
    assert!(items.iter().all(|i| i["action"] != "drift"), "{items:?}");
}

#[test]
fn write_with_nothing_to_change_prints_no_snippet() {
    let root = "[cloudflare]\naccount_id = \"acc\"\n";
    let repo = repo(root, false);
    let mut routes = in_sync_routes();
    routes.retain(|(k, ..)| !k.contains("/rulesets"));
    routes.extend_from_slice(&[
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/rulesets_with_legacy.json",
            &[],
        ),
        (
            "GET /repos/acme/x/rulesets/12",
            200,
            "github/ruleset_ok.json",
            &[],
        ),
        ("GET /repos/acme/x/rulesets/13", 200, RULESET_LEGACY, &[]),
    ]);
    let api = FakeApi::start(&routes);
    let comment = "# live ruleset \"legacy\" is not managed by hoist";
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .arg("import")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.lines().any(|l| l == comment), "{text}");
    let out = with_tokens(&api)
        .current_dir(repo.path())
        .args(["import", "--write"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.lines().any(|l| l == comment), "{text}");
    assert!(!text.contains("# hoist.toml"), "{text}");
    assert_eq!(read(&repo.path().join("hoist.toml")).as_deref(), Some(root));
}

/// R32: `--area github` reads only GitHub, so it needs no Cloudflare credentials.
#[test]
fn area_github_needs_no_cloudflare_credentials() {
    let repo = repo(ROOT_PINNED, true);
    let api = FakeApi::start(&different_routes());
    let out = hoist()
        .current_dir(repo.path())
        .env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .args(["import", "--area", "github", "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let items = items(&out);
    assert!(
        items
            .iter()
            .any(|i| i["key"] == "hoist.toml:github.actions_permissions"),
        "{items:?}"
    );
    assert!(items.iter().all(|i| i["area"] == "github"), "{items:?}");
    assert!(
        api.requests()
            .iter()
            .all(|r| !r.path.starts_with("/accounts/")),
        "no Cloudflare reads"
    );
}
