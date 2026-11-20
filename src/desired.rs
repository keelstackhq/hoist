//! The desired GitHub and Cloudflare state, joined from config and repo inference (spec §4, §6).

use crate::config::{EnvCfg, Managed, SecretScope, load_root};
use crate::error::HoistError;
use crate::release::{Package, read_release_model};
use crate::repo::Sourced;
use crate::repo::git::{GithubRepo, read_repo};
use crate::repo::targets::{Packages, TargetKind, read_targets};
use crate::repo::workflows::{JobEnv, Workflows, read_workflows};
use std::collections::BTreeMap;
use std::path::Path;

pub const RULESET_NAME: &str = "hoist: default branch";
const ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredSettings {
    pub visibility: Option<Sourced<String>>,
    pub allow_auto_merge: Sourced<bool>,
    pub delete_branch_on_merge: Sourced<bool>,
    pub immutable_releases: Option<Sourced<bool>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredRuleset {
    pub name: String,
    pub branch: String,
    pub required_checks: Vec<Sourced<String>>,
    pub merge_queue: Sourced<bool>,
}

/// `[github.environments.<name>]` for one environment.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvSettings {
    /// `None`: the default branch; `Some([])`: no branch restriction.
    pub branches: Option<Vec<String>>,
    pub manage: bool,
    /// `hoist.toml:github.environments.<name>`
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredGithub {
    pub settings: DesiredSettings,
    pub ruleset: DesiredRuleset,
    pub actions_permissions: Sourced<String>,
    pub environments: Vec<Sourced<String>>,
    /// `environment:` expressions in workflows (`${{ … }}`): reported `unknown`, never created.
    pub unresolved_environments: Vec<Sourced<String>>,
    /// `[github.environments.<name>]` overrides.
    pub env_settings: BTreeMap<String, EnvSettings>,
    pub secrets: Vec<Sourced<String>>,
    /// Repository variables the workflows read (`vars.X`) and `[github.variables]` lists,
    /// upper-cased.
    pub variables: Vec<Sourced<String>>,
    /// `[github.variables]` values by upper-cased name.
    pub variable_values: BTreeMap<String, Sourced<String>>,
    /// Per variable the workflows read: the environments of the jobs that read it and the
    /// jobs that read it without one.
    pub variable_homes: Vec<SecretHomes>,
    /// `[github] optional_variables`, upper-cased: reported apart, never a human step.
    pub optional_variables: Vec<String>,
    /// `[github] optional_secrets`, upper-cased: reported apart, never a human step.
    pub optional_secrets: Vec<String>,
    /// `[github] statuses`: required checks that are commit statuses, not jobs.
    pub statuses: Vec<String>,
    pub apps: Vec<Sourced<String>>,
    pub managed: Managed,
    /// `secret_scope = "environment"`: secrets live in the environments of the jobs that
    /// read them, never in the repository.
    pub env_scope: bool,
    /// Per secret the workflows read: the environments of the jobs that read it, and the jobs
    /// that read it with no `environment:` (spec §8.6).
    pub secret_homes: Vec<SecretHomes>,
    /// `file:job` of each job that passes `secrets: inherit` to another repo's workflow.
    pub opaque_inherits: Vec<String>,
    /// `file:job` of each job with a `secrets` use the scan cannot name.
    pub unresolved_secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SecretHomes {
    pub name: String,
    pub envs: Vec<String>,
    /// `file:job` of each job that reads it without an environment.
    pub envless: Vec<String>,
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
    /// `[cloudflare.dns]`, validated (spec §6.3).
    pub dns: Option<crate::dns::DnsCfg>,
    /// `[cloudflare.r2]` (spec §6.2).
    pub r2: Option<crate::r2::DesiredR2>,
}

/// What publishing needs (spec §9.4): the release environment and the workflow job that runs
/// in it, plus the packages with their `[publish]` settings. Present only when a package
/// publishes to npm or crates.io or has a GitHub Release.
#[derive(Debug, Clone, PartialEq)]
pub struct DesiredRelease {
    pub environment: Sourced<String>,
    /// `[release] reviewers`: logins that approve the environment (empty: the token's user).
    pub reviewers: Vec<String>,
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
    /// `apply --adopt`: never remove or loosen; a ruleset update that would drop live parts
    /// hoist does not model is drift.
    pub adopt: bool,
}

/// Each referenced secret's job environments and environment-less jobs.
fn secret_homes(w: &Workflows) -> Vec<SecretHomes> {
    let mut out: Vec<SecretHomes> = Vec::new();
    for u in &w.secret_uses {
        let i = match out.iter().position(|h| h.name == u.secret) {
            Some(i) => i,
            None => {
                out.push(SecretHomes {
                    name: u.secret.clone(),
                    ..SecretHomes::default()
                });
                out.len() - 1
            }
        };
        let h = &mut out[i];
        match &u.environment {
            Some(env) if !h.envs.contains(env) => h.envs.push(env.clone()),
            Some(_) => {}
            None => h.envless.push(u.job.clone()),
        }
    }
    out
}

fn env_settings(name: &str, c: &EnvCfg) -> EnvSettings {
    EnvSettings {
        branches: c.branches.clone(),
        manage: c.manage,
        source: format!("hoist.toml:github.environments.{name}"),
    }
}

/// The workflow-referenced environments, then the ones only `hoist.toml` lists.
fn environments(w: &Workflows, cfg: &BTreeMap<String, EnvCfg>) -> Vec<Sourced<String>> {
    let mut out = w.environments.clone();
    for name in cfg.keys() {
        if !out.iter().any(|e| &e.value == name) {
            out.push(Sourced {
                value: name.clone(),
                source: format!("hoist.toml:github.environments.{name}"),
            });
        }
    }
    out
}

/// The variables the workflows read, then the ones only `hoist.toml` lists.
fn variables(w: &Workflows, cfg: &BTreeMap<String, String>) -> Vec<Sourced<String>> {
    let mut out = w.variables.clone();
    for name in cfg.keys() {
        let upper = name.to_ascii_uppercase();
        // A name a workflow reads keeps the workflow line as its source.
        if !out.iter().any(|v| v.value == upper) {
            out.push(Sourced {
                value: upper,
                source: format!("hoist.toml:github.variables.{name}"),
            });
        }
    }
    out
}

/// Each referenced variable's job environments and environment-less jobs.
fn variable_homes(w: &Workflows) -> Vec<SecretHomes> {
    let mut out: Vec<SecretHomes> = Vec::new();
    for u in &w.variable_uses {
        let i = match out.iter().position(|h| h.name == u.name) {
            Some(i) => i,
            None => {
                out.push(SecretHomes {
                    name: u.name.clone(),
                    ..SecretHomes::default()
                });
                out.len() - 1
            }
        };
        let h = &mut out[i];
        match &u.environment {
            Some(env) if !h.envs.contains(env) => h.envs.push(env.clone()),
            Some(_) => {}
            None => h.envless.push(u.job.clone()),
        }
    }
    out
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
    let reviewers = raw
        .get("release")
        .and_then(|r| r.get("reviewers"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(DesiredRelease {
        environment,
        reviewers,
        workflow,
        packages: model.packages.into_iter().filter(publishes).collect(),
    }))
}

/// The release part of the desired model alone (no GitHub or Cloudflare inference), for
/// `release bootstrap`: `None` when no package publishes.
pub fn read_desired_release(dir: &Path) -> Result<Option<DesiredRelease>, HoistError> {
    desired_release(dir, &raw_root(dir), &read_workflows(dir)?)
}

impl Desired {
    /// Read the release model into `release` (R18: only `plan`, `check` and `apply` do, and
    /// `build_desired` never does). The release environment joins the workflow-inferred
    /// environments once. On an error `self` is unchanged.
    pub fn read_release(&mut self, dir: &Path) -> Result<(), HoistError> {
        let workflows = read_workflows(dir)?;
        let release = desired_release(dir, &raw_root(dir), &workflows)?;
        if let Some(r) = &release
            && !self
                .github
                .environments
                .iter()
                .any(|e| e.value == r.environment.value)
        {
            self.github.environments.push(r.environment.clone());
        }
        self.release = release;
        Ok(())
    }

    /// The deployment branch policies wanted for environment `name`: its `branches` in
    /// hoist.toml, else the default branch. Empty means no branch restriction.
    pub fn env_branches(&self, name: &str) -> Vec<String> {
        match self
            .github
            .env_settings
            .get(name)
            .and_then(|e| e.branches.clone())
        {
            Some(b) => b,
            None => vec![self.default_branch.clone()],
        }
    }

    /// Whether hoist manages environment `name` (`manage = false` leaves it alone). The
    /// release environment is never excluded here: the release area owns it.
    pub fn env_managed(&self, name: &str) -> bool {
        self.release_environment() == Some(name)
            || self.github.env_settings.get(name).is_none_or(|e| e.manage)
    }

    fn homes_of(&self, name: &str) -> Option<&SecretHomes> {
        match self.github.env_scope {
            true => self.github.secret_homes.iter().find(|h| h.name == name),
            false => None,
        }
    }

    /// Under `secret_scope = "environment"`, the environments that must hold secret `name`
    /// (empty in repository scope).
    pub fn homes(&self, name: &str) -> &[String] {
        self.homes_of(name).map_or(&[], |h| h.envs.as_slice())
    }

    /// The jobs that read `name` without an environment, which an environment secret cannot
    /// reach (empty in repository scope).
    pub fn envless(&self, name: &str) -> &[String] {
        self.homes_of(name).map_or(&[], |h| h.envless.as_slice())
    }

    /// Whether the release environment holds one of the secrets (it is then made with the
    /// github area too).
    pub fn release_holds_secrets(&self) -> bool {
        let release = self.release_environment();
        self.github
            .secret_homes
            .iter()
            .any(|h| self.github.env_scope && h.envs.iter().any(|e| Some(e.as_str()) == release))
    }

    /// The release environment's name, when publishing needs one.
    pub fn release_environment(&self) -> Option<&str> {
        self.release.as_ref().map(|r| r.environment.value.as_str())
    }

    /// The Cloudflare account id, required only when the Cloudflare area is read or explained
    /// and deploy targets exist (`plan --area github` and `auth check` do not need it).
    pub fn require_account_id(&self) -> Result<(), HoistError> {
        let cf = &self.cloudflare;
        if cf.account_id.is_none()
            && !(cf.pages.is_empty() && cf.workers.is_empty() && cf.r2.is_none())
        {
            return Err(HoistError::Config {
                file: "hoist.toml".to_string(),
                message: format!(
                    "Cloudflare resources found but no account id; set `[cloudflare] \
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
            immutable_releases: root.github.immutable_releases.map(|value| Sourced {
                value,
                source: "hoist.toml:github.immutable_releases".to_string(),
            }),
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
        environments: environments(&workflows, &root.github.environments),
        env_settings: root
            .github
            .environments
            .iter()
            .map(|(name, c)| (name.clone(), env_settings(name, c)))
            .collect(),
        unresolved_environments: workflows.unresolved_environments.clone(),
        secrets: workflows.secrets.clone(),
        variables: variables(&workflows, &root.github.variables),
        variable_homes: variable_homes(&workflows),
        variable_values: root
            .github
            .variables
            .iter()
            .map(|(k, v)| {
                (
                    k.to_ascii_uppercase(),
                    Sourced {
                        value: v.clone(),
                        source: format!("hoist.toml:github.variables.{k}"),
                    },
                )
            })
            .collect(),
        optional_variables: root
            .github
            .optional_variables
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect(),
        optional_secrets: root
            .github
            .optional_secrets
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect(),
        statuses: root.github.statuses.clone(),
        apps: if renovate {
            vec![Sourced {
                value: "renovate".to_string(),
                source: "renovate.json:1".to_string(),
            }]
        } else {
            Vec::new()
        },
        managed: root.github.managed,
        env_scope: root.github.secret_scope == SecretScope::Environment,
        secret_homes: secret_homes(&workflows),
        opaque_inherits: workflows.opaque_inherits.clone(),
        unresolved_secrets: workflows.unresolved_secrets.clone(),
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
    if let Some(dns) = &root.cloudflare.dns {
        crate::dns::validate(dns).map_err(|message| HoistError::Config {
            file: "hoist.toml".to_string(),
            message,
        })?;
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
            dns: root.cloudflare.dns.clone(),
            r2: crate::r2::desired_from_cfg(
                &dir.join("hoist.toml").display().to_string(),
                &root.cloudflare.r2,
            )?,
        },
        packages,
        release: None,
        adopt: false,
    })
}
