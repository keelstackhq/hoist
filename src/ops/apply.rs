//! `hoist apply` (spec §7.2): plan, preflight, confirm, write in dependency order, then re-read
//! and verify with the `check` logic. Errors before the first write exit 3; a failed write
//! stops the run and reports what was done (exit 1).

use crate::auth::github_token;
use crate::change::{Change, ChangeOpts, plan_changes};
use crate::cli::Area;
use crate::cloudflare::{TokenStatus, VERIFY_PATH, token_status};
use crate::cloudflare_write::permission_group_ids;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github::LiveGithub;
use crate::http::{Fetched, Http, Service};
use crate::ops::check::verify_state;
use crate::ops::plan::run_with_state;
use crate::report::{Item, Report};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

mod execute;
mod r2_sink;
use execute::{
    add_steps, declined, late_error, mark_plan_limited, mark_reviewer_limited,
    mark_reviewer_pending, write_all,
};
pub use r2_sink::SinkOpts;

pub fn apply(
    dir: &Path,
    areas: &[Area],
    yes: bool,
    prune: bool,
    adopt: bool,
    verbose: bool,
    sink_opts: SinkOpts,
) -> Result<Report, HoistError> {
    if adopt && prune {
        return Err(HoistError::Usage(
            "--adopt never removes anything: drop --prune".into(),
        ));
    }
    // The R2 credential sink (spec §6.2): a bad combination is refused before anything is read.
    let sink = sink_opts.parse()?;
    let state = run_with_state(dir, areas, verbose, "apply", adopt)?;
    let opts = ChangeOpts {
        prune,
        areas: areas.to_vec(),
    };
    let mut changes = plan_changes(
        &state.desired,
        state.github.as_ref(),
        state.cloudflare.as_ref(),
        &opts,
    );
    // A stray A/AAAA record is deleted only with `--prune` (spec §6.3).
    changes.extend(
        state
            .dns_writes
            .iter()
            .filter(|w| prune || !matches!(w.op, crate::dns::DnsOp::Delete { .. }))
            .cloned()
            .map(Change::Dns),
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
        let gh = gh.ok_or_else(|| not_read("GitHub"))?;
        preflight_github_admin(gh, live, d)?;
        // The release environment's reviewers (configured logins, else the token's user):
        // resolve them before any write.
        let logins = d.release.as_ref().map(|r| r.reviewers.as_slice());
        for c in &mut changes {
            if let Change::Environment {
                release: true,
                reviewers,
                ..
            } = c
            {
                *reviewers = crate::github_write::resolve_reviewers(gh, logins.unwrap_or(&[]))?;
            }
        }
    }
    let mut groups = None;
    if changes.iter().any(writes_cloudflare) {
        let cf = cf.ok_or_else(|| not_read("Cloudflare"))?;
        // DNS records need a token only; the other Cloudflare writes also need an account id.
        match changes
            .iter()
            .any(|c| writes_cloudflare(c) && !matches!(c, Change::Dns(_)))
        {
            true => preflight_cloudflare(cf, d)?,
            false => preflight_cloudflare_token(cf)?,
        }
        // Like rotate (R25): the permission groups are a GET, so resolve them before the
        // first write; a failure here exits 3.
        if changes
            .iter()
            .any(|c| matches!(c, Change::DeployToken { .. }))
        {
            groups = Some(permission_group_ids(cf, d)?);
        }
    }
    // A new R2 token needs somewhere to put its credentials, checked before any write.
    let mints_r2 = changes
        .iter()
        .any(|c| matches!(c, Change::R2(crate::r2::R2Change::Token { .. })));
    let own_gh = match (&sink, gh) {
        (Some(r2_sink::Sink::GhEnv(_)), None) if mints_r2 => {
            Some(Http::new(Service::Github, github_token()?).verbose(verbose))
        }
        _ => None,
    };
    let has_r2 = changes.iter().any(|c| matches!(c, Change::R2(_)));
    let mut r2_run = None;
    if has_r2 {
        let mut run = r2_sink::R2Run {
            groups: Vec::new(),
            sink: None,
            gh: None,
            dir: dir.to_path_buf(),
            left_over: Default::default(),
        };
        // Only the token change needs the sink and the permission groups; a bucket or a
        // lifecycle write on its own (the token already exists) needs neither.
        if mints_r2 {
            let sink = sink.clone().ok_or_else(|| {
                HoistError::Usage(
                    "the R2 token's credentials need a destination: pass `--secrets-to \
                     gh-env:<environment>`, `--secrets-to fly:<app>` or `--emit-secrets`"
                        .into(),
                )
            })?;
            let gh_client = gh.or(own_gh.as_ref());
            r2_sink::preflight(&sink, d, gh_client, dir)?;
            let cf = cf.ok_or_else(|| not_read("Cloudflare"))?;
            run.groups = crate::r2::permission_groups(cf)?;
            run.sink = Some(sink);
            run.gh = gh_client;
        }
        r2_run = Some(run);
    }
    if !yes && !confirm(&changes, &state.report.items)? {
        return Ok(declined(&changes, &state.report));
    }

    let written = match write_all(
        gh,
        cf,
        d,
        &changes,
        &state.report,
        groups.as_deref(),
        r2_run.as_ref(),
    ) {
        Ok(w) => w,
        Err(report) => return Ok(report),
    };

    // Writes happened: from here on an error is reported with what was applied (exit 1),
    // never as a bare exit 3.
    let verified = run_with_state(dir, areas, verbose, "apply", adopt)
        .and_then(|after| verify_state(dir, areas, after, &written.created));
    let mut report = match verified {
        Ok(mut report) => {
            add_steps(&mut report, written.steps);
            report
        }
        Err(err) => late_error(&changes, &state.report, written.steps, &err),
    };
    if written.plan_limited {
        mark_plan_limited(&mut report, d);
    }
    if written.reviewer_limited {
        mark_reviewer_limited(&mut report);
    }
    if changes.iter().any(|c| {
        matches!(
            c,
            Change::Environment {
                reviewers: crate::change::Reviewers::HumanNeeded,
                ..
            }
        )
    }) {
        mark_reviewer_pending(&mut report, d);
    }
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
        Change::PagesProject { .. }
            | Change::Domain { .. }
            | Change::RevokeToken { .. }
            | Change::Dns(_)
            | Change::R2(_)
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
            | Change::Dns(_)
            | Change::R2(_)
    )
}

/// GitHub writes need admin on the repo. Fine-grained tokens cannot list their own write
/// permissions, so `permissions.admin` from `GET /repos/{o}/{r}` is the check. For an App
/// installation token without `admin: true` there, the repo must be one the installation
/// reaches; a permission the App lacks shows as a failed write (exit 1).
pub(crate) fn preflight_github_admin(
    http: &Http,
    live: &LiveGithub,
    d: &Desired,
) -> Result<(), HoistError> {
    let (owner, repo) = (&d.repo.owner, &d.repo.name);
    let detail = match live.settings.as_got() {
        Ok(s) if s.admin == Some(true) => return Ok(()),
        Ok(_) if http.app_installation() => {
            return installation_reaches(http, owner, repo);
        }
        Ok(_) => format!("GET /repos/{owner}/{repo} does not report `permissions.admin: true`"),
        Err(why) => format!("GET /repos/{owner}/{repo} was not readable: {why}"),
    };
    Err(HoistError::Usage(format!(
        "the token needs admin on {owner}/{repo} (Administration: write); {detail}. Use a token \
         with Administration, Secrets, Variables, Environments and Actions: write on the repo"
    )))
}

/// Whether the App installation behind `http`'s token includes `owner/repo`.
fn installation_reaches(http: &Http, owner: &str, repo: &str) -> Result<(), HoistError> {
    let usage = |msg: String| Err(HoistError::Usage(msg));
    let listed = match http.get_all("/installation/repositories", Some("repositories"))? {
        Fetched::Ok(Value::Array(all)) => all,
        Fetched::Ok(_) => {
            return usage(
                "cannot list the app installation's repositories: unexpected response".into(),
            );
        }
        Fetched::NotFound => {
            return usage("cannot list the app installation's repositories (404)".into());
        }
        Fetched::Forbidden(why) => {
            return usage(format!(
                "cannot list the app installation's repositories ({why})"
            ));
        }
    };
    let full = format!("{owner}/{repo}");
    let reached = listed.iter().any(|r| {
        r.get("full_name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.eq_ignore_ascii_case(&full))
    });
    match reached {
        true => Ok(()),
        false => usage(format!(
            "the app installation does not include {full}; install the App on it (Administration, \
             Secrets, Environments and Actions: write)"
        )),
    }
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
    preflight_cloudflare_token(cf_http)
}

/// The token half of the Cloudflare preflight: `GET /user/tokens/verify` reports it active.
pub(crate) fn preflight_cloudflare_token(cf_http: &Http) -> Result<(), HoistError> {
    let status = match token_status(cf_http)? {
        TokenStatus::Active => return Ok(()),
        TokenStatus::NotActive(why) => why,
    };
    Err(HoistError::Usage(format!(
        "the Cloudflare token is not active (GET {VERIFY_PATH}: {status}); export \
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
    input: impl BufRead,
    out: impl Write,
) -> Result<bool, HoistError> {
    let lines: Vec<String> = changes.iter().map(|c| describe(c, items)).collect();
    ask_lines(
        &format!("hoist apply will make {} change(s):", changes.len()),
        &lines,
        "Apply these changes?",
        input,
        out,
    )
}

/// Print `header`, one indented line per entry and `question [y/N] ` to `out`, then read the
/// answer from `input`: `y`/`yes` (any case) is true, anything else false.
pub(crate) fn ask_lines(
    header: &str,
    lines: &[String],
    question: &str,
    mut input: impl BufRead,
    mut out: impl Write,
) -> Result<bool, HoistError> {
    let _ = writeln!(out, "{header}");
    for line in lines {
        let _ = writeln!(out, "  {line}");
    }
    let _ = write!(out, "{question} [y/N] ");
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
        Change::Dns(w) if matches!(w.op, crate::dns::DnsOp::Delete { .. }) => {
            format!("delete cloudflare/dns_record {} (--prune)", w.key)
        }
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
            branches: vec!["main".into()],
            write_env: true,
            exists: false,
            release: false,
            reviewers: crate::change::Reviewers::Unresolved,
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
            text: None,
            hint: None,
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

    #[test]
    fn prompt_names_each_stray_dns_delete() {
        let change = Change::Dns(crate::dns::DnsWrite {
            zone_id: "z1".into(),
            key: "app.example.com A 198.51.100.1".into(),
            op: crate::dns::DnsOp::Delete { id: "r2".into() },
        });
        // The item's reason is the pre-prune one; the prompt still says what happens.
        let items = vec![Item {
            area: "cloudflare".into(),
            kind: "dns_record".into(),
            key: "app.example.com A 198.51.100.1".into(),
            action: Action::Drift,
            reason: "not a target".into(),
            source: "hoist.toml".into(),
            verify: None,
        }];
        let mut err = Vec::new();
        ask(&[change], &items, "n\n".as_bytes(), &mut err).unwrap();
        let shown = String::from_utf8(err).unwrap();
        assert!(
            shown.contains("delete cloudflare/dns_record app.example.com A 198.51.100.1"),
            "{shown}"
        );
    }
}
