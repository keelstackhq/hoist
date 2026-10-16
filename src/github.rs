//! Live GitHub state, read with GET requests only (spec §6). `Fetch::Unknown` carries the
//! reason a resource could not be read (permission or plan limits); transport and
//! response-shape errors, a rejected token (401) and an invisible repo fail the whole read.

use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, HttpError};
use serde_json::Value;

const API_TOKEN_SECRET: &str = "CLOUDFLARE_API_TOKEN";

/// A live value, or why it could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch<T> {
    Got(T),
    Unknown(String),
}

impl<T> Fetch<T> {
    pub fn as_got(&self) -> Result<&T, &str> {
        match self {
            Fetch::Got(v) => Ok(v),
            Fetch::Unknown(why) => Err(why),
        }
    }
}

/// Repo settings. The booleans are `None` when the token cannot see them (GitHub omits
/// them for tokens without admin access).
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSettings {
    pub visibility: String,
    pub allow_auto_merge: Option<bool>,
    pub delete_branch_on_merge: Option<bool>,
    /// `permissions.admin`: whether the token may change settings, rulesets and secrets.
    pub admin: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ruleset {
    /// Numeric id from the list endpoint (also in the detail response).
    pub id: u64,
    pub name: String,
    /// `conditions.ref_name.include`
    pub include: Vec<String>,
    /// `required_status_checks` contexts
    pub required_checks: Vec<String>,
    pub merge_queue: bool,
    /// `active`, `evaluate` or `disabled`; `None` when the detail omits it.
    pub enforcement: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveGithub {
    pub settings: Fetch<RepoSettings>,
    pub rulesets: Fetch<Vec<Ruleset>>,
    /// Classic branch protection on the default branch.
    pub classic_protection: Fetch<bool>,
    pub actions_permissions: Fetch<String>,
    pub environments: Fetch<Vec<String>>,
    /// Repository secrets plus the organization secrets this repo can use.
    pub secrets: Fetch<Vec<String>>,
    /// Why organization secrets could not be listed (they then count as none).
    pub org_secrets_note: Option<String>,
    /// `updated_at` of the `CLOUDFLARE_API_TOKEN` secret (repository, else organization) when
    /// listed: tells which minted deploy token the secret holds (R29).
    pub api_token_updated_at: Option<String>,
    /// Slugs of the desired apps that are installed on this repo (others are not checked).
    pub apps: Fetch<Vec<String>>,
}

const APPS_UNKNOWN: &str = "cannot list app installations with this token";

fn denied(what: &str, f: Fetched) -> Fetch<Value> {
    match f {
        Fetched::Ok(v) => Fetch::Got(v),
        Fetched::NotFound => Fetch::Unknown(format!("cannot read {what}: not found (404)")),
        Fetched::Forbidden(hint) => Fetch::Unknown(format!("cannot read {what}: {hint}")),
    }
}

fn get(http: &Http, what: &str, path: &str) -> Result<Fetch<Value>, HttpError> {
    Ok(denied(what, http.get_json(path)?))
}

fn list(
    http: &Http,
    what: &str,
    path: &str,
    key: Option<&str>,
) -> Result<Fetch<Vec<Value>>, HttpError> {
    Ok(match denied(what, http.get_all(path, key)?) {
        Fetch::Got(Value::Array(items)) => Fetch::Got(items),
        Fetch::Got(_) => Fetch::Unknown(format!("cannot read {what}: unexpected response")),
        Fetch::Unknown(why) => Fetch::Unknown(why),
    })
}

fn names(items: Vec<Value>, field: &str) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| i.get(field).and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// `updated_at` of the secret named `name` in a secrets list.
fn updated_at(items: &[Value], name: &str) -> Option<String> {
    items
        .iter()
        .find(|i| i.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|i| i.get("updated_at").and_then(Value::as_str))
        .map(str::to_string)
}

fn parse_ruleset(v: &Value, id: u64) -> Ruleset {
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let rules: &[Value] = v
        .get("rules")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut required_checks = Vec::new();
    let mut merge_queue = false;
    for rule in rules {
        match rule.get("type").and_then(Value::as_str) {
            Some("required_status_checks") => {
                let checks = rule
                    .pointer("/parameters/required_status_checks")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                required_checks.extend(names(checks.to_vec(), "context"));
            }
            Some("merge_queue") => merge_queue = true,
            _ => {}
        }
    }
    Ruleset {
        id,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        include: strings(v.pointer("/conditions/ref_name/include")),
        required_checks,
        merge_queue,
        enforcement: v
            .get("enforcement")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `GET /repos/{o}/{r}`. A 404 here means the token cannot see the repo at all, so every
/// other read would be meaningless: that is an error (exit 3), not an unknown.
fn read_settings(http: &Http, d: &Desired, base: &str) -> Result<Fetch<RepoSettings>, HoistError> {
    let fetched = http.get_json(base)?;
    if fetched == Fetched::NotFound {
        return Err(HoistError::Usage(format!(
            "token cannot see {}/{} (404); grant it access or check the remote",
            d.repo.owner, d.repo.name
        )));
    }
    Ok(match denied("repo settings", fetched) {
        Fetch::Unknown(why) => Fetch::Unknown(why),
        Fetch::Got(v) => {
            let visibility = v
                .get("visibility")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    v.get("private")
                        .and_then(Value::as_bool)
                        .map(|p| if p { "private" } else { "public" }.to_string())
                });
            match visibility {
                Some(visibility) => Fetch::Got(RepoSettings {
                    visibility,
                    allow_auto_merge: v.get("allow_auto_merge").and_then(Value::as_bool),
                    delete_branch_on_merge: v
                        .get("delete_branch_on_merge")
                        .and_then(Value::as_bool),
                    admin: v.pointer("/permissions/admin").and_then(Value::as_bool),
                }),
                None => {
                    Fetch::Unknown("cannot read repo settings: unexpected response".to_string())
                }
            }
        }
    })
}

fn read_rulesets(http: &Http, base: &str) -> Result<Fetch<Vec<Ruleset>>, HttpError> {
    let path = format!("{base}/rulesets");
    let summaries = match list(http, "rulesets", &path, None)? {
        Fetch::Got(items) => items,
        Fetch::Unknown(why) => return Ok(Fetch::Unknown(why)),
    };
    let mut out = Vec::new();
    for summary in &summaries {
        let Some(id) = summary.get("id").and_then(Value::as_u64) else {
            continue;
        };
        match get(http, "ruleset details", &format!("{path}/{id}"))? {
            Fetch::Got(v) => out.push(parse_ruleset(&v, id)),
            Fetch::Unknown(why) => return Ok(Fetch::Unknown(why)),
        }
    }
    Ok(Fetch::Got(out))
}

fn read_classic(http: &Http, base: &str, branch: &str) -> Result<Fetch<bool>, HttpError> {
    let path = format!("{base}/branches/{branch}/protection");
    Ok(match http.get_json(&path)? {
        Fetched::Ok(_) => Fetch::Got(true),
        Fetched::NotFound => Fetch::Got(false),
        Fetched::Forbidden(hint) => {
            Fetch::Unknown(format!("cannot read branch protection: {hint}"))
        }
    })
}

fn read_apps(http: &Http, d: &Desired) -> Result<Fetch<Vec<String>>, HttpError> {
    if d.github.apps.is_empty() {
        return Ok(Fetch::Got(Vec::new()));
    }
    let unknown = || Fetch::Unknown(APPS_UNKNOWN.to_string());
    let installs = match http.get_all("/user/installations", Some("installations"))? {
        Fetched::Ok(Value::Array(items)) => items,
        Fetched::Ok(_) | Fetched::NotFound | Fetched::Forbidden(_) => return Ok(unknown()),
    };
    let full = format!("{}/{}", d.repo.owner, d.repo.name).to_lowercase();
    let mut found = Vec::new();
    for install in &installs {
        let (Some(id), Some(slug)) = (
            install.get("id").and_then(Value::as_u64),
            install.get("app_slug").and_then(Value::as_str),
        ) else {
            continue;
        };
        if !d.github.apps.iter().any(|a| a.value == slug) {
            continue;
        }
        let path = format!("/user/installations/{id}/repositories");
        match http.get_all(&path, Some("repositories"))? {
            Fetched::Ok(Value::Array(repos)) => {
                let listed = repos.iter().any(|r| {
                    r.get("full_name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| n.to_lowercase() == full)
                });
                if listed {
                    found.push(slug.to_string());
                }
            }
            _ => return Ok(unknown()),
        }
    }
    Ok(Fetch::Got(found))
}

/// Read the live GitHub state for the desired repo. Errors on transport or response-shape
/// failures, a 401, or a 404 on the repo itself; other 403/404 become `Fetch::Unknown` (or
/// "absent" where 404 means so).
pub fn read_github(http: &Http, d: &Desired) -> Result<LiveGithub, HoistError> {
    let base = format!("/repos/{}/{}", d.repo.owner, d.repo.name);
    let settings = read_settings(http, d, &base)?;
    let secrets = list(
        http,
        "secrets",
        &format!("{base}/actions/secrets"),
        Some("secrets"),
    )?;
    // Organization secrets visible to this repo. A 404 (user-owned repo) is none; a 403 is
    // none plus a note, so it never turns the repository secrets unknown.
    let mut org_token_updated = None;
    let (org_secrets, org_secrets_note) = match http.get_all(
        &format!("{base}/actions/organization-secrets"),
        Some("secrets"),
    )? {
        Fetched::Ok(Value::Array(items)) => {
            org_token_updated = updated_at(&items, API_TOKEN_SECRET);
            (names(items, "name"), None)
        }
        Fetched::Ok(_) => (
            Vec::new(),
            Some("organization secrets not readable: unexpected response".to_string()),
        ),
        Fetched::NotFound => (Vec::new(), None),
        Fetched::Forbidden(hint) => (
            Vec::new(),
            Some(format!("organization secrets not readable: {hint}")),
        ),
    };
    let environments = list(
        http,
        "environments",
        &format!("{base}/environments"),
        Some("environments"),
    )?;
    // A repository secret shadows an organization secret of the same name.
    let api_token_updated_at = match &secrets {
        Fetch::Got(items) => updated_at(items, API_TOKEN_SECRET).or(org_token_updated),
        Fetch::Unknown(_) => None,
    };
    Ok(LiveGithub {
        settings,
        rulesets: read_rulesets(http, &base)?,
        classic_protection: read_classic(http, &base, &d.default_branch)?,
        actions_permissions: match get(
            http,
            "actions permissions",
            &format!("{base}/actions/permissions/workflow"),
        )? {
            Fetch::Got(v) => match v
                .get("default_workflow_permissions")
                .and_then(Value::as_str)
            {
                Some(p) => Fetch::Got(p.to_string()),
                None => {
                    Fetch::Unknown("cannot read actions permissions: unexpected response".into())
                }
            },
            Fetch::Unknown(why) => Fetch::Unknown(why),
        },
        environments: match environments {
            Fetch::Got(items) => Fetch::Got(names(items, "name")),
            Fetch::Unknown(why) => Fetch::Unknown(why),
        },
        secrets: match secrets {
            Fetch::Got(items) => {
                let mut all = names(items, "name");
                all.extend(org_secrets);
                Fetch::Got(all)
            }
            Fetch::Unknown(why) => Fetch::Unknown(why),
        },
        org_secrets_note,
        api_token_updated_at,
        apps: read_apps(http, d)?,
    })
}
