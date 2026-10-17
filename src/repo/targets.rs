//! Infer deploy targets, publishable npm packages and crates (spec §4).

use super::Sourced;
use crate::HoistError;
use crate::config::{TargetConfig, load_target};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    CloudflarePages,
    CloudflareWorkers,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub name: String,
    pub dir: PathBuf,
    pub kind: Sourced<TargetKind>,
    pub config: TargetConfig,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Packages {
    pub npm: Vec<Sourced<String>>,
    pub crates: Vec<Sourced<String>>,
}

pub(crate) fn config_err(file: &str, message: impl Into<String>) -> HoistError {
    HoistError::Config {
        file: file.to_string(),
        message: message.into(),
    }
}

pub(crate) fn read_optional(path: &Path, rel: &str) -> Result<Option<String>, HoistError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(config_err(
            rel,
            format!("cannot read file: {e}; check its permissions"),
        )),
    }
}

/// Repo-relative directories matching `globs`. Only literal paths and a trailing `/*` are
/// supported (what hull renders); anything else is a config error naming the glob.
fn expand(root: &Path, file: &str, globs: &[String]) -> Result<Vec<String>, HoistError> {
    let mut out = BTreeSet::new();
    for glob in globs {
        let pattern = glob.trim_start_matches("./").trim_end_matches('/');
        let unsupported = || {
            config_err(
                file,
                format!(
                    "unsupported workspace glob `{glob}`; use literal paths or a trailing `/*`"
                ),
            )
        };
        if pattern.is_empty() || pattern.starts_with('!') {
            return Err(unsupported());
        }
        match pattern.strip_suffix("/*") {
            Some(parent) => {
                if parent.contains(['*', '?', '[', '{']) {
                    return Err(unsupported());
                }
                let Ok(entries) = fs::read_dir(root.join(parent)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        out.insert(format!("{parent}/{}", entry.file_name().to_string_lossy()));
                    }
                }
            }
            None => {
                if pattern.contains(['*', '?', '[', '{']) {
                    return Err(unsupported());
                }
                if root.join(pattern).is_dir() {
                    out.insert(pattern.to_string());
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

fn pnpm_dirs(root: &Path) -> Result<Vec<String>, HoistError> {
    let file = "pnpm-workspace.yaml";
    let Some(text) = read_optional(&root.join(file), file)? else {
        return Ok(Vec::new());
    };
    let value: yaml_serde::Value =
        yaml_serde::from_str(&text).map_err(|e| config_err(file, e.to_string()))?;
    let globs: Vec<String> = value
        .get("packages")
        .and_then(|p| p.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    expand(root, file, &globs)
}

fn cargo_dirs(root: &Path) -> Result<Vec<String>, HoistError> {
    let file = "Cargo.toml";
    let Some(text) = read_optional(&root.join(file), file)? else {
        return Ok(Vec::new());
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(file, e.to_string().trim_end().to_string()))?;
    let globs: Vec<String> = table
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    expand(root, file, &globs)
}

fn provider_kind(provider: &str, file: &str) -> Result<TargetKind, HoistError> {
    match provider {
        "cloudflare-pages" => Ok(TargetKind::CloudflarePages),
        "cloudflare-workers" => Ok(TargetKind::CloudflareWorkers),
        other => Err(config_err(
            file,
            format!(
                "unknown deploy provider `{other}`; use `cloudflare-pages` or `cloudflare-workers`"
            ),
        )),
    }
}

fn read_target(root: &Path, rel: &str) -> Result<Option<Target>, HoistError> {
    let dir = root.join(rel);
    let wrangler_rel = format!("{rel}/wrangler.jsonc");
    let wrangler = read_optional(&dir.join("wrangler.jsonc"), &wrangler_rel)?;
    let inferred = match &wrangler {
        Some(text) => {
            let value: Value = jsonc_parser::parse_to_serde_value(text, &Default::default())
                .map_err(|e| config_err(&wrangler_rel, format!("invalid JSONC: {e}")))?;
            if value.get("main").is_some() {
                Some(TargetKind::CloudflareWorkers)
            } else if value.get("assets").is_some() {
                Some(TargetKind::CloudflarePages)
            } else {
                None
            }
        }
        None => None,
    };
    let config = load_target(&dir)?;
    let provider = config.deploy.as_ref().and_then(|d| d.provider.as_deref());
    let kind = match (provider, inferred) {
        (Some(p), _) => Sourced {
            value: provider_kind(p, &format!("{rel}/hoist.toml"))?,
            source: format!("{rel}/hoist.toml:deploy.provider"),
        },
        (None, Some(kind)) => Sourced {
            value: kind,
            source: format!("{wrangler_rel}:1"),
        },
        (None, None) => return Ok(None),
    };
    let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
    Ok(Some(Target {
        name,
        dir,
        kind,
        config,
    }))
}

/// `<rel>/<name>`, or just `<name>` for the repo root (`rel` is empty).
pub(crate) fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

/// npm package directories: workspace members, or the root alone when it is no workspace.
pub(crate) fn npm_dirs(root: &Path) -> Result<Vec<String>, HoistError> {
    if root.join("pnpm-workspace.yaml").is_file() {
        return pnpm_dirs(root);
    }
    let file = "package.json";
    let Some(text) = read_optional(&root.join(file), file)? else {
        return Ok(Vec::new());
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| config_err(file, format!("invalid JSON: {e}")))?;
    Ok(if value.get("workspaces").is_none() {
        vec![String::new()]
    } else {
        Vec::new()
    })
}

/// Crate directories: workspace members, or the root alone when it has `[package]` and no
/// `[workspace]`.
pub(crate) fn crate_dirs(root: &Path) -> Result<Vec<String>, HoistError> {
    let file = "Cargo.toml";
    let Some(text) = read_optional(&root.join(file), file)? else {
        return Ok(Vec::new());
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(file, e.to_string().trim_end().to_string()))?;
    if table.contains_key("workspace") {
        return cargo_dirs(root);
    }
    Ok(if table.contains_key("package") {
        vec![String::new()]
    } else {
        Vec::new()
    })
}

pub(crate) fn read_npm(root: &Path, rel: &str) -> Result<Option<Sourced<String>>, HoistError> {
    let file = join_rel(rel, "package.json");
    let Some(text) = read_optional(&root.join(&file), &file)? else {
        return Ok(None);
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| config_err(&file, format!("invalid JSON: {e}")))?;
    let public = value.get("private").and_then(Value::as_bool) == Some(false)
        || value.get("publishConfig").is_some();
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(public.then(|| Sourced {
        value: name.to_string(),
        source: format!("{file}:1"),
    }))
}

pub(crate) fn read_crate(root: &Path, rel: &str) -> Result<Option<Sourced<String>>, HoistError> {
    let file = join_rel(rel, "Cargo.toml");
    let Some(text) = read_optional(&root.join(&file), &file)? else {
        return Ok(None);
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(&file, e.to_string().trim_end().to_string()))?;
    let Some(package) = table.get("package") else {
        return Ok(None);
    };
    let unpublished = match package.get("publish") {
        Some(toml::Value::Boolean(b)) => !b,
        Some(toml::Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    // Cargo >= 1.75: a package without `version` cannot be published.
    let unpublished = unpublished || package.get("version").is_none();
    let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
        return Ok(None);
    };
    Ok((!unpublished).then(|| Sourced {
        value: name.to_string(),
        source: format!("{file}:1"),
    }))
}

/// Deploy targets, publishable packages and whether `renovate.json` exists.
pub fn read_targets(dir: &Path) -> Result<(Vec<Target>, Packages, bool), HoistError> {
    let mut targets = Vec::new();
    let mut packages = Packages::default();
    for rel in pnpm_dirs(dir)? {
        if let Some(target) = read_target(dir, &rel)? {
            targets.push(target);
        }
    }
    for rel in npm_dirs(dir)? {
        if let Some(npm) = read_npm(dir, &rel)? {
            packages.npm.push(npm);
        }
    }
    for rel in crate_dirs(dir)? {
        if let Some(krate) = read_crate(dir, &rel)? {
            packages.crates.push(krate);
        }
    }
    let renovate = dir.join("renovate.json").is_file();
    Ok((targets, packages, renovate))
}
