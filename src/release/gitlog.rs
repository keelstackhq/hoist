//! Release history from git: the last tag of each package and the commits since it.

use super::commits::Commit;
use super::planner::History;
use super::semver::Version;
use super::tools::{Tool, run};
use super::{Package, ReleaseModel};
use crate::error::HoistError;
use std::collections::HashMap;
use std::path::Path;

const NO_HISTORY: &str = "this is a shallow clone: fetch full history and tags \
(fetch-depth: 0 in actions/checkout, or `git fetch --unshallow --tags`) before releasing";

const NO_TAGS: &str = "release tags exist on `origin` but not in this clone: \
fetch full history and tags (fetch-depth: 0 in actions/checkout, or `git fetch --tags`) before releasing";

/// The highest version among `tags` that match the model's tag format for `p`.
fn last_tag(m: &ReleaseModel, p: &Package, tags: &[&str]) -> Option<String> {
    const MARK: &str = "\u{1}";
    let pattern = m.tag_for(p, MARK);
    let (prefix, suffix) = pattern.split_once(MARK)?;
    tags.iter()
        .filter_map(|t| {
            let mid = t.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((Version::parse(mid).ok()?, *t))
        })
        .max_by_key(|(v, _)| *v)
        .map(|(_, t)| t.to_string())
}

/// No local tag matches any package. When the remote has one, the clone lacks its tags.
fn check_remote_tags(dir: &Path, m: &ReleaseModel) -> Result<(), HoistError> {
    let listing = match run(Tool::Git, dir, &["ls-remote", "--tags", "origin"], &[]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "note: no release tags found locally and the remote's tags could not be read ({e}); assuming first releases"
            );
            return Ok(());
        }
    };
    let names: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split('\t').nth(1)?.strip_prefix("refs/tags/"))
        .map(|n| n.trim_end_matches("^{}"))
        .collect();
    if m.packages.iter().any(|p| last_tag(m, p, &names).is_some()) {
        return Err(HoistError::Usage(NO_TAGS.to_string()));
    }
    Ok(())
}

/// Parse `git log --format=%H%x1f%s%x1f%b%x1e --name-only` output.
fn parse_log(out: &str) -> Vec<Commit> {
    let mut commits: Vec<Commit> = Vec::new();
    for chunk in out.split('\u{1e}') {
        // The header starts on the last line before its first separator.
        let header_at = chunk
            .find('\u{1f}')
            .map(|i| chunk[..i].rfind('\n').map_or(0, |n| n + 1));
        let (names, header) = match header_at {
            Some(h) => chunk.split_at(h),
            None => (chunk, ""),
        };
        if let Some(prev) = commits.last_mut() {
            prev.paths.extend(
                names
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string),
            );
        }
        if header.is_empty() {
            continue;
        }
        let mut parts = header.splitn(3, '\u{1f}');
        let sha = parts.next().unwrap_or("").trim().to_string();
        let subject = parts.next().unwrap_or("").to_string();
        let body = parts.next().unwrap_or("").trim_end().to_string();
        commits.push(Commit {
            sha,
            subject,
            body,
            paths: vec![],
        });
    }
    commits
}

/// Exit 3 on a shallow clone: release commands need full history and tags.
pub fn require_full_history(dir: &Path) -> Result<(), HoistError> {
    let shallow = run(
        Tool::Git,
        dir,
        &["rev-parse", "--is-shallow-repository"],
        &[],
    )?;
    if shallow.trim() == "true" {
        return Err(HoistError::Usage(NO_HISTORY.to_string()));
    }
    Ok(())
}

/// Read the last tag and the commits since it for every package. Exits 3 on a shallow clone.
pub fn read_history(dir: &Path, m: &ReleaseModel) -> Result<History, HoistError> {
    require_full_history(dir)?;
    let tag_list = run(Tool::Git, dir, &["tag", "--list", "--merged", "HEAD"], &[])?;
    let tags: Vec<&str> = tag_list.lines().map(str::trim).collect();
    if !m.packages.is_empty() && m.packages.iter().all(|p| last_tag(m, p, &tags).is_none()) {
        check_remote_tags(dir, m)?;
    }
    let mut h = History {
        last_tag: HashMap::new(),
        commits: HashMap::new(),
    };
    for p in &m.packages {
        let tag = last_tag(m, p, &tags);
        let range = tag
            .as_deref()
            .map_or_else(|| "HEAD".to_string(), |t| format!("{t}..HEAD"));
        let path = if p.dir.as_os_str().is_empty() {
            ".".to_string()
        } else {
            p.dir.to_string_lossy().into_owned()
        };
        let out = run(
            Tool::Git,
            dir,
            &[
                "-c",
                "core.quotepath=off",
                "-c",
                "log.showSignature=false",
                "log",
                &range,
                "--format=%H%x1f%s%x1f%b%x1e",
                "--name-only",
                "--",
                &path,
            ],
            &[],
        )?;
        h.commits.insert(p.name.clone(), parse_log(&out));
        h.last_tag.insert(p.name.clone(), tag);
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commits_with_paths_and_multiline_bodies() {
        let out = "aaa\u{1f}feat: x\u{1f}line1\nline2\n\u{1e}\n\npackages/b/x.ts\npackages/b/y.ts\n\
                   bbb\u{1f}fix: y\u{1f}\u{1e}\n\npackages/b/z.ts\n";
        let c = parse_log(out);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].sha, "aaa");
        assert_eq!(c[0].body, "line1\nline2");
        assert_eq!(c[0].paths, vec!["packages/b/x.ts", "packages/b/y.ts"]);
        assert_eq!(c[1].subject, "fix: y");
        assert_eq!(c[1].paths, vec!["packages/b/z.ts"]);
    }
}
