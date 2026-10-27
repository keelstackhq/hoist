//! Guards of `hoist secrets exec` (spec §15.7.2): decryption runs only in CI, on a protected ref,
//! never in an agent session. Defence in depth: the binding controls are the environment branch
//! rules and the context restrictions. Every refusal is exit 2 and happens before any request.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// A release tag (`*-v<digit>…` or `v<digit>…`, spec §15.8.2).
static RELEASE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[A-Za-z0-9._-]+-)?v[0-9][A-Za-z0-9._+-]*$").unwrap());

/// GitHub events whose workflow definition and code come from a protected ref.
const EVENTS: [&str; 5] = [
    "push",
    "workflow_dispatch",
    "workflow_run",
    "schedule",
    "release",
];

/// CircleCI has no default-branch variable; contexts are restricted to `main` (spec §15.8.3).
const CIRCLECI_DEFAULT_BRANCH: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    Github,
    Circleci,
}

/// Where decryption may run. `env` reads a variable; `event` is the GitHub event payload
/// (`GITHUB_EVENT_PATH`). `Err` is the refusal, starting with `secrets: `.
pub fn check(env: &dyn Fn(&str) -> Option<String>, event: Option<&Value>) -> Result<Ci, String> {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    let refuse = |why: String| Err(format!("secrets: {why}"));
    if set("CLAUDECODE") || set("CLAUDE_CODE_REMOTE") {
        return refuse("decryption is owner- or CI-only; refused in an agent session".into());
    }
    if env("CI").as_deref() != Some("true") {
        return refuse("`secrets exec` runs only in CI (CI=true)".into());
    }
    match (set("GITHUB_ACTIONS"), set("CIRCLECI")) {
        (true, false) => github(env, event).map_or_else(refuse, |()| Ok(Ci::Github)),
        (false, true) => circleci(env).map_or_else(refuse, |()| Ok(Ci::Circleci)),
        _ => refuse("exactly one of GITHUB_ACTIONS and CIRCLECI must be set".into()),
    }
}

fn github(env: &dyn Fn(&str) -> Option<String>, event: Option<&Value>) -> Result<(), String> {
    let name = env("GITHUB_EVENT_NAME").unwrap_or_default();
    if !EVENTS.contains(&name.as_str()) {
        return Err(format!(
            "event `{name}` may not decrypt; only {} (spec §15.7.2)",
            EVENTS.join(", ")
        ));
    }
    let event = event.cloned().unwrap_or_default();
    let Some(default) = event["repository"]["default_branch"].as_str() else {
        return Err("the default branch is unknown (no event payload); refused".into());
    };
    let git_ref = env("GITHUB_REF").unwrap_or_default();
    let tag = git_ref
        .strip_prefix("refs/tags/")
        .filter(|t| RELEASE_TAG.is_match(t));
    if git_ref != format!("refs/heads/{default}") && tag.is_none() {
        return Err(format!(
            "`{git_ref}` is not the default branch or a release tag"
        ));
    }
    if name == "workflow_run" {
        let head = event["workflow_run"]["head_repository"]["full_name"].as_str();
        if head.is_none() || head != env("GITHUB_REPOSITORY").as_deref() {
            return Err("the triggering run's head repository is not this repository".into());
        }
    }
    Ok(())
}

fn circleci(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    if let Some(pr) = env("CIRCLE_PR_REPONAME").filter(|p| !p.is_empty())
        && Some(pr) != env("CIRCLE_PROJECT_REPONAME")
    {
        return Err("a fork's pipeline may not decrypt".into());
    }
    let branch = env("CIRCLE_BRANCH").unwrap_or_default();
    let tag = env("CIRCLE_TAG").filter(|t| RELEASE_TAG.is_match(t));
    if branch != CIRCLECI_DEFAULT_BRANCH && tag.is_none() {
        return Err(format!(
            "branch `{branch}` is not the default branch and no release tag is set"
        ));
    }
    Ok(())
}
