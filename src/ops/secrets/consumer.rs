//! `hoist secrets check [--remote]` in a consumer repo (spec §15.3, §15.4): `secrets.toml` is
//! valid; with `--remote`, each referenced file is read once through the contents API (GET) and
//! every referenced key must be in it. Values stay encrypted; only key names are compared.

use super::check::{item, read_regular};
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Service, seg};
use crate::ops::check::exit_code;
use crate::report::{Action, Report};
use crate::secrets::manifest::{Manifest, Source};
use crate::secrets::sopsfile;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A file's top-level keys, or the drift reason for every reference to it.
type Looked = Result<BTreeSet<String>, String>;

/// What `--remote` needs, in every error about access.
const NEEDS: &str = "a GitHub token with Contents: read on the secrets repos";

pub(super) fn check(dir: &Path, remote: bool, verbose: bool) -> Result<Report, HoistError> {
    let Some(text) = read_regular(&dir.join("secrets.toml")) else {
        let msg = format!(
            "{}: secrets.toml is not a readable regular file",
            dir.display()
        );
        return Err(HoistError::Usage(msg));
    };
    let manifest = Manifest::parse(&text, "secrets.toml")?;
    let refs: Vec<_> = manifest
        .env
        .iter()
        .flat_map(|(env, spec)| spec.vars.iter().chain(&spec.files).map(move |r| (env, r)))
        .map(|(env, (var, r))| (format!("{env}.{var}"), r))
        .collect();
    let mut files: BTreeMap<(&str, &str), Looked> = BTreeMap::new();
    if remote {
        let token = github_token().map_err(|_| {
            let how = "set GITHUB_TOKEN or GH_TOKEN, or run `gh auth login`";
            HoistError::Usage(format!("`--remote` needs {NEEDS}: {how}"))
        })?;
        let http = Http::new(Service::Github, token)
            .verbose(verbose)
            .same_host_only();
        for (_, r) in &refs {
            if !files.contains_key(&(r.source.as_str(), r.path.as_str())) {
                let looked = look(&http, &manifest.sources[&r.source], &r.path)?;
                files.insert((&r.source, &r.path), looked);
            }
        }
    }
    let items: Vec<_> = refs
        .iter()
        .map(|(key, r)| {
            let what = format!("{}:{}#{}", r.source, r.path, r.key);
            let source = &manifest.sources[&r.source];
            let (action, reason) = match files.get(&(r.source.as_str(), r.path.as_str())) {
                None => (Action::Ok, what),
                Some(Ok(keys)) if keys.contains(&r.key) => (Action::Ok, format!("{what} exists")),
                Some(Ok(_)) => {
                    let at = format!("{}@{}", source.repo, source.git_ref);
                    (
                        Action::Drift,
                        format!("key `{}` is not in {} at {at}", r.key, r.path),
                    )
                }
                Some(Err(why)) => (Action::Drift, why.clone()),
            };
            item("secret", key, action, reason, "secrets.toml")
        })
        .collect();
    Ok(Report {
        command: "secrets check".into(),
        exit_code: exit_code(&items),
        items,
        human_steps: Vec::new(),
        text: None,
        hint: None,
    })
}

/// GET one file of a secrets repo at its source's ref and read its keys without decrypting.
fn look(http: &Http, source: &Source, path: &str) -> Result<Looked, HoistError> {
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
            let msg = format!("cannot read {path} in {at}: {hint}; `--remote` needs {NEEDS}");
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
    let invalid = |why: String| Err(format!("not a valid sops file: {why}"));
    Ok(match text.as_deref().map(sopsfile::parse) {
        None => invalid("cannot decode the contents API body".into()),
        Some(Err(why)) => invalid(why),
        Some(Ok(f)) if !f.plaintext.is_empty() => {
            invalid(format!("plaintext: {}", f.plaintext.join(", ")))
        }
        Some(Ok(f)) => Ok(f.keys.into_iter().collect()),
    })
}
