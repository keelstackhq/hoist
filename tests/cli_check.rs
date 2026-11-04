mod common;

use common::{FakeApi, fixture_repo, hoist};
use predicates::prelude::*;
use serde_json::Value;

const SITE: &str = "https://github.com/acme/x.git";

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

/// Routes under which the `site` fixture is fully in sync except where a test overrides them.
fn base_routes() -> Vec<Route> {
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
        (
            "GET /accounts/acc/pages/projects/keelstack-landing",
            200,
            "cloudflare/pages_project.json",
            &[],
        ),
    ]
}

fn fake(overrides: &[Route]) -> FakeApi {
    let mut routes: Vec<Route> = base_routes()
        .into_iter()
        .filter(|(key, ..)| !overrides.iter().any(|(k, ..)| k == key))
        .collect();
    routes.extend_from_slice(overrides);
    FakeApi::start(&routes)
}

fn pages(status: u16) -> FakeApi {
    FakeApi::start(&[("GET /", status, "", &[])])
}

fn check(api: &FakeApi, pages: &FakeApi) -> assert_cmd::Command {
    let mut cmd = hoist();
    cmd.env("HOIST_GITHUB_API", api.url())
        .env("HOIST_CLOUDFLARE_API", api.url())
        .env("HOIST_PAGES_BASE", pages.url())
        .env("GITHUB_TOKEN", "gh-test-token")
        .env("CLOUDFLARE_API_TOKEN", "cf-test-token")
        .env("CLOUDFLARE_ACCOUNT_ID", "acc");
    cmd
}

fn run_json(api: &FakeApi, pages: &FakeApi, dir: &std::path::Path, code: i32) -> Value {
    let out = check(api, pages)
        .current_dir(dir)
        .args(["check", "--json"])
        .assert()
        .code(code)
        .get_output()
        .stdout
        .clone();
    api.assert_only_get();
    pages.assert_only_get();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("test-token"));
    let report: Value = serde_json::from_str(&text).expect("one JSON object");
    assert_eq!(report["command"], "check");
    assert_eq!(report["exit_code"], code);
    report
}

fn item<'a>(report: &'a Value, kind: &str, key: &str) -> &'a Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind && i["key"] == key)
        .unwrap_or_else(|| panic!("no {kind} {key} in {report}"))
}

#[test]
fn in_sync_check_exits_0() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let pages = pages(200);
    let report = run_json(&api, &pages, repo.path(), 0);
    let check = item(&report, "required_check", "ci-ok");
    assert_eq!(check["verify"]["ok"], true);
    assert_eq!(check["action"], "ok");
    let project = item(&report, "pages_project", "keelstack-landing");
    assert_eq!(project["verify"]["ok"], true);
    // The Pages probe is anonymous.
    let requests = pages.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].auth.is_none(), "{requests:?}");
}

#[test]
fn create_item_exits_1() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /repos/acme/x/rulesets",
        200,
        "github/empty_list.json",
        &[],
    )]);
    let report = run_json(&api, &pages(200), repo.path(), 1);
    assert_eq!(
        item(&report, "ruleset", "hoist: default branch")["action"],
        "create"
    );
}

#[test]
fn pending_only_exits_2() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /user/installations/501/repositories",
        200,
        "github/installation_repos_other.json",
        &[],
    )]);
    let report = run_json(&api, &pages(200), repo.path(), 2);
    assert_eq!(item(&report, "app", "renovate")["action"], "pending");
}

#[test]
fn unknown_only_exits_0_and_reports() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /repos/acme/x/actions/secrets",
        403,
        "github/forbidden.json",
        &[],
    )]);
    let report = run_json(&api, &pages(200), repo.path(), 0);
    assert_eq!(
        item(&report, "secret", "CLOUDFLARE_API_TOKEN")["action"],
        "unknown"
    );
}

#[test]
fn renamed_job_fails_verification() {
    let repo = fixture_repo("site", SITE);
    std::fs::write(
        repo.path().join("hoist.toml"),
        "[github]\nrequired_checks = [\"ci-ok\"]\n",
    )
    .unwrap();
    let ci = repo.path().join(".github/workflows/ci.yml");
    let text = std::fs::read_to_string(&ci).unwrap();
    assert!(text.contains("\n  ci-ok:"));
    std::fs::write(&ci, text.replace("\n  ci-ok:", "\n  ci-done:")).unwrap();
    let api = fake(&[]);
    let report = run_json(&api, &pages(200), repo.path(), 1);
    let check = item(&report, "required_check", "ci-ok");
    assert_eq!(check["verify"]["ok"], false);
    assert_eq!(
        check["verify"]["detail"],
        "required check `ci-ok` is not the name of any job in `.github/workflows/*.yml`"
    );
}

#[test]
fn pages_url_500_fails_verification() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let pages = pages(500);
    let report = run_json(&api, &pages, repo.path(), 1);
    let project = item(&report, "pages_project", "keelstack-landing");
    assert_eq!(project["verify"]["ok"], false);
    assert!(
        project["verify"]["detail"]
            .as_str()
            .unwrap()
            .contains("500")
    );
}

#[test]
fn pages_url_404_means_no_deployment_yet() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let report = run_json(&api, &pages(404), repo.path(), 1);
    let project = item(&report, "pages_project", "keelstack-landing");
    assert_eq!(project["verify"]["ok"], false);
    assert_eq!(
        project["verify"]["detail"],
        "no deployment answers yet (404)"
    );
}

#[test]
fn missing_project_is_not_probed() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /accounts/acc/pages/projects/keelstack-landing",
        404,
        "cloudflare/not_found.json",
        &[],
    )]);
    let pages = pages(500);
    let report = run_json(&api, &pages, repo.path(), 1);
    assert!(item(&report, "pages_project", "keelstack-landing")["verify"].is_null());
    assert!(pages.requests().is_empty());
}

#[test]
fn table_output_works() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let pages = pages(200);
    check(&api, &pages)
        .current_dir(repo.path())
        .arg("check")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("github/required_check ci-ok"));
}

#[test]
fn github_401_exits_3() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[(
        "GET /repos/acme/x/actions/secrets",
        401,
        "github/bad_credentials.json",
        &[],
    )]);
    let pages = pages(200);
    let out = check(&api, &pages)
        .current_dir(repo.path())
        .args(["check", "--json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    api.assert_only_get();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("GitHub rejected the token (401)"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("test-token")
            && !String::from_utf8_lossy(&out.stdout).contains("test-token")
    );
}

#[test]
fn repo_404_exits_3() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[("GET /repos/acme/x", 404, "github/forbidden.json", &[])]);
    let pages = pages(200);
    check(&api, &pages)
        .current_dir(repo.path())
        .args(["check", "--area", "github"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("token cannot see acme/x (404)"));
    api.assert_only_get();
}

#[test]
fn area_github_needs_no_account_id() {
    let repo = fixture_repo("site", SITE);
    let api = fake(&[]);
    let pages = pages(200);
    check(&api, &pages)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .current_dir(repo.path())
        .args(["check", "--area", "github"])
        .assert()
        .code(0);
    api.assert_only_get();
}

/// The `site` fixture requiring `ci-ok` and a commit status `risk-check`, with `gate.yml` as
/// given (`""` for none) and `status_checks` as given (`None` leaves the key out).
fn status_repo(gate: &str, status_checks: Option<&str>) -> tempfile::TempDir {
    let repo = fixture_repo("site", SITE);
    let mut toml = "[github]\nrequired_checks = [\"ci-ok\", \"risk-check\"]\n".to_string();
    if let Some(list) = status_checks {
        toml.push_str(&format!("status_checks = {list}\n"));
    }
    std::fs::write(repo.path().join("hoist.toml"), toml).unwrap();
    if !gate.is_empty() {
        std::fs::write(repo.path().join(".github/workflows/gate.yml"), gate).unwrap();
    }
    repo
}

const JUDGE_JOB: &str = "on: pull_request\njobs:\n  judge:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n      statuses: write\n    steps:\n      - run: bosun gate\n";

fn risk_check(repo: &tempfile::TempDir) -> Value {
    let api = fake(&[]);
    let report = run_json(&api, &pages(200), repo.path(), 1);
    item(&report, "required_check", "risk-check").clone()
}

#[test]
fn a_declared_status_check_posted_by_a_job_with_statuses_write_verifies() {
    let check = risk_check(&status_repo(JUDGE_JOB, Some(r#"["risk-check"]"#)));
    assert_eq!(check["verify"]["ok"], true, "{check}");
    assert_eq!(
        check["verify"]["detail"],
        "a commit status (hoist.toml status_checks); `statuses: write` in gate.yml:judge"
    );
}

#[test]
fn workflow_level_statuses_write_counts_unless_the_job_overrides_it() {
    let top = "on: pull_request\npermissions:\n  statuses: write\njobs:\n  judge:\n    runs-on: ubuntu-latest\n    steps:\n      - run: bosun gate\n";
    let check = risk_check(&status_repo(top, Some(r#"["risk-check"]"#)));
    assert_eq!(check["verify"]["ok"], true, "{check}");
    let all = "on: pull_request\njobs:\n  verdict:\n    permissions: write-all\n    runs-on: ubuntu-latest\n    steps:\n      - run: bosun verdict apply\n";
    let check = risk_check(&status_repo(all, Some(r#"["risk-check"]"#)));
    assert_eq!(check["verify"]["ok"], true, "{check}");
    let overridden = "on: pull_request\npermissions:\n  statuses: write\njobs:\n  judge:\n    permissions:\n      contents: read\n    runs-on: ubuntu-latest\n    steps:\n      - run: bosun gate\n";
    let check = risk_check(&status_repo(overridden, Some(r#"["risk-check"]"#)));
    assert_eq!(check["verify"]["ok"], false, "{check}");
}

#[test]
fn a_declared_status_check_without_a_statuses_write_job_fails() {
    let read_only = JUDGE_JOB.replace("statuses: write", "statuses: read");
    let check = risk_check(&status_repo(&read_only, Some(r#"["risk-check"]"#)));
    assert_eq!(check["verify"]["ok"], false, "{check}");
    assert_eq!(
        check["verify"]["detail"],
        "required check `risk-check` is a commit status (hoist.toml status_checks), but no \
         workflow job has `statuses: write` to post it"
    );
}

#[test]
fn an_undeclared_status_check_still_fails_as_before() {
    let check = risk_check(&status_repo(JUDGE_JOB, None));
    assert_eq!(check["verify"]["ok"], false, "{check}");
    assert_eq!(
        check["verify"]["detail"],
        "required check `risk-check` is not the name of any job in `.github/workflows/*.yml`"
    );
}

#[test]
fn status_checks_must_be_a_list_of_names() {
    let repo = status_repo(JUDGE_JOB, Some(r#""risk-check""#));
    let api = fake(&[]);
    let out = check(&api, &pages(200))
        .current_dir(repo.path())
        .args(["check", "--json"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("hoist.toml: invalid `github`"), "{stderr}");
    assert!(stderr.contains("expected a sequence"), "{stderr}");
}
