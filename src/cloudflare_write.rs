//! Cloudflare writes (spec §6): Pages projects and custom domains. Every URL segment that
//! comes from config goes through `seg()`.

use crate::change::Change;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Http, HttpError, Method, seg};
use crate::report::HumanStep;
use serde_json::json;

/// Apply one Cloudflare change. `Ok(Some(step))` means hoist could not do it and a human must.
pub fn execute_cloudflare(
    http: &Http,
    d: &Desired,
    change: &Change,
) -> Result<Option<HumanStep>, HoistError> {
    match change {
        Change::PagesProject { .. } | Change::Domain { .. } => {}
        // Token minting and revocation arrive with Task 6.
        _ => return Err(HoistError::Usage("not a Cloudflare change".into())),
    }
    let account = d.cloudflare.account_id.as_ref().ok_or_else(|| {
        HoistError::Usage(
            "no Cloudflare account id; set `[cloudflare] account_id` in hoist.toml or export \
             CLOUDFLARE_ACCOUNT_ID"
                .into(),
        )
    })?;
    let projects = format!("/accounts/{}/pages/projects", seg(&account.value));
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
                }) if (400..500).contains(&status) && message.to_lowercase().contains("zone") => {
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
        _ => unreachable!("filtered above"),
    }
}
