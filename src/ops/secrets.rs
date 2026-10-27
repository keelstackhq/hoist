//! `hoist secrets set NAME` (spec §8): seal a value read from the environment or a hidden
//! prompt into a repository Actions secret. The value never touches argv, stdout, stderr or
//! the report; it lives in a `Secret`. `hoist secrets rotate` lives in `rotate.rs` (§8.3).

use crate::auth::{Secret, github_token};
use crate::desired::build_desired;
use crate::error::HoistError;
use crate::github::read_github;
use crate::http::{Http, Service};
use crate::ops::apply::preflight_github_admin;
use crate::report::{Action, Item, Report};
use crate::seal::put_secret;
use std::io::IsTerminal;
use std::path::Path;

mod check;
mod consumer;
mod rotate;
pub use check::secrets_check;
pub use rotate::secrets_rotate;

/// GitHub's rule for Actions secret names, plus the reserved `GITHUB_` prefix. Errors never
/// show anything after a `=` (R30: `NAME=VALUE` must not echo the value).
fn validate_name(name: &str) -> Result<(), HoistError> {
    if let Some((before, _)) = name.split_once('=') {
        return Err(HoistError::Usage(format!(
            "invalid secret name `{before}=…`: `NAME=VALUE` is not accepted (the value was not \
             used); hoist reads the value from env {before} or a hidden prompt"
        )));
    }
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(HoistError::Usage(format!(
            "invalid secret name `{name}`: use only A-Z, 0-9 and `_`, not starting with a digit \
             (GitHub's rule, `^[A-Z_][A-Z0-9_]*$`)"
        )));
    }
    if name.starts_with("GITHUB_") {
        return Err(HoistError::Usage(format!(
            "invalid secret name `{name}`: GitHub rejects names starting with `GITHUB_`; pick \
             another name"
        )));
    }
    Ok(())
}

/// Names that are also hoist's own credentials (`auth.rs` reads them from the environment):
/// their env value is the operator's credential, never a value to seal (R21).
fn is_own_credential(name: &str) -> bool {
    matches!(
        name,
        "CLOUDFLARE_API_TOKEN" | "CLOUDFLARE_ACCOUNT_ID" | "GH_TOKEN"
    )
}

fn no_source(name: &str, protected: bool) -> HoistError {
    HoistError::Usage(if protected {
        let mint = if name == "CLOUDFLARE_API_TOKEN" {
            " or let `hoist apply` mint a scoped deploy token"
        } else {
            ""
        };
        format!(
            "{name} is also hoist's own credential; enter the value at the prompt in a \
             terminal{mint}"
        )
    } else {
        format!("set {name} in the environment or run in a terminal")
    })
}

/// Pick the value: env (non-empty, not for a protected name), else the prompt on a terminal,
/// else exit 3. Returns the value and its source label (`env NAME` or `prompt`).
fn resolve(
    name: &str,
    env: Option<String>,
    tty: bool,
    protected: bool,
    prompt: impl FnOnce() -> std::io::Result<String>,
) -> Result<(Secret, String), HoistError> {
    if !protected && let Some(value) = env.filter(|v| !v.is_empty()) {
        return Ok((Secret::from(value), format!("env {name}")));
    }
    if !tty {
        return Err(no_source(name, protected));
    }
    let value = prompt().map_err(|e| HoistError::Usage(format!("cannot read the value: {e}")))?;
    if value.is_empty() {
        return Err(HoistError::Usage(format!("no value given for {name}")));
    }
    Ok((Secret::from(value), "prompt".to_string()))
}

/// The value of env var `name` if set and non-empty (never for hoist's own credential names);
/// else a hidden prompt on a terminal; else exit 3. The value is never an argument.
pub fn read_secret_value(name: &str) -> Result<Secret, HoistError> {
    read_with_source(name).map(|(v, _)| v)
}

fn read_with_source(name: &str) -> Result<(Secret, String), HoistError> {
    resolve(
        name,
        std::env::var(name).ok(),
        std::io::stdin().is_terminal(),
        is_own_credential(name),
        || rpassword::prompt_password(format!("value for {name}: ")),
    )
}

pub fn secrets_set(
    dir: &Path,
    name: &str,
    extra: &[String],
    verbose: bool,
) -> Result<Report, HoistError> {
    validate_name(name)?;
    if !extra.is_empty() {
        // Fixed text: the extra arguments may be the secret value, so they are never shown.
        return Err(HoistError::Usage(format!(
            "`hoist secrets set` takes only NAME; the extra argument was refused and not used. \
             hoist reads the value of {name} from the environment or a hidden prompt, never \
             from the command line"
        )));
    }
    // Fail before any request when no value source can exist.
    let protected = is_own_credential(name);
    let has_env = !protected && std::env::var(name).is_ok_and(|v| !v.is_empty());
    if !has_env && !std::io::stdin().is_terminal() {
        return Err(no_source(name, protected));
    }
    let desired = build_desired(dir)?;
    let http = Http::new(Service::Github, github_token()?).verbose(verbose);
    let live = read_github(&http, &desired)?;
    preflight_github_admin(&live, &desired)?;
    let exists = live
        .secrets
        .as_got()
        .is_ok_and(|have| have.iter().any(|n| n == name));
    // Read or prompt last, just before the write.
    let (value, source) = read_with_source(name)?;
    let (exit_code, reason) = match put_secret(&http, &desired, name, &value) {
        Ok(()) => (0, "set".to_string()),
        Err(err) => (1, format!("failed: {err}")),
    };
    Ok(Report {
        command: "secrets set".to_string(),
        items: vec![Item {
            area: "github".into(),
            kind: "secret".into(),
            key: name.into(),
            action: match (exit_code, exists) {
                (0, _) => Action::Ok,
                (_, true) => Action::Update,
                (_, false) => Action::Create,
            },
            reason,
            source,
            verify: None,
        }],
        human_steps: Vec::new(),
        exit_code,
        text: None,
        hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_prompt() -> std::io::Result<String> {
        panic!("must not prompt")
    }

    #[test]
    fn env_value_wins() {
        let (v, src) = resolve("X", Some("v".into()), false, false, no_prompt).unwrap();
        assert_eq!((v.expose(), src.as_str()), ("v", "env X"));
    }

    #[test]
    fn empty_env_falls_to_prompt() {
        let (v, src) = resolve("X", Some(String::new()), true, false, || Ok("p".into())).unwrap();
        assert_eq!((v.expose(), src.as_str()), ("p", "prompt"));
    }

    #[test]
    fn empty_prompt_is_an_error() {
        assert!(resolve("X", None, true, false, || Ok(String::new())).is_err());
    }

    #[test]
    fn no_tty_without_env_is_an_error() {
        let err = resolve("X", None, false, false, no_prompt).unwrap_err();
        assert!(err.to_string().contains("set X in the environment"));
    }

    #[test]
    fn protected_name_ignores_env() {
        let (v, src) = resolve("X", Some("env".into()), true, true, || Ok("p".into())).unwrap();
        assert_eq!((v.expose(), src.as_str()), ("p", "prompt"));
        let err = resolve("X", Some("env".into()), false, true, no_prompt).unwrap_err();
        assert!(err.to_string().contains("also hoist's own credential"));
    }
}
