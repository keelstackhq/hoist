//! npm launchers (spec §9.3): a launcher package plus one platform package per release
//! target of the crate that builds its binary.

use super::{Package, PkgKind, json_key_line, parse_version};
use crate::config::{NpmPublish, PublishCfg};
use crate::error::HoistError;
use crate::repo::targets::{config_err, join_rel};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// An npm launcher: it carries no binary itself, its platform packages do. It shares the
/// version of crate `crate_name`, releases whenever that crate does and has no tag, changelog
/// or GitHub Release of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Launcher {
    /// The crate whose `github_release.bin` (or name) is the launcher's `bin` command.
    pub crate_name: String,
    /// The command name: the single key of `bin` in package.json (or the unscoped name).
    pub bin: String,
    /// One per `github_release` target of the crate, in target order.
    pub platforms: Vec<Platform>,
}

/// A platform package of a launcher: `<launcher dir>-<platform>` next to the launcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Platform {
    pub target: String,
    /// npm's `<os>-<cpu>`, e.g. `linux-x64`.
    pub platform: String,
    /// Never released on its own; published by the launcher's release.
    pub pkg: Package,
}

impl Platform {
    /// `bin/<bin>[.exe]` inside the platform package.
    pub fn bin_file(&self, bin: &str) -> String {
        format!("bin/{bin}{}", exe_suffix(&self.target))
    }
}

/// `.exe` for Windows targets, else empty.
pub fn exe_suffix(target: &str) -> &'static str {
    if target.contains("-windows-") {
        ".exe"
    } else {
        ""
    }
}

/// The npm platform (`process.platform`-`process.arch`) of a release target.
pub fn npm_platform(target: &str) -> Option<&'static str> {
    Some(match target {
        "x86_64-unknown-linux-musl" => "linux-x64",
        "aarch64-unknown-linux-musl" => "linux-arm64",
        "x86_64-apple-darwin" => "darwin-x64",
        "aarch64-apple-darwin" => "darwin-arm64",
        "x86_64-pc-windows-msvc" => "win32-x64",
        _ => return None,
    })
}

const SUPPORTED_TARGETS: &str = "x86_64-unknown-linux-musl, aarch64-unknown-linux-musl, \
     x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc";

/// The command a launcher installs: the single key of `bin`, or the unscoped package name
/// when `bin` is a string.
fn launcher_bin(root: &Path, p: &Package) -> Result<String, HoistError> {
    let file = join_rel(&p.dir.to_string_lossy(), "package.json");
    let text = std::fs::read_to_string(root.join(&file))
        .map_err(|e| config_err(&file, format!("cannot read file: {e}")))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| config_err(&file, format!("invalid JSON: {e}")))?;
    match value.get("bin") {
        Some(Value::String(_)) => Ok(p.name.rsplit('/').next().unwrap_or(&p.name).to_string()),
        Some(Value::Object(map)) if map.len() == 1 => {
            Ok(map.keys().next().cloned().unwrap_or_default())
        }
        _ => Err(config_err(
            &file,
            format!(
                "launcher `{}` needs a `bin` with exactly one command (the binary it runs)",
                p.name
            ),
        )),
    }
}

/// Read a platform package: its package.json must exist with a `name` and a `version`.
fn platform_pkg(
    root: &Path,
    rel: &str,
    launcher: &Package,
    target: &str,
) -> Result<Package, HoistError> {
    let file = join_rel(rel, "package.json");
    let Ok(text) = std::fs::read_to_string(root.join(&file)) else {
        return Err(config_err(
            &file,
            format!(
                "platform package `{file}` of launcher `{}` (target `{target}`) is missing; \
                 create it with `name`, `version`, `os` and `cpu`",
                launcher.name
            ),
        ));
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| config_err(&file, format!("invalid JSON: {e}")))?;
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return Err(config_err(&file, "platform package has no `name`; add one"));
    };
    let Some(version) = value.get("version").and_then(Value::as_str) else {
        return Err(config_err(
            &file,
            "platform package has no `version`; add one",
        ));
    };
    let source = format!("{file}:{}", json_key_line(&text, "version"));
    let access = launcher.publish.npm.clone().unwrap_or_default().access;
    Ok(Package {
        name: name.to_string(),
        kind: PkgKind::Npm,
        dir: PathBuf::from(rel),
        version: parse_version(&file, version, source)?,
        deps: Vec::new(),
        publish: PublishCfg {
            npm: Some(NpmPublish {
                access,
                launcher: false,
            }),
            ..PublishCfg::default()
        },
        launcher: None,
    })
}

/// Attach each launcher's crate and platform packages, then drop the platform packages from
/// the model (they are never released on their own) and from every dependency list.
pub(super) fn resolve_launchers(
    root: &Path,
    packages: &mut Vec<Package>,
) -> Result<(), HoistError> {
    let mut claimed: Vec<String> = Vec::new();
    for i in 0..packages.len() {
        let p = &packages[i];
        if p.kind != PkgKind::Npm || !p.publish.npm.as_ref().is_some_and(|n| n.launcher) {
            continue;
        }
        let rel = p.dir.to_string_lossy().into_owned();
        let toml_file = join_rel(&rel, "hoist.toml");
        let (Some(parent), Some(dir_name)) = (p.dir.parent(), p.dir.file_name()) else {
            return Err(config_err(
                &toml_file,
                format!(
                    "launcher `{}` must live in its own directory (e.g. npm/<name>)",
                    p.name
                ),
            ));
        };
        let bin = launcher_bin(root, p)?;
        let Some(krate) = packages.iter().find(|c| {
            c.kind == PkgKind::Crate
                && c.publish
                    .github_release
                    .as_ref()
                    .and_then(|g| g.bin.as_deref())
                    .unwrap_or(&c.name)
                    == bin
        }) else {
            return Err(config_err(
                &toml_file,
                format!(
                    "launcher `{}` runs `{bin}`, but no crate has `github_release.bin` (or name) \
                     `{bin}`; add that crate's `[publish] github_release`",
                    p.name
                ),
            ));
        };
        let targets = krate
            .publish
            .github_release
            .as_ref()
            .map(|g| g.targets.clone())
            .unwrap_or_default();
        let crate_toml = join_rel(&krate.dir.to_string_lossy(), "hoist.toml");
        if targets.is_empty() {
            return Err(config_err(
                &crate_toml,
                format!(
                    "launcher `{}` needs binaries, but crate `{}` has no `github_release.targets`",
                    p.name, krate.name
                ),
            ));
        }
        let mut platforms = Vec::new();
        for target in &targets {
            let Some(platform) = npm_platform(target) else {
                return Err(config_err(
                    &crate_toml,
                    format!(
                        "target `{target}` of crate `{}` has no npm platform for launcher `{}`; \
                         supported targets: {SUPPORTED_TARGETS}",
                        krate.name, p.name
                    ),
                ));
            };
            let dir = parent.join(format!("{}-{platform}", dir_name.to_string_lossy()));
            let dir = dir.to_string_lossy().into_owned();
            platforms.push(Platform {
                target: target.clone(),
                platform: platform.to_string(),
                pkg: platform_pkg(root, &dir, p, target)?,
            });
            claimed.push(dir);
        }
        let launcher = Launcher {
            crate_name: krate.name.clone(),
            bin,
            platforms,
        };
        packages[i].launcher = Some(launcher);
    }
    let dropped: Vec<String> = packages
        .iter()
        .filter(|p| {
            p.kind == PkgKind::Npm && claimed.contains(&p.dir.to_string_lossy().into_owned())
        })
        .map(|p| p.name.clone())
        .collect();
    packages.retain(|p| !(p.kind == PkgKind::Npm && dropped.contains(&p.name)));
    for p in packages.iter_mut().filter(|p| p.kind == PkgKind::Npm) {
        p.deps.retain(|d| !dropped.contains(d));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{exe_suffix, npm_platform};

    #[test]
    fn targets_map_to_npm_platforms() {
        assert_eq!(npm_platform("x86_64-unknown-linux-musl"), Some("linux-x64"));
        assert_eq!(
            npm_platform("aarch64-unknown-linux-musl"),
            Some("linux-arm64")
        );
        assert_eq!(npm_platform("x86_64-apple-darwin"), Some("darwin-x64"));
        assert_eq!(npm_platform("aarch64-apple-darwin"), Some("darwin-arm64"));
        assert_eq!(npm_platform("x86_64-pc-windows-msvc"), Some("win32-x64"));
        assert_eq!(npm_platform("x86_64-unknown-linux-gnu"), None);
        assert_eq!(exe_suffix("x86_64-pc-windows-msvc"), ".exe");
        assert_eq!(exe_suffix("aarch64-apple-darwin"), "");
    }
}
