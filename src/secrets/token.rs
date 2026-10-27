//! The token `hoist secrets exec` reads secrets repos with (spec §15.7.1 step 2):
//! `HOIST_SECRETS_TOKEN` when set, else an installation token of the secrets-read GitHub App. It is
//! minted with a 9-minute RS256 JWT from `SECRETS_READ_APP_ID` and `SECRETS_READ_APP_KEY` and
//! limited to the source repos with `contents: read`. No key or token is ever printed.

use crate::auth::Secret;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use zeroize::Zeroizing;

fn fail(why: &str) -> HoistError {
    HoistError::Usage(format!("secrets: {why}"))
}

/// The token for `repos` (`owner/name`): the given one, or one minted for them. `http` builds a
/// GitHub client around a token (the App JWT here).
pub fn secrets_token(
    env: &dyn Fn(&str) -> Option<String>,
    repos: &[&str],
    now: u64,
    http: &dyn Fn(Secret) -> Http,
) -> Result<Secret, HoistError> {
    // Both the raw and the trimmed copy are wiped on drop.
    let set = |name: &str| {
        let raw = Zeroizing::new(env(name)?);
        let value = raw.trim();
        (!value.is_empty()).then(|| Zeroizing::new(value.to_string()))
    };
    if let Some(token) = set("HOIST_SECRETS_TOKEN") {
        return Ok(Secret::from(token.as_str()));
    }
    let (Some(app), Some(pem)) = (set("SECRETS_READ_APP_ID"), set("SECRETS_READ_APP_KEY")) else {
        return Err(fail(
            "set HOIST_SECRETS_TOKEN or SECRETS_READ_APP_ID and SECRETS_READ_APP_KEY \
             (the secrets-read App, spec §15.8)",
        ));
    };
    let mut names: Vec<&str> = Vec::new();
    let mut owner: Option<&str> = None;
    for repo in repos {
        let (o, name) = match repo.split_once('/') {
            Some((o, n)) if !o.is_empty() && !n.is_empty() && !n.contains('/') => (o, n),
            _ => return Err(fail("a source repo must be owner/name")),
        };
        if owner.is_some_and(|x| !x.eq_ignore_ascii_case(o)) {
            return Err(fail(
                "an App token covers one owner; give every source the same owner",
            ));
        }
        owner.get_or_insert(o);
        names.push(name);
    }
    // Repo names are case-insensitive on GitHub: one entry per repo, in a stable order.
    names.sort_by_key(|n| n.to_ascii_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let (Some(owner), Some(first)) = (owner, names.first()) else {
        return Err(fail("no source repo to read"));
    };
    let jwt = http(app_jwt(&app, &pem, now)?).same_host_only();
    let rejected = |e: HttpError| match e {
        HttpError::Unauthorized { .. } => fail(
            "the secrets-read App rejected its JWT (401); check SECRETS_READ_APP_ID, \
             SECRETS_READ_APP_KEY and the clock",
        ),
        e => e.into(),
    };
    let path = format!("/repos/{}/{}/installation", seg(owner), seg(first));
    let id = match jwt.get_json(&path).map_err(rejected)? {
        Fetched::Ok(v) => v["id"].as_u64(),
        Fetched::Forbidden(why) => {
            return Err(fail(&format!("installation lookup refused: {why}")));
        }
        Fetched::NotFound => None,
    };
    let Some(id) = id else {
        return Err(fail(&format!(
            "the secrets-read App is not installed on {owner}/{first}"
        )));
    };
    let body = json!({ "repositories": names, "permissions": { "contents": "read" } });
    let path = format!("/app/installations/{id}/access_tokens");
    let written = jwt
        .send_json(Method::Post, &path, Some(&body))
        .map_err(|e| match e {
            HttpError::Rejected { status: 422, .. } => fail(&format!(
                "the secrets-read App is not installed on all of {owner}/{{{}}}; add them to its \
             repository access",
                names.join(",")
            )),
            e => rejected(e),
        })?;
    let mut grant = written.body.unwrap_or_default();
    let token = match grant["token"].take() {
        Value::String(t) => Secret::from(t),
        _ => return Err(fail("the App returned no token")),
    };
    if !granted_exactly(&grant, &names) {
        // Best effort: the token is never used, so a failed revoke leaves it to expire in an hour.
        let _ = http(token).send_json(Method::Delete, "/installation/token", None);
        return Err(fail(
            "the App token grants more than contents: read on the sources; refused and revoked",
        ));
    }
    Ok(token)
}

/// Whether `grant` is `contents: read` (plus GitHub's `metadata: read`) on exactly `names`.
fn granted_exactly(grant: &Value, names: &[&str]) -> bool {
    let permissions = grant["permissions"].as_object();
    let read = permissions.is_some_and(|p| {
        p.get("contents").is_some_and(|v| v == "read")
            && p.iter()
                .all(|(k, v)| matches!(k.as_str(), "contents" | "metadata") && v == "read")
    });
    // Repo names are case-insensitive on GitHub.
    // Every entry must name its repo, so an entry without one fails closed.
    let lower = |n: &str| n.to_ascii_lowercase();
    let got: Option<Vec<String>> = (grant["repositories"].as_array().into_iter().flatten())
        .map(|r| r["name"].as_str().map(lower))
        .collect();
    let Some(mut got) = got else {
        return false;
    };
    let mut want: Vec<String> = names.iter().map(|n| lower(n)).collect();
    got.sort_unstable();
    want.sort_unstable();
    read && grant["repository_selection"] == "selected" && got == want
}

/// An RS256 JWT for GitHub App `app` (an id or client id): issued a minute back for clock drift,
/// valid 9 minutes (GitHub allows 10).
pub fn app_jwt(app: &str, pem: &str, now: u64) -> Result<Secret, HoistError> {
    let key = rsa_key(pem).ok_or_else(|| {
        fail("SECRETS_READ_APP_KEY is not an RSA private key in PEM (PKCS#1 or PKCS#8)")
    })?;
    let part = |v: Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let header = part(json!({ "alg": "RS256", "typ": "JWT" }));
    let claims = part(
        json!({ "iat": now.saturating_sub(60), "exp": now.saturating_add(540),
                              "iss": app }),
    );
    let signed = format!("{header}.{claims}");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|_| fail("could not sign the App JWT"))?;
    Ok(Secret::from(format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    )))
}

/// The key in `pem`: `RSA PRIVATE KEY` (PKCS#1, what GitHub hands out) or `PRIVATE KEY` (PKCS#8).
/// Literal `\n` (a key pasted into a CI variable) counts as a line break; PEM has no `\`.
fn rsa_key(pem: &str) -> Option<RsaKeyPair> {
    let pem = Zeroizing::new(pem.replace("\\n", "\n"));
    let begin = pem.find("-----BEGIN ")?;
    let (label, rest) = pem[begin + 11..].split_once("-----")?;
    let (body, _) = rest.split_once(&format!("-----END {label}-----"))?;
    let mut b64 = Zeroizing::new(String::with_capacity(body.len()));
    body.split_whitespace().for_each(|w| b64.push_str(w));
    let der = Zeroizing::new(STANDARD.decode(b64.as_bytes()).ok()?);
    match label {
        "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der).ok(),
        "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der).ok(),
        _ => None,
    }
}
