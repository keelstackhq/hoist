mod common;

use hoist::HoistError;
use hoist::repo::workflows::read_workflows;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/repos")
        .join(name)
}

fn inline(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let wf = dir.path().join(".github/workflows");
    fs::create_dir_all(&wf).expect("mkdir");
    for (name, text) in files {
        fs::write(wf.join(name), text).expect("write");
    }
    dir
}

#[test]
fn site_fixture_jobs_include_ci_ok() {
    let w = read_workflows(&fixture("site")).expect("workflows");
    let ids: Vec<&str> = w.jobs.iter().map(|j| j.value.as_str()).collect();
    for want in ["ci", "ci-ok", "preview", "deploy"] {
        assert!(ids.contains(&want), "missing job {want}: {ids:?}");
    }
    let ci_ok = w.jobs.iter().find(|j| j.value == "ci-ok").expect("ci-ok");
    assert!(
        ci_ok.source.starts_with(".github/workflows/ci.yml:"),
        "{}",
        ci_ok.source
    );
    assert_eq!(ci_ok.source, ".github/workflows/ci.yml:98");
}

#[test]
fn site_fixture_secrets() {
    let w = read_workflows(&fixture("site")).expect("workflows");
    let mut names: Vec<&str> = w.secrets.iter().map(|s| s.value.as_str()).collect();
    names.sort();
    assert_eq!(names, ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_TOKEN"]);
}

#[test]
fn merge_group_detected() {
    let w = read_workflows(&fixture("site")).expect("workflows");
    assert!(w.merge_group);
    let b = read_workflows(&fixture("bare")).expect("workflows");
    assert!(!b.merge_group);
    assert_eq!(b.jobs.len(), 1);
}

#[test]
fn environment_object_form() {
    let dir = inline(&[(
        "release.yml",
        "name: release\non: push\njobs:\n  publish:\n    environment: { name: release }\n    runs-on: ubuntu-latest\n    steps: []\n  other:\n    environment: staging\n    runs-on: ubuntu-latest\n    steps: []\n",
    )]);
    let w = read_workflows(dir.path()).expect("workflows");
    let envs: Vec<&str> = w.environments.iter().map(|e| e.value.as_str()).collect();
    assert_eq!(envs, ["release", "staging"]);
}

#[test]
fn merge_group_detected_with_boolean_on_key() {
    let dir = inline(&[("a.yml", "on: true\njobs: {}\n")]);
    assert!(!read_workflows(dir.path()).expect("workflows").merge_group);
    let dir = inline(&[(
        "a.yml",
        "on:\n  merge_group:\njobs:\n  ci:\n    steps: []\n",
    )]);
    assert!(read_workflows(dir.path()).expect("workflows").merge_group);
    let dir = inline(&[(
        "a.yml",
        "on: [push, merge_group]\njobs:\n  ci:\n    steps: []\n",
    )]);
    assert!(read_workflows(dir.path()).expect("workflows").merge_group);
    let dir = inline(&[("a.yml", "on: merge_group\njobs:\n  ci:\n    steps: []\n")]);
    assert!(read_workflows(dir.path()).expect("workflows").merge_group);
}

#[test]
fn secrets_dedupe_and_skip_github_token() {
    let dir = inline(&[
        (
            "a.yml",
            "on: push\njobs:\n  a:\n    env:\n      T: ${{ secrets.GITHUB_TOKEN }}\n      X: ${{secrets.FOO}}\n",
        ),
        (
            "b.yaml",
            "on: push\njobs:\n  b:\n    env:\n      X: ${{ secrets.FOO }}\n",
        ),
    ]);
    let w = read_workflows(dir.path()).expect("workflows");
    assert_eq!(w.secrets.len(), 1);
    assert_eq!(w.secrets[0].value, "FOO");
    assert_eq!(w.secrets[0].source, ".github/workflows/a.yml:6");
}

#[test]
fn missing_workflows_dir_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let w = read_workflows(dir.path()).expect("workflows");
    assert!(w.jobs.is_empty() && w.secrets.is_empty() && !w.merge_group);
}

#[test]
fn invalid_yaml_names_file() {
    let dir = inline(&[("broken.yml", "on: push\njobs: [\n")]);
    let err = read_workflows(dir.path()).expect_err("must fail");
    match &err {
        HoistError::Config { file, .. } => {
            assert_eq!(file, ".github/workflows/broken.yml")
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(err.to_string().contains("broken.yml"));
}
