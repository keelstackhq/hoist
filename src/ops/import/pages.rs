//! `[deploy] project` overrides for Pages targets (spec §7.5).

use super::{Found, ROOT, Val};
use crate::cloudflare::read_cloudflare;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::{Fetched, Http, seg};
use serde_json::Value;

/// `[deploy] project` for a Pages target whose inferred project is missing live, when exactly
/// one live Pages project name starts with the target name.
pub(super) fn pages_overrides(
    http: &Http,
    d: &Desired,
    found: &mut Found,
) -> Result<(), HoistError> {
    let Some(account) = &d.cloudflare.account_id else {
        return Ok(());
    };
    let live = read_cloudflare(http, d)?;
    let mut listed: Option<Fetch<Vec<String>>> = None;
    for p in &d.cloudflare.pages {
        if p.project.source != "default" {
            continue;
        }
        let file = format!("{}/{ROOT}", p.dir);
        let state = live
            .pages
            .iter()
            .find(|(n, _)| *n == p.project.value)
            .map(|(_, f)| f.clone());
        match state {
            Some(Fetch::Got(None)) => {}
            Some(Fetch::Unknown(why)) => {
                found.unknown(&file, "cloudflare", "deploy", "project", &why);
                continue;
            }
            Some(Fetch::Got(Some(_))) | None => continue,
        }
        if listed.is_none() {
            listed = Some(list_projects(http, &account.value)?);
        }
        match listed.as_ref().map(Fetch::as_got) {
            Some(Ok(names)) => {
                let matches: Vec<&String> =
                    names.iter().filter(|n| n.starts_with(&p.target)).collect();
                if let [only] = matches.as_slice() {
                    found.set(
                        &file,
                        "cloudflare",
                        "deploy",
                        "project",
                        Val::Str((*only).clone()),
                    );
                }
            }
            Some(Err(why)) => found.unknown(&file, "cloudflare", "deploy", "project", why),
            None => {}
        }
    }
    Ok(())
}

fn list_projects(http: &Http, account: &str) -> Result<Fetch<Vec<String>>, HoistError> {
    let path = format!("/accounts/{}/pages/projects", seg(account));
    Ok(match http.get_all(&path, None)? {
        Fetched::Ok(Value::Array(items)) => Fetch::Got(
            items
                .iter()
                .filter_map(|i| i.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
        ),
        Fetched::Ok(_) => Fetch::Unknown("cannot list Pages projects: unexpected response".into()),
        Fetched::NotFound => Fetch::Unknown("cannot list Pages projects: not found (404)".into()),
        Fetched::Forbidden(hint) => Fetch::Unknown(format!("cannot list Pages projects: {hint}")),
    })
}
