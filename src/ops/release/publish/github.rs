//! The GitHub steps of `release publish`: tag and GitHub Release (run, dry run, verify).

use super::{Job, Step};
use crate::error::HoistError;
use crate::http::Http;
use crate::release::github_rel::{
    Done, ReleaseState, TagState, ensure_release, ensure_tag, release_state, tag_state,
    verify_release, verify_tag,
};
use crate::repo::git::GithubRepo;
use crate::report::{Action, Verify};

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

/// `--dry-run`: what a GitHub step would do, from GETs only.
pub(super) fn dry_run(
    gh: &Http,
    repo: &GithubRepo,
    job: &Job,
    step: Step,
    head: &str,
) -> Result<(Action, String), HoistError> {
    if step == Step::Tag {
        return Ok(match tag_state(gh, repo, &job.tag)? {
            TagState::Absent => (Action::Create, format!("would tag {}", short(head))),
            TagState::At(at) if at == head => (Action::Ok, format!("already at {}", short(head))),
            TagState::At(at) => (
                Action::Drift,
                format!("tag exists at another commit {at}; publish would stop here"),
            ),
        });
    }
    Ok(match release_state(gh, repo, &job.tag, &job.assets)? {
        ReleaseState::Complete => (Action::Ok, "already published".to_string()),
        ReleaseState::Published(problem) => (
            Action::Drift,
            format!(
                "published release {} {problem}; releases are immutable, publish would stop \
                 here",
                job.tag
            ),
        ),
        ReleaseState::Draft(_, fixes) => (
            Action::Update,
            format!(
                "draft exists; would upload {} and publish it",
                count(fixes.len())
            ),
        ),
        ReleaseState::Absent => (
            Action::Create,
            format!(
                "would create a draft, upload {}, then publish it",
                count(job.assets.len())
            ),
        ),
    })
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
        _ => {
            let n = count(job.assets.len());
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
        _ => verify_release(gh, repo, &job.tag, &job.assets),
    };
    match result {
        Ok(detail) => Verify { ok: true, detail },
        Err(detail) => Verify { ok: false, detail },
    }
}
