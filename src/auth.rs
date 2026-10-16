//! Local credentials (spec §8.1). A `Secret` never prints its value.

use crate::error::HoistError;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A token. `Debug` and `Display` print `***`; only the HTTP client reads the value.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// The raw value, for the `Authorization` header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Secret(value.to_string())
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Secret(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

fn from_env(name: &str) -> Option<Secret> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| Secret::from(value))
}

/// Where a token came from, as a label safe to print (never the value).
pub type TokenSource = &'static str;

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// `gh auth token`, only when `gh` resolves on PATH. Empty or failing output is "absent".
/// The output is never logged.
fn gh_auth_token() -> Option<Secret> {
    let gh = on_path("gh")?;
    let output = Command::new(gh)
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| Secret::from(text))
}

/// `GITHUB_TOKEN`, then `GH_TOKEN`, then `gh auth token`.
pub fn github_token() -> Result<Secret, HoistError> {
    github_token_with_source().map(|(token, _)| token)
}

/// Like `github_token`, also returning the source label.
pub fn github_token_with_source() -> Result<(Secret, TokenSource), HoistError> {
    from_env("GITHUB_TOKEN")
        .map(|t| (t, "env GITHUB_TOKEN"))
        .or_else(|| from_env("GH_TOKEN").map(|t| (t, "env GH_TOKEN")))
        .or_else(|| gh_auth_token().map(|t| (t, "gh auth token")))
        .ok_or_else(|| {
            HoistError::Usage(
                "no GitHub token found. Set GITHUB_TOKEN or GH_TOKEN, or run `gh auth login`. \
                 The token needs the fine-grained permissions Administration, Secrets, \
                 Environments and Actions (write) and Contents and Metadata (read)"
                    .to_string(),
            )
        })
}

/// `CLOUDFLARE_API_TOKEN`.
pub fn cloudflare_token() -> Result<Secret, HoistError> {
    cloudflare_token_with_source().map(|(token, _)| token)
}

/// Like `cloudflare_token`, also returning the source label.
pub fn cloudflare_token_with_source() -> Result<(Secret, TokenSource), HoistError> {
    from_env("CLOUDFLARE_API_TOKEN")
        .map(|t| (t, "env CLOUDFLARE_API_TOKEN"))
        .ok_or_else(|| {
            HoistError::Usage(
                "no Cloudflare token found. Set CLOUDFLARE_API_TOKEN to an API token that can \
                 read Pages projects, Workers scripts and API tokens for the account"
                    .to_string(),
            )
        })
}
