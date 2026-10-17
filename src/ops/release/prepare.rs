//! `hoist release prepare`: open, update or close the release PR on `hoist/release` (spec §9.2).

use crate::auth::github_token;
use crate::config::read_pins;
use crate::error::HoistError;
use crate::http::{Fetched, Http, Method, Service, seg};
use crate::release::edit::{
    prepend_changelog, set_cargo_dep_version, set_cargo_version, set_package_json_dep,
    set_package_json_version, today,
};
use crate::release::gitlog::read_history;
use crate::release::planner::{NextVersion, plan_versions};
use crate::release::tools::{Tool, run};
use crate::release::{Package, PkgKind, ReleaseModel, read_release_model};
use crate::repo::git::{GithubRepo, read_repo};
use crate::repo::targets::{config_err, join_rel};
use crate::report::{Action, Item, Report};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const BRANCH: &str = "hoist/release";
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

/// The release commit, prepared in memory before anything is written.
struct Prepared {
    /// Repo-relative path -> new content.
    files: BTreeMap<String, String>,
    /// Files that did not exist before (removed again when a run stops).
    created: Vec<String>,
    npm: bool,
    cargo: bool,
    title: String,
    body: String,
}

/// Files read once from disk, then edited in memory.
struct Files<'a> {
    root: &'a Path,
    map: BTreeMap<String, String>,
}

impl Files<'_> {
    fn get(&mut self, rel: &str) -> Result<String, HoistError> {
        if let Some(t) = self.map.get(rel) {
            return Ok(t.clone());
        }
        std::fs::read_to_string(self.root.join(rel))
            .map_err(|e| config_err(rel, format!("cannot read file: {e}")))
    }

    fn edit(
        &mut self,
        rel: &str,
        f: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<(), HoistError> {
        let text = self.get(rel)?;
        let new = f(&text).map_err(|m| config_err(rel, m))?;
        self.map.insert(rel.to_string(), new);
        Ok(())
    }
}

fn manifest(p: &Package) -> String {
    let dir = p.dir.to_string_lossy();
    match p.kind {
        PkgKind::Npm => join_rel(&dir, "package.json"),
        PkgKind::Crate => join_rel(&dir, "Cargo.toml"),
    }
}

/// `version.workspace = true` in the crate's own Cargo.toml.
fn inherits_version(files: &mut Files, p: &Package) -> Result<bool, HoistError> {
    let rel = manifest(p);
    let table: toml::Table = files
        .get(&rel)?
        .parse()
        .map_err(|e: toml::de::Error| config_err(&rel, e.to_string().trim_end().to_string()))?;
    Ok(table
        .get("package")
        .and_then(|p| p.get("version"))
        .is_some_and(|v| v.is_table()))
}

/// Set `[workspace.package] version` of the root Cargo.toml; comments and spacing stay.
fn set_workspace_version(text: &str, v: &str) -> Result<String, String> {
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("invalid TOML: {e}"))?;
    let item = doc
        .get_mut("workspace")
        .and_then(|w| w.get_mut("package"))
        .and_then(|p| p.get_mut("version"))
        .filter(|i| i.is_str())
        .ok_or_else(|| "no `[workspace.package] version` to bump".to_string())?;
    let decor = item.as_value().map(|o| o.decor().clone());
    let mut value = toml_edit::Value::from(v);
    if let Some(d) = decor {
        *value.decor_mut() = d;
    }
    *item = toml_edit::Item::Value(value);
    Ok(doc.to_string())
}

/// R15: an inherited version moves only when every crate inheriting it gets the same version.
fn bump_inherited(
    files: &mut Files,
    next: &[NextVersion],
    inheriting: &[&Package],
) -> Result<(), HoistError> {
    let targets: Vec<(&str, Option<String>)> = inheriting
        .iter()
        .map(|p| {
            let to = next
                .iter()
                .find(|n| n.package == p.name)
                .map(|n| n.to.to_string());
            (p.name.as_str(), to)
        })
        .collect();
    let distinct: BTreeSet<&Option<String>> = targets.iter().map(|(_, t)| t).collect();
    match distinct.into_iter().collect::<Vec<_>>().as_slice() {
        [] | [None] => Ok(()),
        [Some(v)] => files.edit("Cargo.toml", |t| set_workspace_version(t, v)),
        _ => {
            let list: Vec<String> = targets
                .iter()
                .map(|(n, t)| format!("{n} {}", t.as_deref().unwrap_or("unchanged")))
                .collect();
            Err(config_err(
                "Cargo.toml",
                format!(
                    "`[workspace.package] version` is inherited by crates that would get \
                     different versions ({}); give each crate its own `version` or use \
                     versioning = \"fixed\"",
                    list.join(", ")
                ),
            ))
        }
    }
}

/// `[workspace.dependencies]` of the root Cargo.toml has a path entry for crate `name`.
fn workspace_dep(files: &mut Files, name: &str) -> bool {
    let Ok(text) = files.get("Cargo.toml") else {
        return false;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .is_some_and(|deps| {
            deps.iter().any(|(key, d)| {
                d.get("path").is_some()
                    && d.get("package").and_then(|p| p.as_str()).unwrap_or(key) == name
            })
        })
}

fn changelog_section(p: &Package, nv: &NextVersion, date: &str) -> String {
    let text = prepend_changelog(None, nv, date);
    let section = text.strip_prefix("# Changelog\n\n").unwrap_or(&text);
    match section.strip_prefix("## ") {
        Some(rest) => format!("### {} {rest}", p.name),
        None => section.to_string(),
    }
    .replace("\n### ", "\n#### ")
}

fn pr_body(model: &ReleaseModel, next: &[NextVersion], date: &str) -> String {
    let mut body = String::from(
        "Prepared by `hoist release prepare`. Merging this PR releases the packages below; \
         `hoist release publish` then creates the tags.\n\n\
         | Package | Version | Tag |\n| --- | --- | --- |\n",
    );
    let mut sections = String::new();
    let mut tags = String::new();
    for n in next {
        let Some(p) = model.packages.iter().find(|p| p.name == n.package) else {
            continue;
        };
        let tag = model.tag_for(p, &n.to.to_string());
        let version = if n.from == n.to {
            format!("{} (first release)", n.to)
        } else {
            format!("{} → {}", n.from, n.to)
        };
        body.push_str(&format!("| `{}` | {version} | `{tag}` |\n", n.package));
        sections.push_str(&format!("\n{}", changelog_section(p, n, date)));
        tags.push_str(&format!("- `{tag}`\n"));
    }
    format!("{body}\n## Changelog\n{sections}\n## Tags to create\n\n{tags}")
}

/// Every edit of the release commit, in memory. Errors here are exit 3 (nothing written yet).
fn prepare_files(
    dir: &Path,
    model: &ReleaseModel,
    next: &[NextVersion],
) -> Result<Prepared, HoistError> {
    let mut files = Files {
        root: dir,
        map: BTreeMap::new(),
    };
    let find = |name: &str| model.packages.iter().find(|p| p.name == name);
    let mut inheriting = Vec::new();
    for p in model.packages.iter().filter(|p| p.kind == PkgKind::Crate) {
        if inherits_version(&mut files, p)? {
            inheriting.push(p);
        }
    }
    bump_inherited(&mut files, next, &inheriting)?;
    for n in next {
        let Some(p) = find(&n.package) else { continue };
        let v = n.to.to_string();
        match p.kind {
            PkgKind::Npm => files.edit(&manifest(p), |t| set_package_json_version(t, &v))?,
            PkgKind::Crate if inheriting.iter().any(|i| i.name == p.name) => {}
            PkgKind::Crate => files.edit(&manifest(p), |t| set_cargo_version(t, &v))?,
        }
    }
    // Internal dependents' requirements.
    for n in next.iter().filter(|n| n.from != n.to) {
        let Some(dep) = find(&n.package) else {
            continue;
        };
        let v = n.to.to_string();
        let dependents = model
            .packages
            .iter()
            .filter(|d| d.kind == dep.kind && d.deps.contains(&dep.name));
        for d in dependents {
            match d.kind {
                PkgKind::Npm => files.edit(&manifest(d), |t| {
                    set_package_json_dep(t, &dep.name, &format!("^{v}"))
                })?,
                PkgKind::Crate => {
                    files.edit(&manifest(d), |t| set_cargo_dep_version(t, &dep.name, &v))?
                }
            }
        }
        if dep.kind == PkgKind::Crate && workspace_dep(&mut files, &dep.name) {
            files.edit("Cargo.toml", |t| set_cargo_dep_version(t, &dep.name, &v))?;
        }
    }
    let date = today();
    let mut created = Vec::new();
    for n in next {
        let Some(p) = find(&n.package) else { continue };
        let rel = join_rel(&p.dir.to_string_lossy(), "CHANGELOG.md");
        let existing = match files.get(&rel) {
            Ok(t) => Some(t),
            Err(_) if !dir.join(&rel).exists() => {
                created.push(rel.clone());
                None
            }
            Err(e) => return Err(e),
        };
        let text = prepend_changelog(existing.as_deref(), n, &date);
        files.map.insert(rel, text);
    }
    let kind_of = |n: &NextVersion| find(&n.package).map(|p| p.kind);
    let npm = next.iter().any(|n| kind_of(n) == Some(PkgKind::Npm));
    let cargo = next.iter().any(|n| kind_of(n) == Some(PkgKind::Crate));
    for (lock, used) in [("pnpm-lock.yaml", npm), ("Cargo.lock", cargo)] {
        if used && !dir.join(lock).exists() {
            created.push(lock.to_string());
        }
    }
    let list: Vec<String> = next
        .iter()
        .map(|n| format!("{}@{}", n.package, n.to))
        .collect();
    Ok(Prepared {
        files: files.map,
        created,
        npm,
        cargo,
        title: format!("release: {}", list.join(", ")),
        body: pr_body(model, next, &date),
    })
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
    match git(dir, &["push", "-q", "origin", "--delete", BRANCH]) {
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
            echo(git(dir, &["push", "-q", "--force", "origin", BRANCH])?);
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

/// Build and push the branch, then restore. `Err` is the stopped report (exit 1).
fn write_branch(dir: &Path, default: &str, p: &Prepared, pr: Action) -> Result<(), Report> {
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
    if !failed && restored.is_ok() {
        return Ok(());
    }
    items.push(match restored {
        Ok(()) => step_item("restore", Action::Ok, format!("back on `{default}`")),
        Err(e) => step_item(
            "restore",
            Action::Unknown,
            format!("failed: {e}; run `git checkout -f {default}`"),
        ),
    });
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
    if let Err(stopped) = write_branch(dir, &default, &prepared, pr_action) {
        return Ok(stopped);
    }
    let n = match upsert_pr(&gh, &repo, &default, &prepared, pr) {
        Ok(n) => n,
        Err(e) => {
            let mut items: Vec<Item> = STEPS
                .iter()
                .map(|s| step_item(s, Action::Ok, "done".to_string()))
                .collect();
            items.push(step_item(
                "restore",
                Action::Ok,
                format!("back on `{default}`"),
            ));
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
