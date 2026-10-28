//! `hoist secrets revoke <recipient> [--delete-secret]` in a secrets repo (spec §15.4, §15.5): the
//! recipient leaves access.toml, every file gets a new data key (`sync --rotate`), and every value
//! it could read is listed: git keeps the old ciphertext, so those must be rotated at their
//! source. A `ci` key is shared by the secrets repos that list its consumer, so its secret stays
//! unless `--delete-secret`. hoist runs no git: it prints the commands.

use super::check::{item, read_regular, walk};
use super::sync::{commit_hint, owner_only, replace, report, sync_after_write};
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Http, HttpError, Method, Service, seg};
use crate::report::{Action, HumanStep, Report};
use crate::secrets::access::{Access, Kind};
use crate::secrets::access_edit::remove_recipient;
use crate::secrets::decrypt::resolve;
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
    // What it could read: every sops file its rule listed it in or its key opens, by key name.
    let age = gone.map(|r| r.age.as_str()).unwrap_or_default();
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    let could: Vec<String> = files
        .iter()
        .filter(|(rel, odd)| odd.is_none() && rel.ends_with(".sops.yaml") && rel != ".sops.yaml")
        .filter_map(|(rel, _)| {
            let listed = access
                .rule_for(rel)
                .is_some_and(|r| r.recipients.iter().any(|n| n == name));
            match read_regular(&dir.join(rel)).map(|t| sopsfile::parse(&t)) {
                Some(Ok(f)) if listed || f.recipients.contains(age) => {
                    Some(format!("{rel}: {}", f.keys.join(", ")))
                }
                Some(Ok(_)) => None,
                _ if listed => Some(format!(
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
                items.push(delete(&http, &repo, &env, &what));
                steps.push(step(
                    format!("regrant:{name}"),
                    format!(
                        "hoist does not know your other secrets repos: if one lists {repo} \
                         ({env}), its runs now fail; grant {repo} --env {env} there again"
                    ),
                ));
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
