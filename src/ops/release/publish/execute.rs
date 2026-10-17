//! Running `release publish`'s steps: the dry run, and the run that stops at the first failure
//! and then verifies (spec §9.3, §9.5).

use super::registries::{self, Outcome};
use super::{Clients, Job, Step, github, item, item_of, report};
use crate::error::HoistError;
use crate::repo::git::GithubRepo;
use crate::report::{Action, HumanStep, Report};
use std::path::Path;

fn add_step(steps: &mut Vec<HumanStep>, step: HumanStep) {
    if !steps.iter().any(|s| s.id == step.id) {
        steps.push(step);
    }
}

/// `--dry-run`: every step from GETs only (R17: exit 0).
pub(super) fn dry_run(
    c: &Clients,
    repo: &GithubRepo,
    jobs: &[Job],
    head: &str,
) -> Result<Report, HoistError> {
    let mut items = Vec::new();
    let mut humans = Vec::new();
    for job in jobs {
        for step in job.steps() {
            let (action, why) = if step.github() {
                github::dry_run(&c.gh, repo, job, step, head)?
            } else {
                let (action, why, human) = registries::dry_run(c, job, step)?;
                if let Some(h) = human {
                    add_step(&mut humans, h);
                }
                (action, why)
            };
            items.push(item(job, step, action, why));
        }
    }
    Ok(report(items, humans, 0))
}

fn run_step(
    c: &Clients,
    dir: &Path,
    repo: &GithubRepo,
    job: &Job,
    step: Step,
    head: &str,
    warnings: &mut Vec<String>,
) -> Result<Outcome, HoistError> {
    if step.github() {
        github::run_step(&c.gh, &c.up, repo, job, step, head).map(Outcome::Done)
    } else {
        registries::run_step(c, dir, job, step, warnings)
    }
}

/// Every step in order; the first failure stops the run (exit 1), a package waiting on a
/// person (first publish) does not. Then verify each step that ran; human steps alone exit 2.
pub(super) fn execute(
    c: &Clients,
    dir: &Path,
    repo: &GithubRepo,
    jobs: &[Job],
    head: &str,
) -> Report {
    let mut items = Vec::new();
    let mut humans = Vec::new();
    let mut done = Vec::new(); // (item index, job, step) to verify
    let mut failed = false;
    for job in jobs {
        for step in job.steps() {
            if failed {
                items.push(item(job, step, Action::Update, "not attempted".to_string()));
                continue;
            }
            let mut warnings = Vec::new();
            match run_step(c, dir, repo, job, step, head, &mut warnings) {
                Ok(Outcome::Done(why)) => {
                    done.push((items.len(), job, step));
                    items.push(item(job, step, Action::Ok, why));
                }
                Ok(Outcome::Pending(why, human)) => {
                    add_step(&mut humans, human);
                    items.push(item(job, step, Action::Pending, why));
                }
                Err(e) => {
                    failed = true;
                    items.push(item(job, step, Action::Update, format!("failed: {e}")));
                }
            }
            for w in warnings {
                items.push(item_of(job, "crates_token", Action::Unknown, w));
            }
        }
    }
    if failed {
        return report(items, humans, 1);
    }
    let mut all_ok = true;
    for (index, job, step) in done {
        let v = match step {
            Step::Tag | Step::Release => github::verify_step(&c.gh, repo, job, step, head),
            Step::Npm | Step::Crates | Step::Platform(_) => registries::verify(c, job, step),
            Step::Deploy => continue,
        };
        all_ok &= v.ok;
        items[index].verify = Some(v);
    }
    let code = match (all_ok, humans.is_empty()) {
        (false, _) => 1,
        (true, false) => 2,
        (true, true) => 0,
    };
    report(items, humans, code)
}
