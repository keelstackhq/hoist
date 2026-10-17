//! `hoist release plan`: the next versions, read-only.

use crate::config::read_pins;
use crate::error::HoistError;
use crate::release::commits::{ChangeEntry, Section};
use crate::release::gitlog::read_history;
use crate::release::planner::{NextVersion, plan_versions};
use crate::release::read_release_model;
use crate::report::{Action, Item, Report};
use std::path::Path;

fn headline(entries: &[ChangeEntry]) -> Option<String> {
    let pick = |s: Section| entries.iter().find(|e| e.section == s);
    let (label, e) = pick(Section::Breaking)
        .map(|e| ("breaking", e))
        .or_else(|| pick(Section::Features).map(|e| ("feat", e)))
        .or_else(|| pick(Section::Fixes).map(|e| ("fix", e)))?;
    Some(format!("{label}: {}", e.text))
}

fn reason(n: &NextVersion) -> String {
    if n.first_release && n.from == n.to {
        return format!("first release {}", n.to);
    }
    let why = match headline(&n.entries) {
        Some(h) if n.reason != "first release" => format!("{}, {h}", n.reason),
        _ => n.reason.clone(),
    };
    format!("{} -> {} ({why})", n.from, n.to)
}

/// One item per package: `create` when it releases, `ok` otherwise. Exit 1 when any releases.
pub fn release_plan(dir: &Path) -> Result<Report, HoistError> {
    let model = read_release_model(dir)?;
    let history = read_history(dir, &model)?;
    let pins = read_pins(dir)?;
    let next = plan_versions(&model, &history, &pins)?;
    let mut items = Vec::new();
    for p in &model.packages {
        let (action, why) = match next.iter().find(|n| n.package == p.name) {
            Some(n) => (Action::Create, reason(n)),
            None => (
                Action::Ok,
                format!("no release needed at {}", p.version.value),
            ),
        };
        items.push(Item {
            area: "release".to_string(),
            kind: "version".to_string(),
            key: p.name.clone(),
            action,
            reason: why,
            source: p.version.source.clone(),
            verify: None,
        });
    }
    items.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Report {
        command: "release plan".to_string(),
        // R17: a preview exits 0 like `plan`; the items show what would release.
        exit_code: 0,
        items,
        human_steps: vec![],
        text: None,
    })
}
