//! `hoist import`: the `hoist.toml` overrides that make `plan` match the live state (spec
//! §7.5). GET requests only; `--write` edits the files (ruling R27).

mod pages;
mod snippet;
mod toml;

use self::toml::{Edit, Outcome, Val, line, write_files};
use crate::auth::{cloudflare_token, github_token};
use crate::desired::{Desired, RULESET_NAME, build_desired};
use crate::error::HoistError;
use crate::github::{LiveGithub, Ruleset, read_github};
use crate::http::{Http, Service};
use crate::report::{Action, Item, Report};
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
        pages::pages_overrides(http, &desired, &mut found)?;
    }

    let mut items = found.items;
    // Group the edits by file, keeping the order the overrides were found in.
    let mut files: Vec<(String, Vec<Edit>)> = Vec::new();
    for o in &found.overrides {
        let edit = (o.table.to_string(), o.key.to_string(), o.val.clone());
        match files.iter_mut().find(|(f, _)| *f == o.file) {
            Some((_, edits)) => edits.push(edit),
            None => files.push((o.file.clone(), vec![edit])),
        }
    }
    // Every file is rendered before any is replaced: a failure writes nothing.
    let outcomes = if write {
        Some(write_files(dir, &files)?)
    } else {
        None
    };
    for (fi, (file, _)) in files.iter().enumerate() {
        let mine = found.overrides.iter().filter(|o| o.file == *file);
        for (k, o) in mine.enumerate() {
            let action = match outcomes.as_ref().map(|all| all[fi][k]) {
                None => Action::Drift,
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
    let drift = !write && !found.overrides.is_empty();
    // The TOML text is for reading before a write; after `--write` the files hold it.
    let text = if write {
        None
    } else {
        Some(snippet::snippet(&items)).filter(|t| !t.is_empty())
    };
    Ok(Report {
        command: "import".to_string(),
        items,
        human_steps: Vec::new(),
        exit_code: i32::from(drift),
        text,
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
                unimportable(rs, &d.default_branch, found);
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
                let text = format!("# live ruleset \"{}\" is not managed by hoist", other.name);
                comment(found, text);
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

fn comment(found: &mut Found, text: String) {
    found.items.push(item(
        "github",
        "comment",
        format!("{ROOT}:github"),
        Action::Ok,
        text,
    ));
}

/// Ruling R28: ruleset state with no `hoist.toml` key becomes a comment (exit code unchanged).
fn unimportable(rs: &Ruleset, branch: &str, found: &mut Found) {
    if let Some(mode) = rs.enforcement.as_deref().filter(|m| *m != "active") {
        comment(
            found,
            format!(
                "# live ruleset \"{RULESET_NAME}\" enforcement \"{mode}\" is not importable; \
                 apply sets \"active\""
            ),
        );
    }
    let explicit = format!("refs/heads/{branch}");
    if !rs
        .include
        .iter()
        .any(|i| i == "~DEFAULT_BRANCH" || *i == explicit)
    {
        comment(
            found,
            format!(
                "# live ruleset \"{RULESET_NAME}\" target \"{}\" is not importable; apply \
                 sets \"~DEFAULT_BRANCH\"",
                rs.include.join(", ")
            ),
        );
    }
}
