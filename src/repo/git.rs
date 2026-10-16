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

/// Parse `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)` and
/// `ssh://git@github.com/o/r(.git)`. Anything else is `None`.
pub fn parse_github_remote(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
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

fn git_output(dir: &Path, args: &[&str]) -> Result<Option<String>, HoistError> {
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
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// The GitHub repo behind `origin` and the default branch (`origin/HEAD`, else `main`).
pub fn read_repo(dir: &Path) -> Result<(GithubRepo, String), HoistError> {
    let url = git_output(dir, &["remote", "get-url", "origin"])?
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            git_error(format!(
                "no `origin` remote found in {}: {NO_ORIGIN}",
                dir.display()
            ))
        })?;
    let repo = parse_github_remote(&url).ok_or_else(|| {
        git_error(format!(
            "`origin` is {url}, which is not a GitHub remote: {NO_ORIGIN}"
        ))
    })?;
    let branch = git_output(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )?
    .and_then(|b| b.strip_prefix("origin/").map(str::to_string))
    .filter(|b| !b.is_empty())
    .unwrap_or_else(|| "main".to_string());
    Ok((repo, branch))
}
