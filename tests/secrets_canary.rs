mod common;

use common::{FakeApi, hoist};

const GITHUB_CANARY: &str = "hoist-canary-7f3a";
const CLOUDFLARE_CANARY: &str = "hoist-canary-cf9b";

/// No token value may reach stdout or stderr, even with `-v`. Meaningful once `plan` and
/// `auth check` call the APIs (Tasks 10 and 12); remove the ignore with Task 10.
#[test]
#[ignore = "meaningful once plan (Task 10) and auth check (Task 12) call the APIs"]
fn canary_never_printed() {
    let api = FakeApi::start(&[]);
    let repo = common::fixture_repo("site", "https://github.com/acme/x.git");
    for args in [
        vec!["plan", "--json", "-v"],
        vec!["plan", "-v"],
        vec!["auth", "check", "-v"],
    ] {
        let output = hoist()
            .current_dir(repo.path())
            .env("GITHUB_TOKEN", GITHUB_CANARY)
            .env("CLOUDFLARE_API_TOKEN", CLOUDFLARE_CANARY)
            .env("HOIST_GITHUB_API", api.url())
            .env("HOIST_CLOUDFLARE_API", api.url())
            .args(&args)
            .output()
            .expect("run hoist");
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
    api.assert_only_get();
}
