//! `hoist release publish`: tag each pending package and publish its GitHub Release (spec
//! §9.3–9.5). Exit 3 only before the first write; a failed write or verification is exit 1.

use crate::auth::github_token;
use crate::error::HoistError;
use crate::http::{Http, Service};
use crate::release::edit::changelog_section;
use crate::release::github_rel::{LocalAsset, local_assets};
use crate::release::gitlog::require_full_history;
use crate::release::planner::order;
use crate::release::tools::{Tool, run};
use crate::release::{Package, ReleaseModel, read_release_model};
use crate::repo::git::{GithubRepo, read_repo};
use crate::report::{Action, Item, Report};
use std::path::{Path, PathBuf};

mod github;
mod preflight;
use github::{dry_run, run_step, verify_step};
use preflight::{preflight, require_actions};

const COMMAND: &str = "release publish";

/// One pending package: its tag, release body and asset files.
pub(super) struct Job<'a> {
    pkg: &'a Package,
    tag: String,
    body: String,
    assets: Vec<LocalAsset>,
}

/// The steps of one package, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Tag,
    Release,
}

const STEPS: [Step; 2] = [Step::Tag, Step::Release];

impl Step {
    fn kind(self) -> &'static str {
        match self {
            Step::Tag => "tag",
            Step::Release => "github_release",
        }
    }
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
    Item {
        area: "release".to_string(),
        kind: step.kind().to_string(),
        key: job.tag.clone(),
        action,
        reason,
        source: job.pkg.version.source.clone(),
        verify: None,
    }
}

pub(super) fn report(items: Vec<Item>, exit_code: i32) -> Report {
    Report {
        command: COMMAND.to_string(),
        items,
        human_steps: vec![],
        exit_code,
        text: None,
    }
}

/// Every step in order; the first failure stops the run (exit 1). Then verify each step.
fn execute(gh: &Http, up: &Http, repo: &GithubRepo, jobs: &[Job], head: &str) -> Report {
    let mut items = Vec::new();
    let mut failed = false;
    for job in jobs {
        for step in STEPS {
            if failed {
                items.push(item(job, step, Action::Update, "not attempted".to_string()));
                continue;
            }
            match run_step(gh, up, repo, job, step, head) {
                Ok(done) => items.push(item(job, step, Action::Ok, done)),
                Err(e) => {
                    failed = true;
                    items.push(item(job, step, Action::Update, format!("failed: {e}")));
                }
            }
        }
    }
    if failed {
        return report(items, 1);
    }
    let mut all_ok = true;
    let steps = jobs.iter().flat_map(|j| STEPS.iter().map(move |s| (j, *s)));
    for (it, (job, step)) in items.iter_mut().zip(steps) {
        let v = verify_step(gh, repo, job, step, head);
        all_ok &= v.ok;
        it.verify = Some(v);
    }
    report(items, if all_ok { 0 } else { 1 })
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
        return Ok(report(vec![it], 0));
    }
    let mut jobs = Vec::new();
    for (pkg, tag) in todo {
        jobs.push(Job {
            assets: local_assets(&asset_files(pkg, assets)?)?,
            body: release_body(dir, pkg, &tag),
            pkg,
            tag,
        });
    }
    let token = github_token()?;
    let gh = Http::new(Service::Github, token.clone()).verbose(verbose);
    if dry_run_only {
        return dry_run(&gh, &repo, &jobs, &head);
    }
    let up = Http::new(Service::GithubUploads, token).verbose(verbose);
    Ok(execute(&gh, &up, &repo, &jobs, &head))
}
