//! `hoist secrets rotate-key <recipient> [--public AGE1]` in a secrets repo (spec §15.4, §15.5):
//! a routine rotation of a consumer's key. A new key `<name>-YYYYMM` joins exactly the old one's
//! rules: sealed into the consumer's SOPS_AGE_KEY (over the old key), or, with `--public`, the key
//! another secrets repo already sealed there. The old recipient leaves and every file gets a new
//! data key. Unlike `revoke`, it lists no values to rotate: the old key was not exposed.

use super::check::{item, read_regular, walk};
use super::grant::{regrant_step, store_key};
use super::sync::{commit_hint, owner_only, owner_opens, replace, report, sync_after_write};
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Http, Service};
use crate::report::{Action, Report};
use crate::secrets::access::{AGE, Access, Kind};
use crate::secrets::access_edit::{NewCi, add_ci_recipient, remove_recipient};
use crate::secrets::agekey;
use crate::secrets::decrypt::resolve;
use crate::secrets::sopsfile;
use crate::timeutil::{add_months, civil_from_days, now_secs};
use std::collections::BTreeSet;
use std::path::Path;

pub fn secrets_rotate_key(
    dir: &Path,
    old: &str,
    public: Option<&str>,
    verbose: bool,
) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets rotate-key") {
        return Ok(refused);
    }
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets rotate-key: {msg}")));
    if let Some(p) = public.filter(|p| !AGE.is_match(p)) {
        return usage(format!("--public `{p}` is not an age public key (age1…)"));
    }
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
    let Some(gone) = access.recipients.iter().find(|r| r.name == old) else {
        return usage(format!("no recipient `{old}` in access.toml"));
    };
    let (Kind::Ci, Some(repo), Some(env)) = (gone.kind, gone.repo.as_deref(), gone.env.as_deref())
    else {
        return usage(format!(
            "only a ci recipient has a key to rotate; for `{old}`, set its `age` in access.toml \
             and run `hoist secrets sync --rotate`"
        ));
    };
    // `<name>-YYYYMM`, replacing a month the name already ends with.
    let today = now.div_euclid(86_400);
    let (y, m, d) = civil_from_days(today);
    let month = old
        .len()
        .checked_sub(7)
        .filter(|&i| old.as_bytes()[i] == b'-' && old[i + 1..].bytes().all(|b| b.is_ascii_digit()));
    let base = month.map_or(old, |i| &old[..i]);
    let name = format!("{base}-{y:04}{m:02}");
    if name == old {
        return usage(format!("`{old}` was already rotated this month"));
    }
    if access.recipients.iter().any(|r| r.name == name) {
        return usage(format!(
            "`{name}` is already a recipient; rename it in access.toml, then rotate `{old}`"
        ));
    }
    // The files are re-keyed after the new key is sealed: the owner's key must open them now.
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    // A path under the key's env that sync cannot open or enter would stay as it is after the
    // new key is sealed: refused now rather than drift later.
    let prefix = format!("{env}/");
    let odd: Vec<&str> = files
        .iter()
        .filter(|(rel, why)| why.is_some() && rel.starts_with(&prefix))
        .map(|(rel, _)| rel.as_str())
        .collect();
    if !odd.is_empty() {
        return usage(format!(
            "{} cannot be re-keyed (a symlink, an odd name or an unreadable directory; `hoist \
             secrets check`); fix that first, then rotate `{old}`",
            odd.join(", ")
        ));
    }
    // sync's per-file steps are dropped below, so no file may have another change pending.
    let pending = files.iter().find(|(rel, odd)| {
        let (Some(rule), true) = (access.rule_for(rel), odd.is_none()) else {
            return false;
        };
        let want: BTreeSet<&str> = access.public_keys(rule).into_iter().collect();
        let have = read_regular(&dir.join(rel)).and_then(|t| sopsfile::parse(&t).ok());
        have.is_some_and(|f| {
            f.recipients
                .iter()
                .map(String::as_str)
                .ne(want.iter().copied())
        })
    });
    if let Some((rel, _)) = pending {
        return usage(format!(
            "{rel} does not have its rule's recipients yet; run `hoist secrets sync` first (its \
             steps say what to rotate), then rotate `{old}`"
        ));
    }
    // Every file sync will re-key for the old key's rules. (With every file in sync, as checked
    // above, a file holds the old key exactly when its rule lists it.)
    let reads = |rel: &str| {
        access
            .rule_for(rel)
            .is_some_and(|r| r.recipients.iter().any(|n| n == old))
    };
    let closed: Vec<&str> = files
        .iter()
        .filter(|(rel, odd)| odd.is_none() && rel.ends_with(".sops.yaml") && rel != ".sops.yaml")
        .map(|(rel, _)| rel.as_str())
        .filter(|rel| reads(rel) && !owner_opens(dir, rel))
        .collect();
    if !closed.is_empty() {
        return usage(format!(
            "your age key (SOPS_AGE_KEY or its key file) cannot open {}, so they could not be \
             re-keyed after the new key is sealed; rotate as an owner who can open them",
            closed.join(", ")
        ));
    }
    let key = match public {
        Some(_) => None,
        None => Some(agekey::generate()?),
    };
    let new_key = public
        .map_or_else(
            || key.as_ref().map(|k| k.public.clone()),
            |p| Some(p.into()),
        )
        .unwrap_or_default();
    let date = |(y, m, d): (i64, u32, u32)| format!("{y:04}-{m:02}-{d:02}");
    let (added, expires) = (
        date((y, m, d)),
        date(civil_from_days(add_months(today, 12))),
    );
    let new = NewCi {
        name: &name,
        repo,
        env,
        age: &new_key,
        added: &added,
        expires: &expires,
        rotating: Some(old),
    };
    let updated = remove_recipient(&add_ci_recipient(&text, now, &new)?, now, old)?;
    let http = Http::new(Service::Github, github_token()?).verbose(verbose);
    let (secret, held) = store_key(&http, repo, env, key.as_ref(), true)?;
    // The other secrets repos must take a key this run sealed over theirs.
    let regrant = (key.is_some() && held).then(|| regrant_step(&name, repo, env, &new_key));
    let mut items = vec![secret];
    if let Err(e) = replace(&dir.join("access.toml"), updated.as_bytes()) {
        let why = format!(
            "could not write it ({}); the key is set: rotate again with --public {new_key}",
            e.kind()
        );
        items.push(item("recipient", &name, Action::Drift, why, "access.toml"));
        return Ok(report(
            "secrets rotate-key",
            items,
            regrant.into_iter().collect(),
        ));
    }
    items.push(item(
        "recipient",
        &name,
        Action::Create,
        format!("ci key of {repo} ({env}), in `{old}`'s rules"),
        "access.toml",
    ));
    let why = format!("removed: rotated to `{name}`");
    items.push(item("recipient", old, Action::Update, why, "access.toml"));
    let (synced, steps) = sync_after_write(dir, true);
    items.extend(synced);
    // A routine rotation: sync's "rotate the values it could read" steps do not apply.
    let mut steps: Vec<_> = steps
        .into_iter()
        .filter(|s| !s.id.starts_with("rotate:"))
        .collect();
    steps.extend(regrant);
    let hint = commit_hint(
        dir,
        &format!("hoist/rotate-key-{name}"),
        &format!("secrets: rotate {old} to {name}"),
        &items,
    );
    let mut done = report("secrets rotate-key", items, steps);
    done.hint = Some(hint);
    Ok(done)
}
