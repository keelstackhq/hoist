//! The `release`, `npm` and `crates` areas of `plan`/`check` (spec §6, §9.4): the release
//! environment, the workflow job that runs in it, trusted publishers and first publishes.

use crate::config::Managed;
use crate::desired::{Desired, DesiredRelease};
use crate::error::HoistError;
use crate::github::{Fetch, LiveGithub};
use crate::http::Http;
use crate::release::registry::package_exists;
use crate::release::{Package, PkgKind};
use crate::report::{Action, HumanStep, Item};

/// R6: a trusted publisher can neither be read nor written with a CI token.
pub const PUBLISHER_UNREADABLE: &str =
    "registered by a person; hoist cannot read it with a CI token";
/// The workflow file named in trust commands when no job uses the release environment yet.
const DEFAULT_WORKFLOW: &str = "release.yml";

/// Whether each publishing package exists on its registry, in `DesiredRelease::packages` order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveRegistries {
    pub exists: Vec<(String, PkgKind, Fetch<bool>)>,
}

/// Every package that publishes to a registry: the release packages, each launcher followed by
/// its platform packages (R22: they are no release packages, but each needs its own trusted
/// publisher and manual first publish).
fn registry_packages(r: &DesiredRelease) -> impl Iterator<Item = &Package> {
    r.packages.iter().flat_map(|p| {
        let platforms = p
            .launcher
            .iter()
            .flat_map(|l| l.platforms.iter().map(|x| &x.pkg));
        std::iter::once(p).chain(platforms)
    })
}

/// Read the packages of the registries that have a client (anonymous GETs only).
pub fn read_registries(
    d: &Desired,
    npm: Option<&Http>,
    crates: Option<&Http>,
) -> Result<LiveRegistries, HoistError> {
    let mut out = LiveRegistries::default();
    let Some(r) = &d.release else {
        return Ok(out);
    };
    for p in registry_packages(r) {
        let http = match p.kind {
            PkgKind::Npm => npm,
            PkgKind::Crate => crates,
        };
        if let Some(http) = http {
            out.exists
                .push((p.name.clone(), p.kind, package_exists(http, p)?));
        }
    }
    Ok(out)
}

fn item(area: &str, kind: &str, key: &str, action: Action, reason: String, source: &str) -> Item {
    Item {
        area: area.to_string(),
        kind: kind.to_string(),
        key: key.to_string(),
        action,
        reason,
        source: source.to_string(),
        verify: None,
    }
}

fn step(id: String, title: String, url: String) -> HumanStep {
    HumanStep {
        id,
        title,
        url,
        status: "pending".to_string(),
    }
}

/// The `first-publish:<name>` human step: trusted publishing needs an existing package.
pub fn first_publish_step(name: &str, kind: PkgKind) -> HumanStep {
    step(
        format!("first-publish:{name}"),
        "publish the first version by hand; trusted publishing needs an existing package"
            .to_string(),
        match kind {
            // The package page 404s until the first publish.
            PkgKind::Npm => "https://docs.npmjs.com/cli/commands/npm-publish".to_string(),
            PkgKind::Crate => {
                "https://doc.rust-lang.org/cargo/commands/cargo-publish.html".to_string()
            }
        },
    )
}

fn workflow_file(r: &DesiredRelease) -> &str {
    r.workflow
        .as_ref()
        .map_or(DEFAULT_WORKFLOW, |w| w.file.as_str())
}

/// The release environment against the live list (`None` without publishing packages).
/// Externally managed GitHub resources are never created: that is drift.
pub fn release_env_item(d: &Desired, l: &LiveGithub) -> Option<Item> {
    let r = d.release.as_ref()?;
    let env = &r.environment;
    let (action, reason) = match l.environments.as_got() {
        Err(why) => (Action::Unknown, why.to_string()),
        Ok(have) if have.contains(&env.value) => (Action::Ok, "present".to_string()),
        Ok(_) if d.github.managed == Managed::External => (
            Action::Drift,
            "missing; would create the environment".to_string(),
        ),
        Ok(_) => (
            Action::Create,
            "missing; would create the environment".to_string(),
        ),
    };
    Some(item(
        "release",
        "environment",
        &env.value,
        action,
        reason,
        &env.source,
    ))
}

/// The `release` area: the release environment (needs the GitHub read) and the workflow job.
pub fn diff_release(d: &Desired, l: &LiveGithub) -> Vec<Item> {
    let Some(r) = &d.release else {
        return Vec::new();
    };
    let env = &r.environment.value;
    let mut items: Vec<Item> = release_env_item(d, l).into_iter().collect();
    items.push(match &r.workflow {
        Some(w) => item(
            "release",
            "workflow",
            env,
            Action::Ok,
            format!(
                "job `{}` in `.github/workflows/{}` uses environment `{env}`",
                w.job, w.file
            ),
            &format!(".github/workflows/{}", w.file),
        ),
        None => item(
            "release",
            "workflow",
            env,
            Action::Pending,
            format!("no workflow job uses environment `{env}`"),
            &r.environment.source,
        ),
    });
    items
}

/// The `npm` or `crates` area: per publishing package a trusted publisher (unreadable, R6)
/// with its human step, and a first publish when the registry does not have the package.
pub fn diff_registry(
    d: &Desired,
    kind: PkgKind,
    live: &LiveRegistries,
) -> (Vec<Item>, Vec<HumanStep>) {
    let (mut items, mut steps) = (Vec::new(), Vec::new());
    let Some(r) = &d.release else {
        return (items, steps);
    };
    let area = match kind {
        PkgKind::Npm => "npm",
        PkgKind::Crate => "crates",
    };
    let (repo, env, file) = (
        format!("{}/{}", d.repo.owner, d.repo.name),
        &r.environment.value,
        workflow_file(r),
    );
    // Every npm package publishes to npm and every crate to crates.io (spec §5 defaults).
    for p in registry_packages(r).filter(|p| p.kind == kind) {
        let name = &p.name;
        let source = &p.version.source;
        items.push(item(
            area,
            "trusted_publisher",
            name,
            Action::Unknown,
            PUBLISHER_UNREADABLE.to_string(),
            source,
        ));
        steps.push(match kind {
            PkgKind::Npm => step(
                format!("npm-trust:{name}"),
                format!(
                    "npm trust github {name} --file {file} --repo {repo} --env {env} \
                     --allow-publish"
                ),
                format!("https://www.npmjs.com/package/{name}/access"),
            ),
            PkgKind::Crate => step(
                format!("crates-trust:{name}"),
                format!(
                    "add a trusted publisher to crate {name}: repository {repo}, workflow \
                     {file}, environment {env}"
                ),
                format!("https://crates.io/crates/{name}/settings"),
            ),
        });
        let exists = live
            .exists
            .iter()
            .find(|(n, k, _)| n == name && *k == kind)
            .map(|(_, _, f)| f);
        match exists {
            Some(Fetch::Got(true)) => items.push(item(
                area,
                "first_publish",
                name,
                Action::Ok,
                format!("{name} exists on the registry"),
                source,
            )),
            Some(Fetch::Got(false)) => {
                items.push(item(
                    area,
                    "first_publish",
                    name,
                    Action::Pending,
                    format!("{name} is not on the registry yet"),
                    source,
                ));
                steps.push(first_publish_step(name, kind));
            }
            Some(Fetch::Unknown(why)) => items.push(item(
                area,
                "first_publish",
                name,
                Action::Unknown,
                format!("cannot read the registry: {why}"),
                source,
            )),
            None => {}
        }
    }
    (items, steps)
}
