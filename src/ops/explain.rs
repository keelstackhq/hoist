//! `hoist explain`: every desired element with the file it came from.

use crate::desired::{Desired, build_desired};
use crate::diff::ruleset_summary;
use crate::error::HoistError;
use crate::release::{ReleaseModel, read_release_model};
use crate::repo::Sourced;
use crate::report::{Action, Item, Report};
use std::path::Path;

fn item(area: &str, kind: &str, key: &str, reason: &str, source: &str) -> Item {
    Item {
        area: area.to_string(),
        kind: kind.to_string(),
        key: key.to_string(),
        action: Action::Ok,
        reason: reason.to_string(),
        source: source.to_string(),
        verify: None,
    }
}

fn each(items: &mut Vec<Item>, area: &str, kind: &str, list: &[Sourced<String>]) {
    for s in list {
        items.push(item(area, kind, &s.value, &s.value, &s.source));
    }
}

pub fn items(desired: &Desired, release: &ReleaseModel) -> Vec<Item> {
    let gh = &desired.github;
    let mut items = Vec::new();
    if let Some(v) = &gh.settings.visibility {
        items.push(item(
            "github",
            "repo_setting",
            "visibility",
            &v.value,
            &v.source,
        ));
    }
    let s = &gh.settings;
    items.push(item(
        "github",
        "repo_setting",
        "allow_auto_merge",
        &s.allow_auto_merge.value.to_string(),
        &s.allow_auto_merge.source,
    ));
    items.push(item(
        "github",
        "repo_setting",
        "delete_branch_on_merge",
        &s.delete_branch_on_merge.value.to_string(),
        &s.delete_branch_on_merge.source,
    ));
    each(
        &mut items,
        "github",
        "required_check",
        &gh.ruleset.required_checks,
    );
    let (reason, source) = ruleset_summary(desired);
    items.push(item(
        "github",
        "ruleset",
        &gh.ruleset.name,
        &reason,
        &source,
    ));
    let mq = &gh.ruleset.merge_queue;
    items.push(item(
        "github",
        "merge_queue",
        "default",
        &mq.value.to_string(),
        &mq.source,
    ));
    items.push(item(
        "github",
        "actions_permissions",
        "default",
        &gh.actions_permissions.value,
        &gh.actions_permissions.source,
    ));
    each(&mut items, "github", "environment", &gh.environments);
    each(&mut items, "github", "secret", &gh.secrets);
    each(&mut items, "github", "app", &gh.apps);

    let cf = &desired.cloudflare;
    for p in &cf.pages {
        items.push(item(
            "cloudflare",
            "pages_project",
            &p.project.value,
            &p.project.value,
            &p.project.source,
        ));
        if let Some(d) = &p.domain {
            items.push(item(
                "cloudflare",
                "custom_domain",
                &d.value,
                &d.value,
                &d.source,
            ));
        }
    }
    each(&mut items, "cloudflare", "workers_script", &cf.workers);
    each(&mut items, "npm", "package", &desired.packages.npm);
    each(&mut items, "crates", "crate", &desired.packages.crates);
    for p in &release.packages {
        items.push(item(
            "release",
            "package",
            &p.name,
            &format!("version {}", p.version.value),
            &p.version.source,
        ));
    }

    items.sort_by(|a, b| (&a.area, &a.kind, &a.key).cmp(&(&b.area, &b.kind, &b.key)));
    items
}

pub fn run(dir: &Path) -> Result<Report, HoistError> {
    let desired = build_desired(dir)?;
    desired.require_account_id()?;
    let release = read_release_model(dir)?;
    Ok(Report {
        command: "explain".to_string(),
        items: items(&desired, &release),
        human_steps: Vec::new(),
        exit_code: 0,
        text: None,
        hint: None,
    })
}
