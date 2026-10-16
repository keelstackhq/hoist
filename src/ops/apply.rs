//! `hoist apply` (spec §7.2): plan, preflight, confirm, write in dependency order, then re-read
//! and verify with the `check` logic. Errors before the first write exit 3; a failed write
//! stops the run and reports what was done (exit 1).

use crate::change::{Change, ChangeOpts, plan_changes};
use crate::cli::Area;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::LiveGithub;
use crate::http::{Fetched, Http};
use crate::ops::check::verify_state;
use crate::ops::plan::run_with_state;
use crate::report::{Item, Report};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

mod execute;
use execute::{add_steps, declined, late_error, write_all};

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
        return verify_state(dir, areas, state, &[]);
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
    if !yes && !confirm(&changes, &state.report.items)? {
        return Ok(declined(&changes, &state.report));
    }

    let written = match write_all(gh, cf, d, &changes, &state.report) {
        Ok(w) => w,
        Err(report) => return Ok(report),
    };

    // Writes happened: from here on an error is reported with what was applied (exit 1),
    // never as a bare exit 3.
    let verified = run_with_state(dir, areas, verbose, "apply")
        .and_then(|after| verify_state(dir, areas, after, &written.created));
    Ok(match verified {
        Ok(mut report) => {
            add_steps(&mut report, written.steps);
            report
        }
        Err(err) => late_error(&changes, &state.report, written.steps, &err),
    })
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

/// Ask on a terminal; without one, refuse before the first write (use `--yes`).
fn confirm(changes: &[Change], items: &[Item]) -> Result<bool, HoistError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(HoistError::Usage(
            "stdin is not a terminal; use --yes in non-interactive runs".into(),
        ));
    }
    ask(changes, items, stdin.lock(), std::io::stderr().lock())
}

/// Print the change list to `out` (stderr in `apply`, so `--json` stays one object) and read
/// the answer from `input`: `y`/`yes` (any case) is true, anything else false.
pub(crate) fn ask(
    changes: &[Change],
    items: &[Item],
    mut input: impl BufRead,
    mut out: impl Write,
) -> Result<bool, HoistError> {
    let _ = writeln!(out, "hoist apply will make {} change(s):", changes.len());
    for change in changes {
        let _ = writeln!(out, "  {}", describe(change, items));
    }
    let _ = write!(out, "Apply these changes? [y/N] ");
    let _ = out.flush();
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .map_err(|e| HoistError::Usage(format!("cannot read the answer: {e}; use --yes")))?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Action;

    fn env_change() -> Change {
        Change::Environment {
            name: "production".into(),
            branch: "main".into(),
        }
    }

    fn plan() -> Report {
        Report {
            command: "apply".into(),
            items: vec![Item {
                area: "github".into(),
                kind: "environment".into(),
                key: "production".into(),
                action: Action::Create,
                reason: "missing; would create the environment".into(),
                source: "deploy.yml:3".into(),
                verify: None,
            }],
            human_steps: Vec::new(),
            exit_code: 0,
        }
    }

    #[test]
    fn prompt_accepts_y_and_yes_only() {
        let changes = [env_change()];
        let items = plan().items;
        for (answer, want) in [
            ("y\n", true),
            ("YES\n", true),
            ("n\n", false),
            ("\n", false),
        ] {
            let mut err = Vec::new();
            let got = ask(&changes, &items, answer.as_bytes(), &mut err).unwrap();
            assert_eq!(got, want, "{answer:?}");
            let shown = String::from_utf8(err).unwrap();
            assert!(
                shown.contains("create github/environment production"),
                "{shown}"
            );
        }
    }

    #[test]
    fn declined_report_exits_1_and_lists_changes() {
        let report = declined(&[env_change()], &plan());
        assert_eq!(report.exit_code, 1);
        assert_eq!(report.command, "apply");
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].reason, "not applied: declined");
        assert_eq!(report.items[0].action, Action::Create);
    }
}
