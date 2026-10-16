//! `hoist secrets rotate [NAME]` (spec §8.3): mint a new deploy token, verify it with
//! Cloudflare, seal it into `CLOUDFLARE_API_TOKEN`, then revoke every older
//! `hoist:<owner>/<repo>:deploy` token. Errors before the first write exit 3 (R22); after
//! it, failures are report items with exit 1 (R23). The new value lives only in a `Secret`.

use crate::auth::{Secret, cloudflare_token, github_token};
use crate::change::deploy_token_name;
use crate::cloudflare::{TokenStatus, VERIFY_PATH, read_cloudflare, token_status};
use crate::cloudflare_write::{mint_with_groups, permission_group_ids, revoke_token};
use crate::config::Managed;
use crate::desired::{Desired, build_desired};
use crate::error::HoistError;
use crate::github::{Fetch, read_github};
use crate::http::{Http, HttpError, Service};
use crate::ops::apply::{preflight_cloudflare, preflight_github_admin};
use crate::report::{Action, HumanStep, Item, Report};
use crate::seal::put_secret;
use std::path::Path;

const ROTATABLE: &str = "CLOUDFLARE_API_TOKEN";
/// R24: a fresh token may need a moment before Cloudflare reports it active.
const VERIFY_ATTEMPTS: u32 = 3;

pub fn secrets_rotate(dir: &Path, name: Option<&str>, verbose: bool) -> Result<Report, HoistError> {
    if name.is_some_and(|n| n != ROTATABLE) {
        return Err(HoistError::Usage(format!(
            "only minted secrets can be rotated: {ROTATABLE}"
        )));
    }
    let desired = build_desired(dir)?;
    require_hoist_cloudflare(&desired)?;
    let gh = Http::new(Service::Github, github_token()?).verbose(verbose);
    preflight_github_admin(&read_github(&gh, &desired)?, &desired)?;
    let cf = Http::new(Service::Cloudflare, cloudflare_token()?).verbose(verbose);
    preflight_cloudflare(&cf, &desired)?;
    let token_name = deploy_token_name(&desired);
    let old_ids: Vec<String> = match read_cloudflare(&cf, &desired)?.tokens {
        Fetch::Got(list) => list
            .into_iter()
            .filter(|t| t.name == token_name)
            .map(|t| t.id)
            .collect(),
        Fetch::Unknown(_) => {
            return Err(HoistError::Usage(
                "cannot list Cloudflare tokens; hoist will not rotate blind".into(),
            ));
        }
    };

    // R25: resolve the permission groups (a GET) before the first write.
    let groups = permission_group_ids(&cf, &desired)?;

    let mut out = Out::new(&token_name);
    // First write: mint. A failure here leaves nothing behind (mint cleans up after itself).
    let (new_id, value) = match mint_with_groups(&cf, &desired, &token_name, &groups) {
        Ok(minted) => minted,
        Err(err) => return Ok(out.secret_failed(&format!("minting the new token: {err}"))),
    };
    let sealed = verify_new(&cf, &value)
        .map_err(|why| format!("verifying the new token: {why}"))
        .and_then(|()| {
            put_secret(&gh, &desired, ROTATABLE, &value)
                .map_err(|err| format!("sealing the new token: {err}"))
        });
    drop(value);
    if let Err(why) = sealed {
        // The old secret is untouched; take the new token back.
        if let Err(err) = revoke_token(&cf, &new_id) {
            out.revoke_failed(&new_id, &err);
        }
        return Ok(out.secret_failed(&why));
    }
    out.secret_rotated();
    for id in &old_ids {
        match revoke_token(&cf, id) {
            Ok(()) => out.token(id, "revoked".into()),
            Err(err) => out.revoke_failed(id, &err),
        }
    }
    Ok(out.report)
}

/// R22: rotation needs hoist-managed Cloudflare with a deploy target and an account id.
fn require_hoist_cloudflare(d: &Desired) -> Result<(), HoistError> {
    let cf = &d.cloudflare;
    let has_target = !(cf.pages.is_empty() && cf.workers.is_empty());
    if cf.managed != Managed::Hoist || !has_target || cf.account_id.is_none() {
        return Err(HoistError::Usage(
            "rotate needs hoist-managed Cloudflare (hoist.toml / inference)".into(),
        ));
    }
    Ok(())
}

/// `GET /user/tokens/verify` with the new token itself, on cf's base and sleep cap; up to
/// `VERIFY_ATTEMPTS` tries about 1 s apart while it is not active or answers 401 (R24). The
/// reason never carries the value.
fn verify_new(cf: &Http, value: &Secret) -> Result<(), String> {
    let http = Http::with_base_like(cf, value.clone());
    let mut last = String::new();
    for attempt in 1..=VERIFY_ATTEMPTS {
        if attempt > 1 {
            http.pause(1);
        }
        last = match token_status(&http) {
            Ok(TokenStatus::Active) => return Ok(()),
            Ok(TokenStatus::NotActive(why)) => why,
            Err(HttpError::Unauthorized { .. }) => "rejected (401)".to_string(),
            Err(err) => return Err(err.to_string()),
        };
    }
    Err(format!(
        "the newly minted token did not verify after {VERIFY_ATTEMPTS} attempts (GET \
         {VERIFY_PATH}: {last})"
    ))
}

/// The report under construction.
struct Out {
    report: Report,
    source: String,
}

impl Out {
    fn new(token_name: &str) -> Out {
        Out {
            report: Report {
                command: "secrets rotate".into(),
                items: Vec::new(),
                human_steps: Vec::new(),
                exit_code: 0,
                text: None,
            },
            source: token_name.to_string(),
        }
    }

    fn push(&mut self, area: &str, kind: &str, key: &str, reason: String) {
        self.report.items.push(Item {
            area: area.into(),
            kind: kind.into(),
            key: key.into(),
            action: Action::Update,
            reason,
            source: self.source.clone(),
            verify: None,
        });
    }

    fn secret_rotated(&mut self) {
        self.push("github", "secret", ROTATABLE, "rotated".into());
    }

    /// The secret was not rotated (old value untouched): exit 1.
    fn secret_failed(mut self, why: &str) -> Report {
        self.push("github", "secret", ROTATABLE, format!("failed: {why}"));
        self.report.exit_code = 1;
        self.report
    }

    fn token(&mut self, id: &str, reason: String) {
        self.push("cloudflare", "token", id, reason);
    }

    /// A token hoist could not revoke: exit 1 and a human step naming its id.
    fn revoke_failed(&mut self, id: &str, err: &HoistError) {
        self.token(id, format!("failed: {err}"));
        self.report.human_steps.push(HumanStep {
            id: format!("revoke-token:{id}"),
            title: format!("revoke Cloudflare token {id} in the dashboard"),
            url: "https://dash.cloudflare.com/profile/api-tokens".into(),
            status: "pending".into(),
        });
        self.report.exit_code = 1;
    }
}
