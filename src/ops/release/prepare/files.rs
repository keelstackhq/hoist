//! The release commit's edits, computed in memory before anything is written: versions,
//! internal dependents' requirements, the inherited workspace version, changelogs, PR body.

pub(super) use crate::changeset::changelog::Shas;
use crate::changeset::changelog::blocks;
use crate::changeset::plan::Rewrite;
use crate::changeset::range::rewrite;
use crate::changeset::{Changeset, CsConfig, CsType};
use crate::error::HoistError;
use crate::release::edit::{
    prepend_changelog, prepend_section, rewrite_cargo_dep, rewrite_package_json_dep,
    set_cargo_dep_version, set_cargo_version, set_package_json_dep, set_package_json_version,
    today,
};
use crate::release::launcher::check_optional_deps;
use crate::release::planner::NextVersion;
use crate::release::semver::Version;
use crate::release::{DepKind, Package, PkgKind, ReleaseModel};
use crate::repo::targets::{config_err, crate_dirs, join_rel};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The release commit, prepared in memory before anything is written.
pub(super) struct Prepared {
    /// Repo-relative path -> new content.
    pub files: BTreeMap<String, String>,
    /// Files that did not exist before (removed again when a run stops).
    pub created: Vec<String>,
    /// Consumed changesets, deleted in the release commit (changesets spec §8).
    pub deleted: Vec<String>,
    pub npm: bool,
    pub cargo: bool,
    pub title: String,
    pub body: String,
}

/// Changesets mode: the changesets, config, idle rewrites and the sha that added each file.
pub(super) struct Cs {
    pub cfg: CsConfig,
    pub sets: Vec<Changeset>,
    pub idle: Vec<Rewrite>,
    pub shas: Shas,
}

impl Cs {
    /// Every changeset whose packages (types above `none`) all release; empty ones too.
    fn consumed(&self, next: &[NextVersion]) -> Vec<&Changeset> {
        let releases = |n: &String| next.iter().any(|x| x.package == *n);
        let all = |c: &&Changeset| {
            c.releases
                .iter()
                .all(|(n, t)| *t == CsType::None || releases(n))
        };
        self.sets.iter().filter(all).collect()
    }
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

/// Every workspace member (published or not) whose Cargo.toml has `version.workspace = true`:
/// (crate name, Cargo.toml path).
fn inheriting_members(dir: &Path, files: &mut Files) -> Result<Vec<(String, String)>, HoistError> {
    let mut dirs = crate_dirs(dir)?;
    if !dirs.iter().any(String::is_empty) && dir.join("Cargo.toml").exists() {
        // A root `[package]` next to `[workspace]` is a member too.
        dirs.push(String::new());
    }
    let mut out = Vec::new();
    for rel in dirs {
        let file = join_rel(&rel, "Cargo.toml");
        if !dir.join(&file).exists() {
            continue;
        }
        let table: toml::Table = files.get(&file)?.parse().map_err(|e: toml::de::Error| {
            config_err(&file, e.to_string().trim_end().to_string())
        })?;
        let Some(package) = table.get("package") else {
            continue;
        };
        if package.get("version").is_some_and(|v| v.is_table()) {
            let name = package.get("name").and_then(|n| n.as_str()).unwrap_or(&rel);
            out.push((name.to_string(), file));
        }
    }
    out.sort();
    Ok(out)
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

/// R15/R16: the inherited version moves only when every member inheriting it, published or
/// not, gets the same new version; otherwise exit 3 naming them, before any write.
fn bump_inherited(
    files: &mut Files,
    model: &ReleaseModel,
    next: &[NextVersion],
    inheriting: &[(String, String)],
) -> Result<(), HoistError> {
    let new_version = |name: &str| {
        next.iter()
            .find(|n| {
                n.package == name
                    && model
                        .packages
                        .iter()
                        .any(|p| p.name == name && p.kind == PkgKind::Crate)
            })
            .map(|n| n.to.to_string())
    };
    let targets: Vec<(&str, Option<String>)> = inheriting
        .iter()
        .map(|(name, _)| (name.as_str(), new_version(name)))
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
                    "`[workspace.package] version` is inherited by crates that would not all \
                     get the same version ({}); give each crate its own `version`, release \
                     them together (versioning = \"fixed\") or pin them in .hoist/release.toml",
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

/// The new CHANGELOG text of `nv` (commits or changesets layout).
fn changelog(
    existing: Option<&str>,
    nv: &NextVersion,
    next: &[NextVersion],
    cs: Option<&Cs>,
    date: &str,
) -> String {
    match cs {
        Some(cs) => {
            let b = blocks(nv, next, &cs.sets, &cs.shas);
            prepend_section(existing, &nv.to.to_string(), date, &b, true)
        }
        None => prepend_changelog(existing, nv, date),
    }
}

fn changelog_section(
    p: &Package,
    nv: &NextVersion,
    next: &[NextVersion],
    cs: Option<&Cs>,
    date: &str,
) -> String {
    let text = changelog(None, nv, next, cs, date);
    let section = text.strip_prefix("# Changelog\n\n").unwrap_or(&text);
    match section.strip_prefix("## ") {
        Some(rest) => format!("### {} {rest}", p.name),
        None => section.to_string(),
    }
    .replace("\n### ", "\n#### ")
}

fn pr_body(model: &ReleaseModel, next: &[NextVersion], cs: Option<&Cs>, date: &str) -> String {
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
        let version = if n.from == n.to {
            format!("{} (first release)", n.to)
        } else {
            format!("{} → {}", n.from, n.to)
        };
        if let Some(l) = &p.launcher {
            body.push_str(&format!(
                "| `{}` | {version} | none (npm launcher of `{}`) |\n",
                n.package, l.crate_name
            ));
            continue;
        }
        let tag = model.tag_for(p, &n.to.to_string());
        body.push_str(&format!("| `{}` | {version} | `{tag}` |\n", n.package));
        sections.push_str(&format!("\n{}", changelog_section(p, n, next, cs, date)));
        tags.push_str(&format!("- `{tag}`\n"));
    }
    if let Some(cs) = cs {
        body.push_str("\n## Changesets\n\n");
        for n in next.iter().filter(|n| !n.changesets.is_empty()) {
            body.push_str(&format!("- `{}`: {}\n", n.package, n.changesets.join(", ")));
        }
        for c in cs.consumed(next).iter().filter(|c| c.releases.is_empty()) {
            body.push_str(&format!("- no package: {}\n", c.id));
        }
    }
    format!("{body}\n## Changelog\n{sections}\n## Tags to create\n\n{tags}")
}

/// Every edit of the release commit, in memory. Errors here are exit 3 (nothing written yet).
pub(super) fn prepare_files(
    dir: &Path,
    model: &ReleaseModel,
    next: &[NextVersion],
    cs: Option<&Cs>,
) -> Result<Prepared, HoistError> {
    let mut files = Files {
        root: dir,
        map: BTreeMap::new(),
    };
    let find = |name: &str| model.packages.iter().find(|p| p.name == name);
    let inheriting = inheriting_members(dir, &mut files)?;
    bump_inherited(&mut files, model, next, &inheriting)?;
    for n in next {
        let Some(p) = find(&n.package) else { continue };
        let v = n.to.to_string();
        match p.kind {
            PkgKind::Npm => files.edit(&manifest(p), |t| set_package_json_version(t, &v))?,
            PkgKind::Crate if inheriting.iter().any(|(_, f)| *f == manifest(p)) => {}
            PkgKind::Crate => files.edit(&manifest(p), |t| set_cargo_version(t, &v))?,
        }
    }
    // Launchers: platform package versions and the launcher's optionalDependencies.
    for n in next {
        let Some(p) = find(&n.package) else { continue };
        let Some(l) = &p.launcher else { continue };
        let file = manifest(p);
        check_optional_deps(&file, &files.get(&file)?, p)?;
        let v = n.to.to_string();
        for plat in &l.platforms {
            files.edit(&manifest(&plat.pkg), |t| set_package_json_version(t, &v))?;
            files.edit(&manifest(p), |t| {
                set_package_json_dep(t, &plat.pkg.name, &v)
            })?;
        }
    }
    if let Some(cs) = cs {
        rewrite_ranges(&mut files, model, next, cs)?;
    }
    // Internal dependents' requirements (commits mode).
    for n in next.iter().filter(|n| n.from != n.to && cs.is_none()) {
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
        if p.launcher.is_some() || cs.is_some_and(|c| !c.cfg.changelog) {
            continue; // a launcher has no changelog of its own
        }
        let rel = join_rel(&p.dir.to_string_lossy(), "CHANGELOG.md");
        let existing = match files.get(&rel) {
            Ok(t) => Some(t),
            Err(_) if !dir.join(&rel).exists() => {
                created.push(rel.clone());
                None
            }
            Err(e) => return Err(e),
        };
        let text = changelog(existing.as_deref(), n, next, cs, &date);
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
    let deleted = cs.map_or(vec![], |cs| {
        let ids = cs.consumed(next).into_iter();
        ids.map(|c| format!(".changeset/{}.md", c.id)).collect()
    });
    Ok(Prepared {
        files: files.map,
        created,
        deleted,
        npm,
        cargo,
        title: format!("release: {}", list.join(", ")),
        body: pr_body(model, next, cs, &date),
    })
}

/// Changesets mode: rewrite the ranges the planner chose (spec §5.2), keeping operators, in
/// releasing packages (`dep_updates`) and idle ones (dev edges), only in the tables of that
/// edge kind; root `[workspace.dependencies]` for `workspace = true` entries.
fn rewrite_ranges(
    files: &mut Files,
    model: &ReleaseModel,
    next: &[NextVersion],
    cs: &Cs,
) -> Result<(), HoistError> {
    let updates = next.iter().flat_map(|n| {
        let pkg = n.package.clone();
        let each = n.dep_updates.iter();
        each.map(move |(d, v, k)| (pkg.clone(), d.clone(), v.clone(), *k))
    });
    let mut root = Vec::new();
    for (pkg, dep, v, kind) in updates.chain(cs.idle.iter().cloned()) {
        let Some(p) = model.packages.iter().find(|p| p.name == pkg) else {
            continue;
        };
        let ver = Version::parse(&v).map_err(|e| config_err(&manifest(p), e.to_string()))?;
        let f = |old: &str| rewrite(old, &ver);
        match p.kind {
            PkgKind::Npm => {
                let sections: &[&str] = match kind {
                    DepKind::Normal | DepKind::Build => &["dependencies", "optionalDependencies"],
                    DepKind::Peer => &["peerDependencies"],
                    DepKind::Dev => &["devDependencies"],
                };
                files.edit(&manifest(p), |t| {
                    rewrite_package_json_dep(t, &dep, sections, f)
                })?
            }
            PkgKind::Crate => {
                let table = match kind {
                    DepKind::Normal | DepKind::Peer => "dependencies",
                    DepKind::Build => "build-dependencies",
                    DepKind::Dev => "dev-dependencies",
                };
                let mut inherits = false;
                files.edit(&manifest(p), |t| {
                    let (text, ws) = rewrite_cargo_dep(t, &dep, &[table], &f)?;
                    inherits = ws;
                    Ok(text)
                })?;
                if inherits && workspace_dep(files, &dep) && !root.contains(&dep) {
                    let all = ["dependencies"];
                    files.edit(
                        "Cargo.toml",
                        |t| Ok(rewrite_cargo_dep(t, &dep, &all, &f)?.0),
                    )?;
                    root.push(dep);
                }
            }
        }
    }
    Ok(())
}
