mod common;

use common::{FakeApi, hoist};

const GITHUB_CANARY: &str = "hoist-canary-7f3a";
const CLOUDFLARE_CANARY: &str = "hoist-canary-cf9b";

/// No token value may reach stdout or stderr, even with `-v`, on the success and the error
/// path. The fake serves 200 routes plus a 401 and a 500, so both run, and the test fails if
/// no request was made or a command exits with an unexpected code.
#[test]
fn canary_never_printed() {
    let api = FakeApi::start(&[
        ("GET /repos/acme/x", 200, "github/repo.json", &[]),
        (
            "GET /repos/acme/x/rulesets",
            200,
            "github/empty_list.json",
            &[],
        ),
        (
            "GET /repos/acme/x/actions/secrets",
            401,
            "github/forbidden.json",
            &[],
        ),
        ("GET /repos/acme/x/environments", 500, "", &[]),
        (
            "GET /repos/acme/x/actions/permissions/workflow",
            200,
            "github/actions_permissions.json",
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
            "GET /user/tokens/verify",
            200,
            "cloudflare/token_verify_active.json",
            &[],
        ),
        (
            "GET /accounts/acc/pages/projects/keelstack-landing",
            200,
            "cloudflare/pages_project.json",
            &[],
        ),
    ]);
    let repo = common::fixture_repo("site", "https://github.com/acme/x.git");
    // (args, expected exit code): `plan` fails on the 401 (and the 500) with exit 3;
    // `auth check` reports the failed probes (1).
    let cases: [(&[&str], i32); 3] = [
        (&["plan", "--json", "-v"], 3),
        (&["plan", "-v"], 3),
        (&["auth", "check", "-v"], 1),
    ];
    let mut seen = 0;
    for (args, code) in cases {
        let output = hoist()
            .current_dir(repo.path())
            .env("GITHUB_TOKEN", GITHUB_CANARY)
            .env("CLOUDFLARE_API_TOKEN", CLOUDFLARE_CANARY)
            .env("HOIST_GITHUB_API", api.url())
            .env("HOIST_CLOUDFLARE_API", api.url())
            .env("CLOUDFLARE_ACCOUNT_ID", "acc")
            .args(args)
            .output()
            .expect("run hoist");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let now = api.requests().len();
        assert!(now > seen, "{args:?} made no request");
        seen = now;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !text.contains("hoist-canary"),
            "{args:?} leaked a token:\n{text}"
        );
    }
    let requests = api.requests();
    assert!(requests.iter().any(|(_, p, _)| p.ends_with("/secrets")));
    assert!(
        requests
            .iter()
            .any(|(_, p, _)| p.ends_with("/environments"))
    );
    api.assert_only_get();
}
