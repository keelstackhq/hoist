//! `hoist init`: write a thin `.github/workflows/release.yml` that calls the pinned release
//! action (spec §12.5). Idempotent; a hand-edited file is never overwritten without `--force`.

use crate::auth::github_token;
use crate::desired::build_desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Service};
use crate::report::{Action, Item, Report};
use sha2::{Digest, Sha256};
use std::path::Path;

const FILE: &str = ".github/workflows/release.yml";
const MARK: &str = "# hoist:managed sha256=";
/// `actions/checkout` v4.4.0, the pin hoist's own CI uses.
const CHECKOUT: &str = "actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0";
const ACTION_REPO: &str = "keelstackhq/hoist";

fn digest(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The managed file: the marker line (a hash of everything after it), then the body.
fn render(branch: &str, env: &str, sha: &str, version: &str) -> String {
    let body = format!(
        r#"name: release
on:
  push:
    branches: [{branch}]
  workflow_dispatch:
permissions:
  contents: read
concurrency:
  group: release
  cancel-in-progress: false
jobs:
  detect:
    runs-on: ubuntu-latest
    outputs:
      pending: ${{{{ steps.hoist.outputs.pending }}}}
    steps:
      - uses: {CHECKOUT}
        with:
          fetch-depth: 0
          persist-credentials: false
      - id: hoist
        uses: {ACTION_REPO}/release@{sha} # v{version}
        with:
          command: detect
  publish:
    needs: detect
    if: needs.detect.outputs.pending == 'true'
    runs-on: ubuntu-latest
    environment: {env}
    permissions:
      contents: write
      id-token: write
    steps:
      - uses: {CHECKOUT}
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: {ACTION_REPO}/release@{sha} # v{version}
        with:
          command: publish
  prepare:
    needs: [detect, publish]
    if: always() && needs.detect.result == 'success' && needs.publish.result != 'failure'
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: {CHECKOUT}
        with:
          fetch-depth: 0
      - uses: {ACTION_REPO}/release@{sha} # v{version}
        with:
          command: prepare
          token: ${{{{ secrets.HOIST_RELEASE_TOKEN || github.token }}}}
          app-id: ${{{{ vars.HOIST_RELEASE_APP_ID }}}}
          app-key: ${{{{ secrets.HOIST_RELEASE_APP_KEY }}}}
"#
    );
    format!("{MARK}{}\n{body}", digest(&body))
}

#[derive(Debug, PartialEq)]
enum State {
    Missing,
    /// Written by hoist and untouched.
    Managed,
    /// Has the marker but the body changed, or has none.
    Edited,
}

fn state(text: Option<&str>) -> State {
    let Some(text) = text else {
        return State::Missing;
    };
    match text.split_once('\n') {
        Some((first, body)) if first.strip_prefix(MARK) == Some(&digest(body)) => State::Managed,
        _ => State::Edited,
    }
}

fn valid_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The commit of hoist's release tag `v<version>` (one GET), unless `--sha` gave it.
fn action_sha(given: Option<&str>) -> Result<String, HoistError> {
    if let Some(s) = given {
        let s = s.to_ascii_lowercase();
        return if valid_sha(&s) {
            Ok(s)
        } else {
            Err(HoistError::Usage(
                "--sha must be a full 40-character commit SHA".into(),
            ))
        };
    }
    let v = env!("CARGO_PKG_VERSION");
    let http = Http::new(Service::Github, github_token()?);
    let path = format!("/repos/{ACTION_REPO}/commits/v{v}");
    let fail = |why: &str| {
        HoistError::Usage(format!(
            "cannot resolve tag v{v} of {ACTION_REPO} ({why}); pass --sha <full commit SHA of that tag>"
        ))
    };
    match http.get_json(&path).map_err(|e| fail(&e.to_string()))? {
        Fetched::Ok(j) => j["sha"]
            .as_str()
            .filter(|s| valid_sha(s))
            .map(str::to_string)
            .ok_or_else(|| fail("no sha in the answer")),
        _ => Err(fail("not found or forbidden")),
    }
}

pub fn init(
    dir: &Path,
    sha: Option<&str>,
    force: bool,
    dry_run: bool,
) -> Result<Report, HoistError> {
    let mut d = build_desired(dir)?;
    d.read_release(dir)?;
    let Some(r) = &d.release else {
        return Err(HoistError::Usage(
            "no publishable package found; nothing to write (see `hoist explain`)".into(),
        ));
    };
    if r.packages.iter().any(|p| p.launcher.is_some()) {
        return Err(HoistError::Usage(
            "a launcher package needs build steps hoist cannot generate; write release.yml by \
             hand with the release action (README \"The release action\")"
                .into(),
        ));
    }
    let path = dir.join(FILE);
    let current = std::fs::read_to_string(&path).ok();
    let st = state(current.as_deref());
    let (action, reason) = match st {
        State::Edited if !force => (
            Action::Ok,
            "edited by hand: left alone (--force replaces it)".to_string(),
        ),
        _ => {
            let sha = action_sha(sha)?;
            let text = render(
                &d.default_branch,
                &r.environment.value,
                &sha,
                env!("CARGO_PKG_VERSION"),
            );
            if current.as_deref() == Some(text.as_str()) {
                (Action::Ok, "up to date".to_string())
            } else {
                if !dry_run {
                    if let Some(p) = path.parent() {
                        std::fs::create_dir_all(p).ok();
                    }
                    std::fs::write(&path, text)
                        .map_err(|e| HoistError::Usage(format!("cannot write {FILE}: {e}")))?;
                }
                let verb = if st == State::Missing {
                    "create"
                } else {
                    "update"
                };
                let done = if dry_run { "would " } else { "" };
                (
                    if st == State::Missing {
                        Action::Create
                    } else {
                        Action::Update
                    },
                    format!("{done}{verb} {FILE}, then commit and push it"),
                )
            }
        }
    };
    Ok(Report {
        command: "init".to_string(),
        items: vec![Item {
            area: "release".into(),
            kind: "workflow-file".into(),
            key: FILE.into(),
            action,
            reason,
            source: "hoist.toml and the repo's packages".into(),
            verify: None,
        }],
        human_steps: Vec::new(),
        exit_code: 0,
        text: None,
        hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_until_edited() {
        let t = render("main", "release", &"a".repeat(40), "1.2.3");
        assert_eq!(state(Some(&t)), State::Managed);
        assert_eq!(state(Some(&t.replace("main", "dev"))), State::Edited);
        assert_eq!(state(Some("name: x\n")), State::Edited);
        assert_eq!(state(None), State::Missing);
    }

    #[test]
    fn every_uses_is_pinned() {
        let t = render("main", "release", &"b".repeat(40), "1.2.3");
        for l in t.lines().filter(|l| l.trim_start().contains("uses:")) {
            assert!(l.split('@').nth(1).is_some_and(|r| r.len() > 40), "{l}");
        }
    }
}
