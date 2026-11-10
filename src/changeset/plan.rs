//! The changesets planner (changesets spec §5, as `@changesets/assemble-release-plan`). Pure.

use super::range::{cargo_satisfies, npm_satisfies, rewrite};
use super::{Changeset, CsConfig, CsType, glob_match};
use crate::config::{ReleasePins, Versioning};
use crate::error::HoistError;
use crate::release::planner::{History, NextVersion, awaiting_publish, check_pins};
use crate::release::planner::{current, follow_launchers, order};
use crate::release::semver::Version;
use crate::release::{DepKind, Edge, Package, PkgKind, ReleaseModel};
use std::collections::{BTreeMap, BTreeSet};

const CONFIG: &str = ".changeset/config.json";
type Map<T> = BTreeMap<String, T>;

fn err(file: &str, message: String) -> HoistError {
    let file = file.to_string();
    HoistError::Config { file, message }
}

/// Semver increment, also below 1.0.0 (changesets has no 0.x rule).
fn bump(v: Version, t: CsType) -> Version {
    let (ma, mi, pa) = (v.major, v.minor, v.patch);
    let (major, minor, patch) = match t {
        CsType::None => (ma, mi, pa),
        CsType::Patch => (ma, mi, pa + 1),
        CsType::Minor => (ma, mi + 1, 0),
        CsType::Major => (ma + 1, 0, 0),
    };
    Version {
        major,
        minor,
        patch,
    }
}

/// Does `e`'s range admit `v`; `old` is the dependency's current version (`workspace:*`).
fn in_range(kind: PkgKind, e: &Edge, old: &Version, v: &Version) -> Option<bool> {
    let r = e.range.as_deref()?;
    match kind {
        PkgKind::Npm => npm_satisfies(r, old, v),
        PkgKind::Crate => cargo_satisfies(r, v),
    }
}

/// `fixed`/`linked` patterns as package names; a pattern matching nothing is exit 3.
fn groups(key: &str, pats: &[Vec<String>], names: &[&str]) -> Result<Vec<Vec<String>>, HoistError> {
    let mut out = Vec::new();
    for g in pats {
        let mut members = BTreeSet::new();
        for pat in g {
            let hit: Vec<_> = names.iter().filter(|n| glob_match(pat, n)).collect();
            if hit.is_empty() {
                let m = format!("`{key}` pattern `{pat}` matches no releasable package; fix it");
                return Err(err(CONFIG, m));
            }
            members.extend(hit.into_iter().map(|n| n.to_string()));
        }
        out.push(members.into_iter().collect());
    }
    Ok(out)
}

/// Each package's highest type and its changeset ids; names checked (spec §4.3, `ignore`).
fn own_types(
    m: &ReleaseModel,
    sets: &[Changeset],
    names: &[&str],
    ignored: impl Fn(&str) -> bool,
) -> Result<(Map<CsType>, Map<Vec<String>>), HoistError> {
    let (mut ty, mut ids) = (Map::new(), Map::<Vec<String>>::new());
    for c in sets {
        let file = format!(".changeset/{}.md", c.id);
        for (n, _) in &c.releases {
            let pkg = m.packages.iter().find(|p| &p.name == n);
            if let Some(k) = pkg.and_then(|p| p.launcher.as_ref()).map(|l| &l.crate_name) {
                return Err(err(&file, format!("`{n}` follows `{k}`; name `{k}`")));
            }
            if !names.contains(&n.as_str()) {
                let m = format!("`{n}` is not a releasable package; name one hoist publishes");
                return Err(err(&file, m));
            }
        }
        let ign = c.releases.iter().filter(|(n, _)| ignored(n)).count();
        if ign > 0 && ign < c.releases.len() {
            let m = format!("it mixes packages in `ignore` of {CONFIG} with others; split it");
            return Err(err(&file, m));
        }
        for (n, t) in c.releases.iter().filter(|(n, _)| !ignored(n)) {
            let slot = ty.entry(n.clone()).or_insert(CsType::None);
            *slot = (*slot).max(*t);
            ids.entry(n.clone()).or_default().push(c.id.clone());
        }
    }
    Ok((ty, ids))
}

/// Plan the next versions from changesets: own bumps, then dependents, fixed and linked groups
/// until nothing changes (spec §5); pins and skips as in commits mode, launchers follow.
pub fn plan_versions(
    m: &ReleaseModel,
    h: &History,
    sets: &[Changeset],
    cfg: &CsConfig,
    pins: &ReleasePins,
) -> Result<Vec<NextVersion>, HoistError> {
    check_pins(m, pins)?;
    let pkgs: Vec<&Package> = order(m)
        .into_iter()
        .filter(|p| p.launcher.is_none())
        .collect();
    let mut seen = BTreeSet::new();
    if let Some(p) = m
        .packages
        .iter()
        .find(|p| p.launcher.is_none() && !seen.insert(&p.name))
    {
        let m = format!(
            "a crate and an npm package are both named `{}`; rename one",
            p.name
        );
        return Err(err(CONFIG, m));
    }
    let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
    let mut cur = Map::new();
    for p in &pkgs {
        cur.insert(p.name.clone(), current(p)?);
    }
    let mut fixed = groups("fixed", &cfg.fixed, &names)?;
    let linked = groups("linked", &cfg.linked, &names)?;
    if m.versioning == Versioning::Fixed {
        if !fixed.is_empty() || !linked.is_empty() {
            let m = format!("`release.versioning = \"fixed\"` and `fixed`/`linked` in {CONFIG}");
            return Err(err("hoist.toml", format!("{m}; keep one")));
        }
        fixed = vec![names.iter().map(|n| n.to_string()).collect()];
    }
    let mut seen = BTreeSet::new();
    if let Some(n) = fixed
        .iter()
        .chain(&linked)
        .flatten()
        .find(|n| !seen.insert(*n))
    {
        let m = format!("`{n}` is in more than one `fixed`/`linked` group; keep it in one");
        return Err(err(CONFIG, m));
    }
    let ignored = |n: &str| cfg.ignore.iter().any(|p| glob_match(p, n));
    let (mut ty, mut ids) = own_types(m, sets, &names, ignored)?;
    let waiting = awaiting_publish(m, h);
    if m.versioning == Versioning::Fixed && !waiting.is_empty() {
        return Ok(vec![]);
    }
    let skip = |n: &str| pins.packages.get(n).is_some_and(|x| x.skip);
    let blocked = |n: &str| ignored(n) || waiting.iter().any(|w| w == n) || skip(n);
    ty.retain(|n, _| !blocked(n));
    let own = ty.clone();
    // A pin releases its package (at least a patch) and so feeds dependents and groups.
    let pin_of = |n: &str| Version::parse(pins.packages.get(n)?.version.as_deref()?).ok();
    for n in names.iter().filter(|n| !blocked(n) && pin_of(n).is_some()) {
        let slot = ty.entry(n.to_string()).or_insert(CsType::None);
        *slot = (*slot).max(CsType::Patch);
    }
    let untagged = |n: &str| matches!(h.last_tag.get(n), Some(None) | None);
    let first = |p: &Package| untagged(&p.name);
    let releasing = |ty: &Map<CsType>, n: &str| ty.get(n).is_some_and(|t| *t > CsType::None);
    // fixed: every member moves; linked: only releasing members (spec §5.3).
    let all_groups: Vec<(&Vec<String>, bool)> = fixed
        .iter()
        .map(|g| (g, true))
        .chain(linked.iter().map(|g| (g, false)))
        .collect();
    let members = |ty: &Map<CsType>, g: &Vec<String>, all: bool| -> Vec<String> {
        let keep = |n: &&String| !blocked(n) && (all || releasing(ty, n));
        g.iter().filter(keep).cloned().collect()
    };
    let next = |ty: &Map<CsType>| -> Map<Version> {
        let mut out = Map::new();
        for p in pkgs.iter().filter(|p| releasing(ty, &p.name)) {
            let group = all_groups.iter().find(|(g, _)| g.contains(&p.name));
            let base = group.and_then(|(g, all)| members(ty, g, *all).iter().map(|n| cur[n]).max());
            let base = base.unwrap_or(cur[&p.name]);
            // No bump on a first release, except a new member of a fixed group with tags.
            let fixed_tagged = group.is_some_and(|(g, all)| *all && !g.iter().all(|n| untagged(n)));
            let v = if first(p) && !fixed_tagged {
                base
            } else {
                bump(base, ty[&p.name])
            };
            // A fixed group shares its highest pin (as `plan_fixed`).
            let pinned = match group {
                Some((g, true)) => g
                    .iter()
                    .filter(|n| !blocked(n))
                    .filter_map(|n| pin_of(n))
                    .max(),
                _ => pin_of(&p.name),
            };
            out.insert(p.name.clone(), pinned.map_or(v, |x| x.max(v)));
        }
        out
    };

    let mut why = Map::new();
    let mut raise = |ty: &mut Map<CsType>, n: &str, t: CsType, reason: String| {
        let up = t > ty.get(n).copied().unwrap_or(CsType::None);
        if up {
            ty.insert(n.to_string(), t);
            why.insert(n.to_string(), reason);
        }
        up
    };
    loop {
        let nv = next(&ty);
        let mut changed = false;
        for p in pkgs.iter().filter(|p| !blocked(&p.name)) {
            for e in &p.edges {
                let (Some(&dt), Some(dv)) = (ty.get(&e.name), nv.get(&e.name)) else {
                    continue;
                };
                let inr = in_range(p.kind, e, &cur[&e.name], dv).unwrap_or(true);
                let peer = e.kind == DepKind::Peer && dt >= CsType::Minor;
                let (want, how) = match e.kind {
                    DepKind::Dev => continue,
                    _ if peer && !(cfg.only_peer_out_of_range && inr) => (CsType::Major, "peer "),
                    _ if !inr || cfg.dependents_always => (CsType::Patch, ""),
                    _ => continue,
                };
                let state = if inr { "bumped" } else { "out of range" };
                let reason = format!("{how}dependency {} {state}", e.name);
                changed |= raise(&mut ty, &p.name, want, reason);
            }
        }
        for (g, all) in &all_groups {
            let Some(&max) = g.iter().filter_map(|n| ty.get(n)).max() else {
                continue;
            };
            let kind = if *all { "fixed" } else { "linked" };
            for n in members(&ty, g, *all) {
                changed |= raise(&mut ty, &n, max, format!("{kind} with {}", g.join(", ")));
            }
        }
        if !changed {
            break;
        }
    }

    let nv = next(&ty);
    let mut out = Vec::new();
    for p in pkgs.iter().filter(|p| !blocked(&p.name)) {
        let (from, first_release) = (cur[&p.name], first(p));
        let mut to = nv.get(&p.name).copied();
        let changesets = ids.remove(&p.name).unwrap_or_default();
        let mut reason = match why.remove(&p.name) {
            Some(w) if !releasing(&own, &p.name) => w,
            _ => format!("changesets: {}", changesets.join(", ")),
        };
        if first_release && to.is_none_or(|t| t == from) {
            (to, reason) = (Some(from), "first release".to_string());
        }
        if let Some(pinned) = pin_of(&p.name)
            && to.is_none_or(|t| pinned >= t)
        {
            (to, reason) = (Some(pinned), "pinned in .hoist/release.toml".to_string());
        }
        let Some(to) = to else { continue };
        let (package, entries, dep_updates) = (p.name.clone(), vec![], vec![]);
        let n = NextVersion {
            package,
            from,
            to,
            reason,
            entries,
            first_release,
            changesets,
            dep_updates,
        };
        out.push(n);
    }
    // Range rewrites (spec §5.2): out of range, or the dependency's type reaches the threshold.
    let bumped: Map<(Version, CsType)> = out
        .iter()
        .filter(|n| n.to != n.from)
        .map(|n| {
            (
                n.package.clone(),
                (n.to, ty.get(&n.package).copied().unwrap_or(CsType::Patch)),
            )
        })
        .collect();
    for n in &mut out {
        let p = pkgs
            .iter()
            .find(|p| p.name == n.package)
            .expect("planned package");
        for e in &p.edges {
            let Some((v, t)) = bumped.get(&e.name) else {
                continue;
            };
            let (Some(old), Some(inr)) =
                (e.range.as_deref(), in_range(p.kind, e, &cur[&e.name], v))
            else {
                continue;
            };
            let update = (e.name.clone(), v.to_string());
            let wanted = !inr || *t >= cfg.update_internal_dependencies;
            if wanted && rewrite(old, v) != old && !n.dep_updates.contains(&update) {
                n.dep_updates.push(update);
            }
        }
    }
    follow_launchers(m, &mut out)?;
    Ok(out)
}
