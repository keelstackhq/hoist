//! Cloudflare writes (spec §6): Pages projects, custom domains and the minted deploy token.
//! Every URL segment that comes from config goes through `seg()`. The minted token value
//! lives only in a `Secret` and reaches GitHub sealed; no error, log or report carries it.

use crate::auth::Secret;
use crate::change::Change;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use crate::report::HumanStep;
use crate::seal::put_secret;
use crate::timeutil::{format_rfc3339, now_secs};
use serde_json::{Value, json};

const API_TOKEN_SECRET: &str = "CLOUDFLARE_API_TOKEN";
const TOKEN_LIFETIME_DAYS: i64 = 365;

fn account_id(d: &Desired) -> Result<&str, HoistError> {
    d.cloudflare
        .account_id
        .as_ref()
        .map(|a| a.value.as_str())
        .ok_or_else(|| {
            HoistError::Usage(
                "no Cloudflare account id; set `[cloudflare] account_id` in hoist.toml or export \
                 CLOUDFLARE_ACCOUNT_ID"
                    .into(),
            )
        })
}

/// Permission group ids by exact name. Scope is account-level: Cloudflare cannot scope Pages
/// to one project.
fn permission_group_ids(http: &Http, d: &Desired) -> Result<Vec<String>, HoistError> {
    let mut wanted = Vec::new();
    if !d.cloudflare.pages.is_empty() {
        wanted.push("Pages Write");
    }
    if !d.cloudflare.workers.is_empty() {
        wanted.push("Workers Scripts Write");
    }
    let path = "/user/tokens/permission_groups";
    let list = match http.get_json(path)? {
        Fetched::Ok(v) => v,
        Fetched::NotFound => {
            return Err(HoistError::Network(format!(
                "Cloudflare has no {path} (404); check CLOUDFLARE_API_TOKEN"
            )));
        }
        Fetched::Forbidden(msg) => {
            return Err(HoistError::Network(format!(
                "Cloudflare refused GET {path} (403): {msg}. The token needs permission to \
                 create API tokens"
            )));
        }
    };
    let groups = list.get("result").and_then(Value::as_array);
    wanted
        .into_iter()
        .map(|name| {
            groups
                .into_iter()
                .flatten()
                .find(|g| g.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|g| g.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .ok_or_else(|| {
                    HoistError::Network(format!(
                        "Cloudflare has no permission group named \"{name}\" in GET {path}"
                    ))
                })
        })
        .collect()
}

/// Mint the deploy token. Returns its id and its value, which only ever leaves as a sealed
/// GitHub secret.
pub fn mint_deploy_token(
    http: &Http,
    d: &Desired,
    name: &str,
) -> Result<(String, Secret), HoistError> {
    let account = account_id(d)?;
    let groups: Vec<Value> = permission_group_ids(http, d)?
        .into_iter()
        .map(|id| json!({ "id": id }))
        .collect();
    let body = json!({
        "name": name,
        "policies": [{
            "effect": "allow",
            "resources": { format!("com.cloudflare.api.account.{account}"): "*" },
            "permission_groups": groups,
        }],
        "expires_on": format_rfc3339(now_secs() + TOKEN_LIFETIME_DAYS * 86_400),
    });
    let written = http.send_json(Method::Post, "/user/tokens", Some(&body))?;
    let mut response = written.body.unwrap_or(Value::Null);
    let invalid = || {
        HoistError::Network(
            "Cloudflare returned no token id or value for POST /user/tokens".to_string(),
        )
    };
    let id = response
        .pointer("/result/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let value = match response.pointer_mut("/result/value").map(Value::take) {
        Some(Value::String(v)) => Secret::from(v),
        _ => return Err(invalid()),
    };
    drop(response);
    Ok((id.ok_or_else(invalid)?, value))
}

/// Delete an API token by id; a 404 means it is already gone.
pub fn revoke_token(http: &Http, id: &str) -> Result<(), HoistError> {
    match http.send_json(Method::Delete, &format!("/user/tokens/{}", seg(id)), None) {
        Ok(_) | Err(HttpError::Rejected { status: 404, .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Mint, seal into `CLOUDFLARE_API_TOKEN`, then revoke the replaced tokens. A failed seal
/// revokes the new token first.
fn deploy_token(
    cf: &Http,
    gh: &Http,
    d: &Desired,
    name: &str,
    replace_ids: &[String],
) -> Result<(), HoistError> {
    let (id, value) = mint_deploy_token(cf, d, name)?;
    if let Err(err) = put_secret(gh, d, API_TOKEN_SECRET, &value) {
        return Err(match revoke_token(cf, &id) {
            Ok(()) => err,
            Err(_) => HoistError::Network(format!(
                "{err}. The new Cloudflare token {id} ({name}) could not be revoked; revoke it \
                 by hand in the Cloudflare dashboard (My Profile, API Tokens)"
            )),
        });
    }
    drop(value);
    for old in replace_ids {
        revoke_token(cf, old)?;
    }
    Ok(())
}

/// Apply one Cloudflare change. `Ok(Some(step))` means hoist could not do it and a human must.
/// `cf` is the Cloudflare client; `gh` the GitHub one (the deploy token is sealed into a
/// GitHub secret).
pub fn execute_cloudflare(
    cf: &Http,
    gh: &Http,
    d: &Desired,
    change: &Change,
) -> Result<Option<HumanStep>, HoistError> {
    let http = cf;
    match change {
        Change::PagesProject { .. }
        | Change::Domain { .. }
        | Change::DeployToken { .. }
        | Change::RevokeToken { .. } => {}
        _ => return Err(HoistError::Usage("not a Cloudflare change".into())),
    }
    if let Change::RevokeToken { id, .. } = change {
        revoke_token(http, id)?;
        return Ok(None);
    }
    let account = account_id(d)?;
    if let Change::DeployToken { name, replace_ids } = change {
        deploy_token(cf, gh, d, name, replace_ids)?;
        return Ok(None);
    }
    let projects = format!("/accounts/{}/pages/projects", seg(account));
    match change {
        Change::PagesProject {
            name,
            production_branch,
            exists,
        } => {
            if *exists {
                http.send_json(
                    Method::Patch,
                    &format!("{projects}/{}", seg(name)),
                    Some(&json!({ "production_branch": production_branch })),
                )?;
            } else {
                http.send_json(
                    Method::Post,
                    &projects,
                    Some(&json!({ "name": name, "production_branch": production_branch })),
                )?;
            }
            Ok(None)
        }
        Change::Domain { project, host } => {
            let path = format!("{projects}/{}/domains", seg(project));
            match http.send_json(Method::Post, &path, Some(&json!({ "name": host }))) {
                Ok(_) => Ok(None),
                Err(HttpError::Rejected {
                    status, message, ..
                }) if (400..500).contains(&status)
                    && status != 403
                    && message.to_lowercase().contains("zone") =>
                {
                    Ok(Some(HumanStep {
                        id: format!("domain:{host}"),
                        title: format!(
                            "add {host}'s zone to Cloudflare or point a CNAME to \
                             {project}.pages.dev"
                        ),
                        url: "https://dash.cloudflare.com/".into(),
                        status: "pending".into(),
                    }))
                }
                Err(e) => Err(e.into()),
            }
        }
        _ => unreachable!("token changes returned above, others filtered"),
    }
}
