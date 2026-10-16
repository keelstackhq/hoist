//! Behaviour checks for `hoist check` (spec §7). Offline against the working tree, or
//! anonymous GETs of public URLs; no token is ever sent to a site host.

use crate::cloudflare::PagesProject;
use crate::desired::Desired;
use crate::http::probe;
use crate::repo::workflows::Workflows;
use crate::report::{Action, Item, Verify};

/// Env override (tests only) that replaces `https://<subdomain>` with `<base>`.
const PAGES_BASE_ENV: &str = "HOIST_PAGES_BASE";

/// One `github/required_check` item per required check: it must be a job id or job `name` in a
/// workflow under `.github/workflows/`.
pub fn verify_required_checks(d: &Desired, w: &Workflows) -> Vec<Item> {
    d.github
        .ruleset
        .required_checks
        .iter()
        .map(|check| {
            let found = w.jobs.iter().any(|j| j.value == check.value);
            let (reason, ok) = if found {
                ("a job with this name exists in the workflows".to_string(), true)
            } else {
                (
                    format!(
                        "required check `{}` is not the name of any job in `.github/workflows/*.yml`",
                        check.value
                    ),
                    false,
                )
            };
            Item {
                area: "github".to_string(),
                kind: "required_check".to_string(),
                key: check.value.clone(),
                action: Action::Ok,
                reason: reason.clone(),
                source: check.source.clone(),
                verify: Some(Verify { ok, detail: reason }),
            }
        })
        .collect()
}

/// 2xx is ok. Only things that exist are probed, so a 404 means no deployment answers yet.
fn verdict(url: &str, result: Result<u16, String>) -> Verify {
    match result {
        Ok(200..=299) => Verify {
            ok: true,
            detail: format!("GET {url} answered 2xx"),
        },
        Ok(404) => Verify {
            ok: false,
            detail: "no deployment answers yet (404)".to_string(),
        },
        Ok(status) => Verify {
            ok: false,
            detail: format!("GET {url} answered {status}"),
        },
        Err(why) => Verify {
            ok: false,
            detail: format!("GET {url} failed: {why}"),
        },
    }
}

/// `GET https://<subdomain>/` (base from `HOIST_PAGES_BASE` when set): 2xx is ok. A 404 on an
/// existing project means the first deploy has not happened yet, which is still a failure.
pub fn verify_pages(p: &PagesProject) -> Verify {
    let base = std::env::var(PAGES_BASE_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://{}", p.subdomain));
    let url = format!("{base}/");
    verdict(&url, probe(&url))
}

/// HTTPS GET `https://<host>/`: 2xx with a certificate the platform roots accept (rustls
/// validation; a bad certificate is a transport error and fails the check). Only attached
/// domains are probed, so a 404 is "no deployment answers yet", a failure (spec §7.4).
pub fn verify_domain(host: &str) -> Verify {
    let url = format!("https://{host}/");
    verdict(&url, probe(&url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_classifies_statuses() {
        assert!(verdict("u", Ok(204)).ok);
        assert!(!verdict("u", Ok(500)).ok);
        assert_eq!(
            verdict("u", Ok(404)).detail,
            "no deployment answers yet (404)"
        );
        assert!(!verdict("u", Ok(404)).ok);
        assert!(!verdict("u", Err("tls".into())).ok);
    }
}
