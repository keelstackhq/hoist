//! `hoist plan`: desired state against live state, GET requests only.

use crate::auth::{cloudflare_token, github_token};
use crate::cli::Area;
use crate::cloudflare::{LiveCloudflare, read_cloudflare};
use crate::desired::{Desired, build_desired};
use crate::diff::{diff_cloudflare, diff_github};
use crate::error::HoistError;
use crate::github::read_github;
use crate::http::{Http, Service};
use crate::report::Report;
use std::path::Path;

/// Plan the selected areas (all when `areas` is empty). Cloudflare is only read when a deploy
/// target exists, so a repo without targets needs no Cloudflare token. Exit code 0: a plan is
/// a preview, even with changes.
pub fn plan(dir: &Path, areas: &[Area], verbose: bool) -> Result<Report, HoistError> {
    run(dir, areas, verbose, "plan")
}

pub(crate) fn run(
    dir: &Path,
    areas: &[Area],
    verbose: bool,
    command: &str,
) -> Result<Report, HoistError> {
    run_with_state(dir, areas, verbose, command).map(|s| s.report)
}

/// A plan together with the state it was computed from, for `check` to verify against.
pub(crate) struct PlanState {
    pub report: Report,
    pub desired: Desired,
    pub cloudflare: Option<LiveCloudflare>,
}

pub(crate) fn run_with_state(
    dir: &Path,
    areas: &[Area],
    verbose: bool,
    command: &str,
) -> Result<PlanState, HoistError> {
    let desired = build_desired(dir)?;
    let wants = |area: Area| areas.is_empty() || areas.contains(&area);
    let has_targets =
        !(desired.cloudflare.pages.is_empty() && desired.cloudflare.workers.is_empty());
    if wants(Area::Cloudflare) {
        desired.require_account_id()?;
    }

    // Resolve every needed token before the first request.
    let github = wants(Area::Github)
        .then(github_token)
        .transpose()?
        .map(|t| Http::new(Service::Github, t).verbose(verbose));
    let cloudflare = (wants(Area::Cloudflare) && has_targets)
        .then(cloudflare_token)
        .transpose()?
        .map(|t| Http::new(Service::Cloudflare, t).verbose(verbose));

    let mut items = Vec::new();
    let mut human_steps = Vec::new();
    let mut live_cloudflare = None;
    if let Some(http) = &cloudflare {
        let live = read_cloudflare(http, &desired)?;
        let (i, s) = diff_cloudflare(&desired, &live);
        items.extend(i);
        human_steps.extend(s);
        live_cloudflare = Some(live);
    }
    if let Some(http) = &github {
        let live = read_github(http, &desired)?;
        let (i, s) = diff_github(&desired, &live, live_cloudflare.as_ref());
        items.extend(i);
        human_steps.extend(s);
    }
    items.sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    Ok(PlanState {
        report: Report {
            command: command.to_string(),
            items,
            human_steps,
            exit_code: 0,
        },
        desired,
        cloudflare: live_cloudflare,
    })
}
