//! Delivery of the R2 token's credentials (spec §6.2): `--secrets-to gh-env:<env>`,
//! `--secrets-to fly:<app>` or `--emit-secrets`. The credentials are four `NAME=value` lines;
//! they go to one sink and nowhere else (never a file, argv, log or report). If delivery fails
//! after the mint, the new token is revoked.

use crate::auth::Secret;
use crate::cloudflare_write::revoke_token;
use crate::desired::Desired;
use crate::error::HoistError;
use crate::http::{Fetched, Http, seg};
use crate::r2::{self, R2Change};
use crate::seal::put_env_secret;
use std::io::Write;
use std::process::{Command, Stdio};

/// The apply flags that choose the sink, and the facts the refusals need.
#[derive(Debug, Clone, Default)]
pub struct SinkOpts {
    /// `--secrets-to`: `gh-env:<env>` or `fly:<app>`.
    pub secrets_to: Option<String>,
    /// `--emit-secrets`.
    pub emit: bool,
    /// `--json`.
    pub json: bool,
    /// Stdout is a terminal.
    pub tty: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sink {
    GhEnv(String),
    Fly(String),
    Emit,
}

const SINKS: &str =
    "`--secrets-to gh-env:<environment>`, `--secrets-to fly:<app>` or `--emit-secrets`";

impl SinkOpts {
    /// The chosen sink, or `None` when no flag is given. Refuses both flags, a bad sink,
    /// `--emit-secrets` to a terminal and `--emit-secrets` with `--json`.
    pub fn parse(&self) -> Result<Option<Sink>, HoistError> {
        let usage = |m: String| Err(HoistError::Usage(m));
        match (&self.secrets_to, self.emit) {
            (Some(_), true) => usage(format!(
                "give exactly one of --secrets-to and --emit-secrets for the R2 credentials: {SINKS}"
            )),
            (None, false) => Ok(None),
            (None, true) if self.json => usage(
                "--emit-secrets prints the credentials on stdout, which --json keeps for the \
                 report; drop --json"
                    .into(),
            ),
            (None, true) if self.tty => usage(
                "--emit-secrets refuses a terminal, so the credentials do not land in your \
                 scrollback; pipe stdout (`hoist apply --emit-secrets | ...`)"
                    .into(),
            ),
            (None, true) => Ok(Some(Sink::Emit)),
            (Some(spec), false) => match spec.split_once(':') {
                Some(("gh-env", env)) if !env.is_empty() && !env.chars().any(char::is_control) => {
                    Ok(Some(Sink::GhEnv(env.to_string())))
                }
                Some(("fly", app)) if valid_app(app) => Ok(Some(Sink::Fly(app.to_string()))),
                _ => usage(format!(
                    "--secrets-to `{spec}` is not a sink: use `gh-env:<environment>` or \
                     `fly:<app>` (an app name is letters, digits, `.`, `_` and `-`)"
                )),
            },
        }
    }
}

fn valid_app(app: &str) -> bool {
    app.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && app
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn fly_program() -> String {
    std::env::var("HOIST_FLY")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "fly".to_string())
}

/// A `fly` that never sees hoist's tokens.
fn fly_command(args: &[&str]) -> Command {
    let mut c = Command::new(fly_program());
    c.args(args);
    for name in ["CLOUDFLARE_API_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        c.env_remove(name);
    }
    c
}

/// Checks before any write: `fly version` runs; the GitHub environment exists.
pub fn preflight(sink: &Sink, d: &Desired, gh: Option<&Http>) -> Result<(), HoistError> {
    match sink {
        Sink::Emit => Ok(()),
        Sink::Fly(_) => {
            let ran = fly_command(&["version"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match ran {
                Ok(s) if s.success() => Ok(()),
                Ok(s) => Err(HoistError::Usage(format!(
                    "`fly version` failed ({s}); install flyctl or point HOIST_FLY at it"
                ))),
                Err(e) => Err(HoistError::Usage(format!(
                    "could not run `fly version`: {e}; install flyctl or point HOIST_FLY at it"
                ))),
            }
        }
        Sink::GhEnv(env) => {
            let gh = gh.ok_or_else(|| {
                HoistError::Usage("--secrets-to gh-env needs a GitHub client".into())
            })?;
            let path = format!(
                "/repos/{}/{}/environments/{}",
                seg(&d.repo.owner),
                seg(&d.repo.name),
                seg(env)
            );
            match gh.get_json(&path)? {
                Fetched::Ok(_) => Ok(()),
                Fetched::NotFound => Err(HoistError::Usage(format!(
                    "GitHub environment `{env}` of {}/{} does not exist; create it first (a \
                     workflow job with `environment: {env}`, then `hoist apply`), or pick \
                     another sink",
                    d.repo.owner, d.repo.name
                ))),
                Fetched::Forbidden(why) => Err(HoistError::Usage(format!(
                    "cannot read GitHub environment `{env}` (403: {why}); the token needs \
                     Environments and Secrets write on the repo"
                ))),
            }
        }
    }
}

/// What the R2 writes need that the preflight resolved.
pub struct R2Run<'a> {
    /// Permission group ids of the bucket token.
    pub groups: Vec<String>,
    pub sink: Sink,
    /// The GitHub client for `gh-env`.
    pub gh: Option<&'a Http>,
}

impl R2Run<'_> {
    pub fn execute(&self, cf: &Http, d: &Desired, change: &R2Change) -> Result<(), HoistError> {
        match change {
            R2Change::Bucket { bucket } => r2::create_bucket(cf, d, bucket),
            R2Change::Lifecycle { bucket, rule } => r2::write_lifecycle(cf, d, bucket, rule),
            R2Change::Token { name, bucket } => {
                let (id, value) = r2::mint_token(cf, d, name, bucket, &self.groups)?;
                let creds = r2::credentials(d, bucket, &id, &value)?;
                drop(value);
                match self.deliver(d, &creds) {
                    Ok(()) => Ok(()),
                    Err(err) => Err(match revoke_token(cf, &id) {
                        Ok(()) => HoistError::Network(format!(
                            "{err}. The new token {id} was revoked; apply again once the \
                             destination works"
                        )),
                        Err(_) => HoistError::Network(format!(
                            "{err}. The new token {id} ({name}) could not be revoked; delete it \
                             by hand in the Cloudflare dashboard (My Profile, API Tokens)"
                        )),
                    }),
                }
            }
        }
    }

    fn deliver(&self, d: &Desired, creds: &[(&'static str, Secret)]) -> Result<(), HoistError> {
        match &self.sink {
            Sink::Emit => {
                let mut out = std::io::stdout().lock();
                for (name, value) in creds {
                    writeln!(out, "{name}={}", value.expose())
                        .map_err(|e| HoistError::Io(format!("cannot write to stdout: {e}")))?;
                }
                out.flush()
                    .map_err(|e| HoistError::Io(format!("cannot write to stdout: {e}")))
            }
            Sink::Fly(app) => {
                let mut child = fly_command(&["secrets", "import", "-a", app])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| {
                        HoistError::Io(format!("could not run `fly secrets import`: {e}"))
                    })?;
                let mut stdin = child.stdin.take().expect("piped stdin");
                let mut sent = Ok(());
                for (name, value) in creds {
                    sent = writeln!(stdin, "{name}={}", value.expose());
                    if sent.is_err() {
                        break;
                    }
                }
                drop(stdin);
                let status = child.wait().map_err(|e| {
                    HoistError::Io(format!("`fly secrets import` did not finish: {e}"))
                })?;
                match (sent, status.success()) {
                    (Ok(()), true) => Ok(()),
                    (_, false) => Err(HoistError::Io(format!(
                        "`fly secrets import -a {app}` failed ({status})"
                    ))),
                    (Err(e), true) => Err(HoistError::Io(format!(
                        "cannot send the credentials to `fly secrets import`: {e}"
                    ))),
                }
            }
            Sink::GhEnv(env) => {
                let gh = self.gh.ok_or_else(|| {
                    HoistError::Usage("--secrets-to gh-env needs a GitHub client".into())
                })?;
                let mut written: Vec<&str> = Vec::new();
                for (name, value) in creds {
                    put_env_secret(gh, (&d.repo.owner, &d.repo.name), env, name, value).map_err(
                        |e| {
                            let done = match written.is_empty() {
                                true => String::new(),
                                false => format!(
                                    " (already written to environment `{env}`: {})",
                                    written.join(", ")
                                ),
                            };
                            HoistError::Network(format!("{e}{done}"))
                        },
                    )?;
                    written.push(name);
                }
                Ok(())
            }
        }
    }
}
