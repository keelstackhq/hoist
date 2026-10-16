mod common;

use hoist::HoistError;
use hoist::repo::targets::{TargetKind, read_targets};
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/repos")
        .join(name)
}

fn inline(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }
    dir
}

#[test]
fn site_has_one_pages_target() {
    let (targets, packages, _) = read_targets(&fixture("site")).expect("targets");
    assert_eq!(targets.len(), 1, "{targets:?}");
    let t = &targets[0];
    assert_eq!(t.name, "site");
    assert_eq!(t.kind.value, TargetKind::CloudflarePages);
    assert_eq!(t.kind.source, "apps/site/hoist.toml:deploy.provider");
    let deploy = t.config.deploy.as_ref().expect("deploy config");
    assert_eq!(deploy.project.as_deref(), Some("keelstack-landing"));
    assert!(packages.npm.is_empty(), "{packages:?}");
    assert!(packages.crates.is_empty());
}

#[test]
fn pages_inferred_from_wrangler_without_override() {
    let dir = inline(&[
        ("pnpm-workspace.yaml", "packages:\n  - apps/*\n"),
        (
            "apps/site/wrangler.jsonc",
            "{ \"assets\": { \"directory\": \"./dist\", }, }\n",
        ),
    ]);
    let (targets, _, _) = read_targets(dir.path()).expect("targets");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].kind.value, TargetKind::CloudflarePages);
    assert_eq!(targets[0].kind.source, "apps/site/wrangler.jsonc:1");
}

#[test]
fn web_has_workers_npm_and_crate() {
    let (targets, packages, renovate) = read_targets(&fixture("web")).expect("targets");
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0].name, "api");
    assert_eq!(targets[0].kind.value, TargetKind::CloudflareWorkers);
    assert_eq!(targets[0].kind.source, "apps/api/wrangler.jsonc:1");
    assert_eq!(packages.npm.len(), 1);
    assert_eq!(packages.npm[0].value, "@acme/ui");
    assert_eq!(packages.npm[0].source, "packages/ui/package.json:1");
    assert_eq!(packages.crates.len(), 1);
    assert_eq!(packages.crates[0].value, "tool");
    assert_eq!(packages.crates[0].source, "crates/tool/Cargo.toml:1");
    assert!(!renovate);
}

#[test]
fn private_package_not_published() {
    let dir = inline(&[
        ("pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
        (
            "packages/ui/package.json",
            "{ \"name\": \"@acme/ui\", \"private\": true }",
        ),
    ]);
    let (_, packages, _) = read_targets(dir.path()).expect("targets");
    assert!(packages.npm.is_empty(), "{packages:?}");
}

#[test]
fn publish_config_and_publish_false() {
    let dir = inline(&[
        ("pnpm-workspace.yaml", "packages:\n  - packages/*\n"),
        (
            "packages/a/package.json",
            "{ \"name\": \"a\", \"publishConfig\": { \"access\": \"public\" } }",
        ),
        ("packages/b/package.json", "{ \"name\": \"b\" }"),
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        (
            "crates/x/Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\npublish = false\n",
        ),
        (
            "crates/y/Cargo.toml",
            "[package]\nname = \"y\"\nversion = \"0.1.0\"\n",
        ),
    ]);
    let (_, packages, _) = read_targets(dir.path()).expect("targets");
    let npm: Vec<&str> = packages.npm.iter().map(|p| p.value.as_str()).collect();
    let crates: Vec<&str> = packages.crates.iter().map(|p| p.value.as_str()).collect();
    assert_eq!(npm, ["a"]);
    assert_eq!(crates, ["y"]);
}

#[test]
fn renovate_detected() {
    let (_, _, renovate) = read_targets(&fixture("site")).expect("targets");
    assert!(renovate);
}

#[test]
fn invalid_jsonc_names_file() {
    let dir = inline(&[
        ("pnpm-workspace.yaml", "packages:\n  - apps/*\n"),
        ("apps/site/wrangler.jsonc", "{ \"assets\": "),
    ]);
    let err = read_targets(dir.path()).expect_err("invalid jsonc");
    let HoistError::Config { file, .. } = &err else {
        panic!("expected config error, got {err:?}");
    };
    assert!(file.contains("apps/site/wrangler.jsonc"), "{err}");
}

#[test]
fn unsupported_glob_is_config_error() {
    let dir = inline(&[("pnpm-workspace.yaml", "packages:\n  - apps/**\n")]);
    let err = read_targets(dir.path()).expect_err("glob");
    assert!(err.to_string().contains("apps/**"), "{err}");
}
