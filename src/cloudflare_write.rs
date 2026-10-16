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

/// Permission group ids by exact name (one GET). Scope is account-level: Cloudflare cannot
/// scope Pages to one project.
pub fn permission_group_ids(http: &Http, d: &Desired) -> Result<Vec<String>, HoistError> {
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
    account_id(d)?;
    let groups = permission_group_ids(http, d)?;
    mint_with_groups(http, d, name, &groups, false)
}

/// `mint_deploy_token` with permission group ids already resolved (`permission_group_ids`).
/// `secret_exists`: `CLOUDFLARE_API_TOKEN` is already set (rotate), which changes the advice
/// after a transport error.
pub fn mint_with_groups(
    http: &Http,
    d: &Desired,
    name: &str,
    group_ids: &[String],
    secret_exists: bool,
) -> Result<(String, Secret), HoistError> {
    let account = account_id(d)?;
    let groups: Vec<Value> = group_ids.iter().map(|id| json!({ "id": id })).collect();
    let body = json!({
        "name": name,
        "policies": [{
            "effect": "allow",
            "resources": { format!("com.cloudflare.api.account.{account}"): "*" },
            "permission_groups": groups,
        }],
        "expires_on": format_rfc3339(now_secs() + TOKEN_LIFETIME_DAYS * 86_400),
    });
    let written = match http.send_json(Method::Post, "/user/tokens", Some(&body)) {
        Ok(w) => w,
        Err(e @ HttpError::Transport { .. }) => {
            let next = if secret_exists {
                "CLOUDFLARE_API_TOKEN still holds the previous token; run `hoist secrets \
                 rotate` again (it revokes every older deploy token)"
            } else {
                "the next `apply` replaces it"
            };
            return Err(HoistError::Network(format!(
                "{e}. A token named `{name}` may have been created; {next}, or revoke it in the \
                 Cloudflare dashboard"
            )));
        }
        Err(e) => return Err(e.into()),
    };
    let mut response = written.body.unwrap_or(Value::Null);
    let id = response
        .pointer("/result/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let value = match response.pointer_mut("/result/value").map(Value::take) {
        Some(Value::String(v)) => Some(Secret::from(v)),
        _ => None,
    };
    drop(response);
    match (id, value) {
        (Some(id), Some(value)) => Ok((id, value)),
        (Some(id), None) => {
            let note = match revoke_token(http, &id) {
                Ok(()) => "the new token was revoked".to_string(),
                Err(_) => format!("revoke token {id} by hand in the Cloudflare dashboard"),
            };
            Err(HoistError::Network(format!(
                "Cloudflare returned no token value for POST /user/tokens; {note}"
            )))
        }
        (None, _) => Err(HoistError::Network(format!(
            "Cloudflare returned no token id for POST /user/tokens; a token named `{name}` may \
             exist; revoke it in the Cloudflare dashboard"
        ))),
    }
}

/// Delete an API token by id; a 404 means it is already gone.
pub fn revoke_token(http: &Http, id: &str) -> Result<(), HoistError> {
    match http.send_json(Method::Delete, &format!("/user/tokens/{}", seg(id)), None) {
        Ok(_) | Err(HttpError::Rejected { status: 404, .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Mint, seal into `CLOUDFLARE_API_TOKEN`, then revoke the replaced tokens. A failed seal
/// revokes the new token first. `groups`: permission group ids resolved in apply's preflight,
/// else resolved here.
fn deploy_token(
    cf: &Http,
    gh: &Http,
    d: &Desired,
    name: &str,
    replace_ids: &[String],
    groups: Option<&[String]>,
) -> Result<(), HoistError> {
    let (id, value) = match groups {
        Some(groups) => mint_with_groups(cf, d, name, groups, false)?,
        None => mint_deploy_token(cf, d, name)?,
    };
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
        if let Err(err) = revoke_token(cf, old) {
            return Err(HoistError::Network(format!(
                "{err}. The new deploy token was written and sealed into \
                 {API_TOKEN_SECRET}; only the revocation of {old} failed. Revoke it by hand in \
                 the Cloudflare dashboard"
            )));
        }
    }
    Ok(())
}

/// Apply one Cloudflare change. `Ok(Some(step))` means hoist could not do it and a human must.
/// `cf` is the Cloudflare client; `gh` the GitHub one, needed only by the deploy token (sealed
/// into a GitHub secret).
pub fn execute_cloudflare(
    cf: &Http,
    gh: Option<&Http>,
    d: &Desired,
    change: &Change,
) -> Result<Option<HumanStep>, HoistError> {
    execute_cloudflare_with(cf, gh, d, change, None)
}

/// `execute_cloudflare` with the deploy token's permission group ids already resolved (apply
/// resolves them in its preflight, so a failure there exits 3 before any write).
pub fn execute_cloudflare_with(
    cf: &Http,
    gh: Option<&Http>,
    d: &Desired,
    change: &Change,
    groups: Option<&[String]>,
) -> Result<Option<HumanStep>, HoistError> {
    match change {
        Change::PagesProject { .. }
        | Change::Domain { .. }
        | Change::DeployToken { .. }
        | Change::RevokeToken { .. } => {}
        _ => return Err(HoistError::Usage("not a Cloudflare change".into())),
    }
    if let Change::RevokeToken { id, .. } = change {
        revoke_token(cf, id)?;
        return Ok(None);
    }
    let account = account_id(d)?;
    if let Change::DeployToken { name, replace_ids } = change {
        let gh = gh.ok_or_else(|| {
            HoistError::Usage(
                "the deploy token is sealed into a GitHub secret; GitHub must be read too \
                 (drop `--area cloudflare`)"
                    .into(),
            )
        })?;
        deploy_token(cf, gh, d, name, replace_ids, groups)?;
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
                cf.send_json(
                    Method::Patch,
                    &format!("{projects}/{}", seg(name)),
                    Some(&json!({ "production_branch": production_branch })),
                )?;
            } else {
                cf.send_json(
                    Method::Post,
                    &projects,
                    Some(&json!({ "name": name, "production_branch": production_branch })),
                )?;
            }
            Ok(None)
        }
        Change::Domain { project, host } => {
            let path = format!("{projects}/{}/domains", seg(project));
            match cf.send_json(Method::Post, &path, Some(&json!({ "name": host }))) {
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
