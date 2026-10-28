//! `hoist secrets edit <file>` in a secrets repo (spec §15.4): `sops edit` with the owner's key
//! (SOPS_AGE_KEY or its key file) on a `*.sops.yaml` under a rule, in the owner's $EDITOR. A new
//! file is made for its rule's recipients. Refused while .sops.yaml or the file's recipients
//! differ from access.toml: `hoist secrets sync` first. hoist runs no git: it prints the commands.

use super::check::{item, read_regular};
use super::sync::{commit_hint, owner_only, report};
use crate::error::HoistError;
use crate::report::{Action, Report};
use crate::secrets::access::{Access, sops_yaml};
use crate::secrets::decrypt::resolve;
use crate::secrets::sopsfile;
use crate::timeutil::now_secs;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub fn secrets_edit(dir: &Path, rel: &str, json: bool) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets edit") {
        return Ok(refused);
    }
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets edit: {msg}")));
    if json {
        return usage("the editor owns the terminal, so there is no --json".into());
    }
    let Some(sops) = resolve("sops".as_ref(), std::env::var_os("PATH").as_deref()) else {
        return usage("sops is not on PATH; install sops 3.13.3 (spec §15.9.1)".into());
    };
    let Some(text) = read_regular(&dir.join("access.toml")) else {
        return usage(format!(
            "{}: no access.toml here (a secrets repo)",
            dir.display()
        ));
    };
    let access = Access::parse(&text, "access.toml", now_secs())?;
    // walk's charset: a name sync and check can see, and safe to print. Shown with `?` otherwise.
    let safe = |c: char| c.is_ascii_alphanumeric() || "._-/".contains(c);
    if !rel.chars().all(safe) {
        let shown: String = rel.chars().map(|c| if safe(c) { c } else { '?' }).collect();
        return usage(format!("`{shown}` has characters outside [A-Za-z0-9._-]"));
    }
    if !rel.ends_with(".sops.yaml") || rel.rsplit('/').next() == Some(".sops.yaml") {
        return usage(format!(
            "`{rel}` is not a secrets file (`<env>/….sops.yaml`)"
        ));
    }
    let Some(rule) = access.rule_for(rel) else {
        return usage(format!("no rule in access.toml covers `{rel}`"));
    };
    let sync_first = |why: &str| usage(format!("{why}; run `hoist secrets sync` first"));
    if read_regular(&dir.join(".sops.yaml")).as_deref() != Some(sops_yaml(&access).as_str()) {
        return sync_first(".sops.yaml does not match access.toml");
    }
    let path = dir.join(rel);
    // sops would follow a symlinked directory out of the repo, and a missing one would fail
    // only after the owner typed into the editor.
    let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
    let (root, under) = (dir.canonicalize(), dir.join(parent).canonicalize());
    match (root, under) {
        (Ok(root), Ok(under)) if !under.starts_with(&root) => {
            return usage(format!(
                "`{parent}` leads outside the repo (a symlink); not edited"
            ));
        }
        (Ok(_), Ok(_)) => {}
        _ => return usage(format!("{parent} does not exist; create it first")),
    }
    let want: BTreeSet<&str> = access.public_keys(rule).into_iter().collect();
    let exists = std::fs::symlink_metadata(&path).is_ok();
    if exists {
        let Some(f) = read_regular(&path).and_then(|t| sopsfile::parse(&t).ok()) else {
            return usage(format!("`{rel}` is not a regular sops file"));
        };
        if f.recipients
            .iter()
            .map(String::as_str)
            .ne(want.iter().copied())
        {
            return sync_first(&format!(
                "`{rel}` does not have rule `{}`'s recipients",
                rule.path
            ));
        }
    }
    // The owner's terminal and $EDITOR; sops reads the owner's key itself.
    // Without the variables that pick other recipients, a new file gets its creation rule's.
    let status = Command::new(sops)
        .env_remove("SOPS_AGE_RECIPIENTS")
        .env_remove("SOPS_PGP_FP")
        .env_remove("SOPS_KMS_ARN")
        .env_remove("SOPS_GCP_KMS_IDS")
        .env_remove("SOPS_AZURE_KEYVAULT_URLS")
        .env_remove("SOPS_VAULT_URIS")
        .args(["--config", ".sops.yaml", "edit", "--"])
        .arg(rel)
        .current_dir(dir)
        .status();
    let file = |action, why: String| item("file", rel, action, why, rel);
    // What sops wrote must still be for the rule's recipients, whatever the shell set.
    let recipients_ok = || {
        let f = read_regular(&path).and_then(|t| sopsfile::parse(&t).ok());
        f.is_some_and(|f| {
            f.recipients
                .iter()
                .map(String::as_str)
                .eq(want.iter().copied())
        })
    };
    let wrong = || {
        file(
            Action::Drift,
            "written, but not for its rule's recipients; run `hoist secrets sync`".into(),
        )
    };
    let done = match status.map(|s| s.code()) {
        Ok(Some(0)) if !recipients_ok() => wrong(),
        Ok(Some(0)) if exists => file(Action::Update, "edited".into()),
        Ok(Some(0)) => file(Action::Create, format!("new, for rule `{}`", rule.path)),
        Ok(Some(200)) => file(Action::Ok, "unchanged".into()),
        Ok(code) => {
            let code = code.map_or("a signal".into(), |c| format!("exit {c}"));
            file(Action::Drift, format!("sops edit failed ({code})"))
        }
        Err(e) => file(
            Action::Drift,
            format!("could not start sops ({})", e.kind()),
        ),
    };
    let changed = matches!(done.action, Action::Update | Action::Create);
    let plain = |c: char| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        }
    };
    let branch = format!("hoist/edit-{}", rel.chars().map(plain).collect::<String>());
    let hint = changed.then(|| {
        commit_hint(
            dir,
            &branch,
            &format!("secrets: edit {rel}"),
            std::slice::from_ref(&done),
        )
    });
    let mut out = report("secrets edit", vec![done], Vec::new());
    out.hint = hint;
    Ok(out)
}
