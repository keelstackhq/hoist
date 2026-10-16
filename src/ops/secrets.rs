//! `hoist secrets set NAME` (spec §8): seal a value read from the environment or a hidden
//! prompt into a repository Actions secret. The value never touches argv, stdout, stderr or
//! the report; it lives in a `Secret`.

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

/// GitHub's rule for Actions secret names, plus the reserved `GITHUB_` prefix.
fn validate_name(name: &str) -> Result<(), HoistError> {
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

/// The value of env var `name` if set and non-empty; else a hidden prompt on a terminal;
/// else exit 3. The value is never an argument.
pub fn read_secret_value(name: &str) -> Result<Secret, HoistError> {
    if let Ok(value) = std::env::var(name)
        && !value.is_empty()
    {
        return Ok(Secret::from(value));
    }
    if std::io::stdin().is_terminal() {
        let value = rpassword::prompt_password(format!("value for {name}: "))
            .map_err(|e| HoistError::Usage(format!("cannot read the value: {e}")))?;
        if value.is_empty() {
            return Err(HoistError::Usage(format!("no value given for {name}")));
        }
        return Ok(Secret::from(value));
    }
    Err(HoistError::Usage(format!(
        "set {name} in the environment or run in a terminal"
    )))
}

pub fn secrets_set(dir: &Path, name: &str, verbose: bool) -> Result<Report, HoistError> {
    validate_name(name)?;
    let value = read_secret_value(name)?;
    let desired = build_desired(dir)?;
    let http = Http::new(Service::Github, github_token()?).verbose(verbose);
    let live = read_github(&http, &desired)?;
    preflight_github_admin(&live, &desired)?;
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
            action: if exit_code == 0 {
                Action::Ok
            } else {
                Action::Update
            },
            reason,
            source: "command line".into(),
            verify: None,
        }],
        human_steps: Vec::new(),
        exit_code,
    })
}
