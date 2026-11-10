//! `hoist changeset add`: write `.changeset/<id>.md` (spec changesets §7).

use super::{created, report};
use crate::changeset::{Changeset, CsType, render, words};
use crate::error::HoistError;
use crate::release::read_release_model;
use crate::repo::git::current_branch;
use crate::report::Report;
use std::io::{Read, Write};
use std::path::Path;

fn usage(m: impl Into<String>) -> HoistError {
    HoistError::Usage(m.into())
}

/// The id for a new changeset: the branch with `/` as `-`, else three words.
pub fn default_id(dir: &Path) -> Result<String, HoistError> {
    let branch = current_branch(dir)?
        .map(|b| b.replace('/', "-"))
        .filter(|b| valid_id(b));
    Ok(branch.unwrap_or_else(|| words::human_id(words::seed())))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `name:type` -> (name, type); the type follows the last `:`.
fn parse_arg(arg: &str) -> Result<(String, CsType), HoistError> {
    let (name, ty) = arg
        .rsplit_once(':')
        .filter(|(n, _)| !n.is_empty())
        .ok_or_else(|| {
            usage(format!(
                "`{arg}` is not <name>:<type>; write e.g. `my-crate:minor`"
            ))
        })?;
    let ty = CsType::parse(ty).ok_or_else(|| {
        usage(format!(
            "type `{ty}` in `{arg}` is not valid; use major, minor, patch or none"
        ))
    })?;
    Ok((name.to_string(), ty))
}

/// Exit 3 unless `name` is a package of the release model that a changeset may name.
fn check_name(dir: &Path, names: &[String]) -> Result<(), HoistError> {
    let model = read_release_model(dir)?;
    for name in names {
        for p in &model.packages {
            if let Some(l) = &p.launcher {
                let platform = l.platforms.iter().any(|pl| pl.pkg.name == *name);
                if p.name == *name || platform {
                    return Err(usage(format!(
                        "`{name}` follows `{krate}`; name `{krate}` instead",
                        krate = l.crate_name
                    )));
                }
            }
        }
        if !model.packages.iter().any(|p| p.name == *name) {
            let mut known: Vec<&str> = model
                .packages
                .iter()
                .filter(|p| p.launcher.is_none())
                .map(|p| p.name.as_str())
                .collect();
            known.sort_unstable();
            return Err(usage(format!(
                "`{name}` is not a releasable package; name one of: {}",
                known.join(", ")
            )));
        }
    }
    Ok(())
}

fn summary(message: Option<&str>) -> Result<String, HoistError> {
    let text = match message {
        Some("-") => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| usage(format!("cannot read the summary from stdin: {e}")))?;
            s
        }
        Some(m) => m.to_string(),
        None => String::new(),
    };
    Ok(text.trim().to_string())
}

pub fn add(
    dir: &Path,
    packages: &[String],
    message: Option<&str>,
    empty: bool,
    id: Option<&str>,
) -> Result<Report, HoistError> {
    if empty && !packages.is_empty() {
        return Err(usage(
            "`--empty` takes no <name>:<type>; drop one of them".to_string(),
        ));
    }
    if !empty && packages.is_empty() {
        return Err(usage(
            "name at least one package, e.g. `hoist changeset add my-crate:minor -m \"what changed\"`, \
             or pass `--empty`",
        ));
    }
    let mut releases: Vec<(String, CsType)> = Vec::new();
    for arg in packages {
        let (name, ty) = parse_arg(arg)?;
        if releases.iter().any(|(n, _)| *n == name) {
            return Err(usage(format!("`{name}` is named twice; name it once")));
        }
        releases.push((name, ty));
    }
    let summary = summary(message)?;
    if !empty && summary.is_empty() {
        return Err(usage(
            "a changeset needs a summary; pass `-m \"what changed\"` (or `-m -` to read stdin)",
        ));
    }
    let id = match id {
        Some(i) if valid_id(i) => i.to_string(),
        Some(i) => {
            return Err(usage(format!(
                "`--id {i}` is not valid; use letters, digits, `-`, `_` or `.`, not starting with `.`"
            )));
        }
        None => default_id(dir)?,
    };
    check_name(
        dir,
        &releases.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
    )?;
    let text = render(&Changeset {
        id: id.clone(),
        releases: releases.clone(),
        summary,
    });
    let rel = format!(".changeset/{id}.md");
    std::fs::create_dir_all(dir.join(".changeset"))
        .map_err(|e| HoistError::Io(format!("cannot create .changeset/: {e}")))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(&rel))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                usage(format!(
                    "{rel} exists; pick another with `--id <id>` or edit that file"
                ))
            } else {
                HoistError::Io(format!("cannot write {rel}: {e}"))
            }
        })?;
    f.write_all(text.as_bytes())
        .map_err(|e| HoistError::Io(format!("cannot write {rel}: {e}")))?;
    let reason = if releases.is_empty() {
        "empty changeset".to_string()
    } else {
        releases
            .iter()
            .map(|(n, t)| format!("{n} {}", t.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(report(
        "changeset add",
        vec![created("changeset", &id, &rel, reason)],
    ))
}
