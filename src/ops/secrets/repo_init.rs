//! `hoist secrets repo init OWNER/NAME [--require-check C]… [--dry-run]` (spec §15.4): a private
//! secrets repo, made if missing, with its own ruleset `hoist: secrets repo` (a reviewed PR, no force push,
//! no deletion, the given required checks). A public repo is refused. `--dry-run` reads only.

use super::check::item;
use super::sync::{owner_only, report};
use crate::auth::github_token;
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
        Fetched::Ok(r) => match r["private"].as_bool() {
            Some(true) => {
                items.push(item("repository", repo, Action::Ok, "private", repo));
                true
            }
            Some(false) => {
                return usage(format!("{repo} is public; a secrets repo must be private"));
            }
            None => return usage(format!("cannot tell whether {repo} is private")),
        },
        Fetched::NotFound => false,
        Fetched::Forbidden(why) => return usage(format!("cannot read {repo}: {why}")),
    };
    // An existing repo's ruleset is read before anything is written.
    let found = match exists {
        true => find_ruleset(&http, &base, repo)?,
        false => None,
    };
    if !exists {
        // The token's own account, or an organization.
        let user = match http.get_json("/user")? {
            Fetched::Ok(u) => u["login"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        if user.is_empty() {
            return usage(
                "cannot read the token's account (GET /user); use a token for the account or \
                 organization that will own the repo"
                    .into(),
            );
        }
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
            if let Err(e) = http.send_json(Method::Post, &path, Some(&body)) {
                return usage(format!(
                    "could not create {repo} ({e}); it may already exist where this token \
                     cannot see it, or the token may not create repositories in {owner}"
                ));
            }
        }
        items.push(item("repository", repo, Action::Create, "private", repo));
    }
    let ruleset = |action, why: String| item("ruleset", RULESET, action, why, repo);
    items.push(match found {
        Some(rules) => match weaker(&rules, checks) {
            None => ruleset(Action::Ok, "exists".into()),
            Some(why) => ruleset(
                Action::Drift,
                format!("protects less than `repo init` makes: {why}; fix it in Settings → Rules"),
            ),
        },
        None if dry_run => ruleset(Action::Create, PROTECTS.into()),
        None => {
            let body = ruleset_body(checks);
            match http.send_json(Method::Post, &format!("{base}/rulesets"), Some(&body)) {
                Ok(_) => ruleset(Action::Create, PROTECTS.into()),
                Err(e) => ruleset(
                    Action::Drift,
                    format!(
                        "could not add it ({e}); rulesets on a private repo need a paid plan \
                             and an admin token"
                    ),
                ),
            }
        }
    });
    let steps = match checks.is_empty() {
        true => vec![HumanStep {
            id: "required-check".into(),
            title: format!(
                "once the template's CI runs, add its gate as a required check of ruleset \
                 `{RULESET}` in {repo} (Settings → Rules)"
            ),
            url: String::new(),
            status: "pending".into(),
        }],
        false => Vec::new(),
    };
    let planned = dry_run && items.iter().any(|i| i.action == Action::Create);
    let mut done = report("secrets repo init", items, steps);
    let drift = done.items.iter().any(|i| i.action == Action::Drift);
    if planned {
        done.exit_code = 1;
    } else if !drift {
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

/// The ruleset `repo init` makes and checks.
const RULESET: &str = "hoist: secrets repo";
const PROTECTS: &str = "a reviewed PR, no force push, no deletion";

/// The existing ruleset named [`RULESET`], in full, or `None`. Unreadable rulesets are an error
/// before any write: missing and unreadable must not look alike.
fn find_ruleset(http: &Http, base: &str, repo: &str) -> Result<Option<Value>, HoistError> {
    let fail = |why: String| {
        HoistError::Usage(format!(
            "secrets repo init: cannot read the rulesets of {repo} ({why}); rulesets need an \
             admin token, and a paid plan for a private repo"
        ))
    };
    let list = match http.get_all(&format!("{base}/rulesets"), None)? {
        Fetched::Ok(Value::Array(all)) => all,
        Fetched::Ok(_) => return Err(fail("not a list".into())),
        Fetched::NotFound => return Err(fail("404".into())),
        Fetched::Forbidden(why) => return Err(fail(why)),
    };
    let Some(id) = list
        .iter()
        .find(|r| r["name"] == RULESET)
        .map(|r| r["id"].clone())
    else {
        return Ok(None);
    };
    match http.get_json(&format!("{base}/rulesets/{}", seg(&id.to_string())))? {
        Fetched::Ok(full) => Ok(Some(full)),
        Fetched::NotFound => Err(fail("404".into())),
        Fetched::Forbidden(why) => Err(fail(why)),
    }
}

/// What `rules` lacks of what [`ruleset_body`] makes, or `None`.
fn weaker(rules: &Value, checks: &[String]) -> Option<String> {
    let list = rules["rules"].as_array().cloned().unwrap_or_default();
    let has = |t: &str| list.iter().any(|r| r["type"] == t);
    let contexts: Vec<String> = list
        .iter()
        .filter(|r| r["type"] == "required_status_checks")
        .filter_map(|r| r["parameters"]["required_status_checks"].as_array())
        .flatten()
        .filter_map(|c| c["context"].as_str().map(str::to_string))
        .collect();
    let reviews = list
        .iter()
        .find(|r| r["type"] == "pull_request")
        .and_then(|r| r["parameters"]["required_approving_review_count"].as_u64())
        .unwrap_or(0);
    let default_branch = rules["conditions"]["ref_name"]["include"]
        .as_array()
        .is_some_and(|i| i.iter().any(|r| r == "~DEFAULT_BRANCH"));
    let bypass = rules["bypass_actors"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    if rules["enforcement"] != "active" {
        Some("not enforced".into())
    } else if !default_branch {
        Some("not on the default branch".into())
    } else if bypass {
        Some("some actors may bypass it".into())
    } else if reviews < 1 {
        Some("no required review".into())
    } else if !has("non_fast_forward") || !has("deletion") {
        Some("force push or deletion allowed".into())
    } else {
        checks
            .iter()
            .find(|c| !contexts.contains(c))
            .map(|c| format!("no required check `{c}`"))
    }
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
        "name": RULESET,
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": rules
    })
}
