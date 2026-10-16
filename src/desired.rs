//! The desired GitHub and Cloudflare state, joined from config and repo inference (spec §4, §6).

use crate::config::{Managed, load_root};
use crate::error::HoistError;
use crate::repo::Sourced;
use crate::repo::git::{GithubRepo, read_repo};
use crate::repo::targets::{Packages, TargetKind, read_targets};
use crate::repo::workflows::read_workflows;
use std::path::Path;

pub const RULESET_NAME: &str = "hoist: default branch";
const ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredSettings {
    pub visibility: Option<Sourced<String>>,
    pub allow_auto_merge: Sourced<bool>,
    pub delete_branch_on_merge: Sourced<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredRuleset {
    pub name: String,
    pub branch: String,
    pub required_checks: Vec<Sourced<String>>,
    pub merge_queue: Sourced<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredGithub {
    pub settings: DesiredSettings,
    pub ruleset: DesiredRuleset,
    pub actions_permissions: Sourced<String>,
    pub environments: Vec<Sourced<String>>,
    pub secrets: Vec<Sourced<String>>,
    pub apps: Vec<Sourced<String>>,
    pub managed: Managed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredPages {
    pub target: String,
    pub project: Sourced<String>,
    pub domain: Option<Sourced<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredCloudflare {
    pub account_id: Option<Sourced<String>>,
    pub pages: Vec<DesiredPages>,
    pub workers: Vec<Sourced<String>>,
    pub managed: Managed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Desired {
    pub repo: GithubRepo,
    pub default_branch: String,
    pub github: DesiredGithub,
    pub cloudflare: DesiredCloudflare,
    pub packages: Packages,
}

/// Top-level `hoist.toml` table, to learn which keys the user set (vs defaults).
fn raw_root(dir: &Path) -> toml::Table {
    std::fs::read_to_string(dir.join("hoist.toml"))
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default()
}

fn is_set(raw: &toml::Table, section: &str, key: &str) -> bool {
    raw.get(section).and_then(|s| s.get(key)).is_some()
}

fn pick<T>(raw: &toml::Table, section: &str, key: &str, value: T) -> Sourced<T> {
    let source = if is_set(raw, section, key) {
        format!("hoist.toml:{section}.{key}")
    } else {
        "default".to_string()
    };
    Sourced { value, source }
}

fn rel_dir(root: &Path, dir: &Path, fallback: &str) -> String {
    dir.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| fallback.to_string())
}

pub fn build_desired(dir: &Path) -> Result<Desired, HoistError> {
    let root = load_root(dir)?;
    let raw = raw_root(dir);
    let (repo, default_branch) = read_repo(dir)?;
    let workflows = read_workflows(dir)?;
    let (targets, packages, renovate) = read_targets(dir)?;

    let required_checks = match &root.github.required_checks {
        Some(list) => list
            .iter()
            .map(|c| Sourced {
                value: c.clone(),
                source: "hoist.toml:github.required_checks".to_string(),
            })
            .collect(),
        None => workflows
            .jobs
            .iter()
            .filter(|j| j.value == "ci-ok")
            .cloned()
            .collect(),
    };

    let github = DesiredGithub {
        settings: DesiredSettings {
            visibility: root.github.visibility.clone().map(|value| Sourced {
                value,
                source: "hoist.toml:github.visibility".to_string(),
            }),
            allow_auto_merge: pick(
                &raw,
                "github",
                "allow_auto_merge",
                root.github.allow_auto_merge,
            ),
            delete_branch_on_merge: pick(
                &raw,
                "github",
                "delete_branch_on_merge",
                root.github.delete_branch_on_merge,
            ),
        },
        ruleset: DesiredRuleset {
            name: RULESET_NAME.to_string(),
            branch: default_branch.clone(),
            required_checks,
            merge_queue: pick(&raw, "github", "merge_queue", root.github.merge_queue),
        },
        actions_permissions: pick(
            &raw,
            "github",
            "actions_permissions",
            root.github.actions_permissions.clone(),
        ),
        environments: workflows.environments.clone(),
        secrets: workflows.secrets.clone(),
        apps: if renovate {
            vec![Sourced {
                value: "renovate".to_string(),
                source: "renovate.json:1".to_string(),
            }]
        } else {
            Vec::new()
        },
        managed: root.github.managed,
    };

    let mut pages = Vec::new();
    let mut workers = Vec::new();
    for target in &targets {
        match target.kind.value {
            TargetKind::CloudflarePages => {
                let rel = rel_dir(dir, &target.dir, &target.name);
                let deploy = target.config.deploy.as_ref();
                let project = match deploy.and_then(|d| d.project.clone()) {
                    Some(value) => Sourced {
                        value,
                        source: format!("{rel}/hoist.toml:deploy.project"),
                    },
                    None => Sourced {
                        value: format!("{}-{}", repo.name, target.name),
                        source: "default".to_string(),
                    },
                };
                let domain = deploy.and_then(|d| d.domain.clone()).map(|value| Sourced {
                    value,
                    source: format!("{rel}/hoist.toml:deploy.domain"),
                });
                pages.push(DesiredPages {
                    target: target.name.clone(),
                    project,
                    domain,
                });
            }
            TargetKind::CloudflareWorkers => workers.push(Sourced {
                value: target.name.clone(),
                source: target.kind.source.clone(),
            }),
        }
    }

    let account_id = match &root.cloudflare.account_id {
        Some(value) => Some(Sourced {
            value: value.clone(),
            source: "hoist.toml:cloudflare.account_id".to_string(),
        }),
        None => std::env::var(ACCOUNT_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|value| Sourced {
                value,
                source: format!("env {ACCOUNT_ENV}"),
            }),
    };
    if account_id.is_none() && !(pages.is_empty() && workers.is_empty()) {
        return Err(HoistError::Config {
            file: "hoist.toml".to_string(),
            message: format!(
                "Cloudflare deploy targets found but no account id; set `[cloudflare] account_id` \
                 in hoist.toml or export {ACCOUNT_ENV}"
            ),
        });
    }

    Ok(Desired {
        repo,
        default_branch,
        github,
        cloudflare: DesiredCloudflare {
            account_id,
            pages,
            workers,
            managed: root.cloudflare.managed,
        },
        packages,
    })
}
