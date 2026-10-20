//! `hoist release bootstrap` (spec §9.6): take every missing package to "ready for trusted
//! publishing", locally, with the maintainer's own npm and cargo logins. The preflight reads
//! only and exits 3 naming the failed condition; `--dry-run` stops after it.

use crate::desired::{DesiredRelease, read_desired_release};
use crate::error::HoistError;
use crate::github::Fetch;
use crate::http::{Http, Service};
use crate::ops::apply::ask_lines;
use crate::release::registry::package_exists;
use crate::release::semver::Version;
use crate::release::tools::{Tool, run};
use crate::release::{Package, PkgKind, ReleaseModel, read_release_model};
use crate::repo::git::{GithubRepo, read_repo_local};
use crate::report::{Action, Item, Report};
use std::io::{BufRead, Write};
use std::path::Path;

mod trust;
pub(crate) use trust::trusted;

const COMMAND: &str = "release bootstrap";
/// The first npm with `npm trust`.
const MIN_NPM: Version = Version {
    major: 11,
    minor: 15,
    patch: 0,
};
/// The workflow file when no job uses the release environment yet (as `plan` reports it).
const DEFAULT_WORKFLOW: &str = "release.yml";
const NO_TAG: &str = "run the release first; bootstrap publishes the tagged version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapOpts {
    pub dry_run: bool,
    pub yes: bool,
    /// Whether stdin is a terminal (the CLI sets it; tests pass it).
    pub tty: bool,
}

/// One write of a bootstrap run, in execution order (see [`plan_steps`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// Publish a `0.0.0` placeholder from a temp dir.
    NpmPlaceholder(Package),
    /// Publish the crate's manifest version from a worktree at `tag`.
    Crate { pkg: Package, tag: String },
    /// Register the npm trusted publisher.
    NpmTrust(Package),
    /// Point at crates.io's trusted-publishing settings (no API).
    CrateTrust(Package),
}

/// What the preflight learned about one registry package.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Facts {
    pub pkg: Package,
    /// The package exists on its registry.
    pub exists: bool,
    /// npm: `npm trust list` shows our trusted publisher (always false for crates).
    pub trusted: bool,
    /// Crates: the release tag of the manifest version (empty for npm).
    pub tag: String,
    /// Crates: `tag` exists locally.
    pub tagged: bool,
}

/// The repo, workflow file and environment trusted publishers are registered for.
pub(crate) struct Trust {
    pub repo: GithubRepo,
    pub workflow: String,
    pub env: String,
}

impl Trust {
    fn describe(&self) -> String {
        format!(
            "repo {}, workflow {}, environment {}",
            self.repo, self.workflow, self.env
        )
    }
}

fn item(kind: &str, pkg: &Package, action: Action, reason: String) -> Item {
    Item {
        area: "release".to_string(),
        kind: kind.to_string(),
        key: pkg.name.clone(),
        action,
        reason,
        source: pkg.version.source.clone(),
        verify: None,
    }
}

/// The steps to run, in order (npm placeholders, crates, npm trust, crate trust), plus the
/// `ok` and `pending` items of what needs no step.
pub(crate) fn plan_steps(facts: &[Facts]) -> (Vec<Step>, Vec<Item>) {
    let (mut publish, mut trust, mut items) = (Vec::new(), Vec::new(), Vec::new());
    for f in facts.iter().filter(|f| f.pkg.kind == PkgKind::Npm) {
        let p = &f.pkg;
        if f.exists {
            items.push(item(
                "bootstrap_npm",
                p,
                Action::Ok,
                "exists on npm; not touched".to_string(),
            ));
        } else {
            publish.push(Step::NpmPlaceholder(p.clone()));
        }
        if f.trusted {
            items.push(item(
                "bootstrap_trust",
                p,
                Action::Ok,
                "already trusted".to_string(),
            ));
        } else {
            trust.push(Step::NpmTrust(p.clone()));
        }
    }
    for f in facts.iter().filter(|f| f.pkg.kind == PkgKind::Crate) {
        let p = &f.pkg;
        if f.exists {
            items.push(item(
                "bootstrap_crate",
                p,
                Action::Ok,
                "exists on crates.io; not touched".to_string(),
            ));
        } else if f.tagged {
            publish.push(Step::Crate {
                pkg: p.clone(),
                tag: f.tag.clone(),
            });
        } else {
            items.push(item(
                "bootstrap_crate",
                p,
                Action::Pending,
                format!("no tag {}: {NO_TAG}", f.tag),
            ));
            items.push(item(
                "bootstrap_trust",
                p,
                Action::Pending,
                format!("the trusted publisher needs {} on crates.io first", p.name),
            ));
            continue;
        }
        trust.push(Step::CrateTrust(p.clone()));
    }
    publish.extend(trust);
    (publish, items)
}

/// The planned item of a step (`create`: it will write or ask for a human step).
fn step_item(step: &Step, t: &Trust) -> Item {
    match step {
        Step::NpmPlaceholder(p) => item(
            "bootstrap_npm",
            p,
            Action::Create,
            "not on npm; would publish a placeholder 0.0.0 from a temp dir".to_string(),
        ),
        Step::Crate { pkg, tag } => item(
            "bootstrap_crate",
            pkg,
            Action::Create,
            format!(
                "not on crates.io; would publish {} from tag {tag}",
                pkg.version.value
            ),
        ),
        Step::NpmTrust(p) => item(
            "bootstrap_trust",
            p,
            Action::Create,
            format!("would register the trusted publisher: {}", t.describe()),
        ),
        Step::CrateTrust(p) => item(
            "bootstrap_trust",
            p,
            Action::Create,
            format!(
                "register the trusted publisher by hand at https://crates.io/crates/{}/settings: {}",
                p.name,
                t.describe()
            ),
        ),
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "bootstrap_npm" => 0,
        "bootstrap_crate" => 1,
        _ => 2,
    }
}

/// The step items and the other items, grouped by kind (publishes before trust), each group
/// in package order.
fn all_items(steps: &[Step], other: Vec<Item>, t: &Trust) -> Vec<Item> {
    let mut items: Vec<Item> = steps.iter().map(|s| step_item(s, t)).collect();
    items.extend(other);
    items.sort_by_key(|i| kind_rank(&i.kind));
    items
}

fn report(items: Vec<Item>, exit_code: i32) -> Report {
    Report {
        command: COMMAND.to_string(),
        items,
        human_steps: vec![],
        exit_code,
        text: None,
    }
}

/// First preflight condition, before anything is read: never inside GitHub Actions.
fn refuse_in_actions() -> Result<(), HoistError> {
    match std::env::var("GITHUB_ACTIONS") {
        Ok(v) if v.trim() == "true" => Err(HoistError::Usage(
            "GITHUB_ACTIONS=true: bootstrap runs locally with your own logins; CI publishes \
             through `hoist release publish`"
                .to_string(),
        )),
        _ => Ok(()),
    }
}

/// Every package that publishes to a registry: a launcher's platform packages, then the
/// launcher (the publish order), then the next package.
fn registry_packages(r: &DesiredRelease) -> Vec<&Package> {
    r.packages
        .iter()
        .flat_map(|p| {
            p.launcher
                .iter()
                .flat_map(|l| l.platforms.iter().map(|x| &x.pkg))
                .chain(std::iter::once(p))
        })
        .collect()
}

/// Read whether `p` exists on its registry; an unreadable registry is exit 3 (never write
/// on a guess).
fn exists(npm: &Http, crates: &Http, p: &Package) -> Result<bool, HoistError> {
    let (http, registry) = match p.kind {
        PkgKind::Npm => (npm, "npm"),
        PkgKind::Crate => (crates, "crates.io"),
    };
    match package_exists(http, p)? {
        Fetch::Got(b) => Ok(b),
        Fetch::Unknown(why) => Err(HoistError::Network(format!(
            "cannot read {} on {registry}: {why}; retry when the registry answers",
            p.name
        ))),
    }
}

fn tagged(dir: &Path, tag: &str) -> Result<bool, HoistError> {
    let out = run(Tool::Git, dir, &["tag", "--list", tag], &[])?;
    Ok(out.lines().any(|l| l.trim() == tag))
}

/// npm ≥ 11.15.0 (`npm trust`).
fn require_npm_version(dir: &Path) -> Result<(), HoistError> {
    let out = run(Tool::Npm, dir, &["--version"], &[])?;
    let text = out.trim();
    let version = Version::parse(text).map_err(|_| {
        HoistError::Usage(format!(
            "`npm --version` printed `{text}`, not a version; install npm {MIN_NPM} or newer"
        ))
    })?;
    if version < MIN_NPM {
        return Err(HoistError::Usage(format!(
            "npm {version} has no `npm trust`; bootstrap needs npm {MIN_NPM} or newer: run \
             `npm install -g npm@latest`"
        )));
    }
    Ok(())
}

fn require_npm_login(dir: &Path) -> Result<(), HoistError> {
    run(Tool::Npm, dir, &["whoami"], &[])
        .map(|_| ())
        .map_err(|e| {
            HoistError::Usage(format!(
                "{e}; bootstrap publishes with your own npm account: run `npm login`, then retry"
            ))
        })
}

fn require_cargo(dir: &Path) -> Result<(), HoistError> {
    run(Tool::Cargo, dir, &["--version"], &[])
        .map(|_| ())
        .map_err(HoistError::from)
}

/// `npm trust list <pkg> --json`; a failed call or an unparsable answer is "not trusted".
fn npm_trusted(dir: &Path, p: &Package, t: &Trust) -> bool {
    run(Tool::Npm, dir, &["trust", "list", &p.name, "--json"], &[])
        .is_ok_and(|out| trusted(&out, &t.repo, &t.workflow, &t.env))
}

/// The preflight reads (Global Constraints order after the Actions check and the model):
/// registries, tags, then the tools the missing or untrusted packages need.
fn read_facts(
    dir: &Path,
    model: &ReleaseModel,
    pkgs: &[&Package],
    t: &Trust,
    verbose: bool,
) -> Result<Vec<Facts>, HoistError> {
    let anonymous = || crate::auth::Secret::from("");
    let npm = Http::new(Service::Npm, anonymous()).verbose(verbose);
    let crates = Http::new(Service::Crates, anonymous()).verbose(verbose);
    let mut facts = Vec::new();
    for p in pkgs {
        let exists = exists(&npm, &crates, p)?;
        let (tag, tagged) = match p.kind {
            PkgKind::Crate if !exists => {
                let tag = model.tag_for(p, &p.version.value);
                let has = tagged(dir, &tag)?;
                (tag, has)
            }
            _ => (String::new(), false),
        };
        facts.push(Facts {
            pkg: (*p).clone(),
            exists,
            trusted: false,
            tag,
            tagged,
        });
    }
    if facts.iter().any(|f| f.pkg.kind == PkgKind::Npm) {
        require_npm_version(dir)?;
        for f in facts
            .iter_mut()
            .filter(|f| f.pkg.kind == PkgKind::Npm && f.exists)
        {
            f.trusted = npm_trusted(dir, &f.pkg, t);
        }
        if facts
            .iter()
            .any(|f| f.pkg.kind == PkgKind::Npm && !(f.exists && f.trusted))
        {
            require_npm_login(dir)?;
        }
    }
    if facts
        .iter()
        .any(|f| f.pkg.kind == PkgKind::Crate && !f.exists && f.tagged)
    {
        require_cargo(dir)?;
    }
    Ok(facts)
}

#[derive(Debug, PartialEq, Eq)]
enum Go {
    Run,
    Declined,
}

/// `--yes` runs; otherwise ask on a terminal (the list goes to `out`), refuse without one.
fn confirm(
    planned: &[Item],
    opts: BootstrapOpts,
    input: impl BufRead,
    out: impl Write,
) -> Result<Go, HoistError> {
    if opts.yes {
        return Ok(Go::Run);
    }
    if !opts.tty {
        return Err(HoistError::Usage(
            "stdin is not a terminal; use --yes in non-interactive runs".to_string(),
        ));
    }
    let publishes = planned
        .iter()
        .filter(|i| i.kind != "bootstrap_trust")
        .count();
    let lines: Vec<String> = planned
        .iter()
        .map(|i| {
            format!(
                "{} {}/{} {}: {}",
                i.action.as_str(),
                i.area,
                i.kind,
                i.key,
                i.reason
            )
        })
        .collect();
    let header = format!("hoist release bootstrap will publish {publishes} package(s):");
    Ok(
        if ask_lines(&header, &lines, "Run these steps?", input, out)? {
            Go::Run
        } else {
            Go::Declined
        },
    )
}

/// A declined prompt: nothing written, every planned item "not applied: declined", exit 1.
fn declined(items: Vec<Item>) -> Report {
    let items = items
        .into_iter()
        .map(|mut i| {
            if i.action == Action::Create {
                i.reason = "not applied: declined".to_string();
            }
            i
        })
        .collect();
    report(items, 1)
}

/// The CLI entry: [`release_bootstrap_with`] reading the answer from stdin.
pub fn release_bootstrap(
    dir: &Path,
    opts: BootstrapOpts,
    verbose: bool,
) -> Result<Report, HoistError> {
    release_bootstrap_with(dir, opts, verbose, std::io::stdin().lock())
}

/// Preflight, then the dry run or the confirmed run; the prompt's answer comes from `input`.
pub fn release_bootstrap_with(
    dir: &Path,
    opts: BootstrapOpts,
    verbose: bool,
    input: impl BufRead,
) -> Result<Report, HoistError> {
    refuse_in_actions()?;
    let (repo, _) = read_repo_local(dir)?;
    let model = read_release_model(dir)?;
    let Some(release) = read_desired_release(dir)? else {
        let mut r = report(vec![], 0);
        r.text = Some("nothing to bootstrap: no package publishes to a registry\n".to_string());
        return Ok(r);
    };
    let t = Trust {
        repo,
        workflow: release
            .workflow
            .as_ref()
            .map_or(DEFAULT_WORKFLOW, |w| w.file.as_str())
            .to_string(),
        env: release.environment.value.clone(),
    };
    let facts = read_facts(dir, &model, &registry_packages(&release), &t, verbose)?;
    let (steps, other) = plan_steps(&facts);
    let items = all_items(&steps, other, &t);
    if opts.dry_run {
        // A preview exits 0, like `release plan` (R17).
        return Ok(report(items, 0));
    }
    if steps.is_empty() {
        let code = crate::ops::check::exit_code(&items);
        return Ok(report(items, code));
    }
    if confirm(&items, opts, input, std::io::stderr().lock())? == Go::Declined {
        return Ok(declined(items));
    }
    // Task 2 executes the steps; until then a confirmed run reports its plan.
    let code = crate::ops::check::exit_code(&items);
    Ok(report(items, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PublishCfg;
    use crate::repo::Sourced;
    use std::path::PathBuf;

    fn pkg(name: &str, kind: PkgKind) -> Package {
        Package {
            name: name.to_string(),
            kind,
            dir: PathBuf::new(),
            version: Sourced {
                value: "0.3.0".to_string(),
                source: "x:1".to_string(),
            },
            deps: vec![],
            publish: PublishCfg::default(),
            launcher: None,
        }
    }

    fn facts(p: Package, exists: bool, trusted: bool, tagged: bool) -> Facts {
        Facts {
            tag: format!("{}@0.3.0", p.name),
            pkg: p,
            exists,
            trusted,
            tagged,
        }
    }

    fn opts(yes: bool, tty: bool) -> BootstrapOpts {
        BootstrapOpts {
            dry_run: false,
            yes,
            tty,
        }
    }

    fn planned() -> Vec<Item> {
        vec![item(
            "bootstrap_npm",
            &pkg("a", PkgKind::Npm),
            Action::Create,
            "would publish".to_string(),
        )]
    }

    #[test]
    fn plan_orders_publishes_before_trust() {
        let (steps, items) = plan_steps(&[
            facts(pkg("tool", PkgKind::Crate), false, false, true),
            facts(pkg("a", PkgKind::Npm), false, false, false),
            facts(pkg("b", PkgKind::Npm), true, true, false),
        ]);
        let kinds: Vec<String> = steps
            .iter()
            .map(|s| match s {
                Step::NpmPlaceholder(p) => format!("npm {}", p.name),
                Step::Crate { pkg, tag } => format!("crate {} {tag}", pkg.name),
                Step::NpmTrust(p) => format!("npm-trust {}", p.name),
                Step::CrateTrust(p) => format!("crate-trust {}", p.name),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "npm a",
                "crate tool tool@0.3.0",
                "npm-trust a",
                "crate-trust tool"
            ]
        );
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.action == Action::Ok && i.key == "b"));
    }

    #[test]
    fn untagged_crate_has_no_steps() {
        let (steps, items) = plan_steps(&[facts(pkg("tool", PkgKind::Crate), false, false, false)]);
        assert!(steps.is_empty());
        assert!(items.iter().all(|i| i.action == Action::Pending));
        assert!(items[0].reason.contains(NO_TAG), "{}", items[0].reason);
    }

    #[test]
    fn confirm_with_yes_runs_without_asking() {
        let mut out = Vec::new();
        let go = confirm(&planned(), opts(true, false), &b""[..], &mut out).unwrap();
        assert_eq!(go, Go::Run);
        assert!(out.is_empty());
    }

    #[test]
    fn confirm_without_tty_or_yes_refuses() {
        let err = confirm(&planned(), opts(false, false), &b"y\n"[..], Vec::new()).unwrap_err();
        assert!(
            err.to_string()
                .contains("use --yes in non-interactive runs"),
            "{err}"
        );
    }

    #[test]
    fn confirm_on_tty_asks_and_reads_the_answer() {
        let mut out = Vec::new();
        let go = confirm(&planned(), opts(false, true), &b"y\n"[..], &mut out).unwrap();
        assert_eq!(go, Go::Run);
        let shown = String::from_utf8(out).unwrap();
        assert!(
            shown.starts_with("hoist release bootstrap will publish 1 package(s):"),
            "{shown}"
        );
        assert!(shown.contains("[y/N]"), "{shown}");
        let go = confirm(&planned(), opts(false, true), &b"n\n"[..], Vec::new()).unwrap();
        assert_eq!(go, Go::Declined);
    }

    #[test]
    fn declined_report_exits_1() {
        let r = declined(planned());
        assert_eq!(r.exit_code, 1);
        assert_eq!(r.items[0].reason, "not applied: declined");
    }
}
