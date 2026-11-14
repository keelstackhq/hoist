//! The token that opens the release PR (spec §9.2). Checks run on a PR only when a token that
//! triggers workflows opened it; `GITHUB_TOKEN` in Actions does not.
//! Order: `HOIST_RELEASE_TOKEN`; an installation token of the release GitHub App
//! (`HOIST_RELEASE_APP_ID` + `HOIST_RELEASE_APP_KEY`, `contents` and `pull_requests` write on
//! this repo); else the ordinary token, which in Actions cannot trigger checks.

use crate::auth::Secret;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, Service, seg};
use crate::repo::git::GithubRepo;
use crate::secrets::token::app_jwt;
use serde_json::{Value, json};

fn env(name: &str) -> Option<String> {
    let v = std::env::var(name).ok()?;
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn fail(why: &str) -> HoistError {
    HoistError::Usage(format!("release prepare: {why}"))
}

/// Revoke the App installation token behind `http` (`DELETE /installation/token`). A failure is
/// ignored: the token expires within the hour anyway.
pub(super) fn revoke(http: &Http) {
    let _ = http.send_json(Method::Delete, "/installation/token", None);
}

/// A client for the release PR (`None`: the ordinary one), whether its token triggers
/// workflows, and whether it is a minted installation token (to revoke when done).
pub(super) fn pr_client(
    repo: &GithubRepo,
    verbose: bool,
) -> Result<(Option<Http>, bool, bool), HoistError> {
    if let Some(t) = env("HOIST_RELEASE_TOKEN") {
        let http = Http::new(Service::Github, Secret::from(t)).verbose(verbose);
        return Ok((Some(http), true, false));
    }
    if let (Some(app), Some(pem)) = (env("HOIST_RELEASE_APP_ID"), env("HOIST_RELEASE_APP_KEY")) {
        let token = mint(repo, &app, &pem, verbose)?;
        let http = Http::new(Service::Github, token).verbose(verbose);
        return Ok((Some(http), true, true));
    }
    // Outside Actions the ordinary token is a person's own, which does trigger workflows.
    let triggers = env("GITHUB_ACTIONS").is_none_or(|v| v != "true");
    Ok((None, triggers, false))
}

fn mint(repo: &GithubRepo, app: &str, pem: &str, verbose: bool) -> Result<Secret, HoistError> {
    let now = u64::try_from(crate::timeutil::now_secs()).unwrap_or_default();
    let jwt = app_jwt(app, pem, now).map_err(|_| {
        fail("HOIST_RELEASE_APP_KEY is not an RSA private key in PEM (PKCS#1 or PKCS#8)")
    })?;
    let jwt = Http::new(Service::Github, jwt)
        .verbose(verbose)
        .same_host_only();
    let rejected = |e: HttpError| match e {
        HttpError::Unauthorized { .. } => fail(
            "the release App rejected its JWT (401); check HOIST_RELEASE_APP_ID, \
             HOIST_RELEASE_APP_KEY and the clock",
        ),
        e => e.into(),
    };
    let path = format!(
        "/repos/{}/{}/installation",
        seg(&repo.owner),
        seg(&repo.name)
    );
    let id = match jwt.get_json(&path).map_err(rejected)? {
        Fetched::Ok(v) => v["id"].as_u64(),
        _ => None,
    };
    let Some(id) = id else {
        return Err(fail(&format!("the release App is not installed on {repo}")));
    };
    let body = json!({
        "repositories": [repo.name],
        "permissions": { "contents": "write", "pull_requests": "write" }
    });
    let path = format!("/app/installations/{id}/access_tokens");
    let written = jwt
        .send_json(Method::Post, &path, Some(&body))
        .map_err(rejected)?;
    let mut grant = written.body.unwrap_or_default();
    match grant["token"].take() {
        Value::String(t) => Ok(Secret::from(t)),
        _ => Err(fail("the release App returned no token")),
    }
}
