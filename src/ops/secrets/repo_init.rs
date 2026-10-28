//! `hoist secrets repo init OWNER/NAME [--require-check C]… [--dry-run]` (spec §15.4): a private
//! secrets repo, made if missing, with the default-branch ruleset (a reviewed PR, no force push,
//! no deletion, the given required checks). A public repo is refused. `--dry-run` reads only.

use super::check::item;
use super::sync::{owner_only, report};
use crate::auth::github_token;
use crate::desired::RULESET_NAME;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, Service, seg};
use crate::report::{Action, HumanStep, Report};
use crate::secrets::manifest::REPO;
use serde_json::{Value, json};

pub fn secrets_repo_init(
    repo: &str,
    checks: &[String],
    dry_run: bool,
    verbose: bool,
) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets repo init") {
        return Ok(refused);
    }
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets repo init: {msg}")));
    let (Some((owner, name)), true) = (repo.split_once('/'), REPO.is_match(repo)) else {
        return usage(format!("`{repo}` is not owner/name"));
    };
    let http = Http::new(Service::Github, github_token()?).verbose(verbose);
    let base = format!("/repos/{}/{}", seg(owner), seg(name));
    let mut items = Vec::new();
    let exists = match http.get_json(&base)? {
        Fetched::Ok(r) if r["private"] == Value::Bool(true) => {
            items.push(item("repository", repo, Action::Ok, "private", repo));
            true
        }
        Fetched::Ok(_) => {
            return usage(format!("{repo} is public; a secrets repo must be private"));
        }
        Fetched::NotFound => false,
        Fetched::Forbidden(why) => return usage(format!("cannot read {repo}: {why}")),
    };
    if !exists {
        // The token's own account, or an organization.
        let user = match http.get_json("/user")? {
            Fetched::Ok(u) => u["login"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        let path = match user.eq_ignore_ascii_case(owner) {
            true => "/user/repos".to_string(),
            false => format!("/orgs/{}/repos", seg(owner)),
        };
        if !dry_run {
            let body = json!({
                "name": name,
                "private": true,
                "auto_init": true,
                "has_issues": false,
                "has_wiki": false,
                "has_projects": false,
                "delete_branch_on_merge": true,
            });
            http.send_json(Method::Post, &path, Some(&body))?;
        }
        items.push(item("repository", repo, Action::Create, "private", repo));
    }
    let present = exists && {
        let path = format!("{base}/rulesets");
        match http.get_all(&path, None)? {
            Fetched::Ok(Value::Array(all)) => all.iter().any(|r| r["name"] == RULESET_NAME),
            _ => false,
        }
    };
    let ruleset = |action, why: &str| item("ruleset", RULESET_NAME, action, why, repo);
    if present {
        items.push(ruleset(Action::Ok, "exists"));
    } else {
        if !dry_run {
            let body = ruleset_body(checks);
            http.send_json(Method::Post, &format!("{base}/rulesets"), Some(&body))?;
        }
        items.push(ruleset(
            Action::Create,
            "a reviewed PR, no force push, no deletion",
        ));
    }
    let steps = match checks.is_empty() {
        true => vec![HumanStep {
            id: "required-check".into(),
            title: format!(
                "once the template's CI runs, add its gate as a required check of ruleset \
                 `{RULESET_NAME}` in {repo} (Settings → Rules)"
            ),
            url: String::new(),
            status: "pending".into(),
        }],
        false => Vec::new(),
    };
    let planned = dry_run && items.iter().any(|i| i.action == Action::Create);
    let mut done = report("secrets repo init", items, steps);
    if planned {
        done.exit_code = 1;
    } else {
        done.hint = Some(
            format!(
                "next: render the secrets template and push it to {repo}:\n  splice new \
                 gh:keelstackhq/hull//template-secrets\n"
            )
            .into(),
        );
    }
    Ok(done)
}

/// The default-branch ruleset of a secrets repo: a PR with one approval, no force push, no
/// deletion, and `checks` as required status checks.
fn ruleset_body(checks: &[String]) -> Value {
    let mut rules = vec![
        json!({"type": "pull_request", "parameters": {
            "required_approving_review_count": 1,
            "dismiss_stale_reviews_on_push": true,
            "require_code_owner_review": false,
            "require_last_push_approval": false,
            "required_review_thread_resolution": false
        }}),
        json!({"type": "non_fast_forward"}),
        json!({"type": "deletion"}),
    ];
    if !checks.is_empty() {
        let checks: Vec<Value> = checks.iter().map(|c| json!({ "context": c })).collect();
        rules.push(json!({"type": "required_status_checks", "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": checks
        }}));
    }
    json!({
        "name": RULESET_NAME,
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": rules
    })
}
