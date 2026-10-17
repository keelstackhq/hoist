//! Read the GitHub repo and default branch from the local git checkout.

use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::error::HoistError;

/// A GitHub repository, `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

impl fmt::Display for GithubRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Remove `user[:password]@` from a `scheme://` URL so credentials never get matched or echoed.
/// scp-style URLs (`git@host:path`) carry no secret and are returned unchanged.
fn strip_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host}{path}")
}

/// Parse `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)` and
/// `ssh://git@github.com/o/r(.git)`. Anything else is `None`.
pub fn parse_github_remote(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    let url = strip_userinfo(url);
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://github.com/"))?;
    let path = path.strip_suffix('/').unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let valid = |s: &str| !s.is_empty() && !s.contains('/') && !s.chars().any(char::is_whitespace);
    if valid(owner) && valid(name) {
        Some(GithubRepo {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    } else {
        None
    }
}

const NO_ORIGIN: &str = "set an `origin` remote pointing to github.com \
(for example `git remote add origin https://github.com/<owner>/<repo>.git`)";

fn git_error(message: String) -> HoistError {
    HoistError::Config {
        file: "git remote".to_string(),
        message,
    }
}

/// Run git; `Ok(Err(stderr))` when it exits non-zero.
fn git_run(dir: &Path, args: &[&str]) -> Result<Result<String, String>, HoistError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            git_error(format!(
                "could not run `git`: {e}; install git and put it on PATH"
            ))
        })?;
    if !out.status.success() {
        return Ok(Err(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// The GitHub repo behind `origin` and the default branch (`origin/HEAD`, else `main`).
pub fn read_repo(dir: &Path) -> Result<(GithubRepo, String), HoistError> {
    let (repo, branch) = read_repo_local(dir)?;
    Ok((repo, branch.unwrap_or_else(|| "main".to_string())))
}

/// The GitHub repo behind `origin` and the default branch from `origin/HEAD`, `None` when the
/// checkout has no `origin/HEAD` (actions/checkout).
pub fn read_repo_local(dir: &Path) -> Result<(GithubRepo, Option<String>), HoistError> {
    let url = match git_run(dir, &["remote", "get-url", "origin"])? {
        Ok(u) if !u.is_empty() => u,
        Ok(_) => return Err(git_error(format!("`origin` has no URL: {NO_ORIGIN}"))),
        Err(stderr) if stderr.contains("No such remote") => {
            return Err(git_error(format!(
                "no `origin` remote found in {}: {NO_ORIGIN}",
                dir.display()
            )));
        }
        Err(stderr) => {
            return Err(git_error(format!(
                "`git remote get-url origin` failed in {}: {stderr}",
                dir.display()
            )));
        }
    };
    let repo = parse_github_remote(&url).ok_or_else(|| {
        git_error(format!(
            "`origin` is {}, which is not a GitHub remote: {NO_ORIGIN}",
            strip_userinfo(&url)
        ))
    })?;
    let branch = git_run(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )?
    .ok()
    .and_then(|b| b.strip_prefix("origin/").map(str::to_string))
    .filter(|b| !b.is_empty());
    Ok((repo, branch))
}
