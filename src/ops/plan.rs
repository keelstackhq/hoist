//! `hoist plan`: desired state against live state, GET requests only.

use crate::auth::{Secret, cloudflare_token, github_token};
use crate::cli::Area;
use crate::cloudflare::{LiveCloudflare, read_cloudflare};
use crate::desired::{Desired, build_desired};
use crate::diff::{diff_cloudflare, diff_github};
use crate::diff_release::{diff_registry, diff_release, read_registries};
use crate::error::HoistError;
use crate::github::{LiveGithub, read_github};
use crate::http::{Http, Service};
use crate::release::PkgKind;
use crate::report::Report;
use std::path::Path;

/// Plan the selected areas (all when `areas` is empty). Cloudflare is only read when a deploy
/// target exists, so a repo without targets needs no Cloudflare token. The `npm`, `crates` and
/// `release` areas exist only for repos with publishing packages; `npm`/`crates` read the
/// registries anonymously, `release` reads GitHub (the release environment). Exit code 0: a plan is
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

/// A plan together with the state it was computed from, for `check` to verify against and
/// `apply` to write with. The clients are `None` for areas that were not read.
pub(crate) struct PlanState {
    pub report: Report,
    pub desired: Desired,
    /// Read for the `github` area, or for the `release` area alone.
    pub github: Option<LiveGithub>,
    pub cloudflare: Option<LiveCloudflare>,
    pub github_http: Option<Http>,
    pub cloudflare_http: Option<Http>,
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

    let release = desired.release.as_ref();
    let has = |kind: PkgKind| release.is_some_and(|r| r.packages.iter().any(|p| p.kind == kind));
    let gh_needed = wants(Area::Github) || (wants(Area::Release) && release.is_some());
    // Registry reads are anonymous: an empty token sends no Authorization header.
    let registry = |area: Area, kind: PkgKind, service: Service| {
        (wants(area) && has(kind)).then(|| Http::new(service, Secret::from("")).verbose(verbose))
    };
    let npm = registry(Area::Npm, PkgKind::Npm, Service::Npm);
    let crates = registry(Area::Crates, PkgKind::Crate, Service::Crates);

    // Resolve every needed token before the first request.
    let github = gh_needed
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
    let mut live_github = None;
    if let Some(http) = &cloudflare {
        let live = read_cloudflare(http, &desired)?;
        let (i, s) = diff_cloudflare(&desired, &live);
        items.extend(i);
        human_steps.extend(s);
        live_cloudflare = Some(live);
    }
    if let Some(http) = &github {
        let live = read_github(http, &desired)?;
        if wants(Area::Github) {
            let (i, s) = diff_github(&desired, &live, live_cloudflare.as_ref());
            items.extend(i);
            human_steps.extend(s);
        }
        if wants(Area::Release) {
            items.extend(diff_release(&desired, &live));
        }
        live_github = Some(live);
    }
    if npm.is_some() || crates.is_some() {
        let live = read_registries(&desired, npm.as_ref(), crates.as_ref())?;
        for (http, kind) in [(&npm, PkgKind::Npm), (&crates, PkgKind::Crate)] {
            if http.is_some() {
                let (i, s) = diff_registry(&desired, kind, &live);
                items.extend(i);
                human_steps.extend(s);
            }
        }
    }
    items.sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    Ok(PlanState {
        report: Report {
            command: command.to_string(),
            items,
            human_steps,
            exit_code: 0,
            text: None,
        },
        desired,
        github: live_github,
        cloudflare: live_cloudflare,
        github_http: github,
        cloudflare_http: cloudflare,
    })
}
