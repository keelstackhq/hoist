//! `.changeset/config.json` (the @changesets/config 3.1.4 key set, cli 2.31.1).

use super::CsType;
use crate::HoistError;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

const FILE: &str = ".changeset/config.json";

/// What hoist uses from `config.json`; accepted-only keys are checked and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsConfig {
    /// Default for `changeset status --since` (`master` when absent, as changesets).
    pub base_branch: String,
    /// `false` = no CHANGELOG writes.
    pub changelog: bool,
    pub fixed: Vec<Vec<String>>,
    pub linked: Vec<Vec<String>>,
    pub ignore: Vec<String>,
    /// `Patch` (default) or `Minor`.
    pub update_internal_dependencies: CsType,
    /// Default `["**"]`.
    pub changed_file_patterns: Vec<String>,
    pub only_peer_out_of_range: bool,
    /// `updateInternalDependents: "always"` (default `out-of-range`).
    pub dependents_always: bool,
    /// `public` or `restricted`.
    pub access: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Raw {
    #[serde(rename = "$schema")]
    _schema: Option<String>,
    changelog: Option<Value>,
    commit: Option<Value>,
    access: Option<String>,
    #[serde(rename = "privatePackages")]
    _private_packages: Option<Value>,
    #[serde(rename = "prettier")]
    _prettier: Option<bool>,
    #[serde(rename = "snapshot")]
    _snapshot: Option<Value>,
    #[serde(rename = "bumpVersionsWithWorkspaceProtocolOnly")]
    _bump_workspace_only: Option<bool>,
    base_branch: Option<String>,
    #[serde(default)]
    fixed: Vec<Vec<String>>,
    #[serde(default)]
    linked: Vec<Vec<String>>,
    #[serde(default)]
    ignore: Vec<String>,
    update_internal_dependencies: Option<String>,
    changed_file_patterns: Option<Vec<String>>,
    #[serde(rename = "___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH", default)]
    experimental: Experimental,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Experimental {
    #[serde(default)]
    only_update_peer_dependents_when_out_of_range: bool,
    update_internal_dependents: Option<String>,
}

/// Read `<root>/.changeset/config.json`; `None` when it does not exist (commits mode).
pub fn read_config(root: &Path) -> Result<Option<CsConfig>, HoistError> {
    let err = |message: String| HoistError::Config {
        file: FILE.into(),
        message,
    };
    let text = match std::fs::read_to_string(root.join(FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(format!("cannot read: {e}"))),
    };
    let raw: Raw = serde_json::from_str(&text)
        .map_err(|e| err(format!("{e}; fix the JSON or remove the key")))?;
    if raw
        .commit
        .as_ref()
        .is_some_and(|c| *c != Value::Bool(false))
    {
        return Err(err(
            "`commit` must be false (hoist commits through its release PR)".into(),
        ));
    }
    let changelog = match &raw.changelog {
        None => true,
        Some(Value::Bool(false)) => false,
        Some(Value::String(s))
            if s == "@changesets/cli/changelog" || s == "@changesets/changelog-git" =>
        {
            true
        }
        Some(other) => {
            return Err(err(format!(
                "`changelog` {other} is not supported; use \"@changesets/cli/changelog\", \
                 \"@changesets/changelog-git\" or false"
            )));
        }
    };
    if let Some(a) = &raw.access
        && a != "public"
        && a != "restricted"
    {
        return Err(err(format!(
            "`access` \"{a}\" is not valid; use \"public\" or \"restricted\""
        )));
    }
    let update_internal_dependencies = match raw.update_internal_dependencies.as_deref() {
        None | Some("patch") => CsType::Patch,
        Some("minor") => CsType::Minor,
        Some(o) => {
            return Err(err(format!(
                "`updateInternalDependencies` \"{o}\" is not valid; use \"patch\" or \"minor\""
            )));
        }
    };
    let dependents_always = match raw.experimental.update_internal_dependents.as_deref() {
        None | Some("out-of-range") => false,
        Some("always") => true,
        Some(o) => {
            return Err(err(format!(
                "`updateInternalDependents` \"{o}\" is not valid; use \"out-of-range\" or \
                 \"always\""
            )));
        }
    };
    Ok(Some(CsConfig {
        base_branch: raw.base_branch.unwrap_or_else(|| "master".into()),
        changelog,
        fixed: raw.fixed,
        linked: raw.linked,
        ignore: raw.ignore,
        update_internal_dependencies,
        changed_file_patterns: raw
            .changed_file_patterns
            .unwrap_or_else(|| vec!["**".into()]),
        only_peer_out_of_range: raw
            .experimental
            .only_update_peer_dependents_when_out_of_range,
        dependents_always,
        access: raw.access,
    }))
}
