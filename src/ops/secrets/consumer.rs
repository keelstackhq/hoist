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

/// A file's top-level keys, or the action and reason for every reference to it.
type Looked = Result<BTreeSet<String>, (Action, String)>;

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
        let http = Http::new(Service::Github, github_token()?).verbose(verbose);
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
            let (action, reason) = match files.get(&(r.source.as_str(), r.path.as_str())) {
                None => (Action::Ok, what),
                Some(Ok(keys)) if keys.contains(&r.key) => (Action::Ok, format!("{what} exists")),
                Some(Ok(_)) => (
                    Action::Drift,
                    format!("key `{}` is not in {}", r.key, r.path),
                ),
                Some(Err((action, why))) => (*action, why.clone()),
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
    let invalid = |why: String| Err((Action::Drift, format!("not a valid sops file: {why}")));
    Ok(match http.get_json(&url)? {
        Fetched::NotFound => Err((Action::Drift, format!("not found in {at}"))),
        Fetched::Forbidden(hint) => Err((
            Action::Unknown,
            format!("cannot read {path} in {at}: {hint}"),
        )),
        Fetched::Ok(body) => {
            let b64: String = body["content"]
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            let text = STANDARD
                .decode(b64)
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            match text.as_deref().map(sopsfile::parse) {
                None => invalid("cannot decode the contents API body".into()),
                Some(Err(why)) => invalid(why),
                Some(Ok(f)) if !f.plaintext.is_empty() => {
                    invalid(format!("plaintext: {}", f.plaintext.join(", ")))
                }
                Some(Ok(f)) => Ok(f.keys.into_iter().collect()),
            }
        }
    })
}
