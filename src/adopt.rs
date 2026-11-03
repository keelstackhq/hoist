//! `hoist apply --adopt` (spec §7.6): adopting an existing repo never removes or loosens. Live
//! settings stricter than the desired ones are kept: they become the desired value, and their
//! item says so ("kept stricter live setting"). What adopt cannot reach without loosening is
//! drift (the ruleset rule in `diff`).

use crate::desired::Desired;
use crate::github::LiveGithub;
use crate::repo::Sourced;
use crate::report::Item;

/// One kept live setting: the item it belongs to and what was kept.
pub struct Kept {
    kind: &'static str,
    key: String,
    what: String,
}

/// The source of a setting kept from the live state.
pub const SOURCE: &str = "live (kept by --adopt)";

/// Raise `d` to the stricter live settings; returns what was kept.
pub fn raise(d: &mut Desired, live: &LiveGithub) -> Vec<Kept> {
    let mut kept = Vec::new();
    let rs = &mut d.github.ruleset;
    let live_rs = live
        .rulesets
        .as_got()
        .ok()
        .and_then(|all| all.iter().find(|r| r.name == rs.name));
    // A ruleset with no wanted checks is not managed; adopting does not start managing it.
    if let Some(l) = live_rs.filter(|_| !rs.required_checks.is_empty()) {
        let extra: Vec<String> = l
            .required_checks
            .iter()
            .filter(|c| !rs.required_checks.iter().any(|w| &w.value == *c))
            .cloned()
            .collect();
        if !extra.is_empty() {
            kept.push(Kept {
                kind: "ruleset",
                key: rs.name.clone(),
                what: format!("required checks `{}`", extra.join("`, `")),
            });
        }
        for c in extra {
            rs.required_checks.push(Sourced {
                value: c,
                source: SOURCE.into(),
            });
        }
        if l.merge_queue && !rs.merge_queue.value {
            rs.merge_queue = Sourced {
                value: true,
                source: SOURCE.into(),
            };
            kept.push(Kept {
                kind: "ruleset",
                key: rs.name.clone(),
                what: "the merge queue".into(),
            });
        }
    }
    let perms = &mut d.github.actions_permissions;
    if let Ok(live_perms) = live.actions_permissions.as_got()
        && live_perms == "read"
        && perms.value == "write"
    {
        *perms = Sourced {
            value: "read".into(),
            source: SOURCE.into(),
        };
        kept.push(Kept {
            kind: "actions_permissions",
            key: "default".into(),
            what: "read-only workflow permissions".into(),
        });
    }
    let rank = |v: &str| match v {
        "private" => 0,
        "internal" => 1,
        _ => 2,
    };
    if let (Some(want), Ok(s)) = (
        d.github.settings.visibility.as_mut(),
        live.settings.as_got(),
    ) && rank(&s.visibility) < rank(&want.value)
    {
        kept.push(Kept {
            kind: "repo_setting",
            key: "visibility".into(),
            what: format!("visibility `{}`", s.visibility),
        });
        *want = Sourced {
            value: s.visibility.clone(),
            source: SOURCE.into(),
        };
    }
    kept
}

/// Add each kept setting to its item's reason.
pub fn note(items: &mut [Item], kept: &[Kept]) {
    for k in kept {
        if let Some(i) = items
            .iter_mut()
            .find(|i| i.kind == k.kind && i.key == k.key)
        {
            i.reason = format!("{}; kept stricter live setting: {}", i.reason, k.what);
        }
    }
}
