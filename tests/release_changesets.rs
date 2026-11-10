//! Changesets planner (spec changesets §5) and range helpers, pure.

use hoist::HoistError;
use hoist::changeset::plan::{plan_versions, plan_with_rewrites};
use hoist::changeset::{Changeset, CsConfig, CsType, read_config};
use hoist::config::{ReleasePins, Versioning, parse_pins};
use hoist::release::planner::{History, NextVersion};
use hoist::release::semver::Version;
use hoist::release::{DepKind, Edge, Launcher, Package, PkgKind, ReleaseModel};
use hoist::repo::Sourced;
use std::collections::HashMap;
use std::path::PathBuf;

fn pkg(name: &str, kind: PkgKind, version: &str, edges: &[(&str, DepKind, &str)]) -> Package {
    Package {
        name: name.to_string(),
        kind,
        dir: PathBuf::from(format!("packages/{name}")),
        version: Sourced {
            value: version.to_string(),
            source: format!("packages/{name}/package.json:3"),
        },
        deps: edges
            .iter()
            .filter(|e| e.1 != DepKind::Dev)
            .map(|e| e.0.to_string())
            .collect(),
        edges: edges
            .iter()
            .map(|(n, k, r)| Edge {
                name: n.to_string(),
                kind: *k,
                range: Some(r.to_string()),
            })
            .collect(),
        publish: Default::default(),
        launcher: None,
    }
}

fn npm(name: &str, version: &str, edges: &[(&str, DepKind, &str)]) -> Package {
    pkg(name, PkgKind::Npm, version, edges)
}

fn model(packages: Vec<Package>) -> ReleaseModel {
    ReleaseModel {
        packages,
        versioning: Versioning::PerPackage,
        tag_format: "{name}@{version}".to_string(),
        environment: "release".to_string(),
    }
}

/// Every package tagged at its manifest version.
fn tagged(m: &ReleaseModel) -> History {
    let mut h = History::default();
    for p in &m.packages {
        h.last_tag
            .insert(p.name.clone(), Some(m.tag_for(p, &p.version.value)));
    }
    h
}

fn cs(id: &str, releases: &[(&str, CsType)]) -> Changeset {
    Changeset {
        id: id.to_string(),
        releases: releases.iter().map(|(n, t)| (n.to_string(), *t)).collect(),
        summary: format!("summary of {id}"),
    }
}

fn cfg(json: &str) -> CsConfig {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join(".changeset")).unwrap();
    std::fs::write(d.path().join(".changeset/config.json"), json).unwrap();
    read_config(d.path()).unwrap().unwrap()
}

fn plan(m: &ReleaseModel, sets: &[Changeset], c: &CsConfig) -> Vec<NextVersion> {
    plan_versions(m, &tagged(m), sets, c, &ReleasePins::default()).expect("plan")
}

fn err(m: &ReleaseModel, sets: &[Changeset], c: &CsConfig) -> String {
    match plan_versions(m, &tagged(m), sets, c, &ReleasePins::default()) {
        Err(e @ HoistError::Config { .. }) => e.to_string(),
        other => panic!("expected a config error, got {other:?}"),
    }
}

fn find<'a>(plan: &'a [NextVersion], name: &str) -> &'a NextVersion {
    plan.iter().find(|n| n.package == name).expect(name)
}

fn has(plan: &[NextVersion], name: &str) -> bool {
    plan.iter().any(|n| n.package == name)
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn highest_type_wins() {
    let m = model(vec![npm("a", "1.0.0", &[])]);
    let sets = [
        cs("brave-cat", &[("a", CsType::Patch)]),
        cs("calm-dog", &[("a", CsType::Minor)]),
    ];
    let p = plan(&m, &sets, &cfg("{}"));
    let a = find(&p, "a");
    assert_eq!(a.to, v("1.1.0"));
    assert_eq!(a.changesets, vec!["brave-cat", "calm-dog"]);
    assert_eq!(a.reason, "changesets: brave-cat, calm-dog");
}

#[test]
fn major_on_zero_goes_to_one() {
    let m = model(vec![npm("a", "0.4.1", &[])]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Major)])], &cfg("{}"));
    assert_eq!(find(&p, "a").to, v("1.0.0"));
}

#[test]
fn none_type_does_not_release() {
    let m = model(vec![npm("a", "1.0.0", &[])]);
    let p = plan(&m, &[cs("x", &[("a", CsType::None)])], &cfg("{}"));
    assert!(p.is_empty());
}

#[test]
fn dependent_in_range_not_bumped() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "^1.0.0")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Minor)])], &cfg("{}"));
    assert_eq!(find(&p, "a").to, v("1.1.0"));
    assert!(!has(&p, "b"));
}

#[test]
fn dependent_out_of_range_patch() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "~1.0.0")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Minor)])], &cfg("{}"));
    let b = find(&p, "b");
    assert_eq!(b.to, v("1.0.1"));
    assert!(b.reason.contains('a'), "{}", b.reason);
    assert_eq!(
        b.dep_updates,
        vec![("a".to_string(), "1.1.0".to_string(), DepKind::Normal)]
    );
    assert!(b.changesets.is_empty());
}

#[test]
fn dependents_always_bumps_in_range() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "^1.0.0")]),
    ]);
    let c = cfg(
        r#"{"___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH":{"updateInternalDependents":"always"}}"#,
    );
    let p = plan(&m, &[cs("x", &[("a", CsType::Minor)])], &c);
    assert_eq!(find(&p, "b").to, v("1.0.1"));
}

#[test]
fn chain_of_dependents_until_stable() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "1.0.0")]),
        npm("c", "1.0.0", &[("b", DepKind::Normal, "1.0.0")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Patch)])], &cfg("{}"));
    assert_eq!(find(&p, "c").to, v("1.0.1"));
}

#[test]
fn cargo_zero_caret_out_of_range() {
    let m = model(vec![
        pkg("core", PkgKind::Crate, "0.4.1", &[]),
        pkg(
            "app",
            PkgKind::Crate,
            "1.0.0",
            &[("core", DepKind::Normal, "0.4")],
        ),
    ]);
    let p = plan(&m, &[cs("x", &[("core", CsType::Minor)])], &cfg("{}"));
    assert_eq!(find(&p, "core").to, v("0.5.0"));
    let app = find(&p, "app");
    assert_eq!(app.to, v("1.0.1"));
    assert_eq!(
        app.dep_updates,
        vec![("core".to_string(), "0.5.0".to_string(), DepKind::Normal)]
    );
}

#[test]
fn dev_dependency_none_but_rewritten() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Dev, "1.0.0")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Patch)])], &cfg("{}"));
    assert!(!has(&p, "b"), "a dev edge gives no bump");
    let sets = [cs("x", &[("a", CsType::Patch), ("b", CsType::Patch)])];
    let p = plan(&m, &sets, &cfg("{}"));
    assert_eq!(
        find(&p, "b").dep_updates,
        vec![("a".to_string(), "1.0.1".to_string(), DepKind::Dev)]
    );
}

#[test]
fn in_range_rewrite_follows_update_internal_dependencies() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "^1.0.0")]),
    ]);
    let sets = [cs("x", &[("a", CsType::Patch), ("b", CsType::Patch)])];
    let p = plan(&m, &sets, &cfg("{}"));
    assert_eq!(find(&p, "b").dep_updates.len(), 1, "patch >= default patch");
    let p = plan(&m, &sets, &cfg(r#"{"updateInternalDependencies":"minor"}"#));
    assert!(
        find(&p, "b").dep_updates.is_empty(),
        "patch < minor, in range"
    );
}

#[test]
fn peer_minor_makes_major() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Peer, "^1.0.0")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Minor)])], &cfg("{}"));
    assert_eq!(find(&p, "b").to, v("2.0.0"));
    let only = cfg(
        r#"{"___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH":{"onlyUpdatePeerDependentsWhenOutOfRange":true}}"#,
    );
    let p = plan(&m, &[cs("x", &[("a", CsType::Minor)])], &only);
    assert!(!has(&p, "b"));
}

#[test]
fn fixed_group_all_release() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.2.0", &[]),
        npm("c", "1.0.0", &[]),
    ]);
    let p = plan(
        &m,
        &[cs("x", &[("a", CsType::Patch)])],
        &cfg(r#"{"fixed":[["a","b"]]}"#),
    );
    assert_eq!(find(&p, "a").to, v("1.2.1"));
    assert_eq!(find(&p, "b").to, v("1.2.1"));
    assert!(!has(&p, "c"));
}

#[test]
fn fixed_group_glob_and_unmatched_pattern() {
    let m = model(vec![npm("@s/a", "1.0.0", &[]), npm("@s/b", "1.0.0", &[])]);
    let p = plan(
        &m,
        &[cs("x", &[("@s/a", CsType::Minor)])],
        &cfg(r#"{"fixed":[["@s/*"]]}"#),
    );
    assert_eq!(find(&p, "@s/b").to, v("1.1.0"));
    let e = err(&m, &[], &cfg(r#"{"fixed":[["@t/*"]]}"#));
    assert!(e.contains("@t/*"), "{e}");
    let e = err(&m, &[], &cfg(r#"{"fixed":[["@s/a"]],"linked":[["@s/a"]]}"#));
    assert!(e.contains("@s/a"), "{e}");
}

#[test]
fn linked_only_releasing() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.3.0", &[]),
        npm("c", "2.0.0", &[]),
    ]);
    let sets = [cs("x", &[("a", CsType::Minor), ("b", CsType::Patch)])];
    let p = plan(&m, &sets, &cfg(r#"{"linked":[["a","b","c"]]}"#));
    assert_eq!(find(&p, "a").to, v("1.4.0"));
    assert_eq!(find(&p, "b").to, v("1.4.0"));
    assert!(!has(&p, "c"));
}

#[test]
fn hoist_fixed_versioning_is_one_group() {
    let mut m = model(vec![npm("a", "1.0.0", &[]), npm("b", "1.1.0", &[])]);
    m.versioning = Versioning::Fixed;
    let p = plan(&m, &[cs("x", &[("a", CsType::Patch)])], &cfg("{}"));
    assert_eq!(find(&p, "b").to, v("1.1.1"));
}

#[test]
fn hoist_fixed_plus_config_fixed_exit_3() {
    let mut m = model(vec![npm("a", "1.0.0", &[]), npm("b", "1.0.0", &[])]);
    m.versioning = Versioning::Fixed;
    let e = err(&m, &[], &cfg(r#"{"fixed":[["a","b"]]}"#));
    assert!(e.contains("versioning"), "{e}");
}

#[test]
fn ignored_mixed_changeset_exit_3() {
    let m = model(vec![npm("a", "1.0.0", &[]), npm("b", "1.0.0", &[])]);
    let c = cfg(r#"{"ignore":["b"]}"#);
    let e = err(
        &m,
        &[cs("mixed", &[("a", CsType::Patch), ("b", CsType::Patch)])],
        &c,
    );
    assert!(e.contains("mixed") && e.contains("ignore"), "{e}");
    let p = plan(&m, &[cs("only", &[("b", CsType::Patch)])], &c);
    assert!(p.is_empty(), "ignored packages never release");
}

#[test]
fn unknown_package_exit_3() {
    let m = model(vec![npm("a", "1.0.0", &[])]);
    let e = err(&m, &[cs("x", &[("zzz", CsType::Patch)])], &cfg("{}"));
    assert!(
        e.contains("zzz") && e.contains("not a releasable package") && e.contains("x.md"),
        "{e}"
    );
}

#[test]
fn same_name_crate_and_npm_exit_3() {
    let m = model(vec![
        npm("dup", "1.0.0", &[]),
        pkg("dup", PkgKind::Crate, "1.0.0", &[]),
    ]);
    let e = err(&m, &[], &cfg("{}"));
    assert!(e.contains("dup"), "{e}");
}

fn launcher_model() -> ReleaseModel {
    let mut l = npm("@s/tool", "0.3.0", &[]);
    l.launcher = Some(Launcher {
        crate_name: "tool".to_string(),
        bin: "tool".to_string(),
        platforms: vec![],
    });
    model(vec![pkg("tool", PkgKind::Crate, "0.3.0", &[]), l])
}

#[test]
fn launcher_follows_crate() {
    let m = launcher_model();
    let p = plan(&m, &[cs("x", &[("tool", CsType::Minor)])], &cfg("{}"));
    assert_eq!(find(&p, "tool").to, v("0.4.0"));
    assert_eq!(find(&p, "@s/tool").to, v("0.4.0"));
    let e = err(&m, &[cs("x", &[("@s/tool", CsType::Minor)])], &cfg("{}"));
    assert!(e.contains("follows") && e.contains("`tool`"), "{e}");
}

#[test]
fn pin_overrides_changeset() {
    let m = model(vec![npm("a", "1.0.0", &[]), npm("b", "1.0.0", &[])]);
    let pins =
        parse_pins("[packages.a]\nversion = \"3.0.0\"\n[packages.b]\nskip = true\n").expect("pins");
    let sets = [cs("x", &[("a", CsType::Patch), ("b", CsType::Minor)])];
    let p = plan_versions(&m, &tagged(&m), &sets, &cfg("{}"), &pins).unwrap();
    assert_eq!(find(&p, "a").to, v("3.0.0"));
    assert!(!has(&p, "b"), "skip wins");
}

#[test]
fn awaiting_publish_no_bump() {
    let m = model(vec![npm("a", "1.1.0", &[])]);
    let mut h = History::default();
    h.last_tag.insert("a".into(), Some("a@1.0.0".into()));
    let sets = [cs("x", &[("a", CsType::Minor)])];
    let p = plan_versions(&m, &h, &sets, &cfg("{}"), &ReleasePins::default()).unwrap();
    assert!(p.is_empty());
}

#[test]
fn untagged_package_is_a_first_release() {
    let m = model(vec![npm("a", "0.1.0", &[])]);
    let h = History {
        last_tag: HashMap::from([("a".to_string(), None)]),
        ..History::default()
    };
    let p = plan_versions(&m, &h, &[], &cfg("{}"), &ReleasePins::default()).unwrap();
    let a = find(&p, "a");
    assert!(a.first_release);
    assert_eq!(a.to, v("0.1.0"));
}

#[test]
fn no_changesets_nothing_releases() {
    let m = model(vec![npm("a", "1.0.0", &[])]);
    assert!(plan(&m, &[], &cfg("{}")).is_empty());
}

#[test]
fn workspace_star_dependent_gets_a_patch() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "workspace:*")]),
        npm("c", "1.0.0", &[("a", DepKind::Normal, "workspace:^")]),
    ]);
    let p = plan(&m, &[cs("x", &[("a", CsType::Major)])], &cfg("{}"));
    assert_eq!(find(&p, "b").to, v("1.0.1"));
    assert_eq!(find(&p, "c").to, v("1.0.1"));
    assert!(find(&p, "b").dep_updates.is_empty(), "workspace:* is kept");
}

#[test]
fn untagged_member_of_tagged_fixed_group_moves_with_it() {
    let mut m = model(vec![npm("a", "1.2.0", &[]), npm("b", "0.0.0", &[])]);
    m.versioning = Versioning::Fixed;
    let mut h = History::default();
    h.last_tag.insert("a".into(), Some("a@1.2.0".into()));
    h.last_tag.insert("b".into(), None);
    let sets = [cs("x", &[("a", CsType::Minor)])];
    let p = plan_versions(&m, &h, &sets, &cfg("{}"), &ReleasePins::default()).unwrap();
    assert_eq!(find(&p, "a").to, v("1.3.0"));
    assert_eq!(find(&p, "b").to, v("1.3.0"));
}

#[test]
fn pin_feeds_dependents() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "^1.0.0")]),
    ]);
    let pins = parse_pins("[packages.a]\nversion = \"2.0.0\"\n").expect("pins");
    let p = plan_versions(&m, &tagged(&m), &[], &cfg("{}"), &pins).unwrap();
    assert_eq!(find(&p, "a").to, v("2.0.0"));
    assert_eq!(find(&p, "a").reason, "pinned in .hoist/release.toml");
    let b = find(&p, "b");
    assert_eq!(b.to, v("1.0.1"));
    assert_eq!(
        b.dep_updates,
        vec![("a".to_string(), "2.0.0".to_string(), DepKind::Normal)]
    );
}

#[test]
fn wildcard_range_is_not_rewritten() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, "*")]),
    ]);
    let sets = [cs("x", &[("a", CsType::Patch), ("b", CsType::Patch)])];
    assert!(
        find(&plan(&m, &sets, &cfg("{}")), "b")
            .dep_updates
            .is_empty()
    );
}

#[test]
fn pin_moves_the_whole_fixed_group() {
    let mut m = model(vec![npm("a", "1.2.0", &[]), npm("b", "1.2.0", &[])]);
    m.versioning = Versioning::Fixed;
    let pins = parse_pins("[packages.a]\nversion = \"2.0.0\"\n").expect("pins");
    let p = plan_versions(&m, &tagged(&m), &[], &cfg("{}"), &pins).unwrap();
    assert_eq!(find(&p, "a").to, v("2.0.0"));
    assert_eq!(find(&p, "b").to, v("2.0.0"));
}

#[test]
fn dev_only_dependent_rewritten_without_release() {
    let m = model(vec![
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Dev, "^1.0.0")]),
        npm("c", "1.0.0", &[("a", DepKind::Dev, "~1.0.0")]),
        npm("d", "1.0.0", &[("a", DepKind::Dev, "^1.0.0")]),
    ]);
    let sets = [cs("x", &[("a", CsType::Minor)])];
    let mut c = cfg(r#"{"ignore":["d"]}"#);
    let (next, idle) =
        plan_with_rewrites(&m, &tagged(&m), &sets, &c, &ReleasePins::default()).unwrap();
    assert!(!has(&next, "b") && !has(&next, "c"));
    let want = |p: &str| {
        let (p, a, ver) = (p.to_string(), "a".to_string(), "1.1.0".to_string());
        (p, a, ver, DepKind::Dev)
    };
    assert_eq!(
        idle,
        vec![want("c")],
        "only out of range; ignored d untouched"
    );
    c.dependents_always = true;
    let (_, idle) =
        plan_with_rewrites(&m, &tagged(&m), &sets, &c, &ReleasePins::default()).unwrap();
    assert_eq!(
        idle,
        vec![want("b"), want("c")],
        "always: minor >= patch threshold"
    );
}

#[test]
fn multi_comparator_range_left_bumps_dependent() {
    let m = model(vec![
        pkg("core", PkgKind::Crate, "0.2.0", &[]),
        pkg(
            "app",
            PkgKind::Crate,
            "1.0.0",
            &[("core", DepKind::Normal, ">=0.1, <0.3")],
        ),
        npm("a", "1.0.0", &[]),
        npm("b", "1.0.0", &[("a", DepKind::Normal, ">=1.0.0 <1.1.0")]),
        npm("c", "1.0.0", &[("a", DepKind::Normal, "^1.0.0 || ^2.0.0")]),
    ]);
    let sets = [cs("x", &[("core", CsType::Minor), ("a", CsType::Minor)])];
    let p = plan(&m, &sets, &cfg("{}"));
    assert_eq!(find(&p, "app").to, v("1.0.1"));
    assert_eq!(find(&p, "b").to, v("1.0.1"));
    assert!(!has(&p, "c"), "1.1.0 stays in ^1 || ^2");
}

#[test]
fn changeset_naming_a_blocked_package_is_held() {
    // b awaits publish (manifest above its tag): `both` waits whole, c only takes `c-fix`.
    let m = model(vec![
        npm("b", "1.1.0", &[]),
        npm("c", "1.0.0", &[]),
        npm("d", "1.0.0", &[]),
    ]);
    let mut h = tagged(&m);
    h.last_tag.insert("b".into(), Some("b@1.0.0".into()));
    let sets = [
        cs("both", &[("b", CsType::Minor), ("c", CsType::Minor)]),
        cs("c-fix", &[("c", CsType::Patch)]),
        cs("d-only", &[("b", CsType::None), ("d", CsType::Patch)]),
    ];
    let p = plan_versions(&m, &h, &sets, &cfg("{}"), &ReleasePins::default()).unwrap();
    let c = find(&p, "c");
    assert_eq!(
        (c.to, c.changesets.clone()),
        (v("1.0.1"), vec!["c-fix".to_string()])
    );
    assert_eq!(
        find(&p, "d").changesets,
        vec!["d-only".to_string()],
        "none does not hold"
    );
    assert!(!has(&p, "b"));
}

#[test]
fn rewrites_are_per_dependency_kind() {
    let m = model(vec![
        npm("core", "1.0.0", &[]),
        npm(
            "app",
            "1.0.0",
            &[
                ("core", DepKind::Peer, "^1.0.0"),
                ("core", DepKind::Dev, "1.0.0"),
            ],
        ),
    ]);
    let sets = [cs("x", &[("core", CsType::Patch), ("app", CsType::Patch)])];
    let p = plan(&m, &sets, &cfg(r#"{"updateInternalDependencies":"minor"}"#));
    let want = vec![("core".to_string(), "1.0.1".to_string(), DepKind::Dev)];
    assert_eq!(
        find(&p, "app").dep_updates,
        want,
        "peer ^1.0.0 stays in range"
    );
}
