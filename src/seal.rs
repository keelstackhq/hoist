//! GitHub Actions secrets: a value reaches GitHub only as a libsodium sealed box (spec §6).
//! No plaintext, key bytes or key material is ever printed or put in an error.

use crate::auth::Secret;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, seg};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crypto_box::PublicKey;
use crypto_box::aead::OsRng;
use serde_json::{Value, json};

const BAD_KEY: &str = "GitHub returned an invalid repository public key";

/// Seal `value` for the repository public key (standard base64, 32 bytes). Returns the
/// base64 sealed box.
pub fn seal(public_key_b64: &str, value: &Secret) -> Result<String, HoistError> {
    let bytes = STANDARD
        .decode(public_key_b64.trim())
        .map_err(|_| HoistError::Network(BAD_KEY.into()))?;
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_| HoistError::Network(BAD_KEY.into()))?;
    let key = PublicKey::from(raw);
    let sealed = key
        .seal(&mut OsRng, value.expose().as_bytes())
        .map_err(|_| HoistError::Network("could not seal the secret value".into()))?;
    Ok(STANDARD.encode(sealed))
}

struct RepoKey {
    key_id: String,
    key: String,
}

/// The public key of a secrets endpoint `base`: `/repos/o/r/actions` (the repository's) or
/// `/repos/o/r/environments/e` (an environment's), `what` naming it in errors.
fn fetch_key(http: &Http, base: &str, what: &str) -> Result<RepoKey, HoistError> {
    let path = format!("{base}/secrets/public-key");
    match http.get_json(&path)? {
        Fetched::Ok(v) => {
            let field = |name: &str| v.get(name).and_then(Value::as_str).map(str::to_string);
            match (field("key_id"), field("key")) {
                (Some(key_id), Some(key)) => Ok(RepoKey { key_id, key }),
                _ => Err(HoistError::Network(BAD_KEY.into())),
            }
        }
        Fetched::NotFound => Err(HoistError::Network(format!(
            "GitHub has no Actions public key for {what} (404); check that it exists and the \
             token can see it"
        ))),
        Fetched::Forbidden(msg) => Err(HoistError::Network(format!(
            "GitHub refused GET {path} (403): {msg}. The token needs Secrets write access"
        ))),
    }
}

/// Create or update the repository Actions secret `name` with `value`, sealed. A 422 means
/// the key rotated: the key is re-read once and the write retried; a second 422 is an error.
pub fn put_secret(http: &Http, d: &Desired, name: &str, value: &Secret) -> Result<(), HoistError> {
    let repo = format!("/repos/{}/{}", seg(&d.repo.owner), seg(&d.repo.name));
    let what = format!("{}/{}", d.repo.owner, d.repo.name);
    put_at(http, &format!("{repo}/actions"), &what, name, value)
}

/// The same for environment `env` of `owner/name`: what each consumer's `SOPS_AGE_KEY` is.
pub fn put_env_secret(
    http: &Http,
    (owner, name): (&str, &str),
    env: &str,
    secret: &str,
    value: &Secret,
) -> Result<(), HoistError> {
    let base = format!(
        "/repos/{}/{}/environments/{}",
        seg(owner),
        seg(name),
        seg(env)
    );
    put_at(
        http,
        &base,
        &format!("environment {env} of {owner}/{name}"),
        secret,
        value,
    )
}

fn put_at(
    http: &Http,
    base: &str,
    what: &str,
    name: &str,
    value: &Secret,
) -> Result<(), HoistError> {
    let path = format!("{base}/secrets/{}", seg(name));
    for attempt in 1..=2 {
        let key = fetch_key(http, base, what)?;
        let body = json!({
            "encrypted_value": seal(&key.key, value)?,
            "key_id": key.key_id,
        });
        match http.send_json(Method::Put, &path, Some(&body)) {
            Ok(_) => return Ok(()),
            Err(HttpError::Rejected { status: 422, .. }) if attempt == 1 => continue,
            Err(HttpError::Rejected {
                status: 422,
                message,
                ..
            }) => {
                return Err(HoistError::Network(format!(
                    "GitHub rejected secret {name} twice (422) after re-reading the public key \
                     of {what}: {message}"
                )));
            }
            Err(err) => return Err(err.into()),
        }
    }
    unreachable!("the loop returns on both attempts")
}
