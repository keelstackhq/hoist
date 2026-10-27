//! Guards of `hoist secrets exec` (spec §15.7.2): decryption runs only in CI, on a protected ref,
//! never in an agent session. Defence in depth: the binding controls are the environment branch
//! rules and the context restrictions. Every refusal is exit 2 and happens before any request.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// A release tag `<name>-v<semver>` (spec §15.8.2; only the owner creates them, D-008).
static RELEASE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._]*(-[A-Za-z0-9._]+)*-v[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$")
        .unwrap()
});

/// Events that may run on a release tag; a dispatch, schedule or workflow_run never does.
const TAG_EVENTS: [&str; 2] = ["push", "release"];

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
    let git_ref = env("GITHUB_REF").unwrap_or_default();
    // A schedule payload has no `repository`; a schedule always runs on the default branch.
    let default = match event["repository"]["default_branch"].as_str() {
        Some(branch) => format!("refs/heads/{branch}"),
        None if name == "schedule" && git_ref.starts_with("refs/heads/") => git_ref.clone(),
        None => return Err("the default branch is unknown (no event payload); refused".into()),
    };
    if let Some(tag) = git_ref
        .strip_prefix("refs/tags/")
        .filter(|t| RELEASE_TAG.is_match(t))
    {
        if !TAG_EVENTS.contains(&name.as_str()) {
            return Err(format!(
                "release tag `{tag}` decrypts only for push and release events"
            ));
        }
    } else if git_ref != default {
        return Err(format!(
            "`{git_ref}` is not the default branch or a release tag"
        ));
    }
    if name == "workflow_run" {
        let head = event["workflow_run"]["head_repository"]["id"].as_u64();
        if head.is_none() || head != event["repository"]["id"].as_u64() {
            return Err("the triggering run's head repository is not this repository".into());
        }
    }
    Ok(())
}

fn circleci(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    // CircleCI sets these only for a pull request from a fork (even one keeping the name).
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    if set("CIRCLE_PR_REPONAME") || set("CIRCLE_PR_USERNAME") {
        return Err("a fork's pipeline may not decrypt".into());
    }
    // Releases decrypt on GitHub only (§15.7.4), so no tag path here.
    let branch = env("CIRCLE_BRANCH").unwrap_or_default();
    if branch != CIRCLECI_DEFAULT_BRANCH {
        return Err(format!(
            "branch `{branch}` is not the default branch `{CIRCLECI_DEFAULT_BRANCH}`"
        ));
    }
    Ok(())
}
