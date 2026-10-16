//! Live Cloudflare state, read with GET requests only (spec §6). Reuses `Fetch` from
//! `github.rs`: 403 becomes `Unknown(hint)`, 404 means "absent", 401 is an error.

use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::{Fetched, Http, HttpError};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct PagesProject {
    pub subdomain: String,
    pub production_branch: String,
    pub domains: Vec<String>,
}

/// An account API token as listed by `GET /user/tokens` (the value is never readable).
#[derive(Debug, Clone, PartialEq)]
pub struct CfToken {
    pub id: String,
    pub name: String,
    pub expires_on: Option<String>,
    pub status: String,
    pub issued_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveCloudflare {
    /// (project name, project or `None` when it does not exist)
    pub pages: Vec<(String, Fetch<Option<PagesProject>>)>,
    /// (script name, exists)
    pub workers: Vec<(String, Fetch<bool>)>,
    /// API tokens, to find `hoist:<owner>/<repo>:*` ones (read in M2 Task 6).
    pub tokens: Fetch<Vec<CfToken>>,
}

fn forbidden(what: &str, hint: &str) -> String {
    format!("cannot read {what}: {hint}")
}

fn parse_project(v: &Value) -> Option<PagesProject> {
    let r = v.get("result")?;
    Some(PagesProject {
        subdomain: r.get("subdomain")?.as_str()?.to_string(),
        production_branch: r.get("production_branch")?.as_str()?.to_string(),
        domains: r
            .get("domains")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn read_project(
    http: &Http,
    account: &str,
    name: &str,
) -> Result<Fetch<Option<PagesProject>>, HttpError> {
    let what = format!("Pages project `{name}`");
    let path = format!("/accounts/{account}/pages/projects/{name}");
    Ok(match http.get_json(&path)? {
        Fetched::Ok(v) => match parse_project(&v) {
            Some(p) => Fetch::Got(Some(p)),
            None => Fetch::Unknown(format!("cannot read {what}: unexpected response")),
        },
        Fetched::NotFound => Fetch::Got(None),
        Fetched::Forbidden(hint) => Fetch::Unknown(forbidden(&what, &hint)),
    })
}

fn read_worker(http: &Http, account: &str, name: &str) -> Result<Fetch<bool>, HttpError> {
    let path = format!("/accounts/{account}/workers/scripts/{name}/settings");
    Ok(match http.get_json(&path)? {
        Fetched::Ok(_) => Fetch::Got(true),
        Fetched::NotFound => Fetch::Got(false),
        Fetched::Forbidden(hint) => {
            Fetch::Unknown(forbidden(&format!("Workers script `{name}`"), &hint))
        }
    })
}

/// Read the live Cloudflare state for the desired targets. Errors only on transport or
/// response-shape failures.
pub fn read_cloudflare(http: &Http, d: &Desired) -> Result<LiveCloudflare, HoistError> {
    let cf = &d.cloudflare;
    let mut live = LiveCloudflare {
        pages: Vec::new(),
        workers: Vec::new(),
        tokens: Fetch::Unknown("token list not read yet".into()),
    };
    let Some(account) = &cf.account_id else {
        // The commands require an account id before reading Cloudflare.
        return Ok(live);
    };
    let account = &account.value;
    for p in &cf.pages {
        let name = &p.project.value;
        live.pages
            .push((name.clone(), read_project(http, account, name)?));
    }
    for w in &cf.workers {
        live.workers
            .push((w.value.clone(), read_worker(http, account, &w.value)?));
    }
    Ok(live)
}
