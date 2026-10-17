mod common;

use hoist::HoistError;
use hoist::config::Versioning;
use hoist::release::{PkgKind, read_release_model};
use serde_json::Value;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/repos")
        .join(name)
}

fn config_err(r: Result<hoist::release::ReleaseModel, HoistError>) -> String {
    match r {
        Err(e @ HoistError::Config { .. }) => e.to_string(),
        other => panic!("expected config error, got {other:?}"),
    }
}

#[test]
fn model_reads_versions_and_internal_deps() {
    let model = read_release_model(&fixture("release-ws")).expect("model");
    let a = model.packages.iter().find(|p| p.name == "a").expect("a");
    assert_eq!(a.kind, PkgKind::Npm);
    assert_eq!(a.deps, vec!["b".to_string()]);
    assert_eq!(a.version.value, "1.2.0");
    assert_eq!(a.version.source, "packages/a/package.json:3");
    let b = model.packages.iter().find(|p| p.name == "b").expect("b");
    assert!(b.deps.is_empty());
    assert_eq!(b.version.value, "1.0.1");
    let tool = model
        .packages
        .iter()
        .find(|p| p.name == "tool")
        .expect("tool");
    assert_eq!(tool.kind, PkgKind::Crate);
    assert_eq!(tool.version.source, "crates/tool/Cargo.toml:3");
    assert!(tool.publish.crates.is_some());
    assert_eq!(
        tool.publish.github_release.as_ref().expect("gr").targets,
        vec!["x86_64-unknown-linux-musl", "aarch64-apple-darwin"]
    );
    assert!(a.publish.npm.is_some());
    assert_eq!(model.versioning, Versioning::PerPackage);
    assert_eq!(model.environment, "release");
}

#[test]
fn single_root_crate_is_a_package_with_v_tag() {
    let model = read_release_model(&fixture("release-single")).expect("model");
    assert_eq!(model.packages.len(), 1);
    let p = &model.packages[0];
    assert_eq!(p.name, "solo");
    assert_eq!(p.version.source, "Cargo.toml:3");
    assert_eq!(model.tag_for(p, "1.2.3"), "v1.2.3");
}

#[test]
fn tag_format_default_and_override() {
    let ws = fixture("release-ws");
    let model = read_release_model(&ws).expect("model");
    let a = model.packages.iter().find(|p| p.name == "a").unwrap();
    assert_eq!(model.tag_for(a, "1.3.0"), "a@1.3.0");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("hoist.toml"),
        "[release]\ntag_format = \"rel-{name}-{version}\"\n",
    )
    .unwrap();
    let model = read_release_model(dir.path()).expect("model");
    assert_eq!(model.tag_for(&model.packages[0], "2.0.0"), "rel-solo-2.0.0");
}

#[test]
fn unknown_publish_key_exits_3() {
    let repo = common::fixture_repo("release-ws", "https://github.com/acme/x.git");
    std::fs::write(
        repo.path().join("packages/a/hoist.toml"),
        "[publish]\nnpm = { acess = \"public\" }\n",
    )
    .unwrap();
    let out = common::hoist()
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .args(["explain", "--json", "-C"])
        .arg(repo.path())
        .assert()
        .code(3)
        .get_output()
        .clone();
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("acess"), "{text}");
    assert!(text.contains("hoist.toml"), "{text}");
}

#[test]
fn prerelease_version_exits_3() {
    let repo = common::fixture_repo("release-ws", "https://github.com/acme/x.git");
    std::fs::write(
        repo.path().join("packages/b/package.json"),
        "{\n  \"name\": \"b\",\n  \"version\": \"1.0.0-beta.1\",\n  \"private\": false\n}\n",
    )
    .unwrap();
    let msg = config_err(read_release_model(repo.path()));
    assert!(
        msg.contains("pre-release versions are not supported"),
        "{msg}"
    );
    assert!(msg.contains("packages/b/package.json"), "{msg}");
}

#[test]
fn explain_lists_release_items() {
    let repo = common::fixture_repo("release-ws", "https://github.com/acme/x.git");
    let out = common::hoist()
        .env("CLOUDFLARE_ACCOUNT_ID", "acc")
        .args(["explain", "--json", "-C"])
        .arg(repo.path())
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&out).unwrap();
    let items = report["items"].as_array().unwrap();
    let find = |area: &str, kind: &str, key: &str| {
        items
            .iter()
            .find(|i| i["area"] == area && i["kind"] == kind && i["key"] == key)
    };
    let a = find("release", "package", "a").expect("release/package a");
    assert!(a["reason"].as_str().unwrap().contains("1.2.0"), "{a}");
    assert!(find("npm", "package", "a").is_some());
    assert!(find("crates", "crate", "tool").is_some());
}
