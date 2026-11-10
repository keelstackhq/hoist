//! CHANGELOG blocks in changesets' changelog-git layout (changesets spec §8).

use super::{Changeset, CsType};
use crate::release::planner::NextVersion;
use std::collections::BTreeMap;

/// Changeset id -> 7-character sha of the commit that added its file.
pub type Shas = BTreeMap<String, Option<String>>;

/// `- <sha>: <first line>`, further lines indented two spaces (`- <first line>` without sha).
fn line(c: &Changeset, sha: Option<&str>) -> String {
    let mut lines = c.summary.lines().map(str::trim_end);
    let first = lines.next().unwrap_or("");
    let mut out = match sha {
        Some(s) => format!("- {s}: {first}\n"),
        None => format!("- {first}\n"),
    };
    for l in lines {
        out.push_str(format!("  {l}").trim_end());
        out.push('\n');
    }
    out
}

/// `### Major|Minor|Patch Changes` blocks of `nv`; dependency updates go under Patch.
pub fn blocks(
    nv: &NextVersion,
    next: &[NextVersion],
    sets: &[Changeset],
    shas: &Shas,
) -> Vec<String> {
    let sha = |id: &str| shas.get(id).cloned().flatten();
    let mut by = BTreeMap::<CsType, String>::new();
    for c in sets.iter().filter(|c| nv.changesets.contains(&c.id)) {
        let t = c
            .releases
            .iter()
            .filter(|(n, _)| *n == nv.package)
            .map(|(_, t)| *t)
            .max();
        if let Some(t) = t.filter(|t| *t > CsType::None) {
            by.entry(t)
                .or_default()
                .push_str(&line(c, sha(&c.id).as_deref()));
        }
    }
    if !nv.dep_updates.is_empty() {
        // One `Updated dependencies` line per changeset of the updated dependencies.
        let mut ids: Vec<&String> = Vec::new();
        for (dep, _) in &nv.dep_updates {
            let of = next
                .iter()
                .filter(|n| n.package == *dep)
                .flat_map(|n| &n.changesets);
            ids.extend(of.filter(|id| !ids.contains(id)).collect::<Vec<_>>());
        }
        let mut s = String::new();
        for id in &ids {
            match sha(id) {
                Some(h) => s.push_str(&format!("- Updated dependencies [{h}]\n")),
                None => s.push_str("- Updated dependencies\n"),
            }
        }
        if ids.is_empty() {
            s.push_str("- Updated dependencies\n");
        }
        for (dep, v) in &nv.dep_updates {
            s.push_str(&format!("  - {dep}@{v}\n"));
        }
        by.entry(CsType::Patch).or_default().push_str(&s);
    }
    let mut out: Vec<String> = [
        (CsType::Major, "Major"),
        (CsType::Minor, "Minor"),
        (CsType::Patch, "Patch"),
    ]
    .into_iter()
    .filter_map(|(t, title)| Some(format!("### {title} Changes\n\n{}", by.get(&t)?)))
    .collect();
    if out.is_empty() {
        let l = if nv.first_release {
            "First release."
        } else {
            "Version bump only."
        };
        out.push(format!("- {l}\n"));
    }
    out
}
