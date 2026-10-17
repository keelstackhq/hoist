//! The release model: packages, their versions and internal dependencies (spec §4, §9).

pub mod commits;
pub mod edit;
pub mod github_rel;
pub mod gitlog;
pub mod planner;
pub mod registry;
pub mod semver;
pub mod tools;

use crate::config::{
    CratesPublish, GithubRelease, NpmPublish, PublishCfg, Versioning, load_root, load_target,
};
use crate::error::HoistError;
use crate::repo::Sourced;
use crate::repo::targets::{config_err, crate_dirs, join_rel, npm_dirs, read_crate, read_npm};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgKind {
    Npm,
    Crate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: String,
    pub kind: PkgKind,
    /// Repo-relative; empty for the repo root.
    pub dir: PathBuf,
    pub version: Sourced<String>,
    /// Names of the model's packages (same kind) this one depends on, sorted.
    pub deps: Vec<String>,
    pub publish: PublishCfg,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseModel {
    pub packages: Vec<Package>,
    pub versioning: Versioning,
    pub tag_format: String,
    pub environment: String,
}

impl ReleaseModel {
    pub fn tag_for(&self, p: &Package, version: &str) -> String {
        self.tag_format
            .replace("{name}", &p.name)
            .replace("{version}", version)
    }
}

/// 1-based line of the top-level `key` of a JSON object, 1 when not found.
fn json_key_line(text: &str, key: &str) -> usize {
    let (mut depth, mut line) = (0usize, 1usize);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            '"' => {
                let start = line;
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            s.push(c);
                            if let Some(n) = chars.next() {
                                s.push(n);
                            }
                        }
                        '"' => break,
                        '\n' => {
                            line += 1;
                            s.push(c);
                        }
                        _ => s.push(c),
                    }
                }
                if depth == 1 && s == key {
                    let mut look = chars.clone();
                    let next = look.by_ref().find(|c| !c.is_whitespace());
                    if next == Some(':') {
                        return start;
                    }
                }
            }
            _ => {}
        }
    }
    1
}

/// 1-based line of `key = ` inside `[section]` of a TOML file, 1 when not found.
fn toml_key_line(text: &str, section: &str, key: &str) -> usize {
    let mut current = String::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            current = h.trim().to_string();
        } else if current == section
            && let Some(rest) = t.strip_prefix(key)
            && rest.trim_start().starts_with('=')
        {
            return i + 1;
        }
    }
    1
}

fn parse_version(file: &str, text: &str, source: String) -> Result<Sourced<String>, HoistError> {
    semver::Version::parse(text)
        .map_err(|m| config_err(file, m))
        .map(|v| Sourced {
            value: v.to_string(),
            source,
        })
}

fn string_list(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

struct Raw {
    name: String,
    kind: PkgKind,
    rel: String,
    version: Sourced<String>,
    deps: Vec<String>,
    targets: Vec<String>,
}

fn raw_npm(root: &Path, rel: &str) -> Result<Option<Raw>, HoistError> {
    let Some(name) = read_npm(root, rel)? else {
        return Ok(None);
    };
    let file = join_rel(rel, "package.json");
    let text = std::fs::read_to_string(root.join(&file))
        .map_err(|e| config_err(&file, format!("cannot read file: {e}")))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| config_err(&file, format!("invalid JSON: {e}")))?;
    let Some(version) = value.get("version").and_then(Value::as_str) else {
        return Err(config_err(
            &file,
            "public package has no `version`; add one (MAJOR.MINOR.PATCH)",
        ));
    };
    let source = format!("{file}:{}", json_key_line(&text, "version"));
    let mut deps = BTreeSet::new();
    for field in ["dependencies", "optionalDependencies", "peerDependencies"] {
        if let Some(map) = value.get(field).and_then(Value::as_object) {
            deps.extend(map.keys().cloned());
        }
    }
    Ok(Some(Raw {
        name: name.value,
        kind: PkgKind::Npm,
        rel: rel.to_string(),
        version: parse_version(&file, version, source)?,
        deps: deps.into_iter().collect(),
        targets: Vec::new(),
    }))
}

fn raw_crate(root: &Path, rel: &str) -> Result<Option<Raw>, HoistError> {
    let Some(name) = read_crate(root, rel)? else {
        return Ok(None);
    };
    let file = join_rel(rel, "Cargo.toml");
    let text = std::fs::read_to_string(root.join(&file))
        .map_err(|e| config_err(&file, format!("cannot read file: {e}")))?;
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(&file, e.to_string().trim_end().to_string()))?;
    let package = &table["package"];
    let (version, source) = match package.get("version") {
        Some(toml::Value::String(v)) => (
            v.clone(),
            format!("{file}:{}", toml_key_line(&text, "package", "version")),
        ),
        Some(_) => inherited_version(root, &file)?,
        None => return Ok(None),
    };
    let ws_path_deps: BTreeSet<String> = std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("workspace")?
                .get("dependencies")?
                .as_table()
                .map(|m| {
                    m.iter()
                        .filter(|(_, d)| d.get("path").is_some())
                        .map(|(k, _)| k.clone())
                        .collect()
                })
        })
        .unwrap_or_default();
    let mut deps = BTreeSet::new();
    if let Some(map) = table.get("dependencies").and_then(|d| d.as_table()) {
        for (key, dep) in map {
            if let Some(dep) = dep.as_table()
                && (dep.contains_key("path")
                    || (dep.get("workspace").and_then(|w| w.as_bool()) == Some(true)
                        && ws_path_deps.contains(key)))
            {
                let real = dep.get("package").and_then(|p| p.as_str()).unwrap_or(key);
                deps.insert(real.to_string());
            }
        }
    }
    let targets = string_list(
        package
            .get("metadata")
            .and_then(|m| m.get("hoist"))
            .and_then(|h| h.get("targets")),
    );
    let version_file = source
        .rsplit_once(':')
        .map_or(file.as_str(), |(f, _)| f)
        .to_string();
    Ok(Some(Raw {
        name: name.value,
        kind: PkgKind::Crate,
        rel: rel.to_string(),
        version: parse_version(&version_file, &version, source)?,
        deps: deps.into_iter().collect(),
        targets,
    }))
}

/// `version.workspace = true`: take `[workspace.package] version` of the root `Cargo.toml`.
fn inherited_version(root: &Path, file: &str) -> Result<(String, String), HoistError> {
    let root_file = "Cargo.toml";
    let text = std::fs::read_to_string(root.join(root_file)).unwrap_or_default();
    let table: toml::Table = text.parse().unwrap_or_default();
    match table
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        Some(v) => Ok((
            v.to_string(),
            format!(
                "{root_file}:{}",
                toml_key_line(&text, "workspace.package", "version")
            ),
        )),
        None => Err(config_err(
            file,
            "`version` is inherited but the root `Cargo.toml` has no `[workspace.package] version`",
        )),
    }
}

fn effective_publish(raw: &Raw, configured: Option<PublishCfg>) -> PublishCfg {
    let mut cfg = configured.unwrap_or_default();
    match raw.kind {
        PkgKind::Npm => {
            cfg.npm.get_or_insert_with(NpmPublish::default);
        }
        PkgKind::Crate => {
            cfg.crates.get_or_insert(CratesPublish {});
        }
    }
    if !raw.targets.is_empty() {
        let gr = cfg
            .github_release
            .get_or_insert_with(GithubRelease::default);
        if gr.targets.is_empty() {
            gr.targets = raw.targets.clone();
        }
    }
    cfg
}

/// Read every publishable package with its version, internal deps and `[publish]` settings.
pub fn read_release_model(dir: &Path) -> Result<ReleaseModel, HoistError> {
    let root_cfg = load_root(dir)?;
    let mut raws = Vec::new();
    for rel in npm_dirs(dir)? {
        raws.extend(raw_npm(dir, &rel)?);
    }
    for rel in crate_dirs(dir)? {
        raws.extend(raw_crate(dir, &rel)?);
    }
    let root_is_package = raws.iter().any(|r| r.rel.is_empty());
    if root_cfg.publish.is_some() && !root_is_package {
        return Err(config_err(
            "hoist.toml",
            "`[publish]` has no root package here; put it in the package's own hoist.toml",
        ));
    }
    let mut packages = Vec::new();
    for raw in &raws {
        let configured = if raw.rel.is_empty() {
            root_cfg.publish.clone()
        } else {
            load_target(&dir.join(&raw.rel))?.publish
        };
        let deps = raw
            .deps
            .iter()
            .filter(|d| raws.iter().any(|o| o.kind == raw.kind && &o.name == *d))
            .cloned()
            .collect();
        packages.push(Package {
            name: raw.name.clone(),
            kind: raw.kind,
            dir: PathBuf::from(&raw.rel),
            version: raw.version.clone(),
            deps,
            publish: effective_publish(raw, configured),
        });
    }
    if let Some(f) = &root_cfg.release.tag_format
        && !f.contains("{version}")
    {
        return Err(config_err(
            "hoist.toml",
            format!("invalid `release.tag_format` `{f}`: it must contain `{{version}}`"),
        ));
    }
    if let Some(f) = &root_cfg.release.tag_format
        && !f.contains("{name}")
        && packages.len() > 1
        && root_cfg.release.versioning == Versioning::PerPackage
    {
        return Err(config_err(
            "hoist.toml",
            format!(
                "invalid `release.tag_format` `{f}`: with several packages and per-package \
                 versioning it must contain `{{name}}`, or their tags collide"
            ),
        ));
    }
    let tag_format = root_cfg.release.tag_format.clone().unwrap_or_else(|| {
        if root_is_package {
            "v{version}"
        } else {
            "{name}@{version}"
        }
        .to_string()
    });
    Ok(ReleaseModel {
        packages,
        versioning: root_cfg.release.versioning,
        tag_format,
        environment: root_cfg.release.environment,
    })
}
