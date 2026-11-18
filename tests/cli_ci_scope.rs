//! Which CI runs must have sops and age-keygen (spec §15.9.1).
//!
//! Only hoist's own `ci` workflow installs them. bosun's test gate (`bosun.yml`) also runs the
//! suite with `CI=true` but installs nothing, so a test that failed whenever `CI` was set turned
//! main red there. The secrets tests now require the tools only when `GITHUB_WORKFLOW` is `ci`
//! (`common::in_ci_workflow`) and skip elsewhere. These checks keep that rule tied to `ci.yml`:
//! if the workflow is renamed or stops installing the tools, they fail instead of the secrets
//! tests silently skipping everywhere.
mod common;

use common::{CI_WORKFLOW, requires_tools};
use std::path::Path;

fn ci_yml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    std::fs::read_to_string(path).expect("read .github/workflows/ci.yml")
}

#[test]
fn ci_workflow_constant_matches_ci_yml_name() {
    let name = ci_yml()
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().to_string());
    assert_eq!(name.as_deref(), Some(CI_WORKFLOW), "ci.yml `name:` changed");
}

#[test]
fn ci_yml_still_installs_sops_and_age() {
    let yml = ci_yml();
    for needle in [
        "getsops/sops/releases/download/v3.13.3/",
        "FiloSottile/age/releases/download/v1.3.2/",
        "age/age-keygen",
        "GITHUB_PATH",
    ] {
        assert!(yml.contains(needle), "ci.yml no longer has {needle:?}");
    }
}

#[test]
fn tools_are_required_only_in_the_ci_workflow() {
    assert!(requires_tools(Some("ci")));
    for other in [
        None,
        Some("bosun"),
        Some(""),
        Some("CI"),
        Some("ci-ok"),
        Some("release"),
    ] {
        assert!(
            !requires_tools(other),
            "{other:?} must not require the tools"
        );
    }
}

#[test]
fn no_test_file_checks_ci_directly() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                // Built from pieces so this file does not match itself.
                let patterns = [
                    format!("var_os(\"{}\")", "CI"),
                    format!("var(\"{}\")", "CI"),
                ];
                if patterns.iter().any(|p| text.contains(p.as_str())) {
                    offenders.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use common::in_ci_workflow() instead of reading CI: {offenders:?}"
    );
}
