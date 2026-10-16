//! `hoist import`: the `hoist.toml` overrides that make `plan` match the live state (spec
//! §7.5). GET requests only; `--write` edits the files (ruling R27).

mod toml;

use self::toml::{Edit, Outcome, Val, edit_file, line};
use crate::auth::{cloudflare_token, github_token};
use crate::cloudflare::read_cloudflare;
use crate::desired::{Desired, RULESET_NAME, build_desired};
use crate::error::HoistError;
use crate::github::{Fetch, LiveGithub, read_github};
use crate::http::{Fetched, Http, Service, seg};
use crate::repo::targets::read_targets;
use crate::report::{Action, Item, Report};
use serde_json::Value;
use std::path::Path;

const ROOT: &str = "hoist.toml";

/// One key import would set: file relative to the repo root, area, table, key, value.
struct Override {
    file: String,
    area: &'static str,
    table: &'static str,
    key: &'static str,
    val: Val,
}

#[derive(Default)]
struct Found {
    overrides: Vec<Override>,
    /// Unknown reads and comments, ready as report items.
    items: Vec<Item>,
}

impl Found {
    fn set(
        &mut self,
        file: &str,
        area: &'static str,
        table: &'static str,
        key: &'static str,
        val: Val,
    ) {
        self.overrides.push(Override {
            file: file.to_string(),
            area,
            table,
            key,
            val,
        });
    }

    fn unknown(&mut self, file: &str, area: &str, table: &str, key: &str, why: &str) {
        self.items.push(item(
            area,
            "override",
            format!("{file}:{table}.{key}"),
            Action::Unknown,
            why.to_string(),
        ));
    }
}

fn item(area: &str, kind: &str, key: String, action: Action, reason: String) -> Item {
    Item {
        area: area.to_string(),
        kind: kind.to_string(),
        key,
        action,
        reason,
        source: "live".to_string(),
        verify: None,
    }
}

/// Exit 0 when there is nothing to import or after a successful `--write`; 1 when overrides
/// exist and `--write` was not given.
pub fn import(dir: &Path, write: bool, verbose: bool) -> Result<Report, HoistError> {
    let desired = build_desired(dir)?;
    let pages = !desired.cloudflare.pages.is_empty();
    if pages {
        desired.require_account_id()?;
    }
    // Resolve every needed token before the first request.
    let github = Http::new(Service::Github, github_token()?).verbose(verbose);
    let cloudflare = pages
        .then(cloudflare_token)
        .transpose()?
        .map(|t| Http::new(Service::Cloudflare, t).verbose(verbose));

    let mut found = Found::default();
    let live = read_github(&github, &desired)?;
    github_overrides(&desired, &live, &mut found);
    if let Some(http) = &cloudflare {
        pages_overrides(dir, http, &desired, &mut found)?;
    }

    let mut items = found.items;
    let mut drift = false;
    let mut files: Vec<String> = Vec::new();
    for o in &found.overrides {
        if !files.contains(&o.file) {
            files.push(o.file.clone());
        }
    }
    for file in files {
        let mine: Vec<&Override> = found.overrides.iter().filter(|o| o.file == file).collect();
        let outcomes = if write {
            let edits: Vec<Edit> = mine
                .iter()
                .map(|o| (o.table.to_string(), o.key.to_string(), o.val.clone()))
                .collect();
            edit_file(&dir.join(&file), &file, &edits)?
                .into_iter()
                .map(Some)
                .collect()
        } else {
            vec![None; mine.len()]
        };
        for (o, outcome) in mine.iter().zip(outcomes) {
            let action = match outcome {
                None => {
                    drift = true;
                    Action::Drift
                }
                Some(Outcome::Added) => Action::Create,
                Some(Outcome::Replaced) => Action::Update,
                Some(Outcome::Same) => Action::Ok,
            };
            items.push(item(
                o.area,
                "override",
                format!("{}:{}.{}", o.file, o.table, o.key),
                action,
                line(o.key, &o.val),
            ));
        }
    }
    items.sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    Ok(Report {
        command: "import".to_string(),
        items,
        human_steps: Vec::new(),
        exit_code: i32::from(drift),
    })
}

fn github_overrides(d: &Desired, live: &LiveGithub, found: &mut Found) {
    let g = &d.github;
    match live.settings.as_got() {
        Err(why) => {
            if g.settings.visibility.is_some() {
                found.unknown(ROOT, "github", "github", "visibility", why);
            }
            for key in ["allow_auto_merge", "delete_branch_on_merge"] {
                found.unknown(ROOT, "github", "github", key, why);
            }
        }
        Ok(s) => {
            if let Some(want) = &g.settings.visibility
                && s.visibility != want.value
            {
                found.set(
                    ROOT,
                    "github",
                    "github",
                    "visibility",
                    Val::Str(s.visibility.clone()),
                );
            }
            let flags = [
                (
                    "allow_auto_merge",
                    s.allow_auto_merge,
                    g.settings.allow_auto_merge.value,
                ),
                (
                    "delete_branch_on_merge",
                    s.delete_branch_on_merge,
                    g.settings.delete_branch_on_merge.value,
                ),
            ];
            for (key, have, want) in flags {
                match have {
                    None => found.unknown(
                        ROOT,
                        "github",
                        "github",
                        key,
                        &format!("cannot read {key}: the token cannot see it (needs admin access)"),
                    ),
                    Some(have) if have != want => {
                        found.set(ROOT, "github", "github", key, Val::Bool(have))
                    }
                    Some(_) => {}
                }
            }
        }
    }

    match live.rulesets.as_got() {
        Err(why) => {
            for key in ["required_checks", "merge_queue"] {
                found.unknown(ROOT, "github", "github", key, why);
            }
        }
        Ok(rulesets) => {
            if let Some(rs) = rulesets.iter().find(|r| r.name == RULESET_NAME) {
                let mut have = rs.required_checks.clone();
                let mut need: Vec<String> = g
                    .ruleset
                    .required_checks
                    .iter()
                    .map(|c| c.value.clone())
                    .collect();
                have.sort();
                need.sort();
                if have != need {
                    let live_checks = Val::List(rs.required_checks.clone());
                    found.set(ROOT, "github", "github", "required_checks", live_checks);
                }
                // Merge queue only matters while the ruleset is managed (checks non-empty).
                if !have.is_empty() && rs.merge_queue != g.ruleset.merge_queue.value {
                    found.set(
                        ROOT,
                        "github",
                        "github",
                        "merge_queue",
                        Val::Bool(rs.merge_queue),
                    );
                }
            }
            for other in rulesets.iter().filter(|r| r.name != RULESET_NAME) {
                found.items.push(item(
                    "github",
                    "comment",
                    format!("{ROOT}:github"),
                    Action::Ok,
                    format!("# live ruleset \"{}\" is not managed by hoist", other.name),
                ));
            }
        }
    }

    match live.actions_permissions.as_got() {
        Err(why) => found.unknown(ROOT, "github", "github", "actions_permissions", why),
        Ok(have) if *have != g.actions_permissions.value => found.set(
            ROOT,
            "github",
            "github",
            "actions_permissions",
            Val::Str(have.clone()),
        ),
        Ok(_) => {}
    }
}

/// `[deploy] project` for a Pages target whose inferred project is missing live, when exactly
/// one live Pages project name starts with the target name.
fn pages_overrides(
    dir: &Path,
    http: &Http,
    d: &Desired,
    found: &mut Found,
) -> Result<(), HoistError> {
    let Some(account) = &d.cloudflare.account_id else {
        return Ok(());
    };
    let (targets, ..) = read_targets(dir)?;
    let live = read_cloudflare(http, d)?;
    let mut listed: Option<Fetch<Vec<String>>> = None;
    for p in &d.cloudflare.pages {
        if p.project.source != "default" {
            continue;
        }
        let rel = targets
            .iter()
            .find(|t| t.name == p.target)
            .and_then(|t| t.dir.strip_prefix(dir).ok())
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| p.target.clone());
        let file = format!("{rel}/{ROOT}");
        let state = live
            .pages
            .iter()
            .find(|(n, _)| *n == p.project.value)
            .map(|(_, f)| f.clone());
        match state {
            Some(Fetch::Got(None)) => {}
            Some(Fetch::Unknown(why)) => {
                found.unknown(&file, "cloudflare", "deploy", "project", &why);
                continue;
            }
            Some(Fetch::Got(Some(_))) | None => continue,
        }
        if listed.is_none() {
            listed = Some(list_projects(http, &account.value)?);
        }
        match listed.as_ref().map(Fetch::as_got) {
            Some(Ok(names)) => {
                let matches: Vec<&String> =
                    names.iter().filter(|n| n.starts_with(&p.target)).collect();
                if let [only] = matches.as_slice() {
                    found.set(
                        &file,
                        "cloudflare",
                        "deploy",
                        "project",
                        Val::Str((*only).clone()),
                    );
                }
            }
            Some(Err(why)) => found.unknown(&file, "cloudflare", "deploy", "project", why),
            None => {}
        }
    }
    Ok(())
}

fn list_projects(http: &Http, account: &str) -> Result<Fetch<Vec<String>>, HoistError> {
    let path = format!("/accounts/{}/pages/projects", seg(account));
    Ok(match http.get_all(&path, None)? {
        Fetched::Ok(Value::Array(items)) => Fetch::Got(
            items
                .iter()
                .filter_map(|i| i.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
        ),
        Fetched::Ok(_) => Fetch::Unknown("cannot list Pages projects: unexpected response".into()),
        Fetched::NotFound => Fetch::Unknown("cannot list Pages projects: not found (404)".into()),
        Fetched::Forbidden(hint) => Fetch::Unknown(format!("cannot list Pages projects: {hint}")),
    })
}

/// One table's lines in the printed snippet: (table, lines).
type Lines = (String, Vec<String>);

/// The TOML text for table mode without `--write`: override lines grouped by file and table,
/// then the comments. Empty when the report wrote files or holds nothing to import.
pub fn snippet(report: &Report) -> String {
    let written = report
        .items
        .iter()
        .any(|i| matches!(i.action, Action::Create | Action::Update));
    if written || report.command != "import" {
        return String::new();
    }
    let lines = report
        .items
        .iter()
        .filter(|i| i.kind == "override" && i.action == Action::Drift)
        .chain(report.items.iter().filter(|i| i.kind == "comment"));
    let mut files: Vec<(String, Vec<Lines>)> = Vec::new();
    for i in lines {
        let Some((file, rest)) = i.key.split_once(':') else {
            continue;
        };
        let table = rest.split_once('.').map_or(rest, |(t, _)| t);
        let pos = match files.iter().position(|(f, _)| f == file) {
            Some(pos) => pos,
            None => {
                files.push((file.to_string(), Vec::new()));
                files.len() - 1
            }
        };
        let tables = &mut files[pos].1;
        match tables.iter_mut().find(|(t, _)| t == table) {
            Some((_, l)) => l.push(i.reason.clone()),
            None => tables.push((table.to_string(), vec![i.reason.clone()])),
        }
    }
    files.sort_by_key(|(f, _)| (f != ROOT, f.clone()));
    let mut out = String::new();
    for (file, tables) in files {
        out.push_str(&format!("\n# {file}\n"));
        for (table, lines) in tables {
            out.push_str(&format!("[{table}]\n"));
            for l in lines {
                out.push_str(&l);
                out.push('\n');
            }
        }
    }
    out
}
