//! The `release publish` preflight (Global Constraints, spec §9.4): checked before any request.

use crate::error::HoistError;
use crate::release::ReleaseModel;
use crate::repo::workflows::read_workflows;
use std::path::Path;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn refuse(message: String) -> HoistError {
    HoistError::Usage(message)
}

/// The release job's file name from `GITHUB_WORKFLOW_REF` (`<o>/<r>/.github/workflows/<f>@<ref>`).
fn workflow_file(workflow_ref: &str) -> Option<&str> {
    let (_, rest) = workflow_ref.split_once("/.github/workflows/")?;
    let file = rest.split_once('@').map_or(rest, |(f, _)| f);
    (!file.is_empty() && !file.contains('/')).then_some(file)
}

/// First preflight condition, checked before anything is read: running in GitHub Actions.
pub(super) fn require_actions() -> Result<(), HoistError> {
    if env("GITHUB_ACTIONS").as_deref() == Some("true") {
        return Ok(());
    }
    Err(refuse(
        "`hoist release publish` runs only in GitHub Actions (GITHUB_ACTIONS=true is not set); \
         run it from the release workflow, or use `--dry-run` to preview"
            .to_string(),
    ))
}

/// The other preflight conditions (Global Constraints): on the default branch, at
/// `GITHUB_SHA`, in the release environment. Sends no request.
pub(super) fn preflight(
    dir: &Path,
    default: &str,
    model: &ReleaseModel,
    head: &str,
) -> Result<(), HoistError> {
    let want = format!("refs/heads/{default}");
    let got = env("GITHUB_REF").unwrap_or_default();
    if got != want {
        return Err(refuse(format!(
            "GITHUB_REF is `{got}`, not `{want}`: publish runs only on the default branch \
             (a push to `{default}` or a manual run on it)"
        )));
    }
    match env("GITHUB_SHA") {
        Some(sha) if sha == head => {}
        sha => {
            return Err(refuse(format!(
                "HEAD is {head} but GITHUB_SHA is {}: publish must run on the commit the \
                 workflow was started for (actions/checkout without `ref:`, fetch-depth: 0)",
                sha.as_deref().unwrap_or("not set")
            )));
        }
    }
    let (Some(wref), Some(job)) = (env("GITHUB_WORKFLOW_REF"), env("GITHUB_JOB")) else {
        return Err(refuse(
            "GITHUB_WORKFLOW_REF and GITHUB_JOB are not set: publish must run as a workflow job \
             in the release environment"
                .to_string(),
        ));
    };
    let Some(file) = workflow_file(&wref) else {
        return Err(refuse(format!(
            "GITHUB_WORKFLOW_REF `{wref}` names no file under .github/workflows"
        )));
    };
    let envs = read_workflows(dir)?.job_environments;
    let declared = envs.iter().find(|j| j.file == file && j.job == job);
    let want_env = &model.environment;
    match declared {
        Some(j) if &j.environment == want_env => Ok(()),
        Some(j) => Err(refuse(format!(
            "job `{job}` in .github/workflows/{file} runs in environment `{}`, not `{want_env}` \
             ([release] environment): publish runs only in the release environment",
            j.environment
        ))),
        None => Err(refuse(format!(
            "job `{job}` in .github/workflows/{file} declares no environment: add \
             `environment: {want_env}` to it ([release] environment)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_file_from_ref() {
        let r = "acme/x/.github/workflows/release.yml@refs/heads/main";
        assert_eq!(workflow_file(r), Some("release.yml"));
        assert_eq!(
            workflow_file("acme/x/.github/workflows/r.yaml@refs/heads/a@b"),
            Some("r.yaml")
        );
        assert_eq!(workflow_file("acme/x/release.yml@main"), None);
    }
}
