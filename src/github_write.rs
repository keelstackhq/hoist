//! GitHub writes (spec §6): one `Change` becomes one or two API calls. Every URL segment that
//! comes from config or the repo goes through `seg()`.

use crate::auth::Secret;
use crate::change::Change;
use crate::desired::{Desired, RULESET_NAME};
use crate::error::HoistError;
use crate::http::{Http, HttpError, Method, seg};
use crate::seal::put_secret;
use serde_json::{Map, Value, json};

/// Apply one GitHub change.
pub fn execute_github(http: &Http, d: &Desired, change: &Change) -> Result<(), HoistError> {
    let repo = format!("/repos/{}/{}", seg(&d.repo.owner), seg(&d.repo.name));
    match change {
        Change::RepoSettings { patch } => {
            send(http, Method::Patch, &repo, &Value::Object(patch.clone()))
        }
        Change::Ruleset {
            existing_id,
            branch: _,
            required_checks,
            merge_queue,
        } => {
            let body = ruleset_body(required_checks, *merge_queue);
            match existing_id {
                Some(id) => send(
                    http,
                    Method::Put,
                    &format!("{repo}/rulesets/{}", seg(&id.to_string())),
                    &body,
                ),
                None => send(http, Method::Post, &format!("{repo}/rulesets"), &body),
            }
        }
        Change::ActionsPermissions { default } => send(
            http,
            Method::Put,
            &format!("{repo}/actions/permissions/workflow"),
            &json!({ "default_workflow_permissions": default }),
        ),
        Change::Environment { name, branch } => {
            let env = format!("{repo}/environments/{}", seg(name));
            send(
                http,
                Method::Put,
                &env,
                &json!({"deployment_branch_policy":{
                    "protected_branches": false,
                    "custom_branch_policies": true
                }}),
            )?;
            let path = format!("{env}/deployment-branch-policies");
            let policy = json!({"name": branch, "type": "branch"});
            match http.send_json(Method::Post, &path, Some(&policy)) {
                Ok(_) => Ok(()),
                // 303: the policy exists (GitHub redirects to it); writes never follow it.
                Err(HttpError::Rejected { status: 303, .. }) => Ok(()),
                Err(HttpError::Rejected {
                    status: 422,
                    ref message,
                    ..
                }) if message.to_lowercase().contains("already exists") => Ok(()),
                Err(err) => Err(err.into()),
            }
        }
        Change::AccountIdSecret => {
            let account = d.cloudflare.account_id.as_ref().ok_or_else(|| {
                HoistError::Usage(
                    "no Cloudflare account id to write into CLOUDFLARE_ACCOUNT_ID; set \
                     [cloudflare] account_id in hoist.toml or CLOUDFLARE_ACCOUNT_ID"
                        .into(),
                )
            })?;
            put_secret(
                http,
                d,
                "CLOUDFLARE_ACCOUNT_ID",
                &Secret::from(account.value.as_str()),
            )
        }
        _ => Err(HoistError::Usage("not a GitHub change".into())),
    }
}

fn send(http: &Http, method: Method, path: &str, body: &Value) -> Result<(), HoistError> {
    http.send_json(method, path, Some(body))?;
    Ok(())
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
            "parameters": {
                "merge_method": "MERGE",
                "grouping_strategy": "ALLGREEN",
                "max_entries_to_build": 5,
                "min_entries_to_merge": 1,
                "max_entries_to_merge": 5,
                "min_entries_to_merge_wait_minutes": 5,
                "check_response_timeout_minutes": 60
            }
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
