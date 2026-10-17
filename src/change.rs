//! The typed write plan (spec §6): `plan_changes` turns desired and live state into ordered
//! write operations. It is derived from the `create`/`update` items of `diff_github` and
//! `diff_cloudflare`, so plan and apply never disagree with `hoist plan`.

use crate::Area;
use crate::cloudflare::{CfToken, LiveCloudflare};
use crate::config::Managed;
use crate::desired::Desired;
use crate::diff::{diff_cloudflare, diff_github};
use crate::diff_release::release_env_item;
use crate::github::{Fetch, LiveGithub};
use crate::report::{Action, Item};
use crate::timeutil::parse_rfc3339;
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
        /// The release environment: its report item is in the `release` area.
        release: bool,
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
            Change::Environment { name, release, .. } => (
                if *release { "release" } else { "github" },
                "environment",
                name.clone(),
            ),
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

pub fn token_prefix(d: &Desired) -> String {
    format!("hoist:{}/{}:", d.repo.owner, d.repo.name)
}

fn todo<'a>(items: &'a [Item], kind: &str, key: &str) -> Option<&'a Item> {
    items.iter().find(|i| {
        i.kind == kind && i.key == key && matches!(i.action, Action::Create | Action::Update)
    })
}

/// Which minted deploy token `CLOUDFLARE_API_TOKEN` holds (R29).
#[derive(Debug, Clone, PartialEq)]
pub enum LiveToken<'a> {
    /// No `hoist:<o>/<r>:deploy` token exists.
    NoToken,
    /// The newest deploy token issued at or before the secret's `updated_at`, plus the ids
    /// of deploy tokens issued after it (not in the secret; never pruned).
    Live(&'a CfToken, Vec<String>),
    /// Cannot tell (no `updated_at`, a missing `issued_on`, or no token issued before the
    /// secret was written): the ids of every deploy token.
    Unclear(Vec<String>),
}

/// Seconds of clock difference between Cloudflare's `issued_on` and GitHub's `updated_at`
/// that R33 tolerates before it calls the live token unclear.
const CLOCK_SKEW_SECS: i64 = 60;

/// R29: the live deploy token is the newest one named `name` with `issued_on` <= the
/// secret's `updated_at`. A single deploy token without dates counts as live. R33: ties and
/// near-skew cases with more than one candidate are unclear.
pub fn live_deploy_token<'a>(
    name: &str,
    tokens: &'a [CfToken],
    secret_updated: Option<&str>,
) -> LiveToken<'a> {
    let deploys: Vec<&CfToken> = tokens.iter().filter(|t| t.name == name).collect();
    let ids = || deploys.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
    let issued: Option<Vec<i64>> = deploys
        .iter()
        .map(|t| t.issued_on.as_deref().and_then(parse_rfc3339))
        .collect();
    match (secret_updated.and_then(parse_rfc3339), issued) {
        _ if deploys.is_empty() => LiveToken::NoToken,
        (Some(at), Some(issued)) => {
            let best = (0..deploys.len())
                .filter(|&i| issued[i] <= at)
                .max_by_key(|&i| (issued[i], i));
            // R33: a tie on issued_on, or another candidate while the best one sits within
            // the clock-skew window of the secret's update, cannot be told apart safely.
            let ambiguous = best.is_some_and(|b| {
                (0..deploys.len()).any(|j| {
                    j != b
                        && issued[j] <= at
                        && (issued[j] == issued[b] || at - issued[b] <= CLOCK_SKEW_SECS)
                })
            });
            match best {
                Some(_) if ambiguous => LiveToken::Unclear(ids()),
                Some(i) => {
                    let newer = (0..deploys.len())
                        .filter(|&j| issued[j] > at)
                        .map(|j| deploys[j].id.clone())
                        .collect();
                    LiveToken::Live(deploys[i], newer)
                }
                None => LiveToken::Unclear(ids()),
            }
        }
        _ if deploys.len() == 1 => LiveToken::Live(deploys[0], Vec::new()),
        _ => LiveToken::Unclear(ids()),
    }
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
    let release_items: Vec<Item> = match gh {
        Some(l) if opts.wants(Area::Release) => release_env_item(d, l).into_iter().collect(),
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
        let release = d.release_environment() == Some(env.value.as_str());
        let items = if release { &release_items } else { &gh_items };
        if todo(items, "environment", &env.value).is_some() {
            out.push(Change::Environment {
                name: env.value.clone(),
                branch: d.default_branch.clone(),
                release,
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
        // Keep the live deploy token (R29) and any deploy token newer than the secret; when
        // the live one cannot be told apart, revoke nothing. A token being minted now
        // replaces every deploy token (`replace_ids`), so only other names are pruned then.
        let keep: Option<Vec<String>> = if out.iter().any(is_deploy_token) {
            Some(Vec::new())
        } else {
            let updated = gh.and_then(|l| l.api_token_updated_at.as_deref());
            match live_deploy_token(&token_name, known_tokens, updated) {
                LiveToken::NoToken => Some(Vec::new()),
                LiveToken::Live(t, mut newer) => {
                    newer.push(t.id.clone());
                    Some(newer)
                }
                LiveToken::Unclear(_) => None,
            }
        };
        if let Some(keep) = keep {
            for t in mine {
                if !keep.contains(&t.id) && !replace_ids.contains(&t.id) {
                    out.push(Change::RevokeToken {
                        id: t.id.clone(),
                        name: t.name.clone(),
                    });
                }
            }
        }
    }
    out
}

fn is_deploy_token(c: &Change) -> bool {
    matches!(c, Change::DeployToken { .. })
}
