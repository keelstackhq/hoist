//! `hoist check`: the plan plus behaviour checks, with an exit-code policy (spec §3, §7).

use crate::cli::Area;
use crate::cloudflare::PagesProject;
use crate::error::HoistError;
use crate::github::Fetch;
use crate::ops::plan::{PlanState, run_with_state};
use crate::repo::workflows::read_workflows;
use crate::report::{Action, Item, Report};
use crate::verify::{verify_domain, verify_pages, verify_required_checks};
use std::path::Path;

/// Exit 1 when any item is `create`, `update`, `drift` or failed verification; else 2 when any
/// item is `pending`; else 0. `unknown` alone never fails (it is reported).
pub(crate) fn exit_code(items: &[Item]) -> i32 {
    let failed = |i: &Item| {
        matches!(i.action, Action::Create | Action::Update | Action::Drift)
            || i.verify.as_ref().is_some_and(|v| !v.ok)
    };
    if items.iter().any(failed) {
        1
    } else if items.iter().any(|i| i.action == Action::Pending) {
        2
    } else {
        0
    }
}

/// Verification applies to things that exist: skip `create` and `unknown` items.
fn verifiable(item: &Item) -> bool {
    !matches!(item.action, Action::Create | Action::Unknown)
}

pub fn check(dir: &Path, areas: &[Area], verbose: bool) -> Result<Report, HoistError> {
    let state = run_with_state(dir, areas, verbose, "check")?;
    verify_state(dir, areas, state, &[])
}

/// Reason on a Pages project or domain that `apply` created in the same run (R17).
pub(crate) const CREATED_IN_RUN: &str =
    "created in this run; verified by `hoist check` after the first deploy";

/// The check half of `check`: verification of a computed plan and the exit policy. `apply`
/// runs it on the state it re-reads after writing; `created` lists the Cloudflare
/// `(kind, key)` items it created in that run, which exist now but have no deployment yet, so
/// they are not probed (spec §7.4).
pub(crate) fn verify_state(
    dir: &Path,
    areas: &[Area],
    state: PlanState,
    created: &[(String, String)],
) -> Result<Report, HoistError> {
    let mut report = state.report;
    // Only items the re-read shows as existing; a create still pending stays a failure.
    let fresh = |i: &Item| {
        i.area == "cloudflare"
            && i.action == Action::Ok
            && created.iter().any(|(k, key)| *k == i.kind && *key == i.key)
    };
    for item in report.items.iter_mut().filter(|i| fresh(i)) {
        item.reason = CREATED_IN_RUN.to_string();
    }
    let wants = |area: Area| areas.is_empty() || areas.contains(&area);

    if wants(Area::Github) {
        let workflows = read_workflows(dir)?;
        report
            .items
            .extend(verify_required_checks(&state.desired, &workflows));
    }

    if let Some(live) = &state.cloudflare {
        for item in report
            .items
            .iter_mut()
            .filter(|i| i.area == "cloudflare" && verifiable(i) && !fresh(i))
        {
            match item.kind.as_str() {
                "pages_project" => {
                    let project: Option<&PagesProject> = live
                        .pages
                        .iter()
                        .find(|(name, _)| *name == item.key)
                        .and_then(|(_, fetch)| match fetch {
                            Fetch::Got(Some(p)) => Some(p),
                            _ => None,
                        });
                    if let Some(p) = project {
                        item.verify = Some(verify_pages(p));
                    }
                }
                "custom_domain" => item.verify = Some(verify_domain(&item.key)),
                _ => {}
            }
        }
    }

    report
        .items
        .sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    report.exit_code = exit_code(&report.items);
    Ok(report)
}
