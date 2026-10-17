mod common;

use hoist::HoistError;
use hoist::config::{ReleasePins, Versioning, parse_pins};
use hoist::release::commits::{Bump, Commit, Section, classify};
use hoist::release::planner::{History, NextVersion, plan_versions};
use hoist::release::{Package, PkgKind, ReleaseModel};
use hoist::repo::Sourced;
use std::collections::HashMap;
use std::path::PathBuf;

fn commit(subject: &str, body: &str) -> Commit {
    Commit {
        sha: "0123456789abcdef".to_string(),
        subject: subject.to_string(),
        body: body.to_string(),
        paths: vec![],
    }
}

fn pkg(name: &str, version: &str, deps: &[&str]) -> Package {
    Package {
        name: name.to_string(),
        kind: PkgKind::Npm,
        dir: PathBuf::from(format!("packages/{name}")),
        version: Sourced {
            value: version.to_string(),
            source: format!("packages/{name}/package.json:3"),
        },
        deps: deps.iter().map(|d| d.to_string()).collect(),
        publish: Default::default(),
    }
}

fn model(packages: Vec<Package>, versioning: Versioning) -> ReleaseModel {
    ReleaseModel {
        packages,
        versioning,
        tag_format: "{name}@{version}".to_string(),
        environment: "release".to_string(),
    }
}

/// Every package tagged; `commits` maps package to commit subjects.
fn history(tagged: &[&str], commits: &[(&str, &[&str])]) -> History {
    let mut h = History {
        last_tag: HashMap::new(),
        commits: HashMap::new(),
    };
    for t in tagged {
        h.last_tag.insert(t.to_string(), Some(format!("{t}@1.0.0")));
    }
    for (name, subjects) in commits {
        h.commits.insert(
            name.to_string(),
            subjects.iter().map(|s| commit(s, "")).collect(),
        );
    }
    h
}

fn plan(m: &ReleaseModel, h: &History) -> Vec<NextVersion> {
    plan_versions(m, h, &ReleasePins::default()).expect("plan")
}

fn find<'a>(plan: &'a [NextVersion], name: &str) -> &'a NextVersion {
    plan.iter().find(|n| n.package == name).expect(name)
}

#[test]
fn classify_types() {
    let cases = [
        (commit("feat: x", ""), Bump::Minor),
        (commit("fix(api): y", ""), Bump::Patch),
        (commit("feat!: z", ""), Bump::Major),
        (commit("chore: w", ""), Bump::None),
        (commit("docs: w", ""), Bump::None),
        (commit("perf: w", ""), Bump::None),
        (
            commit("fix: w", "text\n\nBREAKING CHANGE: gone"),
            Bump::Major,
        ),
        (commit("Merge pull request #1 from a/b", ""), Bump::None),
        (commit("updated stuff", ""), Bump::None),
    ];
    for (c, want) in cases {
        assert_eq!(classify(&c).0, want, "{}", c.subject);
    }
}

#[test]
fn classify_entries() {
    let (_, e) = classify(&commit("fix(api): handle nulls", ""));
    let e = e.expect("entry");
    assert_eq!(e.section, Section::Fixes);
    assert_eq!(e.text, "handle nulls");
    assert_eq!(e.short_sha, "0123456");
    let (_, e) = classify(&commit("feat!: drop v1", ""));
    assert_eq!(e.expect("entry").section, Section::Breaking);
    let (_, e) = classify(&commit("feat: add x", ""));
    assert_eq!(e.expect("entry").section, Section::Features);
    assert!(classify(&commit("chore: x", "")).1.is_none());
}

#[test]
fn breaking_below_1_bumps_minor() {
    let m = model(vec![pkg("a", "0.4.2", &[])], Versioning::PerPackage);
    let p = plan(&m, &history(&["a"], &[("a", &["feat!: big"])]));
    assert_eq!(find(&p, "a").to.to_string(), "0.5.0");
    let p = plan(&m, &history(&["a"], &[("a", &["feat: small"])]));
    assert_eq!(find(&p, "a").to.to_string(), "0.5.0");
    let p = plan(&m, &history(&["a"], &[("a", &["fix: tiny"])]));
    assert_eq!(find(&p, "a").to.to_string(), "0.4.3");
}

#[test]
fn dependent_gets_patch() {
    let m = model(
        vec![pkg("a", "2.0.0", &["b"]), pkg("b", "1.0.0", &[])],
        Versioning::PerPackage,
    );
    let p = plan(&m, &history(&["a", "b"], &[("b", &["fix: y"])]));
    assert_eq!(find(&p, "b").to.to_string(), "1.0.1");
    let a = find(&p, "a");
    assert_eq!(a.from.to_string(), "2.0.0");
    assert_eq!(a.to.to_string(), "2.0.1");
    assert_eq!(a.reason, "dependency b bumped");
}

#[test]
fn fixed_versioning_uses_max_bump_for_all() {
    let m = model(
        vec![pkg("a", "1.0.0", &[]), pkg("b", "1.0.0", &[])],
        Versioning::Fixed,
    );
    let p = plan(
        &m,
        &history(&["a", "b"], &[("a", &["fix: x"]), ("b", &["feat: y"])]),
    );
    assert_eq!(find(&p, "a").to.to_string(), "1.1.0");
    assert_eq!(find(&p, "b").to.to_string(), "1.1.0");
}

#[test]
fn first_release_keeps_manifest_version() {
    let m = model(vec![pkg("a", "0.3.0", &[])], Versioning::PerPackage);
    let mut h = history(&[], &[("a", &["feat: first"])]);
    h.last_tag.insert("a".to_string(), None);
    let p = plan(&m, &h);
    let a = find(&p, "a");
    assert!(a.first_release);
    assert_eq!(a.from, a.to);
    assert_eq!(a.to.to_string(), "0.3.0");
}

#[test]
fn docs_only_releases_nothing() {
    let m = model(
        vec![pkg("a", "1.0.0", &["b"]), pkg("b", "1.0.0", &[])],
        Versioning::PerPackage,
    );
    let p = plan(
        &m,
        &history(
            &["a", "b"],
            &[("a", &["docs: x"]), ("b", &["chore: y", "ci: z"])],
        ),
    );
    assert!(p.is_empty(), "{p:?}");
}

#[test]
fn pin_and_skip_apply() {
    let m = model(
        vec![
            pkg("a", "1.0.0", &[]),
            pkg("b", "1.0.0", &[]),
            pkg("c", "1.0.0", &[]),
        ],
        Versioning::PerPackage,
    );
    let h = history(
        &["a", "b", "c"],
        &[("a", &["fix: x"]), ("b", &["fix: x"]), ("c", &["fix: x"])],
    );
    let pins = parse_pins(
        "[packages.a]\nversion = \"2.0.0\"\n[packages.b]\nskip = true\n[packages.c]\nversion = \"1.0.1\"\n",
    )
    .expect("pins");
    let p = plan_versions(&m, &h, &pins).expect("plan");
    assert_eq!(find(&p, "a").to.to_string(), "2.0.0");
    assert!(p.iter().all(|n| n.package != "b"));
    // a pin not above the bump result (1.0.1) does not change it.
    assert_eq!(find(&p, "c").to.to_string(), "1.0.1");
}

#[test]
fn pin_not_above_current_exits_3() {
    let m = model(vec![pkg("a", "1.2.0", &[])], Versioning::PerPackage);
    let h = history(&["a"], &[("a", &["fix: x"])]);
    let pins = parse_pins("[packages.a]\nversion = \"1.2.0\"\n").expect("pins");
    match plan_versions(&m, &h, &pins) {
        Err(e @ HoistError::Config { .. }) => {
            assert_eq!(e.exit_code(), 3);
            let text = e.to_string();
            assert!(text.contains(".hoist/release.toml"), "{text}");
            assert!(text.contains("`a`"), "{text}");
        }
        other => panic!("expected config error, got {other:?}"),
    }
}

#[test]
fn pins_reject_unknown_keys() {
    let err = parse_pins("[packages.a]\nbogus = 1\n").expect_err("unknown key");
    assert!(err.to_string().contains(".hoist/release.toml"), "{err}");
    assert!(err.to_string().contains("bogus"), "{err}");
}

#[test]
fn order_is_dependencies_first() {
    let m = model(
        vec![
            pkg("a", "1.0.0", &["c"]),
            pkg("b", "1.0.0", &[]),
            pkg("c", "1.0.0", &["b"]),
            pkg("d", "1.0.0", &[]),
        ],
        Versioning::PerPackage,
    );
    let h = history(
        &["a", "b", "c", "d"],
        &[
            ("a", &["fix: x"]),
            ("b", &["fix: x"]),
            ("c", &["fix: x"]),
            ("d", &["fix: x"]),
        ],
    );
    let names: Vec<_> = plan(&m, &h).into_iter().map(|n| n.package).collect();
    assert_eq!(names, vec!["b", "c", "a", "d"]);
}

fn fixed(vs: &[(&str, &str)]) -> ReleaseModel {
    model(
        vs.iter().map(|(n, v)| pkg(n, v, &[])).collect(),
        Versioning::Fixed,
    )
}

#[test]
fn fixed_unequal_versions_share_the_highest_bumped() {
    let m = fixed(&[("a", "1.2.0"), ("b", "1.0.0")]);
    let p = plan(&m, &history(&["a", "b"], &[("a", &["fix: x"])]));
    assert_eq!(find(&p, "a").to.to_string(), "1.2.1");
    assert_eq!(find(&p, "b").to.to_string(), "1.2.1");
    assert_eq!(find(&p, "b").from.to_string(), "1.0.0");
}

#[test]
fn fixed_pin_above_shared_applies_to_all_and_untouched_package_gets_reason() {
    let m = fixed(&[("a", "1.3.0"), ("b", "1.2.0")]);
    let h = history(&["a", "b"], &[("b", &["fix: x"])]);
    // shared = max(1.3.0, 1.2.0) bumped by patch; a has no commits but still releases.
    let p = plan(&m, &h);
    assert_eq!(find(&p, "a").to.to_string(), "1.3.1");
    assert_eq!(find(&p, "a").reason, "fixed versioning");
    let pins = parse_pins("[packages.a]\nversion = \"2.0.0\"\n").expect("pins");
    let p = plan_versions(&m, &h, &pins).expect("plan");
    assert_eq!(find(&p, "a").to.to_string(), "2.0.0");
    assert_eq!(find(&p, "b").to.to_string(), "2.0.0");
}

#[test]
fn fixed_untagged_package_takes_the_shared_version() {
    let m = fixed(&[("a", "1.2.0"), ("b", "0.1.0")]);
    let mut h = history(&["a"], &[("a", &["feat: x"])]);
    h.last_tag.insert("b".to_string(), None);
    let p = plan(&m, &h);
    assert_eq!(find(&p, "a").to.to_string(), "1.3.0");
    assert_eq!(find(&p, "b").to.to_string(), "1.3.0");
}

#[test]
fn fixed_with_no_bump_releases_nothing() {
    let m = fixed(&[("a", "1.2.0"), ("b", "1.0.0")]);
    let p = plan(&m, &history(&["a", "b"], &[("a", &["docs: x"])]));
    assert!(p.is_empty());
}

#[test]
fn skip_in_fixed_mode_exits_3() {
    let m = fixed(&[("a", "1.0.0"), ("b", "1.0.0")]);
    let pins = parse_pins("[packages.b]\nskip = true\n").expect("pins");
    match plan_versions(&m, &history(&["a", "b"], &[]), &pins) {
        Err(e @ HoistError::Config { .. }) => {
            let t = e.to_string();
            assert!(
                t.contains(".hoist/release.toml") && t.contains("`b`"),
                "{t}"
            );
        }
        other => panic!("expected config error, got {other:?}"),
    }
}

fn untagged_history(commits: &[(&str, &[&str])]) -> History {
    let mut h = history(&[], commits);
    for n in ["a", "b"] {
        h.last_tag.insert(n.to_string(), None);
    }
    h
}

#[test]
fn fixed_first_release_uses_the_highest_manifest_version() {
    let m = fixed(&[("a", "0.3.0"), ("b", "0.1.0")]);
    let p = plan(&m, &untagged_history(&[]));
    assert_eq!(p.len(), 2);
    for n in &p {
        assert!(n.first_release);
        assert_eq!(n.to.to_string(), "0.3.0", "{}", n.package);
    }
    let a = find(&p, "a");
    assert_eq!(a.from, a.to);
    assert_eq!(find(&p, "b").from.to_string(), "0.1.0");
}

#[test]
fn fixed_first_release_ignores_commits() {
    let m = fixed(&[("a", "0.3.0"), ("b", "0.1.0")]);
    let p = plan(&m, &untagged_history(&[("a", &["feat: x"])]));
    assert_eq!(find(&p, "a").to.to_string(), "0.3.0");
    assert_eq!(find(&p, "b").to.to_string(), "0.3.0");
}

#[test]
fn fixed_first_release_pin_above_wins() {
    let m = fixed(&[("a", "0.3.0"), ("b", "0.1.0")]);
    let pins = parse_pins("[packages.b]\nversion = \"1.0.0\"\n").expect("pins");
    let p = plan_versions(&m, &untagged_history(&[]), &pins).expect("plan");
    assert_eq!(find(&p, "a").to.to_string(), "1.0.0");
    assert_eq!(find(&p, "b").to.to_string(), "1.0.0");
}
