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
use crate::report::{Action, HumanStep, Item, Report};
use std::io::{BufRead, Write};
use std::path::Path;

mod publish;
mod trust;
use trust::{Registered, npm_trusted, trust_command};

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
    /// `--json`: the final hint goes into `Report.text` (never printed) instead of stderr.
    pub json: bool,
}

/// One write of a bootstrap run, in execution order (see [`plan_steps`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// Publish a `0.0.0` placeholder from a temp dir.
    NpmPlaceholder(Package),
    /// Publish the crate's manifest version from a worktree at `tag`.
    Crate { pkg: Package, tag: String },
    /// Register the npm trusted publisher (only on a terminal: npm prompts for 2FA).
    NpmTrust(Package),
}

/// What a run would do: the writes, the items that need no write, and the human steps
/// (R4: crate trust and, without a terminal, npm trust).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub steps: Vec<Step>,
    pub items: Vec<Item>,
    pub human_steps: Vec<HumanStep>,
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
    pub(crate) fn describe(&self) -> String {
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

fn human_step(id: String, title: String, url: String) -> HumanStep {
    HumanStep {
        id,
        title,
        url,
        status: "pending".to_string(),
    }
}

/// The writes in order (npm placeholders, crates, then npm trust when `tty`), plus the items
/// and human steps of what needs no write. R4: crate trust is unreadable, so it is `unknown`
/// with a `crates-trust:` step; without a terminal npm trust is `pending` with an
/// `npm-trust:` step whose title is the command.
pub(crate) fn plan_steps(facts: &[Facts], t: &Trust, tty: bool) -> Plan {
    let (mut publish, mut trust, mut items, mut human) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
        } else if tty {
            trust.push(Step::NpmTrust(p.clone()));
        } else {
            let id = format!("npm-trust:{}", p.name);
            items.push(item(
                "bootstrap_trust",
                p,
                Action::Pending,
                format!("no terminal for npm's 2FA prompt: run the command of human step {id}"),
            ));
            human.push(human_step(
                id,
                trust_command(&p.name, t),
                format!("https://www.npmjs.com/package/{}/access", p.name),
            ));
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
        let id = format!("crates-trust:{}", p.name);
        items.push(item(
            "bootstrap_trust",
            p,
            Action::Unknown,
            format!("crates.io trusted publishers cannot be read; check human step {id}"),
        ));
        human.push(human_step(
            id,
            format!(
                "add a trusted publisher to crate {}: repository {}, workflow {}, environment {}",
                p.name, t.repo, t.workflow, t.env
            ),
            format!("https://crates.io/crates/{}/settings", p.name),
        ));
    }
    publish.extend(trust);
    Plan {
        steps: publish,
        items,
        human_steps: human,
    }
}

/// The planned item of a step (`create`: it will write).
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

fn report(items: Vec<Item>, human_steps: Vec<HumanStep>, exit_code: i32) -> Report {
    Report {
        command: COMMAND.to_string(),
        items,
        human_steps,
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

/// Anonymous registry clients (reads only).
struct Registries {
    npm: Http,
    crates: Http,
}

fn registries(verbose: bool) -> Registries {
    let anonymous = || crate::auth::Secret::from("");
    Registries {
        npm: Http::new(Service::Npm, anonymous()).verbose(verbose),
        crates: Http::new(Service::Crates, anonymous()).verbose(verbose),
    }
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

/// The preflight reads (Global Constraints order after the Actions check and the model):
/// registries, tags, then the tools the missing or untrusted packages need.
fn read_facts(
    dir: &Path,
    model: &ReleaseModel,
    pkgs: &[&Package],
    t: &Trust,
    regs: &Registries,
) -> Result<Vec<Facts>, HoistError> {
    let mut facts = Vec::new();
    for p in pkgs {
        let exists = exists(&regs.npm, &regs.crates, p)?;
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
            f.trusted = npm_trusted(dir, &f.pkg.name, t);
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
/// Only the planned writes (`create` items) are listed and counted.
fn confirm(
    items: &[Item],
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
    let planned: Vec<&Item> = items
        .iter()
        .filter(|i| i.action == Action::Create)
        .collect();
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
fn declined(items: Vec<Item>, human_steps: Vec<HumanStep>) -> Report {
    let items = items
        .into_iter()
        .map(|mut i| {
            if i.action == Action::Create {
                i.reason = "not applied: declined".to_string();
            }
            i
        })
        .collect();
    report(items, human_steps, 1)
}

/// The CLI entry: [`release_bootstrap_with`] reading the answer from stdin.
pub fn release_bootstrap(
    dir: &Path,
    opts: BootstrapOpts,
    verbose: bool,
) -> Result<Report, HoistError> {
    release_bootstrap_with(dir, opts, verbose, prompt_input())
}

/// Where the confirmation answer is read from. std's stdin is buffered: reading the answer
/// through it can swallow the lines after it, which `npm publish` (stdin inherited, R5) needs
/// for the 2FA code. On unix the answer is read one byte at a time from a duplicate of fd 0,
/// which shares its offset, so the rest stays for the child.
fn prompt_input() -> Box<dyn BufRead> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        if let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() {
            return Box::new(std::io::BufReader::with_capacity(
                1,
                std::fs::File::from(fd),
            ));
        }
    }
    Box::new(std::io::stdin().lock())
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
        let mut r = report(vec![], vec![], 0);
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
    let regs = registries(verbose);
    let facts = read_facts(dir, &model, &registry_packages(&release), &t, &regs)?;
    let plan = plan_steps(&facts, &t, opts.tty);
    let other = plan.items.clone();
    let items = all_items(&plan.steps, plan.items, &t);
    let human = plan.human_steps;
    if opts.dry_run {
        // A preview exits 0, like `release plan` (R17).
        return Ok(report(items, human, 0));
    }
    let tags = release_tags(&model, &release);
    let finish = |items: Vec<Item>, code: i32| {
        let mut r = report(items, human, code);
        let hint = hint(&r.items, &tags);
        if opts.json {
            r.text = Some(hint);
        } else {
            eprint!("{hint}");
        }
        r
    };
    if plan.steps.is_empty() {
        // R4: no write left; human steps alone never need a confirmation. `pending` exits 2,
        // `unknown` (crate trust) alone exits 0.
        let code = crate::ops::check::exit_code(&items);
        return Ok(finish(items, code));
    }
    if confirm(&items, opts, input, std::io::stderr().lock())? == Go::Declined {
        let r = declined(items, vec![]);
        return Ok(finish(r.items, r.exit_code));
    }
    let items = execute(dir, &t, &regs, &plan.steps, other);
    // A failed or not attempted step keeps its `create` action: exit 1.
    let code = crate::ops::check::exit_code(&items);
    Ok(finish(items, code))
}

/// The release tags of the current versions (a launcher's platform packages share its tag).
fn release_tags(model: &ReleaseModel, release: &DesiredRelease) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for p in &release.packages {
        let tag = model.tag_for(p, &p.version.value);
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// The final line: re-run the publish job when every publish item is `ok` (done now or
/// before), else finish the remaining steps first.
fn hint(items: &[Item], tags: &[String]) -> String {
    let published = items
        .iter()
        .filter(|i| i.kind != "bootstrap_trust")
        .all(|i| i.action == Action::Ok);
    if published {
        format!(
            "next: re-run the failed \"release publish\" job at {}, or run the release workflow\n",
            tags.join(", ")
        )
    } else {
        "next: finish the steps above, then run bootstrap again\n".to_string()
    }
}

/// How one step of a confirmed run ended.
enum Outcome {
    /// Written now.
    Done,
    /// It appeared on the registry since the preflight: nothing written.
    AlreadyThere,
    Failed(HoistError),
    NotAttempted,
    /// npm trust: `npm trust list` already showed the entry.
    AlreadyTrusted,
}

/// Run the steps in order (placeholders, crates, npm trust); the first failure stops the run.
/// Each write re-reads the registry (or `npm trust list`) first, so a race or a re-run skips
/// it. Returns the step items (done `ok`, `failed: <error>`, `not attempted`) and the `other`
/// items, grouped by kind.
fn execute(
    dir: &Path,
    t: &Trust,
    regs: &Registries,
    steps: &[Step],
    other: Vec<Item>,
) -> Vec<Item> {
    let mut stopped = false;
    let mut items = Vec::new();
    for step in steps {
        let outcome = match step {
            _ if stopped => Outcome::NotAttempted,
            Step::NpmPlaceholder(p) | Step::Crate { pkg: p, .. } | Step::NpmTrust(p) => {
                match run_step(dir, t, regs, step, p) {
                    Ok(o) => o,
                    Err(e) => {
                        stopped = true;
                        Outcome::Failed(e)
                    }
                }
            }
        };
        let mut i = step_item(step, t);
        match outcome {
            Outcome::Done => {
                i.action = Action::Ok;
                i.reason = match step {
                    Step::Crate { pkg, tag } => {
                        format!("published {} from tag {tag}", pkg.version.value)
                    }
                    Step::NpmTrust(_) => {
                        format!("registered the trusted publisher: {}", t.describe())
                    }
                    Step::NpmPlaceholder(_) => {
                        "published placeholder 0.0.0 from a temp dir".to_string()
                    }
                };
            }
            Outcome::AlreadyThere => {
                i.action = Action::Ok;
                i.reason = "already on the registry; not touched".to_string();
            }
            Outcome::Failed(e) => i.reason = format!("failed: {e}"),
            Outcome::NotAttempted => i.reason = "not attempted".to_string(),
            Outcome::AlreadyTrusted => {
                i.action = Action::Ok;
                i.reason = "already trusted".to_string();
            }
        }
        items.push(i);
    }
    items.extend(other);
    items.sort_by_key(|i| kind_rank(&i.kind));
    items
}

fn run_step(
    dir: &Path,
    t: &Trust,
    regs: &Registries,
    step: &Step,
    p: &Package,
) -> Result<Outcome, HoistError> {
    if let Step::NpmTrust(p) = step {
        return Ok(match trust::register(dir, &p.name, t)? {
            Registered::Done => Outcome::Done,
            Registered::AlreadyTrusted => Outcome::AlreadyTrusted,
        });
    }
    if exists(&regs.npm, &regs.crates, p)? {
        return Ok(Outcome::AlreadyThere);
    }
    match step {
        Step::Crate { pkg, tag } => publish::publish_crate(dir, pkg, tag)?,
        _ => publish::publish_placeholder(p, &t.repo, dir)?,
    }
    Ok(Outcome::Done)
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
            json: true,
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

    fn trust() -> Trust {
        Trust {
            repo: GithubRepo {
                owner: "acme".to_string(),
                name: "x".to_string(),
            },
            workflow: "release.yml".to_string(),
            env: "release".to_string(),
        }
    }

    fn names(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .map(|s| match s {
                Step::NpmPlaceholder(p) => format!("npm {}", p.name),
                Step::Crate { pkg, tag } => format!("crate {} {tag}", pkg.name),
                Step::NpmTrust(p) => format!("npm-trust {}", p.name),
            })
            .collect()
    }

    fn all_facts() -> Vec<Facts> {
        vec![
            facts(pkg("tool", PkgKind::Crate), false, false, true),
            facts(pkg("a", PkgKind::Npm), false, false, false),
            facts(pkg("b", PkgKind::Npm), true, true, false),
        ]
    }

    #[test]
    fn plan_orders_publishes_before_trust() {
        let plan = plan_steps(&all_facts(), &trust(), true);
        assert_eq!(
            names(&plan.steps),
            ["npm a", "crate tool tool@0.3.0", "npm-trust a"]
        );
        // R4: crate trust is never a step: `unknown` with a human step.
        let t = plan
            .items
            .iter()
            .find(|i| i.key == "tool" && i.kind == "bootstrap_trust")
            .unwrap();
        assert_eq!(t.action, Action::Unknown);
        let ids: Vec<&str> = plan.human_steps.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["crates-trust:tool"]);
        assert_eq!(
            plan.human_steps[0].url,
            "https://crates.io/crates/tool/settings"
        );
        let b: Vec<&Item> = plan.items.iter().filter(|i| i.key == "b").collect();
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|i| i.action == Action::Ok));
    }

    #[test]
    fn npm_trust_without_tty_is_a_human_step() {
        let plan = plan_steps(&all_facts(), &trust(), false);
        assert_eq!(names(&plan.steps), ["npm a", "crate tool tool@0.3.0"]);
        let t = plan
            .items
            .iter()
            .find(|i| i.key == "a" && i.kind == "bootstrap_trust")
            .unwrap();
        assert_eq!(t.action, Action::Pending);
        let step = plan
            .human_steps
            .iter()
            .find(|h| h.id == "npm-trust:a")
            .unwrap();
        assert_eq!(
            step.title,
            "npm trust github a --file release.yml --repo acme/x --env release --allow-publish"
        );
    }

    #[test]
    fn untagged_crate_has_no_steps() {
        let plan = plan_steps(
            &[facts(pkg("tool", PkgKind::Crate), false, false, false)],
            &trust(),
            true,
        );
        assert!(plan.steps.is_empty());
        assert!(plan.human_steps.is_empty());
        assert!(plan.items.iter().all(|i| i.action == Action::Pending));
        assert!(
            plan.items[0].reason.contains(NO_TAG),
            "{}",
            plan.items[0].reason
        );
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
    fn confirm_lists_and_counts_only_planned_writes() {
        let mut mixed = planned();
        mixed.push(item(
            "bootstrap_crate",
            &pkg("tool", PkgKind::Crate),
            Action::Pending,
            "no tag".to_string(),
        ));
        mixed.push(item(
            "bootstrap_npm",
            &pkg("b", PkgKind::Npm),
            Action::Ok,
            "exists".to_string(),
        ));
        mixed.push(item(
            "bootstrap_trust",
            &pkg("a", PkgKind::Npm),
            Action::Create,
            "would register".to_string(),
        ));
        let mut out = Vec::new();
        confirm(&mixed, opts(false, true), &b"n\n"[..], &mut out).unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(
            shown.starts_with("hoist release bootstrap will publish 1 package(s):"),
            "{shown}"
        );
        let listed: Vec<&str> = shown.lines().filter(|l| l.starts_with("  ")).collect();
        assert_eq!(listed.len(), 2, "{shown}");
        assert!(
            listed.iter().all(|l| l.trim_start().starts_with("create ")),
            "{shown}"
        );
    }

    #[test]
    fn hint_depends_on_the_publish_items() {
        let tags = ["v0.1.0".to_string()];
        let mut items = vec![
            item(
                "bootstrap_crate",
                &pkg("tool", PkgKind::Crate),
                Action::Ok,
                "published".to_string(),
            ),
            item(
                "bootstrap_trust",
                &pkg("tool", PkgKind::Crate),
                Action::Unknown,
                "check".to_string(),
            ),
        ];
        assert_eq!(
            hint(&items, &tags),
            "next: re-run the failed \"release publish\" job at v0.1.0, or run the release workflow\n"
        );
        items[0].action = Action::Pending;
        assert_eq!(
            hint(&items, &tags),
            "next: finish the steps above, then run bootstrap again\n"
        );
    }

    #[test]
    fn declined_report_exits_1() {
        let r = declined(planned(), vec![]);
        assert_eq!(r.exit_code, 1);
        assert_eq!(r.items[0].reason, "not applied: declined");
    }
}
