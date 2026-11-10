//! `hoist changeset status`: every package the branch changes is named in a changeset the
//! branch adds (changesets spec §6). Local git only, no HTTP.

use super::report;
use crate::changeset::{Changeset, glob_match, is_skipped, plan, read_config, read_dir};
use crate::config::read_pins;
use crate::error::HoistError;
use crate::release::planner::History;
use crate::release::read_release_model;
use crate::release::tools::{Tool, run};
use crate::repo::git::current_branch;
use crate::report::Report;
use crate::{Action, Item};
use std::collections::BTreeMap;
use std::path::Path;

const RELEASE_BRANCH: &str = "hoist/release";

fn usage(m: String) -> HoistError {
    HoistError::Usage(m)
}

fn item(kind: &str, key: &str, action: Action, reason: String, source: &str) -> Item {
    Item {
        area: "changeset".into(),
        kind: kind.into(),
        key: key.into(),
        action,
        reason,
        source: source.into(),
        verify: None,
    }
}

/// A directory whose changed files belong to package `owner` (a crate for a launcher or
/// platform dir). Patterns match the path relative to `base`.
struct Owned {
    dir: String,
    owner: String,
    base: String,
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn inside(file: &str, dir: &str) -> bool {
    dir.is_empty()
        || file
            .strip_prefix(dir)
            .is_some_and(|r| r.is_empty() || r.starts_with('/'))
}

fn relative<'a>(file: &'a str, base: &str) -> &'a str {
    if base.is_empty() {
        file
    } else {
        file[base.len()..].trim_start_matches('/')
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, HoistError> {
    run(Tool::Git, dir, args, &[]).map_err(HoistError::from)
}

/// `git diff -z --name-only` output as paths.
fn names(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn status(dir: &Path, since: Option<&str>) -> Result<Report, HoistError> {
    let model = read_release_model(dir)?;
    let cfg = read_config(dir)?.ok_or_else(|| {
        usage(
            "no .changeset/config.json, so this repo does not use changesets; run \
             `hoist changeset init` first"
                .into(),
        )
    })?;
    // Validate every changeset and config.json as `release plan` would (exit 3), so a bad file
    // fails the PR, not the release. History plays no part in the checks.
    let sets = read_dir(dir)?;
    let pins = read_pins(dir)?;
    plan::plan_versions(&model, &History::default(), &sets, &cfg, &pins)?;

    let branch = match std::env::var("GITHUB_HEAD_REF") {
        Ok(b) if !b.is_empty() => Some(b),
        _ => current_branch(dir)?,
    };
    if branch.as_deref() == Some(RELEASE_BRANCH) {
        let i = item("status", "branch", Action::Ok, "release PR".into(), "");
        return Ok(report("changeset status", vec![i]));
    }

    let default_base = format!("origin/{}", cfg.base_branch);
    let base = since.unwrap_or(&default_base);
    let verify = format!("{base}^{{commit}}");
    if git(dir, &["rev-parse", "-q", "--verify", &verify]).is_err() {
        let fetch = match since {
            Some(_) => "pass a commit that exists here".to_string(),
            None => format!(
                "fetch it (`git fetch origin {}`), set `baseBranch` in .changeset/config.json",
                cfg.base_branch
            ),
        };
        return Err(usage(format!(
            "`{base}` is not a commit in this checkout; {fetch}, or pass `--since <rev>`"
        )));
    }
    let mb = git(dir, &["merge-base", base, "HEAD"])?;
    let range = format!("{}..HEAD", mb.trim());
    let diff = ["diff", "-z", "--name-only", "--no-renames", "--relative"];
    let changed = names(&git(dir, &[&diff[..], &[range.as_str()]].concat())?);
    let added = names(&git(
        dir,
        &[
            &diff[..],
            &["--diff-filter=A", range.as_str(), "--", ".changeset"],
        ]
        .concat(),
    )?);
    let added: Vec<&Changeset> = added
        .iter()
        .filter_map(|f| f.strip_prefix(".changeset/"))
        .filter(|n| !n.contains('/') && !is_skipped(n))
        .filter_map(|n| n.strip_suffix(".md"))
        .filter_map(|id| sets.iter().find(|c| c.id == id))
        .collect();

    let mut owned = Vec::new();
    for p in &model.packages {
        let d = slash(&p.dir);
        match &p.launcher {
            None => owned.push(Owned {
                dir: d.clone(),
                owner: p.name.clone(),
                base: d,
            }),
            Some(l) => {
                let krate = model.packages.iter().find(|k| k.name == l.crate_name);
                let kdir = krate.map(|k| slash(&k.dir)).unwrap_or_default();
                let dirs =
                    std::iter::once(d).chain(l.platforms.iter().map(|pl| slash(&pl.pkg.dir)));
                for d in dirs {
                    let base = if inside(&d, &kdir) {
                        kdir.clone()
                    } else {
                        String::new()
                    };
                    owned.push(Owned {
                        dir: d,
                        owner: l.crate_name.clone(),
                        base,
                    });
                }
            }
        }
    }
    // Package -> its first changed file.
    let mut touched: BTreeMap<&str, &str> = BTreeMap::new();
    for f in changed.iter().filter(|f| !inside(f, ".changeset")) {
        let Some(o) = owned
            .iter()
            .filter(|o| inside(f, &o.dir))
            .max_by_key(|o| o.dir.len())
        else {
            continue;
        };
        let rel = relative(f, &o.base);
        if cfg.changed_file_patterns.iter().any(|p| glob_match(p, rel)) {
            touched.entry(o.owner.as_str()).or_insert(f.as_str());
        }
    }

    let empty = added.iter().find(|c| c.releases.is_empty());
    let mut items = Vec::new();
    for (pkg, file) in &touched {
        let named: Vec<String> = added
            .iter()
            .filter_map(|c| {
                let (_, t) = c.releases.iter().find(|(n, _)| n == pkg)?;
                Some(format!("{}: {}", c.id, t.as_str()))
            })
            .collect();
        let first_id = |id: &str| format!(".changeset/{id}.md");
        let i = if let Some(first) = named.first() {
            let id = first.split(": ").next().unwrap_or_default();
            item("package", pkg, Action::Ok, named.join(", "), &first_id(id))
        } else if let Some(c) = empty {
            let why = format!("{}: empty changeset", c.id);
            item("package", pkg, Action::Ok, why, &first_id(&c.id))
        } else {
            let why = format!(
                "changed without a changeset ({file}); run `hoist changeset add {pkg}:patch -m \
                 \"what changed\"` or `hoist changeset add --empty`, then commit it"
            );
            item("package", pkg, Action::Drift, why, file)
        };
        items.push(i);
    }
    if items.is_empty() {
        items.push(item(
            "status",
            "branch",
            Action::Ok,
            "no package changed".into(),
            "",
        ));
    }
    let mut r = report("changeset status", items);
    r.exit_code = crate::ops::check::exit_code(&r.items);
    Ok(r)
}
