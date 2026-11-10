//! Dependency kinds and ranges in the release model, and the range helpers (changesets §5.2).

use hoist::changeset::range::{cargo_satisfies, npm_satisfies, rewrite};
use hoist::release::semver::Version;
use hoist::release::{DepKind, Edge, read_release_model};
use std::path::Path;

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn e(name: &str, kind: DepKind, range: Option<&str>) -> Edge {
    Edge {
        name: name.to_string(),
        kind,
        range: range.map(str::to_string),
    }
}

#[test]
fn model_reads_dependency_kinds_and_ranges() {
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    write(d, "hoist.toml", "[release]\nversioning = \"per-package\"\n");
    write(
        d,
        "package.json",
        "{\"name\": \"root\", \"private\": true}\n",
    );
    write(d, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    write(
        d,
        "packages/a/package.json",
        "{\"name\": \"a\", \"version\": \"1.0.0\", \"private\": false}\n",
    );
    write(
        d,
        "packages/b/package.json",
        r#"{"name": "b", "version": "1.0.0", "private": false, "dependencies": {"left-pad": "1.3.0"},
            "peerDependencies": {"a": "^1.0.0"}, "devDependencies": {"a": "workspace:*"}}"#,
    );
    write(
        d,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\n\
         core = { path = \"crates/core\", version = \"0.4\" }\n",
    );
    write(
        d,
        "crates/core/Cargo.toml",
        "[package]\nname = \"core\"\nversion = \"0.4.1\"\n",
    );
    write(
        d,
        "crates/gen/Cargo.toml",
        "[package]\nname = \"gen\"\nversion = \"0.1.0\"\n",
    );
    write(
        d,
        "crates/app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\n\
         core = { workspace = true }\nserde = \"1\"\n\n[build-dependencies]\n\
         gen = { path = \"../gen\" }\n\n[dev-dependencies]\n\
         c2 = { path = \"../core\", package = \"core\", version = \"=0.4.1\" }\n",
    );
    let m = read_release_model(d).expect("model");
    let pkg = |n: &str| m.packages.iter().find(|p| p.name == n).expect(n);
    assert_eq!(
        pkg("b").edges,
        vec![
            e("a", DepKind::Peer, Some("^1.0.0")),
            e("a", DepKind::Dev, Some("workspace:*")),
        ]
    );
    assert_eq!(pkg("b").deps, vec!["a"], "dev edges stay out of deps");
    assert_eq!(
        pkg("app").edges,
        vec![
            e("core", DepKind::Normal, Some("0.4")),
            e("gen", DepKind::Build, None),
            e("core", DepKind::Dev, Some("=0.4.1")),
        ]
    );
    assert_eq!(
        pkg("app").deps,
        vec!["core"],
        "as before: [dependencies] only"
    );
}

#[test]
fn npm_ranges() {
    let t = |r: &str, ver: &str| npm_satisfies(r, &v("1.0.0"), &v(ver));
    assert_eq!(t("^1.2.3", "1.9.0"), Some(true));
    assert_eq!(t("^1.2.3", "2.0.0"), Some(false));
    assert_eq!(t("^0.2.3", "0.3.0"), Some(false));
    assert_eq!(t("^0.0.3", "0.0.4"), Some(false));
    assert_eq!(t("~1.2.3", "1.3.0"), Some(false));
    assert_eq!(t("~1.2.3", "1.2.9"), Some(true));
    assert_eq!(t("1.2.3", "1.2.4"), Some(false));
    assert_eq!(t("=1.2.3", "1.2.3"), Some(true));
    assert_eq!(t(">=1.0.0", "9.0.0"), Some(true));
    assert_eq!(t("1.x", "1.5.0"), Some(true));
    assert_eq!(t("*", "9.0.0"), Some(true));
    // workspace:* is the exact current version (1.0.0 here), ^ and ~ are ^1.0.0 and ~1.0.0.
    assert_eq!(t("workspace:*", "1.0.0"), Some(true));
    assert_eq!(t("workspace:*", "1.0.1"), Some(false));
    assert_eq!(t("workspace:^", "1.9.0"), Some(true));
    assert_eq!(t("workspace:^", "2.0.0"), Some(false));
    assert_eq!(t("workspace:~", "1.1.0"), Some(false));
    assert_eq!(t("workspace:^1.0.0", "2.0.0"), Some(false));
    assert_eq!(t(">=1.0.0 <2.0.0", "1.0.0"), None);
    assert_eq!(t("^1.0.0 || ^2.0.0", "1.0.0"), None);
}

#[test]
fn cargo_ranges() {
    let t = |r: &str, ver: &str| cargo_satisfies(r, &v(ver));
    assert_eq!(t("0.4", "0.4.9"), Some(true));
    assert_eq!(t("0.4", "0.5.0"), Some(false));
    assert_eq!(t("1.2.3", "1.9.0"), Some(true), "bare is caret");
    assert_eq!(t("^0.0.3", "0.0.4"), Some(false));
    assert_eq!(t("~1.2", "1.3.0"), Some(false));
    assert_eq!(t("=1.2.3", "1.2.4"), Some(false));
    assert_eq!(t("*", "3.0.0"), Some(true));
    assert_eq!(t("1.2.*", "1.3.0"), Some(false), "wildcard, not caret");
    assert_eq!(t("1.2.*", "1.2.7"), Some(true));
    assert_eq!(t(">=1.0, <2.0", "1.0.0"), None);
}

#[test]
fn rewrites_keep_the_operator() {
    let n = v("2.1.0");
    assert_eq!(rewrite("^1.0.0", &n), "^2.1.0");
    assert_eq!(rewrite("~1.0.0", &n), "~2.1.0");
    assert_eq!(rewrite(">=1.0.0", &n), ">=2.1.0");
    assert_eq!(rewrite("=1.0.0", &n), "=2.1.0");
    assert_eq!(rewrite("1.0.0", &n), "2.1.0");
    assert_eq!(rewrite("0.4", &n), "2.1.0");
    assert_eq!(rewrite("*", &n), "*");
    assert_eq!(rewrite("", &n), "");
    assert_eq!(rewrite("workspace:*", &n), "workspace:*");
    assert_eq!(rewrite("workspace:^", &n), "workspace:^");
    assert_eq!(rewrite("workspace:~", &n), "workspace:~");
    assert_eq!(rewrite("workspace:^1.0.0", &n), "workspace:^2.1.0");
}
