//! `hoist init`: write a thin `.github/workflows/release.yml` that calls the pinned release
//! action (spec §12.5). Idempotent; a hand-edited file is never overwritten without `--force`.

use crate::auth::github_token;
use crate::desired::build_desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Service};
use crate::report::{Action, HumanStep, Item, Report};
use sha2::{Digest, Sha256};
use std::path::Path;

const FILE: &str = ".github/workflows/release.yml";
const CI_FILE: &str = ".github/workflows/ci.yml";
const MARK: &str = "# hoist:managed sha256=";
/// `actions/checkout` v4.4.0, the pin hoist's own CI uses.
const CHECKOUT: &str = "actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0";
const ACTION_REPO: &str = "keelstackhq/hoist";

/// Refuse a branch or environment name that could break out of the YAML: letters, digits and
/// `. _ - /` only (branch: also no `..`, no leading `-` or `/`, no trailing `/`; env: no `/`,
/// at most 255 chars).
fn check_names(branch: &str, env: &str) -> Result<(), HoistError> {
    let word = |s: &str, extra: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
    };
    if !word(branch, "/")
        || branch.contains("..")
        || branch.starts_with(['-', '/'])
        || branch.ends_with('/')
    {
        return Err(HoistError::Usage(format!(
            "default branch `{branch}` has characters hoist will not write into a workflow \
             (use letters, digits and `. _ - /`)"
        )));
    }
    if !word(env, "") || env.len() > 255 {
        return Err(HoistError::Usage(format!(
            "[release] environment `{}` must be 1-255 characters of letters, digits and `. _ -`",
            env.escape_debug()
        )));
    }
    Ok(())
}

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
    branches: ["{branch}"]
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
    environment: "{env}"
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

/// The `check`/`plan` item for a release.yml that hoist wrote: `ok` when untouched and pinned to
/// this hoist's version, `drift` when edited since or pinned to another version. A file without
/// the marker is the repo's own: no item.
pub fn file_item(dir: &Path) -> Option<Item> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    let (first, body) = text.split_once('\n')?;
    let wrote = first.strip_prefix(MARK)?;
    let (action, reason) = if wrote != digest(body) {
        (
            Action::Drift,
            "edited since hoist wrote it; `hoist init --force` rewrites it".to_string(),
        )
    } else {
        let pinned = body
            .lines()
            .find_map(|l| l.split_once("/release@")?.1.split_once(" # v").map(|x| x.1))
            .unwrap_or("?");
        if pinned == env!("CARGO_PKG_VERSION") {
            (Action::Ok, "written by hoist init".to_string())
        } else {
            (
                Action::Drift,
                format!("pinned to hoist v{pinned}; `hoist init` updates it"),
            )
        }
    };
    Some(Item {
        area: "release".into(),
        kind: "workflow-file".into(),
        key: FILE.into(),
        action,
        reason,
        source: FILE.into(),
        verify: None,
    })
}

/// Write `rel` unless a hand-edited file is there (`force` replaces it): the item for it.
fn sync_file(
    dir: &Path,
    rel: &str,
    force: bool,
    dry_run: bool,
    make: impl FnOnce() -> Result<String, HoistError>,
) -> Result<(Action, String), HoistError> {
    let path = dir.join(rel);
    let current = std::fs::read_to_string(&path).ok();
    let st = state(current.as_deref());
    if st == State::Edited && !force {
        return Ok((
            Action::Ok,
            "edited by hand: left alone (--force replaces it)".to_string(),
        ));
    }
    let text = make()?;
    if current.as_deref() == Some(text.as_str()) {
        return Ok((Action::Ok, "up to date".to_string()));
    }
    if !dry_run {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        std::fs::write(&path, text)
            .map_err(|e| HoistError::Usage(format!("cannot write {rel}: {e}")))?;
    }
    let (action, verb) = if st == State::Missing {
        (Action::Create, "create")
    } else {
        (Action::Update, "update")
    };
    let done = if dry_run { "would " } else { "" };
    Ok((
        action,
        format!("{done}{verb} {rel}, then commit and push it"),
    ))
}

/// The test commands of the repo's root toolchains (Cargo and/or npm), one CI step each, for
/// the generated `ci-ok` job. `Err` says why hoist cannot pick them: then the owner writes the gate.
fn test_commands(dir: &Path) -> Result<Vec<&'static str>, &'static str> {
    let mut cmds = Vec::new();
    if dir.join("Cargo.toml").is_file() {
        cmds.push(if dir.join("Cargo.lock").is_file() {
            "cargo test --locked"
        } else {
            "cargo test"
        });
    }
    if let Ok(pkg) = std::fs::read_to_string(dir.join("package.json")) {
        let pkg: serde_json::Value = serde_json::from_str(&pkg).unwrap_or_default();
        if let Some(t) = pkg["scripts"]["test"].as_str() {
            // `npm init` writes this script; it always fails.
            if t.contains("no test specified") {
                return Err("package.json `scripts.test` is the npm placeholder (it always fails)");
            }
            // `npm ci` needs a lockfile; without one the repo's own workflow must say how.
            if !dir.join("package-lock.json").is_file() {
                return Err("package.json has a test script but there is no package-lock.json");
            }
            cmds.push("npm ci && npm test");
        }
    }
    if cmds.is_empty() {
        Err("no Cargo.toml or npm test script found")
    } else {
        Ok(cmds)
    }
}

/// A workflow file hoist did not write.
fn has_other_workflow(dir: &Path) -> bool {
    std::fs::read_dir(dir.join(".github/workflows")).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            (n.ends_with(".yml") || n.ends_with(".yaml")) && n != "release.yml"
        })
    })
}

fn ci_text(branch: &str, cmds: &[&str]) -> String {
    let steps: String = cmds.iter().map(|c| format!("      - run: {c}\n")).collect();
    let body = format!(
        r#"name: ci
on:
  pull_request:
  merge_group:
  push:
    branches: ["{branch}"]
permissions:
  contents: read
jobs:
  ci-ok:
    runs-on: ubuntu-latest
    steps:
      - uses: {CHECKOUT}
        with:
          persist-credentials: false
{steps}"#
    );
    format!("{MARK}{}\n{body}", digest(&body))
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
    check_names(&d.default_branch, &r.environment.value)?;
    let item = |kind: &str, key: &str, action, reason: String, source: &str| Item {
        area: if kind == "workflow-file" {
            "release"
        } else {
            "github"
        }
        .into(),
        kind: kind.into(),
        key: key.into(),
        action,
        reason,
        source: source.into(),
        verify: None,
    };
    let (action, reason) = sync_file(dir, FILE, force, dry_run, || {
        Ok(render(
            &d.default_branch,
            &r.environment.value,
            &action_sha(sha)?,
            env!("CARGO_PKG_VERSION"),
        ))
    })?;
    let mut items = vec![item(
        "workflow-file",
        FILE,
        action,
        reason,
        "hoist.toml and the repo's packages",
    )];
    let mut human_steps = Vec::new();
    // No `ci-ok` job (or other required check) anywhere: the default branch has no gate. A
    // generated `ci.yml` is the gate when nothing else exists; else the repo's own workflow
    // must get the job, which hoist does not edit.
    let ci_managed = std::fs::read_to_string(dir.join(CI_FILE)).is_ok_and(|t| t.starts_with(MARK));
    // `required_checks = []` in hoist.toml is the owner's choice of no gate: leave it be.
    let opted_out = crate::config::load_root(dir)?
        .github
        .required_checks
        .is_some_and(|l| l.is_empty());
    let pending = |items: &mut Vec<Item>, steps: &mut Vec<HumanStep>, id: &str, title: String| {
        items.push(item(
            "ci-ok-job",
            id,
            Action::Pending,
            title.clone(),
            "no single job named ci-ok in .github/workflows",
        ));
        steps.push(HumanStep {
            id: id.into(),
            title,
            url: String::new(),
            status: "pending".into(),
        });
    };
    // A `ci-ok` job in a workflow other than the one hoist wrote: two checks with one name.
    let foreign_ci_ok = crate::repo::workflows::files_defining(dir, "ci-ok")?
        .into_iter()
        .find(|f| f != CI_FILE);
    if let (true, Some(src)) = (ci_managed, foreign_ci_ok) {
        // hoist stops managing ci.yml (never rewritten, not even with --force).
        pending(
            &mut items,
            &mut human_steps,
            "ci-ok-duplicate",
            format!(
                "`ci-ok` is defined in {src} and in {CI_FILE} (written by hoist): two checks \
                 share the name. Delete {CI_FILE}; hoist no longer manages it"
            ),
        );
    } else if ci_managed || (d.github.ruleset.required_checks.is_empty() && !opted_out) {
        match test_commands(dir) {
            Ok(cmds) if ci_managed || !has_other_workflow(dir) => {
                let (action, reason) = sync_file(dir, CI_FILE, force, dry_run, || {
                    Ok(ci_text(&d.default_branch, &cmds))
                })?;
                items.push(item(
                    "ci-ok-job",
                    CI_FILE,
                    action,
                    reason,
                    "the repo's toolchain",
                ));
            }
            why => {
                let why = match why {
                    Err(w) => format!(" ({w})"),
                    Ok(_) => String::new(),
                };
                pending(
                    &mut items,
                    &mut human_steps,
                    "ci-ok",
                    format!(
                        "add a job named `ci-ok` to a workflow that runs on pull_request, with \
                         `needs:` listing every test job (or list your checks in hoist.toml \
                         [github] required_checks); the default branch has no required check{why}"
                    ),
                );
            }
        }
    }
    let exit_code = if human_steps.is_empty() { 0 } else { 2 };
    Ok(Report {
        command: "init".to_string(),
        items,
        human_steps,
        exit_code,
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

#[cfg(test)]
mod value_tests {
    use super::*;

    #[test]
    fn names_with_yaml_specials_are_refused() {
        assert!(check_names("main", "release").is_ok());
        assert!(check_names("feat/x-1.2", "prod_env").is_ok());
        for bad in ["a#b", "a]b", "a b", "a\nb", "a,b", "", "-x", "a..b", "a:b"] {
            assert!(check_names(bad, "release").is_err(), "{bad:?}");
        }
        for bad in ["release\nsteps: x", "a b", "", "a#b", &"x".repeat(256)] {
            assert!(check_names("main", bad).is_err(), "{bad:?}");
        }
    }
}
