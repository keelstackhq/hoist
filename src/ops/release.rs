//! `hoist release ...` commands.

use crate::error::HoistError;
use crate::http::{Fetched, Http, seg};
use crate::repo::git::GithubRepo;

pub mod plan;
pub mod prepare;
pub mod publish;

/// The default branch for the write commands (R24): `origin/HEAD` when the checkout has it,
/// else `default_branch` from `GET /repos/{o}/{r}` (actions/checkout sets no `origin/HEAD`).
pub(crate) fn default_branch(
    gh: &Http,
    repo: &GithubRepo,
    local: Option<String>,
) -> Result<String, HoistError> {
    if let Some(b) = local {
        return Ok(b);
    }
    let path = format!("/repos/{}/{}", seg(&repo.owner), seg(&repo.name));
    match gh.get_json(&path)? {
        Fetched::Ok(v) => v
            .get("default_branch")
            .and_then(|b| b.as_str())
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                HoistError::Network(format!(
                    "GitHub: {repo} has no `default_branch` in its repository data"
                ))
            }),
        Fetched::NotFound => Err(HoistError::Network(format!(
            "GitHub: repository {repo} not found (404); check `origin` and that the token can see it"
        ))),
        Fetched::Forbidden(m) => Err(HoistError::Network(format!(
            "GitHub: cannot read repository {repo}: {m}; the token needs Metadata: read"
        ))),
    }
}
