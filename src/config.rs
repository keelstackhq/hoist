//! `hoist.toml` loading (spec §5). Unknown keys are an error naming the file and key.

use crate::error::HoistError;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::path::Path;

const FILE_NAME: &str = "hoist.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Managed {
    #[default]
    Hoist,
    External,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GithubCfg {
    pub visibility: Option<String>,
    pub required_checks: Option<Vec<String>>,
    pub merge_queue: bool,
    pub actions_permissions: String,
    pub allow_auto_merge: bool,
    pub delete_branch_on_merge: bool,
    pub managed: Managed,
}

impl Default for GithubCfg {
    fn default() -> Self {
        Self {
            visibility: None,
            required_checks: None,
            merge_queue: false,
            actions_permissions: "read".to_string(),
            allow_auto_merge: true,
            delete_branch_on_merge: true,
            managed: Managed::Hoist,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CloudflareCfg {
    pub account_id: Option<String>,
    pub managed: Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Versioning {
    #[default]
    PerPackage,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReleaseCfg {
    pub versioning: Versioning,
    pub tag_format: Option<String>,
    pub environment: String,
}

impl Default for ReleaseCfg {
    fn default() -> Self {
        Self {
            versioning: Versioning::PerPackage,
            tag_format: None,
            environment: "release".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RootConfig {
    pub github: GithubCfg,
    pub cloudflare: CloudflareCfg,
    pub release: ReleaseCfg,
    /// Only for a single-package repo, whose root is the package (spec §5).
    pub publish: Option<PublishCfg>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DeployCfg {
    pub provider: Option<String>,
    pub project: Option<String>,
    pub dir: Option<String>,
    pub domain: Option<String>,
    pub on: String,
}

impl Default for DeployCfg {
    fn default() -> Self {
        Self {
            provider: None,
            project: None,
            dir: None,
            domain: None,
            on: "push".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NpmAccess {
    #[default]
    Public,
    Restricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct NpmPublish {
    pub access: NpmAccess,
    pub launcher: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CratesPublish {}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct GithubRelease {
    pub targets: Vec<String>,
    pub bin: Option<String>,
}

/// `[publish]` (spec §5): how a package is released.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct PublishCfg {
    pub npm: Option<NpmPublish>,
    pub crates: Option<CratesPublish>,
    pub github_release: Option<GithubRelease>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TargetConfig {
    pub deploy: Option<DeployCfg>,
    pub publish: Option<PublishCfg>,
}

/// Load `<dir>/hoist.toml` as the root config. A missing file gives the defaults.
pub fn load_root(dir: &Path) -> Result<RootConfig, HoistError> {
    load(dir)
}

/// Load `<dir>/hoist.toml` as a target config. A missing file gives the defaults.
pub fn load_target(dir: &Path) -> Result<TargetConfig, HoistError> {
    load(dir)
}

fn load<T: DeserializeOwned + Default>(dir: &Path) -> Result<T, HoistError> {
    let path = dir.join(FILE_NAME);
    let file = path.display().to_string();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => {
            return Err(HoistError::Config {
                file,
                message: format!("cannot read file: {e}; check its permissions"),
            });
        }
    };
    let config_err = |message: String| HoistError::Config {
        file: file.clone(),
        message,
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(e.to_string().trim_end().to_string()))?;
    match T::deserialize(table.clone()) {
        Ok(cfg) => Ok(cfg),
        Err(whole) => Err(config_err(describe(&table, &whole, |t| {
            T::deserialize(t).err()
        }))),
    }
}

/// Name the offending key as a dotted path (`github.foo`) by retrying one top-level entry at a time.
fn describe<F>(table: &toml::Table, whole: &toml::de::Error, retry: F) -> String
where
    F: Fn(toml::Table) -> Option<toml::de::Error>,
{
    for (key, value) in table {
        let mut single = toml::Table::new();
        single.insert(key.clone(), value.clone());
        let Some(err) = retry(single) else { continue };
        return match unknown_field(err.message()) {
            Some(field) if field == *key => format!("unknown key `{key}`; remove it"),
            Some(field) => format!("unknown key `{key}.{field}`; remove it"),
            None => format!("invalid `{key}`: {}", err.message()),
        };
    }
    whole.message().to_string()
}

fn unknown_field(message: &str) -> Option<&str> {
    let rest = message.strip_prefix("unknown field `")?;
    rest.split('`').next()
}

const PINS_FILE: &str = ".hoist/release.toml";

/// One package's entry in `.hoist/release.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Pin {
    pub version: Option<String>,
    pub skip: bool,
}

/// `.hoist/release.toml`: per-package version pins and skips.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ReleasePins {
    pub packages: std::collections::BTreeMap<String, Pin>,
}

/// Parse the text of `.hoist/release.toml`. Unknown keys are an error.
pub fn parse_pins(text: &str) -> Result<ReleasePins, HoistError> {
    toml::from_str(text).map_err(|e| HoistError::Config {
        file: PINS_FILE.to_string(),
        message: e.message().trim_end().to_string(),
    })
}

/// Read `<dir>/.hoist/release.toml`. A missing file gives no pins.
pub fn read_pins(dir: &Path) -> Result<ReleasePins, HoistError> {
    match std::fs::read_to_string(dir.join(PINS_FILE)) {
        Ok(text) => parse_pins(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ReleasePins::default()),
        Err(e) => Err(HoistError::Config {
            file: PINS_FILE.to_string(),
            message: format!("cannot read file: {e}; check its permissions"),
        }),
    }
}
