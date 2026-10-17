//! The GitHub steps of `release publish`: tag and GitHub Release (run, dry run, verify).

use super::{Job, Step, item, report};
use crate::error::HoistError;
use crate::http::Http;
use crate::release::github_rel::{
    Done, ReleaseState, TagState, ensure_release, ensure_tag, expected_assets, release_state,
    tag_state, verify_release, verify_tag,
};
use crate::repo::git::GithubRepo;
use crate::report::{Action, Report, Verify};

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn count(n: usize) -> String {
    match n {
        0 => "no assets".to_string(),
        1 => "1 asset".to_string(),
        n => format!("{n} assets"),
    }
}

/// `--dry-run`: what each step would do, from GETs only (R17: exit 0).
pub(super) fn dry_run(
    gh: &Http,
    repo: &GithubRepo,
    jobs: &[Job],
    head: &str,
) -> Result<Report, HoistError> {
    let mut items = Vec::new();
    for job in jobs {
        let (action, why) = match tag_state(gh, repo, &job.tag)? {
            TagState::Absent => (Action::Create, format!("would tag {}", short(head))),
            TagState::At(at) if at == head => (Action::Ok, format!("already at {}", short(head))),
            TagState::At(at) => (
                Action::Drift,
                format!("tag exists at another commit {at}; publish would stop here"),
            ),
        };
        items.push(item(job, Step::Tag, action, why));
        let expected = expected_assets(&job.assets);
        let (action, why) = match release_state(gh, repo, &job.tag, &expected)? {
            ReleaseState::Complete => (Action::Ok, "already published".to_string()),
            ReleaseState::PublishedMissing(_, lacking) => (
                Action::Update,
                format!("published; would upload {}", lacking.join(", ")),
            ),
            ReleaseState::Draft(_, lacking) => (
                Action::Update,
                format!(
                    "draft exists; would upload {} and publish it",
                    count(lacking.len())
                ),
            ),
            ReleaseState::Absent => (
                Action::Create,
                format!(
                    "would create a draft, upload {}, then publish it",
                    count(expected.len())
                ),
            ),
        };
        items.push(item(job, Step::Release, action, why));
    }
    Ok(report(items, 0))
}

pub(super) fn run_step(
    gh: &Http,
    up: &Http,
    repo: &GithubRepo,
    job: &Job,
    step: Step,
    head: &str,
) -> Result<String, HoistError> {
    match step {
        Step::Tag => Ok(match ensure_tag(gh, repo, &job.tag, head)? {
            Done::Already => format!("already at {}", short(head)),
            Done::Wrote => format!("created at {}", short(head)),
        }),
        Step::Release => {
            let n = count(expected_assets(&job.assets).len());
            Ok(
                match ensure_release(gh, up, repo, &job.tag, &job.body, &job.assets)? {
                    Done::Already => format!("already published with {n}"),
                    Done::Wrote => format!("published with {n}"),
                },
            )
        }
    }
}

pub(super) fn verify_step(
    gh: &Http,
    repo: &GithubRepo,
    job: &Job,
    step: Step,
    head: &str,
) -> Verify {
    let result = match step {
        Step::Tag => verify_tag(gh, repo, &job.tag, head),
        Step::Release => verify_release(gh, repo, &job.tag, &expected_assets(&job.assets)),
    };
    match result {
        Ok(detail) => Verify { ok: true, detail },
        Err(detail) => Verify { ok: false, detail },
    }
}
