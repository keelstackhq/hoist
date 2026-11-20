//! GitHub writes (spec §6): one `Change` becomes one or two API calls. Every URL segment that
//! comes from config or the repo goes through `seg()`.

use crate::auth::Secret;
use crate::change::{Change, Reviewers};
use crate::desired::{Desired, RULESET_NAME};
use crate::error::HoistError;
use crate::github::plan_limited;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use crate::seal::put_home;
use serde_json::{Map, Value, json};

/// Apply one GitHub change.
pub fn execute_github(http: &Http, d: &Desired, change: &Change) -> Result<(), HoistError> {
    let repo = format!("/repos/{}/{}", seg(&d.repo.owner), seg(&d.repo.name));
    match change {
        Change::RepoSettings { patch } => {
            // Immutable releases are their own endpoint: PUT enables, DELETE disables.
            let mut patch = patch.clone();
            if let Some(on) = patch.remove("immutable_releases").and_then(|v| v.as_bool()) {
                let method = if on { Method::Put } else { Method::Delete };
                http.send_json(method, &format!("{repo}/immutable-releases"), None)?;
            }
            if patch.is_empty() {
                return Ok(());
            }
            send(http, Method::Patch, &repo, &Value::Object(patch))
        }
        Change::Ruleset {
            existing_id,
            branch: _,
            required_checks,
            merge_queue,
        } => {
            let body = ruleset_body(required_checks, *merge_queue);
            let sent = match existing_id {
                Some(id) => {
                    let path = format!("{repo}/rulesets/{}", seg(&id.to_string()));
                    http.send_json(Method::Put, &path, Some(&body))
                }
                None => http.send_json(Method::Post, &format!("{repo}/rulesets"), Some(&body)),
            };
            match sent {
                Ok(_) => Ok(()),
                Err(HttpError::Rejected {
                    status: 403,
                    message,
                    ..
                }) if plan_limited(&message) => Err(HoistError::PlanLimited(message)),
                Err(err) => Err(err.into()),
            }
        }
        Change::ActionsPermissions { default } => send(
            http,
            Method::Put,
            &format!("{repo}/actions/permissions/workflow"),
            &json!({ "default_workflow_permissions": default }),
        ),
        Change::Environment {
            name,
            branches,
            write_env,
            release,
            reviewers,
        } => {
            let env = format!("{repo}/environments/{}", seg(name));
            // No branches: no restriction at all (`null`), as for an environment made by hand.
            let mut body = if branches.is_empty() {
                json!({"deployment_branch_policy": null})
            } else {
                json!({"deployment_branch_policy":{
                    "protected_branches": false,
                    "custom_branch_policies": true
                }})
            };
            if let (true, Reviewers::Users(ids)) = (*release, reviewers) {
                // The release environment gates publishing: its reviewers approve it.
                let users: Vec<Value> = ids.iter().map(|i| json!({"type":"User","id":i})).collect();
                body["reviewers"] = Value::Array(users);
            }
            let mut limited = None;
            let put = if *write_env {
                http.send_json(Method::Put, &env, Some(&body))
            } else {
                Ok(crate::http::Written {
                    status: 200,
                    body: None,
                })
            };
            match put {
                Ok(_) => {}
                // The plan has no required reviewers (private repo): still create the
                // environment and its branch policy, then report the gap. Only a 422 that
                // says so; any other 422 is a real error.
                Err(HttpError::Rejected {
                    status: 422,
                    message,
                    ..
                }) if *release && reviewers_unavailable(&message) => {
                    body.as_object_mut().map(|o| o.remove("reviewers"));
                    send(http, Method::Put, &env, &body)?;
                    limited = Some(message);
                }
                Err(err) => return Err(err.into()),
            }
            let path = format!("{env}/deployment-branch-policies");
            for branch in branches {
                let policy = json!({"name": branch, "type": "branch"});
                match http.send_json(Method::Post, &path, Some(&policy)) {
                    Ok(_) => {}
                    // 303: the policy exists (GitHub redirects to it); writes never follow it.
                    Err(HttpError::Rejected { status: 303, .. }) => {}
                    Err(HttpError::Rejected {
                        status: 422,
                        ref message,
                        ..
                    }) if message.to_lowercase().contains("already exists") => {}
                    Err(err) => return Err(err.into()),
                }
            }
            limited.map_or(Ok(()), |m| Err(HoistError::PlanLimited(m)))
        }
        Change::AccountIdSecret => {
            let account = d.cloudflare.account_id.as_ref().ok_or_else(|| {
                HoistError::Usage(
                    "no Cloudflare account id to write into CLOUDFLARE_ACCOUNT_ID; set \
                     [cloudflare] account_id in hoist.toml or CLOUDFLARE_ACCOUNT_ID"
                        .into(),
                )
            })?;
            put_home(
                http,
                d,
                "CLOUDFLARE_ACCOUNT_ID",
                &Secret::from(account.value.as_str()),
            )
        }
        _ => Err(HoistError::Usage("not a GitHub change".into())),
    }
}

/// Whether a 422 on the environment PUT says the plan allows no required reviewers.
fn reviewers_unavailable(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("reviewer") || m.contains("protection rule")
}

/// Who approves the release environment, resolved in the apply preflight before any write:
/// the `[release] reviewers` logins (`GET /users/<login>`), else the token's user
/// (`GET /user`), unless that is a bot: a bot must not be the sole approver.
pub fn resolve_reviewers(http: &Http, logins: &[String]) -> Result<Reviewers, HoistError> {
    if logins.is_empty() {
        return match http.get_json("/user")? {
            Fetched::Ok(v) if v["id"].is_u64() => {
                let login = v["login"].as_str().unwrap_or_default();
                if v["type"] == "Bot" || login.ends_with("[bot]") {
                    Ok(Reviewers::HumanNeeded)
                } else {
                    Ok(Reviewers::Users(vec![v["id"].as_u64().unwrap_or_default()]))
                }
            }
            _ => Err(HoistError::Usage(
                "cannot name a required reviewer for the release environment: GET /user did not \
                 return a user; apply with a user token (an App installation token has no user) \
                 or set [release] reviewers in hoist.toml"
                    .into(),
            )),
        };
    }
    let mut ids = Vec::new();
    for login in logins {
        match http.get_json(&format!("/users/{}", seg(login)))? {
            Fetched::Ok(v) if v["id"].is_u64() => ids.push(v["id"].as_u64().unwrap_or_default()),
            _ => {
                return Err(HoistError::Usage(format!(
                    "[release] reviewers: `{login}` is not a GitHub user this token can read \
                     (GET /users/{login}); fix the login in hoist.toml"
                )));
            }
        }
    }
    Ok(Reviewers::Users(ids))
}

fn send(http: &Http, method: Method, path: &str, body: &Value) -> Result<(), HoistError> {
    http.send_json(method, path, Some(body))?;
    Ok(())
}

/// The merge queue settings hoist writes.
pub fn merge_queue_parameters() -> Value {
    json!({
        "merge_method": "MERGE",
        "grouping_strategy": "ALLGREEN",
        "max_entries_to_build": 5,
        "min_entries_to_merge": 1,
        "max_entries_to_merge": 5,
        "min_entries_to_merge_wait_minutes": 5,
        "check_response_timeout_minutes": 60
    })
}

fn ruleset_body(required_checks: &[String], merge_queue: bool) -> Value {
    let checks: Vec<Value> = required_checks
        .iter()
        .map(|c| json!({ "context": c }))
        .collect();
    let mut rules = vec![json!({
        "type": "required_status_checks",
        "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": checks
        }
    })];
    if merge_queue {
        rules.push(json!({
            "type": "merge_queue",
            "parameters": merge_queue_parameters()
        }));
    }
    let mut body = Map::new();
    body.insert("name".into(), json!(RULESET_NAME));
    body.insert("target".into(), json!("branch"));
    body.insert("enforcement".into(), json!("active"));
    body.insert(
        "conditions".into(),
        json!({"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}}),
    );
    body.insert("rules".into(), Value::Array(rules));
    Value::Object(body)
}
