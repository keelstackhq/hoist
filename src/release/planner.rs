//! The version planner: which packages release, and at which version. Pure.

use super::commits::{Bump, ChangeEntry, Commit, classify};
use super::{Package, ReleaseModel};
use crate::config::{ReleasePins, Versioning};
use crate::error::HoistError;
use crate::release::semver::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const PINS_FILE: &str = ".hoist/release.toml";

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

fn config_err(message: String) -> HoistError {
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
fn order(m: &ReleaseModel) -> Vec<&Package> {
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
        _ => "fix",
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
            return Err(config_err(format!(
                "`{name}` is not a releasable package; fix or remove its entry"
            )));
        };
        if let Some(v) = &pin.version {
            let pinned = Version::parse(v)
                .map_err(|e| config_err(format!("package `{name}`: {e}; fix the pin")))?;
            if pinned <= current(p)? {
                return Err(config_err(format!(
                    "package `{name}`: pin {pinned} is not above the current version {}; \
                     raise it or remove the pin",
                    p.version.value
                )));
            }
        }
    }

    // Own bump and entries per package, from its commits.
    let mut own: HashMap<&str, (Bump, Vec<ChangeEntry>)> = HashMap::new();
    for p in &m.packages {
        let mut bump = Bump::None;
        let mut entries = Vec::new();
        for c in h.commits.get(&p.name).into_iter().flatten() {
            let (b, e) = classify(c);
            bump = bump.max(b);
            entries.extend(e);
        }
        own.insert(p.name.as_str(), (bump, entries));
    }
    let fixed_max = match m.versioning {
        Versioning::Fixed => own.values().map(|(b, _)| *b).max().unwrap_or(Bump::None),
        Versioning::PerPackage => Bump::None,
    };

    let mut out: Vec<NextVersion> = Vec::new();
    let mut bumped: BTreeSet<&str> = BTreeSet::new();
    for p in order(m) {
        let from = current(p)?;
        let pin = pins.packages.get(&p.name);
        if pin.is_some_and(|x| x.skip) {
            continue;
        }
        let (own_bump, entries) = own.remove(p.name.as_str()).unwrap_or((Bump::None, vec![]));
        let first_release = matches!(h.last_tag.get(&p.name), Some(None) | None);
        let mut bump = own_bump;
        let mut reason = reason_for(own_bump).to_string();
        if m.versioning == Versioning::Fixed && fixed_max > bump {
            bump = fixed_max;
            reason = "fixed versioning".to_string();
        }
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
