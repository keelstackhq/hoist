//! `apply` execution: the writes in order, and the reports of a run that stopped, was
//! declined or could not be re-read after writing.

use super::{find, not_read, writes_cloudflare};
use crate::change::Change;
use crate::cloudflare_write::execute_cloudflare_with;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::github_write::execute_github;
use crate::http::Http;
use crate::ops::check::exit_code;
use crate::report::{Action, HumanStep, Item, Report, Verify};
use std::cmp::Ordering;

/// What a complete run of writes leaves for the report.
pub(super) struct Written {
    /// Human steps the writers returned (a domain whose zone is not on the account).
    pub steps: Vec<HumanStep>,
    /// `(kind, key)` of Pages projects and domains created now: no deployment yet (R17).
    pub created: Vec<(String, String)>,
}

/// Run every change in order. The first failed write stops the run: `Err` is its report
/// (applied, `failed: <error>`, not attempted; exit 1).
pub(super) fn write_all(
    gh: Option<&Http>,
    cf: Option<&Http>,
    d: &Desired,
    changes: &[Change],
    plan: &Report,
    groups: Option<&[String]>,
) -> Result<Written, Report> {
    let mut steps = Vec::new();
    let mut created = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        match execute(gh, cf, d, change, groups) {
            Ok(step) => {
                match (&step, change) {
                    (None, Change::PagesProject { name, exists, .. }) if !exists => {
                        created.push(("pages_project".to_string(), name.clone()))
                    }
                    (None, Change::Domain { host, .. }) => {
                        created.push(("custom_domain".to_string(), host.clone()))
                    }
                    _ => {}
                }
                steps.extend(step);
            }
            Err(err) => {
                let status = |i: usize| match i.cmp(&index) {
                    Ordering::Less => (true, "applied".to_string()),
                    Ordering::Equal => (false, format!("failed: {err}")),
                    Ordering::Greater => (false, "not attempted".to_string()),
                };
                return Err(stopped(changes, plan, status, steps));
            }
        }
    }
    Ok(Written { steps, created })
}

/// All writes succeeded but the re-read or check failed: the applied changes plus a
/// `post_apply_check` item with the error, exit 1.
pub(super) fn late_error(
    changes: &[Change],
    plan: &Report,
    steps: Vec<HumanStep>,
    err: &HoistError,
) -> Report {
    let mut report = stopped(changes, plan, |_| (true, "applied".to_string()), steps);
    report.items.push(Item {
        area: "apply".to_string(),
        kind: "post_apply_check".to_string(),
        key: "re-read".to_string(),
        action: Action::Unknown,
        reason: format!("re-read after the writes failed: {err}; run `hoist check` to verify"),
        source: "default".to_string(),
        verify: Some(Verify {
            ok: false,
            detail: err.to_string(),
        }),
    });
    report
}

/// The report of a declined prompt: nothing written, exit 1.
pub(super) fn declined(changes: &[Change], plan: &Report) -> Report {
    stopped(
        changes,
        plan,
        |_| (false, "not applied: declined".to_string()),
        Vec::new(),
    )
}

fn execute(
    gh: Option<&Http>,
    cf: Option<&Http>,
    d: &Desired,
    change: &Change,
    groups: Option<&[String]>,
) -> Result<Option<HumanStep>, HoistError> {
    if writes_cloudflare(change) {
        let cf = cf.ok_or_else(|| not_read("Cloudflare"))?;
        execute_cloudflare_with(cf, gh, d, change, groups)
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

/// The report of a run that stopped (a failed write, a declined prompt) or could not be
/// verified: one item per change with `status(index)` = (applied, reason), then the plan's
/// items that no change stands for (pending, unknown, ok). Exit 1.
fn stopped(
    changes: &[Change],
    plan: &Report,
    status: impl Fn(usize) -> (bool, String),
    steps: Vec<HumanStep>,
) -> Report {
    let mut items = Vec::new();
    let mut covered = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let (done, reason) = status(index);
        for (area, kind, key) in keys(change) {
            let planned = find(&plan.items, area, kind, &key);
            let action = match (done, planned) {
                (true, _) => Action::Ok,
                (false, Some(p)) => p.action,
                (false, None) => Action::Update,
            };
            covered.push((area, kind, key.clone()));
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
    // Create/update items all pair with a change (R16); the rest is kept as planned.
    items.extend(
        plan.items
            .iter()
            .filter(|i| !matches!(i.action, Action::Create | Action::Update))
            .filter(|i| {
                !covered
                    .iter()
                    .any(|(a, k, key)| *a == i.area && *k == i.kind && *key == i.key)
            })
            .cloned(),
    );
    let mut report = Report {
        command: "apply".to_string(),
        items,
        human_steps: plan.human_steps.clone(),
        exit_code: 1,
        text: None,
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
pub(super) fn add_steps(report: &mut Report, steps: Vec<HumanStep>) {
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
