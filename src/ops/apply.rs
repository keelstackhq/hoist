//! `hoist apply` (spec §7.2): plan, preflight, confirm, write in dependency order, then re-read
//! and verify with the `check` logic. Errors before the first write exit 3; a failed write
//! stops the run and reports what was done (exit 1).

use crate::change::{Change, ChangeOpts, plan_changes};
use crate::cli::Area;
use crate::cloudflare_write::execute_cloudflare;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::LiveGithub;
use crate::github_write::execute_github;
use crate::http::{Fetched, Http};
use crate::ops::check::{exit_code, verify_state};
use crate::ops::plan::run_with_state;
use crate::report::{Action, HumanStep, Item, Report};
use serde_json::Value;
use std::cmp::Ordering;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

pub fn apply(
    dir: &Path,
    areas: &[Area],
    yes: bool,
    prune: bool,
    verbose: bool,
) -> Result<Report, HoistError> {
    let state = run_with_state(dir, areas, verbose, "apply")?;
    let opts = ChangeOpts {
        prune,
        areas: areas.to_vec(),
    };
    let changes = plan_changes(
        &state.desired,
        state.github.as_ref(),
        state.cloudflare.as_ref(),
        &opts,
    );
    if changes.is_empty() {
        // Nothing to write: the plan's own state is verified, no second read.
        return verify_state(dir, areas, state);
    }

    let d = &state.desired;
    let gh = state.github_http.as_ref();
    let cf = state.cloudflare_http.as_ref();
    if changes.iter().any(writes_github) {
        let live = state.github.as_ref().ok_or_else(|| not_read("GitHub"))?;
        preflight_github_admin(live, d)?;
    }
    if changes.iter().any(writes_cloudflare) {
        preflight_cloudflare(cf.ok_or_else(|| not_read("Cloudflare"))?, d)?;
    }
    confirm(&changes, &state.report.items, yes)?;

    let mut steps = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        match execute(gh, cf, d, change) {
            Ok(Some(step)) => steps.push(step),
            Ok(None) => {}
            Err(err) => return Ok(partial(&changes, &state.report, index, &err, steps)),
        }
    }

    let after = run_with_state(dir, areas, verbose, "apply")?;
    let mut report = verify_state(dir, areas, after)?;
    add_steps(&mut report, steps);
    Ok(report)
}

fn not_read(area: &str) -> HoistError {
    HoistError::Usage(format!(
        "{area} was not read, so its changes cannot be applied; drop `--area` or add `--area {}`",
        area.to_lowercase()
    ))
}

/// Changes that write through the GitHub API (the deploy token ends as a GitHub secret).
fn writes_github(c: &Change) -> bool {
    !matches!(
        c,
        Change::PagesProject { .. } | Change::Domain { .. } | Change::RevokeToken { .. }
    )
}

/// Changes that write through the Cloudflare API.
fn writes_cloudflare(c: &Change) -> bool {
    matches!(
        c,
        Change::PagesProject { .. }
            | Change::Domain { .. }
            | Change::DeployToken { .. }
            | Change::RevokeToken { .. }
    )
}

/// GitHub writes need admin on the repo. Fine-grained tokens cannot list their own write
/// permissions, so `permissions.admin` from `GET /repos/{o}/{r}` is the check.
pub(crate) fn preflight_github_admin(live: &LiveGithub, d: &Desired) -> Result<(), HoistError> {
    let (owner, repo) = (&d.repo.owner, &d.repo.name);
    let detail = match live.settings.as_got() {
        Ok(s) if s.admin == Some(true) => return Ok(()),
        Ok(_) => format!("GET /repos/{owner}/{repo} does not report `permissions.admin: true`"),
        Err(why) => format!("GET /repos/{owner}/{repo} was not readable: {why}"),
    };
    Err(HoistError::Usage(format!(
        "the token needs admin on {owner}/{repo} (Administration: write); {detail}. Use a token \
         with Administration, Secrets, Environments and Actions: write on the repo"
    )))
}

/// Cloudflare writes need an account id and a token that `GET /user/tokens/verify` reports
/// as active.
pub(crate) fn preflight_cloudflare(cf_http: &Http, d: &Desired) -> Result<(), HoistError> {
    if d.cloudflare.account_id.is_none() {
        return Err(HoistError::Usage(
            "no Cloudflare account id; set `[cloudflare] account_id` in hoist.toml or export \
             CLOUDFLARE_ACCOUNT_ID"
                .into(),
        ));
    }
    let path = "/user/tokens/verify";
    let status = match cf_http.get_json(path)? {
        Fetched::Ok(v) => match v.pointer("/result/status").and_then(Value::as_str) {
            Some("active") => return Ok(()),
            Some(s) => format!("status `{s}`"),
            None => "no status in the response".to_string(),
        },
        Fetched::NotFound => "not found (404)".to_string(),
        Fetched::Forbidden(hint) => hint,
    };
    Err(HoistError::Usage(format!(
        "the Cloudflare token is not active (GET {path}: {status}); export \
         CLOUDFLARE_API_TOKEN with an active token"
    )))
}

/// `--yes` continues; a terminal is asked (the list goes to stderr so `--json` stays one
/// object); anything else refuses before the first write.
fn confirm(changes: &[Change], items: &[Item], yes: bool) -> Result<(), HoistError> {
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(HoistError::Usage(
            "stdin is not a terminal; use --yes in non-interactive runs".into(),
        ));
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "hoist apply will make {} change(s):", changes.len());
    for change in changes {
        let _ = writeln!(err, "  {}", describe(change, items));
    }
    let _ = write!(err, "Apply these changes? [y/N] ");
    let _ = err.flush();
    let mut answer = String::new();
    stdin
        .lock()
        .read_line(&mut answer)
        .map_err(|e| HoistError::Usage(format!("cannot read the answer: {e}; use --yes")))?;
    match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(HoistError::Usage(
            "apply cancelled; nothing was written".into(),
        )),
    }
}

/// One line per change, from the matching plan item (reasons never carry secret values).
fn describe(change: &Change, items: &[Item]) -> String {
    match change {
        Change::RevokeToken { id, name } => format!("revoke cloudflare/api_token {name} (id {id})"),
        Change::RepoSettings { patch } => {
            let keys: Vec<&str> = patch.keys().map(String::as_str).collect();
            format!("update github/repo_setting {}", keys.join(", "))
        }
        _ => {
            let (area, kind, key) = change.key();
            match find(items, area, kind, &key) {
                Some(i) => format!("{} {area}/{kind} {key}: {}", i.action.as_str(), i.reason),
                None => format!("write {area}/{kind} {key}"),
            }
        }
    }
}

fn find<'a>(items: &'a [Item], area: &str, kind: &str, key: &str) -> Option<&'a Item> {
    items
        .iter()
        .find(|i| i.area == area && i.kind == kind && i.key == key)
}

fn execute(
    gh: Option<&Http>,
    cf: Option<&Http>,
    d: &Desired,
    change: &Change,
) -> Result<Option<HumanStep>, HoistError> {
    if writes_cloudflare(change) {
        let cf = cf.ok_or_else(|| not_read("Cloudflare"))?;
        // Only the deploy token uses the GitHub client, and it is planned only when GitHub
        // was read; the other Cloudflare writers ignore it.
        let gh = match change {
            Change::DeployToken { .. } => gh.ok_or_else(|| not_read("GitHub"))?,
            _ => gh.unwrap_or(cf),
        };
        execute_cloudflare(cf, gh, d, change)
    } else {
        execute_github(gh.ok_or_else(|| not_read("GitHub"))?, d, change).map(|()| None)
    }
}

/// The report items a change stands for: one per patched field for repo settings.
fn keys(change: &Change) -> Vec<(&'static str, &'static str, String)> {
    match change {
        Change::RepoSettings { patch } => patch
            .keys()
            .map(|k| ("github", "repo_setting", k.clone()))
            .collect(),
        _ => vec![change.key()],
    }
}

/// The report of a run stopped by the write at `failed`: applied, failed, not attempted.
fn partial(
    changes: &[Change],
    plan: &Report,
    failed: usize,
    err: &HoistError,
    steps: Vec<HumanStep>,
) -> Report {
    let mut items = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let (done, reason) = match index.cmp(&failed) {
            Ordering::Less => (true, "applied".to_string()),
            Ordering::Equal => (false, format!("failed: {err}")),
            Ordering::Greater => (false, "not attempted".to_string()),
        };
        for (area, kind, key) in keys(change) {
            let planned = find(&plan.items, area, kind, &key);
            let action = match (done, planned) {
                (true, _) => Action::Ok,
                (false, Some(p)) => p.action,
                (false, None) => Action::Update,
            };
            items.push(Item {
                area: area.to_string(),
                kind: kind.to_string(),
                key,
                action,
                reason: reason.clone(),
                source: planned.map_or_else(|| "default".to_string(), |p| p.source.clone()),
                verify: None,
            });
        }
    }
    let mut report = Report {
        command: "apply".to_string(),
        items,
        human_steps: plan.human_steps.clone(),
        exit_code: 1,
    };
    for step in steps {
        if !report.human_steps.iter().any(|s| s.id == step.id) {
            report.human_steps.push(step);
        }
    }
    report
}

/// Add the human steps the writers returned (a domain whose zone is not on the account): the
/// matching item becomes `pending`, so the exit code is 2 when nothing else failed.
fn add_steps(report: &mut Report, steps: Vec<HumanStep>) {
    for step in steps {
        if let Some(host) = step.id.strip_prefix("domain:") {
            for item in report.items.iter_mut().filter(|i| {
                i.kind == "custom_domain" && i.key == host && i.action == Action::Create
            }) {
                item.action = Action::Pending;
                item.reason = step.title.clone();
            }
        }
        if !report.human_steps.iter().any(|s| s.id == step.id) {
            report.human_steps.push(step);
        }
    }
    report.exit_code = match exit_code(&report.items) {
        0 if !report.human_steps.is_empty() => 2,
        code => code,
    };
}
