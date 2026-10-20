//! The publishing writes of `hoist release bootstrap` (spec §9.6 steps 2 and 3): an npm
//! placeholder from a fresh temp dir, and a crate from a temporary worktree at its tag. Tool
//! output goes to stderr; npm and cargo use their own logins (no secret is passed).

use crate::error::HoistError;
use crate::release::Package;
use crate::release::tools::{Tool, run, run_redacted};
use crate::repo::git::GithubRepo;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// `placeholder for trusted publishing; releases come from https://github.com/<o>/<r>`.
fn description(repo: &GithubRepo) -> String {
    format!(
        "placeholder for trusted publishing; releases come from https://github.com/{}/{}",
        repo.owner, repo.name
    )
}

/// The placeholder's `package.json` (keys `name`, `version` 0.0.0, `description`,
/// `repository` and, when given, `license`) and `README.md` (the description sentence).
pub(crate) fn placeholder_files(
    pkg: &Package,
    repo: &GithubRepo,
    license: Option<&str>,
) -> (String, String) {
    let description = description(repo);
    let mut m = Map::new();
    m.insert("name".to_string(), json!(pkg.name));
    m.insert("version".to_string(), json!("0.0.0"));
    m.insert("description".to_string(), json!(description));
    m.insert(
        "repository".to_string(),
        json!({
            "type": "git",
            "url": format!("git+https://github.com/{}/{}.git", repo.owner, repo.name),
        }),
    );
    if let Some(l) = license {
        m.insert("license".to_string(), json!(l));
    }
    let manifest = serde_json::to_string_pretty(&Value::Object(m)).expect("JSON") + "\n";
    let readme = format!("# {}\n\n{description}\n", pkg.name);
    (manifest, readme)
}

/// `license` of the package's own package.json (`<dir>/<pkg.dir>/package.json`), if any.
fn own_license(pkg: &Package, dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(&pkg.dir).join("package.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("license")?.as_str().map(str::to_string)
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> HoistError {
    HoistError::Io(format!("{what} {}: {e}", path.display()))
}

/// A new, empty directory under the system temp dir.
fn fresh_temp_dir(label: &str) -> Result<PathBuf, HoistError> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let safe: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let base = std::env::temp_dir();
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!(
            "hoist-bootstrap-npm-{safe}-{}-{n}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err("cannot create", &path, e)),
        }
    }
}

/// Write the placeholder to a new temp dir and run `npm publish --access public` there; the
/// temp dir is removed afterwards. Nothing from the checkout is published. `dir` is the repo.
pub(crate) fn publish_placeholder(
    pkg: &Package,
    repo: &GithubRepo,
    dir: &Path,
) -> Result<(), HoistError> {
    let license = own_license(pkg, dir);
    let (manifest, readme) = placeholder_files(pkg, repo, license.as_deref());
    let tmp = fresh_temp_dir(&pkg.name)?;
    let result = (|| {
        for (name, text) in [("package.json", &manifest), ("README.md", &readme)] {
            let path = tmp.join(name);
            std::fs::write(&path, text).map_err(|e| io_err("cannot write", &path, e))?;
        }
        run_redacted(Tool::Npm, &tmp, &["publish", "--access", "public"], &[])?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// `git worktree remove --force <wt>`, then make sure the directory and its registration
/// are gone (errors are reported on stderr only: the publish result is what counts).
fn remove_worktree(dir: &Path, wt: &Path) {
    let path = wt.to_string_lossy();
    if let Err(e) = run(
        Tool::Git,
        dir,
        &["worktree", "remove", "--force", &path],
        &[],
    ) {
        eprintln!("warning: {e}");
    }
    if wt.exists() {
        let _ = std::fs::remove_dir_all(wt);
    }
    let _ = run(Tool::Git, dir, &["worktree", "prune"], &[]);
}

/// Publish the crate's manifest version from a temporary worktree at `tag`
/// (`<temp>/hoist-bootstrap-<name>-<pid>`) with `cargo publish -p <name> --locked`; the
/// worktree is removed afterwards, on success or failure. A dead worktree of an interrupted
/// run is pruned first.
pub(crate) fn publish_crate(dir: &Path, pkg: &Package, tag: &str) -> Result<(), HoistError> {
    let wt = std::env::temp_dir().join(format!(
        "hoist-bootstrap-{}-{}",
        pkg.name,
        std::process::id()
    ));
    if wt.exists() {
        remove_worktree(dir, &wt);
    }
    run(Tool::Git, dir, &["worktree", "prune"], &[])?;
    let path = wt.to_string_lossy().into_owned();
    run(
        Tool::Git,
        dir,
        &["worktree", "add", "--detach", &path, tag],
        &[],
    )?;
    let result = run_redacted(
        Tool::Cargo,
        &wt,
        &["publish", "-p", &pkg.name, "--locked"],
        &[],
    )
    .map(|_| ())
    .map_err(HoistError::from);
    remove_worktree(dir, &wt);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PublishCfg;
    use crate::release::PkgKind;
    use crate::repo::Sourced;

    fn pkg(dir: &str) -> Package {
        Package {
            name: "@acme/lib".to_string(),
            kind: PkgKind::Npm,
            dir: PathBuf::from(dir),
            version: Sourced {
                value: "1.0.0".to_string(),
                source: "x:1".to_string(),
            },
            deps: vec![],
            publish: PublishCfg::default(),
            launcher: None,
        }
    }

    fn repo() -> GithubRepo {
        GithubRepo {
            owner: "acme".to_string(),
            name: "x".to_string(),
        }
    }

    const DESCRIPTION: &str =
        "placeholder for trusted publishing; releases come from https://github.com/acme/x";

    #[test]
    fn placeholder_has_only_the_allowed_keys() {
        let (manifest, readme) = placeholder_files(&pkg("packages/lib"), &repo(), Some("MIT"));
        let v: Value = serde_json::from_str(&manifest).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["description", "license", "name", "repository", "version"]
        );
        assert_eq!(v["name"], "@acme/lib");
        assert_eq!(v["version"], "0.0.0");
        assert_eq!(v["description"], DESCRIPTION);
        assert_eq!(
            v["repository"],
            json!({"type": "git", "url": "git+https://github.com/acme/x.git"})
        );
        assert_eq!(v["license"], "MIT");
        assert!(readme.contains(DESCRIPTION), "{readme}");

        // No license in the package's own manifest: the key is omitted.
        let (manifest, _) = placeholder_files(&pkg("packages/lib"), &repo(), None);
        let v: Value = serde_json::from_str(&manifest).unwrap();
        assert!(v.get("license").is_none(), "{manifest}");
        assert_eq!(v.as_object().unwrap().len(), 4);
    }

    #[test]
    fn license_comes_from_the_packages_own_manifest() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("packages/lib")).unwrap();
        std::fs::write(
            d.path().join("packages/lib/package.json"),
            r#"{"name":"@acme/lib","license":"Apache-2.0","files":["dist"]}"#,
        )
        .unwrap();
        assert_eq!(
            own_license(&pkg("packages/lib"), d.path()).as_deref(),
            Some("Apache-2.0")
        );
        assert_eq!(own_license(&pkg("npm/missing"), d.path()), None);
    }
}
