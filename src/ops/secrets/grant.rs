//! `hoist secrets grant <repo> --env E` in a secrets repo (spec §15.4, §15.8.2): a new age key,
//! its private half sealed straight into the consumer's environment secret `SOPS_AGE_KEY` (never
//! printed or written), its public half a `ci` recipient of every rule under `E/`; then `sync`.
//! Everything local is checked before the first request, and GitHub is written before
//! `access.toml`, so a refusal changes nothing. hoist runs no git: it prints the commands.

use super::GrantArgs;
use super::check::{item, read_regular};
use super::sync::{owner_only, replace, report, secrets_sync};
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Http, Service};
use crate::report::{Action, HumanStep, Report};
use crate::seal::put_env_secret;
use crate::secrets::access::AGE;
use crate::secrets::access_edit::{NewCi, add_ci_recipient, default_name};
use crate::secrets::agekey;
use crate::secrets::consumer::{Consumer, holds_age_key, require_branch_rules};
use crate::secrets::manifest::{NAME, REPO};
use crate::timeutil::{add_months, civil_from_days, now_secs};
use std::path::Path;

pub fn secrets_grant(dir: &Path, a: &GrantArgs, verbose: bool) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets grant") {
        return Ok(refused);
    }
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets grant: {msg}")));
    let (Some((owner, repo_name)), true) = (a.repo.split_once('/'), REPO.is_match(a.repo)) else {
        return usage(format!("`{}` is not owner/name", a.repo));
    };
    if a.provider != "github" {
        return usage(
            "`--provider circleci` comes with CircleCI contexts (HO-4); use github".into(),
        );
    }
    if !NAME.is_match(a.env) {
        return usage(format!(
            "`{}` is not an environment name ([a-z0-9-])",
            a.env
        ));
    }
    if let Some(p) = a.public.filter(|p| !AGE.is_match(p)) {
        return usage(format!("--public `{p}` is not an age public key (age1…)"));
    }
    let Some(text) = read_regular(&dir.join("access.toml")) else {
        return usage(format!(
            "{}: no access.toml here (a secrets repo)",
            dir.display()
        ));
    };
    let now = now_secs();
    let date = |days: i64| {
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}")
    };
    let today = now.div_euclid(86_400);
    let name = a
        .name
        .map_or_else(|| default_name(a.repo, a.env), str::to_string);
    // A new key only lives in memory; with --public the environment keeps the one it has.
    let key = match a.public {
        Some(_) => None,
        None => Some(agekey::generate()?),
    };
    let public = a
        .public
        .map_or_else(
            || key.as_ref().map(|k| k.public.clone()),
            |p| Some(p.into()),
        )
        .unwrap_or_default();
    let (added, expires) = (date(today), date(add_months(today, 12)));
    let new = NewCi {
        name: &name,
        repo: a.repo,
        env: a.env,
        age: &public,
        added: &added,
        expires: &expires,
        rotating: None,
    };
    let updated = add_ci_recipient(&text, now, &new)?;
    let http = Http::new(Service::Github, github_token()?).verbose(verbose);
    let consumer = Consumer {
        owner,
        name: repo_name,
        env: a.env,
    };
    require_branch_rules(&http, &consumer)?;
    let held = holds_age_key(&http, &consumer)?;
    let (repo, env) = (a.repo, a.env);
    let target = format!("{repo}:{env}/SOPS_AGE_KEY");
    let secret = match (key, held) {
        (None, false) => {
            return usage(format!(
                "environment {env} of {repo} has no SOPS_AGE_KEY: --public adds the key it \
                 already holds; grant without --public to make one"
            ));
        }
        (None, true) => item(
            "environment-secret",
            &target,
            Action::Ok,
            "holds the consumer's key (--public)",
            repo,
        ),
        (Some(_), true) if !a.replace => {
            return usage(format!(
                "environment {env} of {repo} already holds SOPS_AGE_KEY, likely granted by \
                 another secrets repo: add its key here with --public age1… (from that repo's \
                 access.toml), or overwrite it with --replace"
            ));
        }
        (Some(k), held) => {
            put_env_secret(&http, (owner, repo_name), env, "SOPS_AGE_KEY", &k.private)?;
            let action = [Action::Create, Action::Update][usize::from(held)];
            item(
                "environment-secret",
                &target,
                action,
                "a new age key, sealed",
                repo,
            )
        }
    };
    let mut items = vec![secret];
    if let Err(e) = replace(&dir.join("access.toml"), updated.as_bytes()) {
        let why = format!(
            "could not write it ({}); the key is set: grant again with --public {public}",
            e.kind()
        );
        items.push(item("recipient", &name, Action::Drift, why, "access.toml"));
        return Ok(report("secrets grant", items, Vec::new()));
    }
    items.push(item(
        "recipient",
        &name,
        Action::Create,
        format!("ci key of {repo} ({env}), in every rule under {env}/"),
        "access.toml",
    ));
    let synced = secrets_sync(dir, false)?;
    let files: Vec<String> = synced
        .items
        .iter()
        .filter(|i| i.kind == "file" && i.action == Action::Update)
        .map(|i| sh(&i.key))
        .collect();
    items.extend(synced.items);
    let mut steps = synced.human_steps;
    if a.replace && held {
        steps.push(HumanStep {
            id: format!("regrant:{name}"),
            title: format!(
                "`{name}` has a new key: in every other secrets repo whose access.toml lists \
                 {repo} ({env}), replace its key with {public} (`hoist secrets rotate-key`), or \
                 its runs there fail"
            ),
            url: String::new(),
            status: "pending".into(),
        });
    }
    let mut done = report("secrets grant", items, steps);
    let git = format!("git -C {}", sh(&dir.display().to_string()));
    let branch = format!("hoist/grant-{name}");
    let message = sh(&format!("secrets: grant {name} ({repo} {env})"));
    done.hint = Some(
        format!(
            "next: hoist ran no git; commit and open a PR yourself:\n  {git} switch -c {branch}\n  \
             {git} add access.toml .sops.yaml {}\n  {git} commit -m {message}\n  \
             {git} push -u origin {branch}\nthen open a PR from {branch}\n",
            files.join(" ")
        )
        .into(),
    );
    Ok(done)
}

/// `s` in single quotes for a POSIX shell.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
