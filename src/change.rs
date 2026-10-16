//! The typed write plan (spec §6): `plan_changes` turns desired and live state into ordered
//! write operations. It is derived from the `create`/`update` items of `diff_github` and
//! `diff_cloudflare`, so plan and apply never disagree with `hoist plan`.

use crate::Area;
use crate::cloudflare::{CfToken, LiveCloudflare};
use crate::config::Managed;
use crate::desired::Desired;
use crate::diff::{diff_cloudflare, diff_github};
use crate::github::{Fetch, LiveGithub};
use crate::report::{Action, Item};
use serde_json::{Map, Value};

const API_TOKEN: &str = "CLOUDFLARE_API_TOKEN";
const ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Only the modelled fields that differ.
    RepoSettings {
        patch: Map<String, Value>,
    },
    Ruleset {
        existing_id: Option<u64>,
        branch: String,
        required_checks: Vec<String>,
        merge_queue: bool,
    },
    ActionsPermissions {
        default: String,
    },
    Environment {
        name: String,
        branch: String,
    },
    /// Writes the desired account id into `CLOUDFLARE_ACCOUNT_ID`.
    AccountIdSecret,
    /// Mint a token and seal it into `CLOUDFLARE_API_TOKEN`; `replace_ids` are revoked after.
    DeployToken {
        name: String,
        replace_ids: Vec<String>,
    },
    PagesProject {
        name: String,
        production_branch: String,
        exists: bool,
    },
    Domain {
        project: String,
        host: String,
    },
    RevokeToken {
        id: String,
        name: String,
    },
}

impl Change {
    /// (area, kind, key) in the scheme of the matching report item.
    pub fn key(&self) -> (&'static str, &'static str, String) {
        match self {
            Change::RepoSettings { patch } => (
                "github",
                "repo_setting",
                patch.keys().next().cloned().unwrap_or_default(),
            ),
            Change::Ruleset { .. } => ("github", "ruleset", crate::desired::RULESET_NAME.into()),
            Change::ActionsPermissions { .. } => {
                ("github", "actions_permissions", "default".into())
            }
            Change::Environment { name, .. } => ("github", "environment", name.clone()),
            Change::AccountIdSecret => ("github", "secret", ACCOUNT_ID.into()),
            Change::DeployToken { .. } => ("github", "secret", API_TOKEN.into()),
            Change::PagesProject { name, .. } => ("cloudflare", "pages_project", name.clone()),
            Change::Domain { host, .. } => ("cloudflare", "custom_domain", host.clone()),
            Change::RevokeToken { name, .. } => ("cloudflare", "api_token", name.clone()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChangeOpts {
    pub prune: bool,
    /// Empty means every area.
    pub areas: Vec<Area>,
}

impl ChangeOpts {
    fn wants(&self, area: Area) -> bool {
        self.areas.is_empty() || self.areas.contains(&area)
    }
}

/// The name of the deploy token hoist mints for this repo.
pub fn deploy_token_name(d: &Desired) -> String {
    format!("{}{}", token_prefix(d), "deploy")
}

fn token_prefix(d: &Desired) -> String {
    format!("hoist:{}/{}:", d.repo.owner, d.repo.name)
}

fn todo<'a>(items: &'a [Item], kind: &str, key: &str) -> Option<&'a Item> {
    items.iter().find(|i| {
        i.kind == kind && i.key == key && matches!(i.action, Action::Create | Action::Update)
    })
}

/// Index of the newest token: the latest `issued_on` when present, else the last in the list.
fn newest(tokens: &[&CfToken]) -> Option<usize> {
    (0..tokens.len()).max_by_key(|&i| (tokens[i].issued_on.clone(), i))
}

pub fn plan_changes(
    d: &Desired,
    gh: Option<&LiveGithub>,
    cf: Option<&LiveCloudflare>,
    opts: &ChangeOpts,
) -> Vec<Change> {
    let gh_items = match gh {
        Some(l) if opts.wants(Area::Github) => diff_github(d, l, cf).0,
        _ => Vec::new(),
    };
    let cf_items = match cf {
        Some(l) if opts.wants(Area::Cloudflare) => diff_cloudflare(d, l).0,
        _ => Vec::new(),
    };
    let mut out = Vec::new();

    for p in &d.cloudflare.pages {
        if let Some(i) = todo(&cf_items, "pages_project", &p.project.value) {
            out.push(Change::PagesProject {
                name: p.project.value.clone(),
                production_branch: d.default_branch.clone(),
                exists: i.action == Action::Update,
            });
        }
    }
    for p in &d.cloudflare.pages {
        if let Some(dom) = &p.domain
            && todo(&cf_items, "custom_domain", &dom.value).is_some()
        {
            out.push(Change::Domain {
                project: p.project.value.clone(),
                host: dom.value.clone(),
            });
        }
    }

    let token_name = deploy_token_name(d);
    let known_tokens: &[CfToken] = match cf.map(|l| &l.tokens) {
        Some(Fetch::Got(t)) => t,
        _ => &[],
    };
    let mut replace_ids = Vec::new();
    if todo(&gh_items, "secret", API_TOKEN).is_some() {
        replace_ids = known_tokens
            .iter()
            .filter(|t| t.name == token_name)
            .map(|t| t.id.clone())
            .collect();
        out.push(Change::DeployToken {
            name: token_name.clone(),
            replace_ids: replace_ids.clone(),
        });
    }
    if todo(&gh_items, "secret", ACCOUNT_ID).is_some() {
        out.push(Change::AccountIdSecret);
    }

    for env in &d.github.environments {
        if todo(&gh_items, "environment", &env.value).is_some() {
            out.push(Change::Environment {
                name: env.value.clone(),
                branch: d.default_branch.clone(),
            });
        }
    }

    let rs = &d.github.ruleset;
    if todo(&gh_items, "ruleset", &rs.name).is_some() {
        let existing_id = gh
            .and_then(|l| l.rulesets.as_got().ok())
            .and_then(|list| list.iter().find(|r| r.name == rs.name))
            .map(|r| r.id);
        out.push(Change::Ruleset {
            existing_id,
            branch: rs.branch.clone(),
            required_checks: rs.required_checks.iter().map(|c| c.value.clone()).collect(),
            merge_queue: rs.merge_queue.value,
        });
    }

    let s = &d.github.settings;
    let mut patch = Map::new();
    if let Some(v) = &s.visibility
        && todo(&gh_items, "repo_setting", "visibility").is_some()
    {
        patch.insert("visibility".into(), Value::String(v.value.clone()));
    }
    if todo(&gh_items, "repo_setting", "allow_auto_merge").is_some() {
        patch.insert("allow_auto_merge".into(), s.allow_auto_merge.value.into());
    }
    if todo(&gh_items, "repo_setting", "delete_branch_on_merge").is_some() {
        patch.insert(
            "delete_branch_on_merge".into(),
            s.delete_branch_on_merge.value.into(),
        );
    }
    if !patch.is_empty() {
        out.push(Change::RepoSettings { patch });
    }

    if todo(&gh_items, "actions_permissions", "default").is_some() {
        out.push(Change::ActionsPermissions {
            default: d.github.actions_permissions.value.clone(),
        });
    }

    if opts.prune && opts.wants(Area::Cloudflare) && d.cloudflare.managed == Managed::Hoist {
        let prefix = token_prefix(d);
        let mine: Vec<&CfToken> = known_tokens
            .iter()
            .filter(|t| t.name.starts_with(&prefix))
            .collect();
        // The active token is the newest deploy token, unless a new one is being minted.
        let deploys: Vec<&CfToken> = mine
            .iter()
            .copied()
            .filter(|t| t.name == token_name)
            .collect();
        let active = if !out.iter().any(is_deploy_token) {
            newest(&deploys).map(|i| deploys[i].id.clone())
        } else {
            None
        };
        for t in mine {
            if Some(&t.id) != active.as_ref() && !replace_ids.contains(&t.id) {
                out.push(Change::RevokeToken {
                    id: t.id.clone(),
                    name: t.name.clone(),
                });
            }
        }
    }
    out
}

fn is_deploy_token(c: &Change) -> bool {
    matches!(c, Change::DeployToken { .. })
}
