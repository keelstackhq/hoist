//! One file of a secrets repo through the contents API (spec §15.7.1 step 3): a GET through a
//! same-host-only client, held in memory; nothing is cloned or written to disk.

use crate::error::HoistError;
use crate::http::{Fetched, Http, seg};
use crate::secrets::manifest::Source;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// A GitHub client that follows no redirect off its API host: the only way to `fetch`, so no
/// caller can read secrets-repo content through a client that would send the token elsewhere.
pub struct Reader(Http);

impl Reader {
    pub fn new(http: Http) -> Reader {
        Reader(http.same_host_only())
    }
}

/// What a token needs to read secrets repos.
pub const NEEDS: &str = "a GitHub token with Contents: read on the secrets repos";

/// The text of `path` in `source` at its ref, or why there is none: a drift for `secrets check`,
/// an error for `secrets exec`. No access (403) is an error for both.
pub fn fetch(
    Reader(http): &Reader,
    source: &Source,
    path: &str,
) -> Result<Result<String, String>, HoistError> {
    let (owner, name) = source.repo.split_once('/').expect("validated owner/name");
    let file: Vec<String> = path.split('/').map(seg).collect();
    let url = format!(
        "/repos/{}/{}/contents/{}?ref={}",
        seg(owner),
        seg(name),
        file.join("/"),
        seg(&source.git_ref)
    );
    let at = format!("{}@{}", source.repo, source.git_ref);
    let body = match http.get_json(&url)? {
        Fetched::Ok(body) => body,
        Fetched::NotFound => {
            return Ok(Err(format!(
                "not found in {at} (or the token cannot see that repo or ref)"
            )));
        }
        Fetched::Forbidden(hint) => {
            let msg = format!("cannot read {path} in {at}: {hint}; it needs {NEEDS}");
            return Err(HoistError::Usage(msg));
        }
    };
    if body["type"] != "file" {
        return Ok(Err("not a file (a directory, symlink or submodule)".into()));
    }
    if body["encoding"] != "base64" {
        return Ok(Err(
            "too large for the contents API (over 1 MB); split the bundle".into(),
        ));
    }
    let b64: String = body["content"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let text = STANDARD
        .decode(b64)
        .ok()
        .and_then(|b| String::from_utf8(b).ok());
    Ok(text.ok_or_else(|| "not a valid sops file: cannot decode the contents API body".into()))
}
