//! The registry and deploy steps of `release publish` (spec §9.3 steps 3–5): npm via
//! `npm publish` (npm does the OIDC exchange), crates.io via `cargo publish` with a
//! short-lived trusted-publishing token, and `on = "release"` deploys via pnpm.

use super::{Clients, Job, Step};
use crate::config::NpmAccess;
use crate::diff_release::first_publish_step;
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::Http;
use crate::release::oidc::crates_token;
use crate::release::registry::{package_exists, version_exists};
use crate::release::tools::{Tool, run, run_redacted};
use crate::release::{Package, PkgKind};
use crate::report::{Action, HumanStep, Verify};
use std::path::{Path, PathBuf};

/// `npm publish` performs the OIDC exchange itself from this version on.
const MIN_NPM: (u64, u64, u64) = (11, 5, 1);
/// Verification polls each registry this many times, this many seconds apart (spec §9.5).
const POLLS: u32 = 5;
const POLL_SECS: u64 = 2;

/// What a step did (or found): done (then verified), or waiting on a person.
pub(super) enum Outcome {
    Done(String),
    Pending(String, HumanStep),
}

fn registry(kind: PkgKind) -> &'static str {
    match kind {
        PkgKind::Npm => "npm",
        PkgKind::Crate => "crates.io",
    }
}

fn client(c: &Clients, step: Step) -> &Http {
    match step {
        Step::Npm | Step::Platform(_) => &c.npm,
        _ => &c.crates,
    }
}

fn access(pkg: &Package) -> &'static str {
    match pkg.publish.npm.as_ref().map(|n| n.access) {
        Some(NpmAccess::Restricted) => "restricted",
        _ => "public",
    }
}

fn deploy_cmd(pkg: &Package) -> String {
    format!("pnpm --filter {} run deploy", pkg.name)
}

/// Run a tool and echo its stdout to stderr (stdout is reserved for the report).
fn tool(
    t: Tool,
    dir: &Path,
    args: &[&str],
    env: &[(&str, &crate::auth::Secret)],
) -> Result<String, HoistError> {
    let out = run(t, dir, args, env)?;
    eprint!("{out}");
    Ok(out)
}

/// The first `MAJOR.MINOR.PATCH` in `text`.
fn first_version(text: &str) -> Option<(u64, u64, u64)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|word| {
            let mut parts = word.split('.').map(|p| p.parse::<u64>().ok());
            match (parts.next(), parts.next(), parts.next()) {
                (Some(Some(a)), Some(Some(b)), Some(Some(c))) => Some((a, b, c)),
                _ => None,
            }
        })
}

fn check_npm(dir: &Path) -> Result<(), HoistError> {
    let out = tool(Tool::Npm, dir, &["--version"], &[])?;
    let (a, b, c) = MIN_NPM;
    match first_version(&out) {
        Some(v) if v >= MIN_NPM => Ok(()),
        found => Err(HoistError::Usage(format!(
            "npm {} is older than {a}.{b}.{c}, which trusted publishing (OIDC) needs; install a \
             newer npm in the job (`npm install -g npm@latest`) before hoist",
            found.map_or_else(
                || format!("(unreadable version `{}`)", out.trim()),
                |(x, y, z)| format!("{x}.{y}.{z}")
            )
        ))),
    }
}

/// Is the version already there, or must a person publish the package first? `None` means
/// hoist publishes it now.
fn precheck(http: &Http, pkg: &Package) -> Result<Option<Outcome>, HoistError> {
    let v = &pkg.version.value;
    let reg = registry(pkg.kind);
    match version_exists(http, pkg, v)? {
        Fetch::Got(true) => return Ok(Some(Outcome::Done(format!("{v} already on {reg}")))),
        Fetch::Got(false) => {}
        Fetch::Unknown(why) => {
            return Err(HoistError::Network(format!(
                "cannot read {reg} for {}@{v}: {why}",
                pkg.name
            )));
        }
    }
    match package_exists(http, pkg)? {
        Fetch::Got(true) => Ok(None),
        Fetch::Got(false) => Ok(Some(Outcome::Pending(
            format!(
                "{} is not on {reg} yet; a person publishes it first (see the human step)",
                pkg.name
            ),
            first_publish_step(&pkg.name, pkg.kind),
        ))),
        Fetch::Unknown(why) => Err(HoistError::Network(format!(
            "cannot read {reg} for {}: {why}",
            pkg.name
        ))),
    }
}

/// A launcher's binary copied into a platform package for one `npm publish`; `remove` puts
/// the checkout back as it was.
struct CopiedBin {
    file: PathBuf,
    /// The `bin/` directory, when this copy created it.
    created_dir: Option<PathBuf>,
}

impl CopiedBin {
    fn remove(self) -> Result<(), HoistError> {
        let fail = |p: &Path, e: std::io::Error| {
            HoistError::Io(format!(
                "cannot remove the copied binary {}: {e}",
                p.display()
            ))
        };
        std::fs::remove_file(&self.file).map_err(|e| fail(&self.file, e))?;
        if let Some(dir) = &self.created_dir {
            std::fs::remove_dir(dir).map_err(|e| fail(dir, e))?;
        }
        Ok(())
    }
}

/// Copy a launcher's binary to `<platform package>/bin/<bin>[.exe]`, mode 0755 on unix.
fn copy_bin(job: &Job, i: usize, pkg_dir: &Path) -> Result<CopiedBin, HoistError> {
    let (Some(l), Some(src)) = (&job.pkg.launcher, job.bins.get(i)) else {
        return Err(HoistError::Io(format!(
            "no binary for platform package #{i} of `{}`",
            job.pkg.name
        )));
    };
    let file = pkg_dir.join(l.platforms[i].bin_file(&l.bin));
    let what = format!("cannot copy {} to {}", src.display(), file.display());
    let fail = |e: std::io::Error| HoistError::Io(format!("{what}: {e}"));
    let parent = file.parent().unwrap_or(pkg_dir).to_path_buf();
    let created_dir = (!parent.exists()).then(|| parent.clone());
    std::fs::create_dir_all(&parent).map_err(fail)?;
    let copied = CopiedBin { file, created_dir };
    let result = std::fs::copy(src, &copied.file).and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&copied.file, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(copied),
        Err(e) => {
            let err = fail(e);
            if copied.file.exists() {
                let _ = copied.remove();
            } else if let Some(dir) = &copied.created_dir {
                let _ = std::fs::remove_dir(dir);
            }
            Err(err)
        }
    }
}

/// `npm publish` of the job's package, or of a launcher's platform package (after copying its
/// binary in). Skipped when the version is already on npm.
fn npm(c: &Clients, dir: &Path, job: &Job, step: Step) -> Result<Outcome, HoistError> {
    let pkg = job.step_pkg(step);
    if let Some(done) = precheck(&c.npm, pkg)? {
        return Ok(done);
    }
    let pkg_dir = dir.join(&pkg.dir);
    check_npm(&pkg_dir)?;
    let copied = match step {
        Step::Platform(i) => Some(copy_bin(job, i, &pkg_dir)?),
        _ => None,
    };
    let published = tool(
        Tool::Npm,
        &pkg_dir,
        &["publish", "--access", access(pkg)],
        &[],
    );
    // The binary leaves the checkout again, whether npm succeeded or not.
    let removed = copied.map_or(Ok(()), CopiedBin::remove);
    published?;
    removed?;
    Ok(Outcome::Done(format!(
        "published {} to npm",
        pkg.version.value
    )))
}

/// Exchange, publish, revoke. A failed revoke goes to `warnings` (no value) and does not fail
/// a successful publish.
fn crates(
    c: &Clients,
    dir: &Path,
    job: &Job,
    warnings: &mut Vec<String>,
) -> Result<Outcome, HoistError> {
    let pkg = job.pkg;
    if let Some(done) = precheck(&c.crates, pkg)? {
        return Ok(done);
    }
    let (token, guard) = crates_token(&c.crates)?;
    // R21: cargo's output is captured and the token replaced by `***` before it is shown.
    let published = run_redacted(
        Tool::Cargo,
        dir,
        &["publish", "-p", &pkg.name, "--locked"],
        &[("CARGO_REGISTRY_TOKEN", &token)],
    );
    drop(token);
    if let Err(e) = guard.revoke() {
        warnings.push(format!(
            "could not revoke the short-lived crates.io token: {e}; it expires on its own"
        ));
    }
    published.map_err(HoistError::from)?;
    Ok(Outcome::Done(format!(
        "published {} to crates.io",
        pkg.version.value
    )))
}

fn deploy(dir: &Path, job: &Job) -> Result<Outcome, HoistError> {
    tool(
        Tool::Pnpm,
        dir,
        &["--filter", &job.pkg.name, "run", "deploy"],
        &[],
    )?;
    Ok(Outcome::Done(format!("ran `{}`", deploy_cmd(job.pkg))))
}

/// Run one registry or deploy step.
pub(super) fn run_step(
    c: &Clients,
    dir: &Path,
    job: &Job,
    step: Step,
    warnings: &mut Vec<String>,
) -> Result<Outcome, HoistError> {
    match step {
        Step::Npm | Step::Platform(_) => npm(c, dir, job, step),
        Step::Crates => crates(c, dir, job, warnings),
        Step::Deploy => deploy(dir, job),
        Step::Tag | Step::Release => unreachable!("GitHub steps run in github.rs"),
    }
}

/// `--dry-run`: what a registry or deploy step would do, from GETs only.
pub(super) fn dry_run(
    c: &Clients,
    job: &Job,
    step: Step,
) -> Result<(Action, String, Option<HumanStep>), HoistError> {
    let pkg = job.step_pkg(step);
    let what = match step {
        Step::Deploy => {
            return Ok((
                Action::Create,
                format!("would run `{}`", deploy_cmd(pkg)),
                None,
            ));
        }
        Step::Npm | Step::Platform(_) => format!("npm publish --access {}", access(pkg)),
        _ => format!("cargo publish -p {} --locked", pkg.name),
    };
    Ok(match precheck(client(c, step), pkg) {
        Ok(Some(Outcome::Done(why))) => (Action::Ok, why, None),
        Ok(Some(Outcome::Pending(why, human))) => (Action::Pending, why, Some(human)),
        Ok(None) => (
            Action::Create,
            format!("would publish {} with `{what}`", pkg.version.value),
            None,
        ),
        Err(e) => (Action::Unknown, e.to_string(), None),
    })
}

/// The version is visible on the registry: up to 5 reads, 2 s apart (capped by
/// `HOIST_MAX_SLEEP_MS`).
pub(super) fn verify(c: &Clients, job: &Job, step: Step) -> Verify {
    let pkg = job.step_pkg(step);
    let http = client(c, step);
    let (v, reg) = (&pkg.version.value, registry(pkg.kind));
    for attempt in 1..=POLLS {
        if let Ok(Fetch::Got(true)) = version_exists(http, pkg, v) {
            return Verify {
                ok: true,
                detail: format!("{}@{v} is on {reg}", pkg.name),
            };
        }
        if attempt < POLLS {
            http.pause(POLL_SECS);
        }
    }
    Verify {
        ok: false,
        detail: format!(
            "{}@{v} is not visible on {reg} after {POLLS} checks {POLL_SECS} s apart",
            pkg.name
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::first_version;

    #[test]
    fn npm_version_is_the_first_semver_triple() {
        assert_eq!(first_version("12.2.0\n"), Some((12, 2, 0)));
        assert_eq!(first_version("npm v11.5.1 (node 24)"), Some((11, 5, 1)));
        assert_eq!(first_version("node 24, npm 11.4.2"), Some((11, 4, 2)));
        assert_eq!(first_version("10.9\n"), None);
        assert_eq!(first_version(""), None);
        assert!(first_version("11.5.0").unwrap() < (11, 5, 1));
    }
}
