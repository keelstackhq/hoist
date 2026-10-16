//! `hoist auth check`: which read permissions each token has. GET requests only; write
//! permissions are checked in M2. Reports the token source, never the token.

use crate::auth::{TokenSource, cloudflare_token_with_source, github_token_with_source};
use crate::desired::build_desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Service};
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

/// Probe one path. Returns the verdict (`ok`), the response body when it was a 2xx, and a
/// description of the outcome. `missing_ok` treats a 404 as proof the permission exists.
fn probe(http: &Http, path: &str, missing_ok: bool) -> (bool, Option<Value>, String) {
    match http.get_json(path) {
        Ok(Fetched::Ok(body)) => (true, Some(body), "200".to_string()),
        Ok(Fetched::NotFound) => (missing_ok, None, "404".to_string()),
        Ok(Fetched::Forbidden(hint)) => (false, None, format!("denied ({hint})")),
        Err(err) => (false, None, format!("failed ({err})")),
    }
}

#[allow(clippy::too_many_arguments)]
fn permission(
    out: &mut Vec<Item>,
    http: &Http,
    area: &str,
    key: &str,
    label: &str,
    path: &str,
    source: TokenSource,
    missing_ok: bool,
) -> Option<Value> {
    let (ok, body, outcome) = probe(http, path, missing_ok);
    let verdict = if ok {
        "available"
    } else {
        "missing or not granted"
    };
    out.push(item(
        area,
        "permission",
        key,
        ok,
        format!("{label} {verdict}: GET {path} → {outcome}; token from {source}"),
        source,
    ));
    body
}

pub fn auth_check(dir: &Path, verbose: bool) -> Result<Report, HoistError> {
    let desired = build_desired(dir)?;
    let has_pages = !desired.cloudflare.pages.is_empty();
    let has_targets = has_pages || !desired.cloudflare.workers.is_empty();

    // Resolve every needed token before the first request.
    let (gh_token, gh_source) = github_token_with_source()?;
    let cloudflare = has_targets.then(cloudflare_token_with_source).transpose()?;

    let mut items = Vec::new();
    let http = Http::new(Service::Github, gh_token).verbose(verbose);
    items.push(item(
        "github",
        "token",
        "github",
        true,
        gh_source.to_string(),
        gh_source,
    ));
    let repo = &desired.repo;
    let base = format!("/repos/{}/{}", repo.owner, repo.name);
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
        permission(
            &mut items, &http, "github", key, label, &path, gh_source, false,
        );
    }

    if let Some((cf_token, cf_source)) = cloudflare {
        let http = Http::new(Service::Cloudflare, cf_token).verbose(verbose);
        items.push(item(
            "cloudflare",
            "token",
            "cloudflare",
            true,
            cf_source.to_string(),
            cf_source,
        ));
        let (ok, body, outcome) = probe(&http, "/user/tokens/verify", false);
        let status = body
            .as_ref()
            .and_then(|b| b.pointer("/result/status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let active = ok && status == "active";
        let detail = if ok {
            format!("status {status}")
        } else {
            outcome.clone()
        };
        items.push(item(
            "cloudflare",
            "permission",
            "cloudflare:token:active",
            active,
            format!(
                "Token {}: GET /user/tokens/verify → {outcome}, {detail}; token from {cf_source}",
                if active { "active" } else { "not active" }
            ),
            cf_source,
        ));
        let account = desired.cloudflare.account_id.as_ref().map(|a| &a.value);
        if let (Some(account), Some(first)) = (account, desired.cloudflare.pages.first()) {
            let path = format!("/accounts/{account}/pages/projects/{}", first.project.value);
            // A missing project still proves the token may read Pages.
            permission(
                &mut items,
                &http,
                "cloudflare",
                "cloudflare:pages:read",
                "Pages (read)",
                &path,
                cf_source,
                true,
            );
        }
    }

    let all_ok = items.iter().all(|i| i.action == Action::Ok);
    Ok(Report {
        command: "auth check".to_string(),
        items,
        human_steps: Vec::new(),
        exit_code: if all_ok { 0 } else { 1 },
    })
}
