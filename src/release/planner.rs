//! The version planner: which packages release, and at which version. Pure.

use super::commits::{Bump, ChangeEntry, Commit, classify};
use super::{Package, ReleaseModel};
use crate::config::{PINS_FILE, ReleasePins, Versioning};
use crate::error::HoistError;
use crate::release::semver::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Default)]
pub struct History {
    /// Package name -> its last release tag (`None`: never released).
    pub last_tag: HashMap<String, Option<String>>,
    /// Package name -> commits since that tag.
    pub commits: HashMap<String, Vec<Commit>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NextVersion {
    pub package: String,
    pub from: Version,
    pub to: Version,
    pub reason: String,
    pub entries: Vec<ChangeEntry>,
    pub first_release: bool,
}

fn pins_err(message: String) -> HoistError {
    HoistError::Config {
        file: PINS_FILE.to_string(),
        message,
    }
}

fn current(p: &Package) -> Result<Version, HoistError> {
    Version::parse(&p.version.value).map_err(|e| HoistError::Config {
        file: p.version.source.clone(),
        message: format!("{e}; fix the version of `{}`", p.name),
    })
}

/// Dependencies first; ties by name. Cycles fall back to name order.
pub(crate) fn order(m: &ReleaseModel) -> Vec<&Package> {
    let by_name: BTreeMap<&str, &Package> =
        m.packages.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut done: Vec<&Package> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while seen.len() < by_name.len() {
        let ready = by_name.iter().find(|(name, p)| {
            !seen.contains(*name)
                && p.deps
                    .iter()
                    .all(|d| seen.contains(d.as_str()) || !by_name.contains_key(d.as_str()))
        });
        let (name, p) = match ready {
            Some((n, p)) => (*n, *p),
            None => {
                let (n, p) = by_name
                    .iter()
                    .find(|(n, _)| !seen.contains(*n))
                    .expect("unseen package remains");
                (*n, *p)
            }
        };
        seen.insert(name);
        done.push(p);
    }
    done
}

fn reason_for(bump: Bump) -> &'static str {
    match bump {
        Bump::Major => "breaking change",
        Bump::Minor => "new feature",
        Bump::Patch | Bump::None => "fix",
    }
}

/// Plan the next version of every package that releases, dependencies first.
pub fn plan_versions(
    m: &ReleaseModel,
    h: &History,
    pins: &ReleasePins,
) -> Result<Vec<NextVersion>, HoistError> {
    for (name, pin) in &pins.packages {
        let Some(p) = m.packages.iter().find(|p| &p.name == name) else {
            return Err(pins_err(format!(
                "`{name}` is not a releasable package; fix or remove its entry"
            )));
        };
        if let Some(l) = &p.launcher {
            return Err(pins_err(format!(
                "`{name}` is an npm launcher and shares the version of `{}`; pin `{}` instead",
                l.crate_name, l.crate_name
            )));
        }
        if let Some(v) = &pin.version {
            let pinned = Version::parse(v)
                .map_err(|e| pins_err(format!("package `{name}`: {e}; fix the pin")))?;
            if pinned <= current(p)? {
                return Err(pins_err(format!(
                    "package `{name}`: pin {pinned} is not above the current version {}; \
                     raise it or remove the pin",
                    p.version.value
                )));
            }
        }
    }

    // Own bump and entries per package, from its commits.
    let mut own: HashMap<&str, (Bump, Vec<ChangeEntry>)> = HashMap::new();
    for p in m.packages.iter().filter(|p| p.launcher.is_none()) {
        let mut bump = Bump::None;
        let mut entries = Vec::new();
        for c in h.commits.get(&p.name).into_iter().flatten() {
            let (b, e) = classify(c);
            bump = bump.max(b);
            entries.extend(e);
        }
        own.insert(p.name.as_str(), (bump, entries));
    }
    let mut out = if m.versioning == Versioning::Fixed {
        plan_fixed(m, h, pins, own)?
    } else {
        plan_per_package(m, h, pins, own)?
    };
    follow_launchers(m, &mut out)?;
    Ok(out)
}

/// Each launcher releases with its crate, at the crate's new version (spec §9.3).
fn follow_launchers(m: &ReleaseModel, out: &mut Vec<NextVersion>) -> Result<(), HoistError> {
    for p in &m.packages {
        let Some(l) = &p.launcher else { continue };
        let Some(krate) = out.iter().find(|n| n.package == l.crate_name) else {
            continue;
        };
        let next = NextVersion {
            package: p.name.clone(),
            from: current(p)?,
            to: krate.to,
            reason: format!("follows {}", l.crate_name),
            entries: Vec::new(),
            first_release: krate.first_release,
        };
        out.push(next);
    }
    Ok(())
}

/// Per-package versioning: each package by its own commits, dependents get a patch bump.
fn plan_per_package(
    m: &ReleaseModel,
    h: &History,
    pins: &ReleasePins,
    mut own: HashMap<&str, (Bump, Vec<ChangeEntry>)>,
) -> Result<Vec<NextVersion>, HoistError> {
    let mut out: Vec<NextVersion> = Vec::new();
    let mut bumped: BTreeSet<&str> = BTreeSet::new();
    for p in order(m).into_iter().filter(|p| p.launcher.is_none()) {
        let pin = pins.packages.get(&p.name);
        if pin.is_some_and(|x| x.skip) {
            continue;
        }
        let from = current(p)?;
        let (own_bump, entries) = own.remove(p.name.as_str()).unwrap_or((Bump::None, vec![]));
        let first_release = matches!(h.last_tag.get(&p.name), Some(None) | None);
        let mut bump = own_bump;
        let mut reason = reason_for(own_bump).to_string();
        if bump == Bump::None {
            let deps: Vec<&str> = p
                .deps
                .iter()
                .map(String::as_str)
                .filter(|d| bumped.contains(d))
                .collect();
            if !deps.is_empty() {
                bump = Bump::Patch;
                reason = format!("dependency {} bumped", deps.join(", "));
            }
        }
        let (mut to, mut released) = if first_release {
            (from, true)
        } else {
            (from.bump(bump), bump != Bump::None)
        };
        if first_release {
            reason = "first release".to_string();
        }
        if let Some(pinned) = pin
            .and_then(|x| x.version.as_deref())
            .and_then(|v| Version::parse(v).ok())
            && pinned > to
        {
            to = pinned;
            released = true;
            reason = "pinned in .hoist/release.toml".to_string();
        }
        if !released {
            continue;
        }
        if !first_release || to != from {
            bumped.insert(p.name.as_str());
        }
        out.push(NextVersion {
            package: p.name.clone(),
            from,
            to,
            reason,
            entries,
            first_release,
        });
    }
    Ok(out)
}

/// `versioning = "fixed"` (R9, R11): one shared version, the highest current version bumped by
/// the highest bump, or a pin above it. Every package releases at it, tagged or not.
fn plan_fixed(
    m: &ReleaseModel,
    h: &History,
    pins: &ReleasePins,
    mut own: HashMap<&str, (Bump, Vec<ChangeEntry>)>,
) -> Result<Vec<NextVersion>, HoistError> {
    if let Some(name) = pins.packages.iter().find(|(_, x)| x.skip).map(|(n, _)| n) {
        return Err(pins_err(format!(
            "package `{name}`: `skip` is not allowed with versioning = \"fixed\" (one shared \
             version); remove it or use per-package versioning"
        )));
    }
    let fixed_max = own.values().map(|(b, _)| *b).max().unwrap_or(Bump::None);
    let mut highest = None;
    for p in m.packages.iter().filter(|p| p.launcher.is_none()) {
        highest = highest.max(Some(current(p)?));
    }
    let Some(highest) = highest else {
        return Ok(vec![]);
    };
    let all_untagged = m
        .packages
        .iter()
        .filter(|p| p.launcher.is_none())
        .all(|p| matches!(h.last_tag.get(&p.name), Some(None) | None));
    // R12: with no tag yet the first release keeps the highest manifest version.
    let mut shared = if all_untagged {
        highest
    } else {
        highest.bump(fixed_max)
    };
    let pinned = pins
        .packages
        .values()
        .filter_map(|x| x.version.as_deref())
        .filter_map(|v| Version::parse(v).ok())
        .max();
    let pin_applies = pinned.is_some_and(|v| v > shared);
    if let Some(v) = pinned.filter(|_| pin_applies) {
        shared = v;
    }
    if !all_untagged && fixed_max == Bump::None && !pin_applies {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for p in order(m).into_iter().filter(|p| p.launcher.is_none()) {
        let (own_bump, entries) = own.remove(p.name.as_str()).unwrap_or((Bump::None, vec![]));
        let reason = if all_untagged && !pin_applies {
            "first release".to_string()
        } else if own_bump == Bump::None {
            "fixed versioning".to_string()
        } else {
            reason_for(own_bump).to_string()
        };
        out.push(NextVersion {
            package: p.name.clone(),
            from: current(p)?,
            to: shared,
            reason,
            entries,
            first_release: matches!(h.last_tag.get(&p.name), Some(None) | None),
        });
    }
    Ok(out)
}
