//! `hoist release publish`: tag each pending package, publish its GitHub Release, then publish
//! it to npm / crates.io and run its `on = "release"` deploys (spec §9.3–9.5). Exit 3 only
//! before the first write; a failed write or verification is exit 1; human steps alone exit 2.

use crate::auth::Secret;
use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Http, Service};
use crate::release::edit::changelog_section;
use crate::release::github_rel::{LocalAsset, local_assets};
use crate::release::gitlog::require_full_history;
use crate::release::oidc::require_oidc_env;
use crate::release::planner::order;
use crate::release::tools::{Tool, run};
use crate::release::{Package, ReleaseModel, read_release_model};
use crate::repo::git::read_repo;
use crate::repo::targets::{Target, read_targets};
use crate::report::{Action, HumanStep, Item, Report};
use std::path::{Path, PathBuf};

mod execute;
mod github;
mod preflight;
mod registries;
use execute::{dry_run, execute};
use preflight::{preflight, require_actions};

const COMMAND: &str = "release publish";

/// One pending package: its tag, release body, asset files and whether it deploys.
pub(super) struct Job<'a> {
    pkg: &'a Package,
    tag: String,
    body: String,
    assets: Vec<LocalAsset>,
    /// A target in the package's directory has `[deploy] on = "release"`.
    deploy: bool,
}

impl Job<'_> {
    /// The steps of this package, in order (spec §9.3).
    fn steps(&self) -> Vec<Step> {
        let mut steps = vec![Step::Tag, Step::Release];
        if self.pkg.publish.npm.is_some() {
            steps.push(Step::Npm);
        }
        if self.pkg.publish.crates.is_some() {
            steps.push(Step::Crates);
        }
        if self.deploy {
            steps.push(Step::Deploy);
        }
        steps
    }

    fn publishes(&self) -> bool {
        self.pkg.publish.npm.is_some() || self.pkg.publish.crates.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Tag,
    Release,
    Npm,
    Crates,
    Deploy,
}

impl Step {
    fn kind(self) -> &'static str {
        match self {
            Step::Tag => "tag",
            Step::Release => "github_release",
            Step::Npm => "npm",
            Step::Crates => "crates",
            Step::Deploy => "deploy",
        }
    }

    fn github(self) -> bool {
        matches!(self, Step::Tag | Step::Release)
    }
}

/// The API clients of one run; registry reads are anonymous.
pub(super) struct Clients {
    gh: Http,
    up: Http,
    npm: Http,
    crates: Http,
}

fn git(dir: &Path, args: &[&str], verbose: bool) -> Result<String, HoistError> {
    let out = run(Tool::Git, dir, args, &[])?;
    if verbose {
        eprint!("{out}");
    }
    Ok(out)
}

/// Packages whose manifest version has no tag in `tags`, in dependency order.
fn pending<'a>(m: &'a ReleaseModel, tags: &[String]) -> Vec<(&'a Package, String)> {
    order(m)
        .into_iter()
        .filter_map(|p| {
            let tag = m.tag_for(p, &p.version.value);
            (!tags.contains(&tag)).then_some((p, tag))
        })
        .collect()
}

/// Local tags that mark a finished release: every tag except those on HEAD. A tag on HEAD is
/// this release's own (a re-run), so its package stays pending and every step re-checks.
fn released_tags(dir: &Path, verbose: bool) -> Result<Vec<String>, HoistError> {
    let at_head = git(dir, &["tag", "--points-at", "HEAD"], verbose)?;
    let at_head: Vec<&str> = at_head.lines().map(str::trim).collect();
    Ok(git(dir, &["tag", "--list"], verbose)?
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty() && !at_head.contains(t))
        .map(str::to_string)
        .collect())
}

/// `<assets>/<bin>-<target>` (`.exe` for Windows targets) per `github_release` target, sorted
/// by file name. A missing file is exit 3.
fn asset_files(p: &Package, assets: Option<&Path>) -> Result<Vec<PathBuf>, HoistError> {
    let Some(gr) = &p.publish.github_release else {
        return Ok(Vec::new());
    };
    let bin = gr.bin.as_deref().unwrap_or(&p.name);
    let mut files = Vec::new();
    for target in &gr.targets {
        let exe = if target.contains("-windows-") {
            ".exe"
        } else {
            ""
        };
        let name = format!("{bin}-{target}{exe}");
        let Some(dir) = assets else {
            return Err(HoistError::Usage(format!(
                "`{}` releases binaries ({name}, …): pass `--assets DIR` with the built files",
                p.name
            )));
        };
        let path = dir.join(&name);
        if !path.is_file() {
            return Err(HoistError::Usage(format!(
                "asset {name} for `{}` is missing in {}; download the build artifacts there \
                 first",
                p.name,
                dir.display()
            )));
        }
        files.push(path);
    }
    files.sort_by_key(|f| f.file_name().map(|n| n.to_os_string()));
    Ok(files)
}

fn release_body(dir: &Path, p: &Package, tag: &str) -> String {
    std::fs::read_to_string(dir.join(&p.dir).join("CHANGELOG.md"))
        .ok()
        .and_then(|text| changelog_section(&text, &p.version.value))
        .unwrap_or_else(|| format!("Release {tag}."))
}

pub(super) fn item(job: &Job, step: Step, action: Action, reason: String) -> Item {
    item_of(job, step.kind(), action, reason)
}

fn item_of(job: &Job, kind: &str, action: Action, reason: String) -> Item {
    Item {
        area: "release".to_string(),
        kind: kind.to_string(),
        key: job.tag.clone(),
        action,
        reason,
        source: job.pkg.version.source.clone(),
        verify: None,
    }
}

pub(super) fn report(items: Vec<Item>, human_steps: Vec<HumanStep>, exit_code: i32) -> Report {
    Report {
        command: COMMAND.to_string(),
        items,
        human_steps,
        exit_code,
        text: None,
    }
}

/// Whether a target with `[deploy] on = "release"` lives in the package's directory.
fn deploys_on_release(dir: &Path, targets: &[Target], p: &Package) -> bool {
    let pkg_dir = dir.join(&p.dir);
    targets
        .iter()
        .any(|t| t.config.deploy.as_ref().is_some_and(|d| d.on == "release") && t.dir == pkg_dir)
}

/// Tag and release every package whose manifest version is not yet released.
pub fn release_publish(
    dir: &Path,
    assets: Option<&Path>,
    dry_run_only: bool,
    verbose: bool,
) -> Result<Report, HoistError> {
    if !dry_run_only {
        require_actions()?;
    }
    let (repo, default) = read_repo(dir)?;
    let model = read_release_model(dir)?;
    let head = git(dir, &["rev-parse", "HEAD"], verbose)?
        .trim()
        .to_string();
    if !dry_run_only {
        preflight(dir, &default, &model, &head)?;
    }
    require_full_history(dir)?;
    let todo = pending(&model, &released_tags(dir, verbose)?);
    if todo.is_empty() {
        let it = Item {
            area: "release".to_string(),
            kind: "publish".to_string(),
            key: "pending".to_string(),
            action: Action::Ok,
            reason: "every package version is already tagged".to_string(),
            source: "default".to_string(),
            verify: None,
        };
        return Ok(report(vec![it], vec![], 0));
    }
    let (targets, _, _) = read_targets(dir)?;
    let mut jobs = Vec::new();
    for (pkg, tag) in todo {
        jobs.push(Job {
            assets: local_assets(&asset_files(pkg, assets)?)?,
            body: release_body(dir, pkg, &tag),
            deploy: deploys_on_release(dir, &targets, pkg),
            pkg,
            tag,
        });
    }
    if !dry_run_only && jobs.iter().any(Job::publishes) {
        require_oidc_env()?;
    }
    let token = github_token()?;
    let anonymous = || Secret::from("");
    let c = Clients {
        gh: Http::new(Service::Github, token.clone()).verbose(verbose),
        up: Http::new(Service::GithubUploads, token).verbose(verbose),
        npm: Http::new(Service::Npm, anonymous()).verbose(verbose),
        crates: Http::new(Service::Crates, anonymous()).verbose(verbose),
    };
    if dry_run_only {
        return dry_run(&c, &repo, &jobs, &head);
    }
    Ok(execute(&c, dir, &repo, &jobs, &head))
}
