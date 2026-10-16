mod common;

use common::{FakeApi, hoist};

const GITHUB_CANARY: &str = "hoist-canary-7f3a";
const CLOUDFLARE_CANARY: &str = "hoist-canary-cf9b";

/// No token value may reach stdout or stderr, even with `-v`. `auth check` is a stub until
/// Task 12 and must not print the canary either.
#[test]
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
            .env("CLOUDFLARE_ACCOUNT_ID", "acc")
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
