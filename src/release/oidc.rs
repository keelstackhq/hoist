//! Trusted publishing credentials (spec §9.3): the GitHub Actions OIDC token and the
//! short-lived crates.io token exchanged for it. Values live only in [`Secret`]; errors and
//! logs never carry them.

use crate::auth::Secret;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, Service, seg};
use serde_json::{Value, json};

const URL_VAR: &str = "ACTIONS_ID_TOKEN_REQUEST_URL";
const TOKEN_VAR: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";
const TOKENS: &str = "/api/v1/trusted_publishing/tokens";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Exit 3 unless the job can request an OIDC token (checked before the first write).
pub fn require_oidc_env() -> Result<(), HoistError> {
    match (env(URL_VAR), env(TOKEN_VAR)) {
        (Some(_), Some(_)) => Ok(()),
        _ => Err(HoistError::Usage(format!(
            "{URL_VAR} and {TOKEN_VAR} are not set: the job needs `permissions: id-token: \
             write` for trusted publishing to npm and crates.io"
        ))),
    }
}

/// An Actions OIDC token for audience `aud`, from the job's request URL and token.
pub fn actions_id_token(aud: &str) -> Result<Secret, HoistError> {
    require_oidc_env()?;
    let url = env(URL_VAR).unwrap_or_default();
    let token = Secret::from(env(TOKEN_VAR).unwrap_or_default());
    id_token_from(&url, &token, aud)
}

/// `GET <request_url>&audience=<aud>` with `Authorization: bearer <request_token>`, reading
/// `.value`.
pub fn id_token_from(
    request_url: &str,
    request_token: &Secret,
    aud: &str,
) -> Result<Secret, HoistError> {
    let bad_url = || {
        HoistError::Usage(format!(
            "{URL_VAR} is not an http(s) URL; run publish in GitHub Actions with \
             `permissions: id-token: write`"
        ))
    };
    let (scheme, rest) = request_url.split_once("://").ok_or_else(bad_url)?;
    if scheme != "https" && scheme != "http" {
        return Err(bad_url());
    }
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let sep = if path.contains('?') { '&' } else { '?' };
    let path = format!("{path}{sep}audience={}", seg(aud));
    let http = Http::with_base(
        Service::ActionsOidc,
        request_token.clone(),
        &format!("{scheme}://{host}"),
    );
    match http.get_json(&path)? {
        Fetched::Ok(body) => body
            .get("value")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(Secret::from)
            .ok_or_else(|| {
                HoistError::Network(
                    "the GitHub Actions OIDC response has no `value`; retry the job".to_string(),
                )
            }),
        Fetched::NotFound | Fetched::Forbidden(_) => Err(HoistError::Network(format!(
            "GitHub Actions refused the OIDC token request for audience `{aud}`; the job needs \
             `permissions: id-token: write`"
        ))),
    }
}

/// Revokes the crates.io token: explicitly with [`CratesTokenGuard::revoke`], else on drop.
pub struct CratesTokenGuard {
    /// A crates.io client that authenticates with the token; `None` once revoked.
    http: Option<Http>,
}

impl CratesTokenGuard {
    fn send(http: &Http) -> Result<(), HoistError> {
        http.send_json(Method::Delete, TOKENS, None)?;
        Ok(())
    }

    /// `DELETE /api/v1/trusted_publishing/tokens` with the token.
    pub fn revoke(mut self) -> Result<(), HoistError> {
        match self.http.take() {
            Some(http) => Self::send(&http),
            None => Ok(()),
        }
    }
}

impl Drop for CratesTokenGuard {
    fn drop(&mut self) {
        if let Some(http) = self.http.take()
            && let Err(e) = Self::send(&http)
        {
            eprintln!("could not revoke the crates.io publish token: {e}");
        }
    }
}

/// Exchange an Actions OIDC token (audience `crates.io`) for a short-lived crates.io token.
pub fn crates_token(http: &Http) -> Result<(Secret, CratesTokenGuard), HoistError> {
    let jwt = actions_id_token("crates.io")?;
    crates_token_with_jwt(http, &jwt)
}

/// `POST /api/v1/trusted_publishing/tokens {"jwt": …}`, reading `.token`.
pub fn crates_token_with_jwt(
    http: &Http,
    jwt: &Secret,
) -> Result<(Secret, CratesTokenGuard), HoistError> {
    let body = json!({ "jwt": jwt.expose() });
    let written = http.send_json(Method::Post, TOKENS, Some(&body))?;
    let token = written
        .body
        .as_ref()
        .and_then(|b| b.get("token"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(Secret::from)
        .ok_or_else(|| {
            HoistError::Network(
                "crates.io answered the trusted publishing exchange without a token; retry the \
                 job"
                .to_string(),
            )
        })?;
    let guard = CratesTokenGuard {
        http: Some(Http::with_base_like(http, token.clone())),
    };
    Ok((token, guard))
}
