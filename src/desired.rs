//! The desired GitHub and Cloudflare state, joined from config and repo inference (spec §4, §6).

use crate::config::{Managed, load_root};
use crate::error::HoistError;
use crate::release::{Package, read_release_model};
use crate::repo::Sourced;
use crate::repo::git::{GithubRepo, read_repo};
use crate::repo::targets::{Packages, TargetKind, read_targets};
use crate::repo::workflows::{JobEnv, Workflows, read_workflows};
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
    /// The target directory relative to the repo root (`apps/site`).
    pub dir: String,
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

/// What publishing needs (spec §9.4): the release environment and the workflow job that runs
/// in it, plus the packages with their `[publish]` settings. Present only when a package
/// publishes to npm or crates.io or has a GitHub Release.
#[derive(Debug, Clone, PartialEq)]
pub struct DesiredRelease {
    pub environment: Sourced<String>,
    /// The first workflow job that declares `environment: <environment>`.
    pub workflow: Option<JobEnv>,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Desired {
    pub repo: GithubRepo,
    pub default_branch: String,
    pub github: DesiredGithub,
    pub cloudflare: DesiredCloudflare,
    pub packages: Packages,
    pub release: Option<DesiredRelease>,
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

/// Ruling R9: the `hoist.toml` key when set, else on iff a workflow has a `merge_group`
/// trigger (source: that workflow), else off (`default`).
fn merge_queue(raw: &toml::Table, configured: bool, workflows: &Workflows) -> Sourced<bool> {
    if is_set(raw, "github", "merge_queue") {
        return pick(raw, "github", "merge_queue", configured);
    }
    Sourced {
        value: workflows.merge_group,
        source: workflows
            .merge_group_source
            .clone()
            .unwrap_or_else(|| "default".to_string()),
    }
}

fn desired_release(
    dir: &Path,
    raw: &toml::Table,
    workflows: &Workflows,
) -> Result<Option<DesiredRelease>, HoistError> {
    let model = read_release_model(dir)?;
    let publishes = |p: &Package| {
        p.publish.npm.is_some() || p.publish.crates.is_some() || p.publish.github_release.is_some()
    };
    if !model.packages.iter().any(publishes) {
        return Ok(None);
    }
    let environment = pick(raw, "release", "environment", model.environment);
    let workflow = workflows
        .job_environments
        .iter()
        .find(|j| j.environment == environment.value)
        .cloned();
    Ok(Some(DesiredRelease {
        environment,
        workflow,
        packages: model.packages.into_iter().filter(publishes).collect(),
    }))
}

impl Desired {
    /// The release environment's name, when publishing needs one.
    pub fn release_environment(&self) -> Option<&str> {
        self.release.as_ref().map(|r| r.environment.value.as_str())
    }

    /// The Cloudflare account id, required only when the Cloudflare area is read or explained
    /// and deploy targets exist (`plan --area github` and `auth check` do not need it).
    pub fn require_account_id(&self) -> Result<(), HoistError> {
        let cf = &self.cloudflare;
        if cf.account_id.is_none() && !(cf.pages.is_empty() && cf.workers.is_empty()) {
            return Err(HoistError::Config {
                file: "hoist.toml".to_string(),
                message: format!(
                    "Cloudflare deploy targets found but no account id; set `[cloudflare] \
                     account_id` in hoist.toml or export {ACCOUNT_ENV}"
                ),
            });
        }
        Ok(())
    }
}

pub fn build_desired(dir: &Path) -> Result<Desired, HoistError> {
    let root = load_root(dir)?;
    let raw = raw_root(dir);
    let (repo, default_branch) = read_repo(dir)?;
    let workflows = read_workflows(dir)?;
    let (targets, packages, renovate) = read_targets(dir)?;
    let release = desired_release(dir, &raw, &workflows)?;
    // The release environment joins the workflow-inferred ones, once.
    let mut environments = workflows.environments.clone();
    if let Some(r) = &release
        && !environments.iter().any(|e| e.value == r.environment.value)
    {
        environments.push(r.environment.clone());
    }

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
            merge_queue: merge_queue(&raw, root.github.merge_queue, &workflows),
        },
        actions_permissions: pick(
            &raw,
            "github",
            "actions_permissions",
            root.github.actions_permissions.clone(),
        ),
        environments,
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
                    dir: rel.clone(),
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
        release,
    })
}
