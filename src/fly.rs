//! Running `fly` (the Fly.io CLI) for hoist: shared by the DNS address lookup and the R2
//! credential sink.

use std::process::Command;

/// A Fly app name: letters, digits, `.`, `_` and `-`, starting with a letter or digit (so it
/// can never be read as a flag).
pub fn valid_app(app: &str) -> bool {
    app.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && app
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn fly_program() -> String {
    std::env::var("HOIST_FLY")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "fly".to_string())
}

/// A `fly` that never sees hoist's tokens.
pub fn fly_command(args: &[&str]) -> Command {
    let mut c = Command::new(fly_program());
    c.args(args);
    for name in ["CLOUDFLARE_API_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        c.env_remove(name);
    }
    c
}
