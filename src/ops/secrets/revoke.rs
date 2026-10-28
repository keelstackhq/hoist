//! `hoist secrets revoke <recipient> [--delete-secret]` in a secrets repo (spec §15.4, §15.5): the
//! recipient leaves access.toml, every file gets a new data key (`sync --rotate`), and every value
//! it could read is listed: git keeps the old ciphertext, so those must be rotated at their
//! source. A `ci` key is shared by the secrets repos that list its consumer, so its secret stays
//! unless `--delete-secret`. hoist runs no git: it prints the commands.

use super::check::{item, read_regular, walk};
use super::sync::{commit_hint, owner_only, replace, report, sync_after_write};
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError, Method, Service, seg};
use crate::repo::git::read_repo_local;
use crate::report::{Action, HumanStep, Report};
use crate::secrets::access::{Access, Kind};
use crate::secrets::access_edit::remove_recipient;
use crate::secrets::decrypt::resolve;
use crate::secrets::manifest::Manifest;
use crate::secrets::sopsfile;
use crate::timeutil::now_secs;
use std::path::Path;

fn step(id: String, title: String) -> HumanStep {
    HumanStep {
        id,
        title,
        url: String::new(),
        status: "pending".into(),
    }
}

pub fn secrets_revoke(
    dir: &Path,
    name: &str,
    delete_secret: bool,
    verbose: bool,
) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets revoke") {
        return Ok(refused);
    }
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets revoke: {msg}")));
    if resolve("sops".as_ref(), std::env::var_os("PATH").as_deref()).is_none() {
        return usage("sops is not on PATH; install sops 3.13.3 (spec §15.9.1)".into());
    }
    let Some(text) = read_regular(&dir.join("access.toml")) else {
        return usage(format!(
            "{}: no access.toml here (a secrets repo)",
            dir.display()
        ));
    };
    let now = now_secs();
    let access = Access::parse(&text, "access.toml", now)?;
    let gone = access.recipients.iter().find(|r| r.name == name);
    let consumer = gone
        .filter(|r| r.kind == Kind::Ci)
        .and_then(|r| r.repo.clone().zip(r.env.clone()));
    if delete_secret && gone.is_some() && consumer.is_none() {
        return usage(format!(
            "--delete-secret: only a ci recipient has a secret, not `{name}`"
        ));
    }
    // Another recipient of the same consumer (a rotation in progress) still uses its secret.
    let sharer = consumer.as_ref().and_then(|(repo, env)| {
        access.recipients.iter().find(|r| {
            r.name != name
                && r.kind == Kind::Ci
                && r.repo
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(repo))
                && r.env.as_deref() == Some(env.as_str())
        })
    });
    if let (true, Some(other)) = (delete_secret, sharer) {
        return usage(format!(
            "--delete-secret: `{}` still uses that secret; revoke `{name}` without it",
            other.name
        ));
    }
    let updated = remove_recipient(&text, now, name)?;
    let http = match delete_secret {
        true => Some(Http::new(Service::Github, github_token()?).verbose(verbose)),
        false => None,
    };
    // --delete-secret: the consumer's secrets.toml names the secrets repos it reads; one other
    // than this may still list it. When hoist cannot tell, it stops before changing anything.
    if let (true, Some((repo, _)), None) = (delete_secret, &consumer, sharer) {
        let this = read_repo_local(dir).map(|(r, _)| format!("{}/{}", r.owner, r.name));
        let reader = Http::new(Service::Github, github_token()?).verbose(verbose);
        let unknown = match (this, consumer_sources(reader.same_host_only(), repo)) {
            (Ok(this), Ok(sources)) => {
                if let Some(other) = sources.iter().find(|s| !s.eq_ignore_ascii_case(&this)) {
                    return usage(format!(
                        "--delete-secret: {repo}'s secrets.toml also reads secrets repo {other}, \
                         which may list its key; revoke `{name}` there first, or leave out \
                         --delete-secret"
                    ));
                }
                None
            }
            (Err(_), _) => Some("this checkout has no GitHub `origin` to compare with".into()),
            (_, Err(why)) => Some(why),
        };
        if let Some(why) = unknown {
            let title = format!(
                "nothing changed: hoist cannot tell which secrets repos {repo} reads ({why}); \
                 check its secrets.toml, then revoke `{name}` without --delete-secret and delete \
                 the secret in Settings → Environments once none lists it"
            );
            let steps = vec![step(format!("check-sources:{name}"), title)];
            return Ok(report("secrets revoke", Vec::new(), steps));
        }
    }
    // What it could read: every sops file its rule listed it in or its key opens, by key name.
    let age = gone.map(|r| r.age.as_str()).unwrap_or_default();
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    let could: Vec<String> = files
        .iter()
        .filter(|(rel, _)| rel.ends_with(".sops.yaml") && rel != ".sops.yaml")
        .filter_map(|(rel, odd)| {
            let listed = access
                .rule_for(rel)
                .is_some_and(|r| r.recipients.iter().any(|n| n == name));
            let text = odd
                .is_none()
                .then(|| read_regular(&dir.join(rel)))
                .flatten();
            let names_key = !age.is_empty() && text.as_deref().is_some_and(|t| t.contains(age));
            match text.as_deref().map(sopsfile::parse) {
                Some(Ok(f)) if listed || f.recipients.contains(age) => {
                    Some(format!("{rel}: {}", f.keys.join(", ")))
                }
                Some(Ok(_)) => None,
                // Unreadable or not a sops file, yet its rule or its text says the key opened it.
                _ if listed || names_key => Some(format!(
                    "{rel}: (its keys could not be read; assume every value)"
                )),
                _ => None,
            }
        })
        .collect();
    replace(&dir.join("access.toml"), updated.as_bytes()).map_err(|e| {
        HoistError::Usage(format!(
            "secrets revoke: could not write access.toml ({})",
            e.kind()
        ))
    })?;
    let mut items = vec![item(
        "recipient",
        name,
        Action::Update,
        "removed from access.toml and every rule",
        "access.toml",
    )];
    let (synced, mut steps) = sync_after_write(dir, true);
    items.extend(synced);
    if !could.is_empty() {
        steps.push(step(
            format!("rotate-values:{name}"),
            format!(
                "rotate at their source every value `{name}` could read (git keeps the old \
                 ciphertext): {}",
                could.join("; ")
            ),
        ));
    }
    if let Some((repo, env)) = consumer {
        let what = format!("SOPS_AGE_KEY of environment {env} of {repo}");
        match (http, sharer) {
            (_, Some(other)) => items.push(item(
                "environment-secret",
                &format!("{repo}:{env}/SOPS_AGE_KEY"),
                Action::Ok,
                format!("stays: `{}` uses it", other.name),
                &repo,
            )),
            (None, None) => steps.push(step(
                format!("delete-secret:{name}"),
                format!(
                    "{what} stays: other secrets repos may list {repo} ({env}) with the same \
                     key; once none does, delete it in the repository's Settings → \
                     Environments → {env}"
                ),
            )),
            (Some(http), None) => {
                let deleted = delete(&http, &repo, &env, &what);
                let gone = deleted.action == Action::Update;
                items.push(deleted);
                // Only a deleted key leaves the other secrets repos without one.
                steps.extend(gone.then(|| {
                    step(
                        format!("regrant:{name}"),
                        format!(
                            "hoist does not know your other secrets repos: if one lists {repo} \
                         ({env}), its runs now fail; grant {repo} --env {env} there again"
                        ),
                    )
                }));
            }
        }
    }
    let hint = commit_hint(
        dir,
        &format!("hoist/revoke-{name}"),
        &format!("secrets: revoke {name}"),
        &items,
    );
    let mut done = report("secrets revoke", items, steps);
    done.hint = Some(hint);
    Ok(done)
}

/// Deletes the environment secret: an item, `drift` when GitHub refuses (the local revoke
/// stands either way).
fn delete(http: &Http, repo: &str, env: &str, what: &str) -> crate::report::Item {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let path = format!(
        "/repos/{}/{}/environments/{}/secrets/SOPS_AGE_KEY",
        seg(owner),
        seg(name),
        seg(env)
    );
    let key = format!("{repo}:{env}/SOPS_AGE_KEY");
    let done = |action, why: String| item("environment-secret", &key, action, why, repo);
    match http.send_json(Method::Delete, &path, None) {
        Ok(_) => done(Action::Update, "deleted".into()),
        Err(HttpError::Rejected { status: 404, .. }) => done(Action::Ok, "already gone".into()),
        Err(e) => done(
            Action::Drift,
            format!("could not delete {what} ({e}); delete it in Settings → Environments → {env}"),
        ),
    }
}

/// The secrets repos `repo`'s secrets.toml reads (its default branch), or why hoist cannot tell.
fn consumer_sources(http: Http, repo: &str) -> Result<Vec<String>, String> {
    use base64::Engine;
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let path = format!("/repos/{}/{}/contents/secrets.toml", seg(owner), seg(name));
    let body = match http.get_json(&path) {
        Ok(Fetched::Ok(body)) => body,
        Ok(Fetched::NotFound) => return Err("its secrets.toml was not found".into()),
        Ok(Fetched::Forbidden(why)) => return Err(format!("cannot read its secrets.toml: {why}")),
        Err(e) => return Err(format!("cannot read its secrets.toml: {e}")),
    };
    let b64: String = body["content"]
        .as_str()
        .filter(|_| body["type"] == "file" && body["encoding"] == "base64")
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let text = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or("its secrets.toml is not a readable file")?;
    let manifest = Manifest::parse(&text, "secrets.toml")
        .map_err(|_| "its secrets.toml does not parse".to_string())?;
    Ok(manifest.sources.into_values().map(|s| s.repo).collect())
}
