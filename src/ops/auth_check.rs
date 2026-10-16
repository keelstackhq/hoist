//! `hoist auth check`: which read permissions each token has. GET requests only; write
//! permissions are checked in M2. Reports the token source, never the token.

use crate::auth::{TokenSource, cloudflare_token_with_source, github_token_with_source};
use crate::desired::build_desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Service};
use crate::report::{Action, Item, Report};
use serde_json::Value;
use std::path::Path;

fn item(area: &str, kind: &str, key: &str, ok: bool, reason: String, source: &str) -> Item {
    Item {
        area: area.to_string(),
        kind: kind.to_string(),
        key: key.to_string(),
        action: if ok { Action::Ok } else { Action::Unknown },
        reason,
        source: source.to_string(),
        verify: None,
    }
}

/// One probe's result: the verdict, the 2xx body, the reason text and whether the token
/// itself was rejected (401).
struct Probe {
    ok: bool,
    body: Option<Value>,
    reason: String,
    rejected: bool,
}

/// Probe one path. `missing_ok` treats a 404 as proof the permission exists. A denied probe
/// quotes the API's own message: a 403 is often a plan limit or a proxy, not a missing grant.
fn probe(http: &Http, label: &str, path: &str, source: TokenSource, missing_ok: bool) -> Probe {
    let (ok, body, outcome, rejected) = match http.get_json(path) {
        Ok(Fetched::Ok(body)) => (true, Some(body), "200".to_string(), false),
        Ok(Fetched::NotFound) if missing_ok => (true, None, "404".to_string(), false),
        Ok(Fetched::NotFound) => (false, None, "not found (404)".to_string(), false),
        Ok(Fetched::Forbidden(hint)) => (false, None, hint, false),
        Err(HttpError::Unauthorized { .. }) => {
            (false, None, "token rejected (401)".to_string(), true)
        }
        Err(err) => (false, None, err.to_string(), false),
    };
    let reason = if ok {
        format!("{label} available: GET {path} → {outcome}; token from {source}")
    } else {
        format!("{label} not readable: {outcome}; GET {path}; token from {source}")
    };
    Probe {
        ok,
        body,
        reason,
        rejected,
    }
}

/// The `token` item: ok with the source label, or unknown when any probe got a 401.
fn token_item(area: &str, source: TokenSource, rejected: bool) -> Item {
    if rejected {
        item(
            area,
            "token",
            area,
            false,
            "token rejected (401)".into(),
            source,
        )
    } else {
        item(area, "token", area, true, source.to_string(), source)
    }
}

pub fn auth_check(dir: &Path, verbose: bool) -> Result<Report, HoistError> {
    let desired = build_desired(dir)?;
    let has_pages = !desired.cloudflare.pages.is_empty();
    let has_targets = has_pages || !desired.cloudflare.workers.is_empty();

    // Resolve every token before the first request. Only a missing GitHub token is fatal; a
    // missing Cloudflare token is reported and the GitHub half still runs.
    let (gh_token, gh_source) = github_token_with_source()?;
    let cloudflare = has_targets.then(|| cloudflare_token_with_source().ok());

    let mut items = Vec::new();
    let http = Http::new(Service::Github, gh_token).verbose(verbose);
    let repo = &desired.repo;
    let base = format!("/repos/{}/{}", repo.owner, repo.name);
    let mut rejected = false;
    for (key, label, path) in [
        (
            "github:administration:read",
            "Administration (read)",
            format!("{base}/rulesets"),
        ),
        (
            "github:secrets:read",
            "Secrets (read)",
            format!("{base}/actions/secrets"),
        ),
        (
            "github:environments:read",
            "Environments (read)",
            format!("{base}/environments"),
        ),
        (
            "github:actions:read",
            "Actions (read)",
            format!("{base}/actions/permissions/workflow"),
        ),
    ] {
        let p = probe(&http, label, &path, gh_source, false);
        rejected |= p.rejected;
        items.push(item("github", "permission", key, p.ok, p.reason, gh_source));
    }
    items.insert(0, token_item("github", gh_source, rejected));

    match cloudflare {
        None => {}
        Some(None) => items.push(item(
            "cloudflare",
            "token",
            "cloudflare",
            false,
            "no token; set CLOUDFLARE_API_TOKEN".to_string(),
            "none",
        )),
        Some(Some((cf_token, cf_source))) => {
            let http = Http::new(Service::Cloudflare, cf_token).verbose(verbose);
            let start = items.len();
            let p = probe(&http, "Token", "/user/tokens/verify", cf_source, false);
            let mut rejected = p.rejected;
            let status = p
                .body
                .as_ref()
                .and_then(|b| b.pointer("/result/status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let active = p.ok && status == "active";
            let reason = if p.ok {
                format!(
                    "Token {}: GET /user/tokens/verify → 200, status {status}; token from \
                     {cf_source}",
                    if active { "active" } else { "not active" }
                )
            } else {
                p.reason
            };
            items.push(item(
                "cloudflare",
                "permission",
                "cloudflare:token:active",
                active,
                reason,
                cf_source,
            ));
            let account = desired.cloudflare.account_id.as_ref().map(|a| &a.value);
            match (account, desired.cloudflare.pages.first()) {
                (_, None) => {}
                (None, Some(_)) => items.push(item(
                    "cloudflare",
                    "permission",
                    "cloudflare:pages:read",
                    false,
                    "Pages (read) not checked: no account id; set `[cloudflare] account_id` in \
                     hoist.toml or export CLOUDFLARE_ACCOUNT_ID"
                        .to_string(),
                    cf_source,
                )),
                (Some(account), Some(first)) => {
                    let path =
                        format!("/accounts/{account}/pages/projects/{}", first.project.value);
                    // A missing project still proves the token may read Pages.
                    let p = probe(&http, "Pages (read)", &path, cf_source, true);
                    rejected |= p.rejected;
                    items.push(item(
                        "cloudflare",
                        "permission",
                        "cloudflare:pages:read",
                        p.ok,
                        p.reason,
                        cf_source,
                    ));
                }
            }
            items.insert(start, token_item("cloudflare", cf_source, rejected));
        }
    }

    let all_ok = items.iter().all(|i| i.action == Action::Ok);
    Ok(Report {
        command: "auth check".to_string(),
        items,
        human_steps: Vec::new(),
        exit_code: if all_ok { 0 } else { 1 },
        text: None,
    })
}
