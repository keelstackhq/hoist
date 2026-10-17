//! `hoist release prepare`: open, update or close the release PR on `hoist/release` (spec §9.2).

use crate::auth::github_token;
use crate::config::read_pins;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, Service, seg};
use crate::release::gitlog::read_history;
use crate::release::planner::plan_versions;
use crate::release::read_release_model;
use crate::release::tools::{Tool, run};
use crate::repo::git::{GithubRepo, read_repo};
use crate::report::{Action, Item, Report};
use serde_json::json;
use std::path::Path;

mod files;
use files::{Prepared, prepare_files};

const BRANCH: &str = "hoist/release";
const BRANCH_REF: &str = "refs/heads/hoist/release";
const PUSH_REFSPEC: &str = "HEAD:refs/heads/hoist/release";
const COMMAND: &str = "release prepare";
/// The identity of commits when the checkout has none (a fresh Actions runner).
const BOT_NAME: &str = "user.name=github-actions[bot]";
const BOT_EMAIL: &str = "user.email=41898282+github-actions[bot]@users.noreply.github.com";

fn git(dir: &Path, args: &[&str]) -> Result<String, HoistError> {
    run(Tool::Git, dir, args, &[]).map_err(HoistError::from)
}

/// A clean working tree (no untracked files either) and HEAD on the default branch.
fn preflight(dir: &Path, default: &str) -> Result<(), HoistError> {
    let status = git(dir, &["status", "--porcelain"])?;
    let dirty = status.lines().filter(|l| !l.trim().is_empty()).count();
    if dirty > 0 {
        return Err(HoistError::Usage(format!(
            "the working tree is not clean ({dirty} changed or untracked files); commit or \
             stash them before `hoist release prepare`"
        )));
    }
    let head = git(dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .map(|h| h.trim().to_string())
        .unwrap_or_default();
    if head != default {
        let at = if head.is_empty() {
            "a detached HEAD".to_string()
        } else {
            format!("`{head}`")
        };
        return Err(HoistError::Usage(format!(
            "HEAD is on {at}, not the default branch `{default}`; check out `{default}` first \
             (`hoist release prepare` runs on pushes to the default branch)"
        )));
    }
    Ok(())
}

/// The number of the open PR from `hoist/release`, if any.
fn open_pr(gh: &Http, repo: &GithubRepo) -> Result<Option<u64>, HoistError> {
    let path = format!(
        "/repos/{}/{}/pulls?head={}:{BRANCH}&state=open",
        seg(&repo.owner),
        seg(&repo.name),
        seg(&repo.owner)
    );
    match gh.get_json(&path)? {
        Fetched::Ok(v) => Ok(v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|p| p.get("number"))
            .and_then(|n| n.as_u64())),
        Fetched::NotFound => Err(HoistError::Network(format!(
            "GitHub: repository {repo} not found (404); check `origin` and that the token can see it"
        ))),
        Fetched::Forbidden(m) => Err(HoistError::Network(format!(
            "GitHub: cannot read the pull requests of {repo}: {m}; the token needs \
             Pull requests: read and write"
        ))),
    }
}

fn pulls_path(repo: &GithubRepo) -> String {
    format!("/repos/{}/{}/pulls", seg(&repo.owner), seg(&repo.name))
}

fn pr_item(action: Action, reason: String) -> Item {
    Item {
        area: "release".to_string(),
        kind: "pr".to_string(),
        key: BRANCH.to_string(),
        action,
        reason,
        source: "default".to_string(),
        verify: None,
    }
}

fn step_item(key: &str, action: Action, reason: String) -> Item {
    Item {
        area: "release".to_string(),
        kind: "step".to_string(),
        key: key.to_string(),
        action,
        reason,
        source: "default".to_string(),
        verify: None,
    }
}

fn report(items: Vec<Item>, exit_code: i32) -> Report {
    Report {
        command: COMMAND.to_string(),
        items,
        human_steps: vec![],
        exit_code,
        text: None,
    }
}

/// Nothing releases: close the open release PR and delete its branch.
fn close(gh: &Http, repo: &GithubRepo, dir: &Path, pr: Option<u64>) -> Report {
    let Some(n) = pr else {
        return report(
            vec![pr_item(
                Action::Ok,
                "nothing to release; no open release PR".to_string(),
            )],
            0,
        );
    };
    let path = format!("{}/{n}", pulls_path(repo));
    if let Err(e) = gh.send_json(Method::Patch, &path, Some(&json!({"state": "closed"}))) {
        let reason = format!("nothing to release; closing #{n} failed: {e}");
        return report(vec![pr_item(Action::Update, reason)], 1);
    }
    match git(dir, &["push", "-q", "origin", "--delete", BRANCH_REF]) {
        Ok(out) => {
            eprint!("{out}");
            let reason = format!("nothing to release; closed #{n} and deleted `{BRANCH}`");
            report(vec![pr_item(Action::Ok, reason)], 0)
        }
        Err(e) => {
            let reason = format!(
                "nothing to release; closed #{n}, deleting the branch failed: {e}; \
                 run `git push origin --delete {BRANCH}`"
            );
            report(vec![pr_item(Action::Update, reason)], 1)
        }
    }
}

const STEPS: [&str; 5] = ["branch", "files", "lockfiles", "commit", "push"];

fn echo(out: String) {
    eprint!("{out}");
}

fn identity(dir: &Path) -> Vec<&'static str> {
    let configured = git(dir, &["config", "user.email"]).is_ok_and(|e| !e.trim().is_empty());
    if configured {
        vec![]
    } else {
        vec!["-c", BOT_NAME, "-c", BOT_EMAIL]
    }
}

/// One step of building and pushing the branch.
fn step(dir: &Path, index: usize, p: &Prepared) -> Result<String, HoistError> {
    match STEPS[index] {
        "branch" => {
            git(dir, &["checkout", "-q", "-B", BRANCH])?;
            Ok(format!("`{BRANCH}` created from HEAD"))
        }
        "files" => {
            for (rel, text) in &p.files {
                std::fs::write(dir.join(rel), text)
                    .map_err(|e| HoistError::Usage(format!("cannot write {rel}: {e}")))?;
            }
            Ok(format!("{} files edited", p.files.len()))
        }
        "lockfiles" => {
            let mut ran = Vec::new();
            if p.npm {
                echo(run(Tool::Pnpm, dir, &["install", "--lockfile-only"], &[])?);
                ran.push("`pnpm install --lockfile-only`");
            }
            if p.cargo {
                echo(run(Tool::Cargo, dir, &["update", "-w"], &[])?);
                ran.push("`cargo update -w`");
            }
            Ok(format!("ran {}", ran.join(" and ")))
        }
        "commit" => {
            git(dir, &["add", "-A"])?;
            let mut args = identity(dir);
            args.extend(["commit", "-q", "-m", &p.title]);
            echo(git(dir, &args)?);
            Ok(format!("committed `{}`", p.title))
        }
        _ => {
            echo(git(
                dir,
                &["push", "-q", "--force", "origin", PUSH_REFSPEC],
            )?);
            Ok(format!("pushed `{BRANCH}` to origin"))
        }
    }
}

/// Back to the default branch. After a failure, discard the half-made edits too.
fn restore(dir: &Path, default: &str, p: &Prepared, failed: bool) -> Result<(), HoistError> {
    if failed {
        git(dir, &["checkout", "-q", "-f", default])?;
        for rel in &p.created {
            let path = dir.join(rel);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| HoistError::Usage(format!("cannot remove {rel}: {e}")))?;
            }
        }
        Ok(())
    } else {
        git(dir, &["checkout", "-q", default]).map(|_| ())
    }
}

/// Build and push the branch, then restore. `Ok` holds the step items (all `ok`, restore
/// included); `Err` is the stopped report (exit 1).
fn write_branch(dir: &Path, default: &str, p: &Prepared, pr: Action) -> Result<Vec<Item>, Report> {
    let mut items = Vec::new();
    let mut failed = false;
    for (index, name) in STEPS.iter().enumerate() {
        if failed {
            items.push(step_item(name, Action::Update, "not attempted".to_string()));
            continue;
        }
        match step(dir, index, p) {
            Ok(done) => items.push(step_item(name, Action::Ok, done)),
            Err(e) => {
                failed = true;
                items.push(step_item(name, Action::Update, format!("failed: {e}")));
            }
        }
    }
    let restored = restore(dir, default, p, failed);
    let restored_ok = restored.is_ok();
    items.push(match restored {
        Ok(()) => step_item("restore", Action::Ok, format!("back on `{default}`")),
        Err(e) => step_item(
            "restore",
            Action::Unknown,
            format!("failed: {e}; run `git checkout -f {default}`"),
        ),
    });
    if !failed && restored_ok {
        return Ok(items);
    }
    items.push(pr_item(pr, "not attempted".to_string()));
    Err(report(items, 1))
}

/// Create the PR or update its title and body. Returns its number.
fn upsert_pr(
    gh: &Http,
    repo: &GithubRepo,
    default: &str,
    p: &Prepared,
    pr: Option<u64>,
) -> Result<u64, HoistError> {
    match pr {
        Some(n) => {
            let body = json!({"title": p.title, "body": p.body});
            gh.send_json(
                Method::Patch,
                &format!("{}/{n}", pulls_path(repo)),
                Some(&body),
            )?;
            Ok(n)
        }
        None => {
            let body = json!({"title": p.title, "head": BRANCH, "base": default, "body": p.body});
            let written = gh.send_json(Method::Post, &pulls_path(repo), Some(&body))?;
            written
                .body
                .as_ref()
                .and_then(|b| b.get("number"))
                .and_then(|n| n.as_u64())
                .ok_or_else(|| {
                    HoistError::Network(
                        "GitHub created the pull request but returned no number".to_string(),
                    )
                })
        }
    }
}

/// Open, update or close the release PR. Exit 3 before the first write, 1 when a write fails.
pub fn release_prepare(dir: &Path, verbose: bool) -> Result<Report, HoistError> {
    let (repo, default) = read_repo(dir)?;
    preflight(dir, &default)?;
    let token = github_token()?;
    let model = read_release_model(dir)?;
    let history = read_history(dir, &model)?;
    let pins = read_pins(dir)?;
    let next = plan_versions(&model, &history, &pins)?;
    let prepared = if next.is_empty() {
        None
    } else {
        Some(prepare_files(dir, &model, &next)?)
    };
    let gh = Http::new(Service::Github, token).verbose(verbose);
    let pr = open_pr(&gh, &repo)?;
    let Some(prepared) = prepared else {
        return Ok(close(&gh, &repo, dir, pr));
    };
    let pr_action = if pr.is_some() {
        Action::Update
    } else {
        Action::Create
    };
    let steps = match write_branch(dir, &default, &prepared, pr_action) {
        Ok(steps) => steps,
        Err(stopped) => return Ok(stopped),
    };
    let n = match upsert_pr(&gh, &repo, &default, &prepared, pr) {
        Ok(n) => n,
        Err(e) => {
            let mut items = steps;
            items.push(pr_item(pr_action, format!("failed: {e}")));
            return Ok(report(items, 1));
        }
    };
    let mut items: Vec<Item> = next
        .iter()
        .filter_map(|nv| {
            let p = model.packages.iter().find(|p| p.name == nv.package)?;
            Some(Item {
                area: "release".to_string(),
                kind: "version".to_string(),
                key: p.name.clone(),
                action: Action::Update,
                reason: format!("in release PR #{n}"),
                source: p.version.source.clone(),
                verify: None,
            })
        })
        .collect();
    items.sort_by(|a, b| a.key.cmp(&b.key));
    let verb = if pr.is_some() { "updated" } else { "opened" };
    items.push(pr_item(
        pr_action,
        format!("{verb} #{n}: {}", prepared.title),
    ));
    Ok(report(items, 0))
}
