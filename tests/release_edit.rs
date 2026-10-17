use hoist::release::commits::{ChangeEntry, Section};
use hoist::release::edit::{
    changelog_section, prepend_changelog, set_cargo_dep_version, set_cargo_version,
    set_package_json_dep, set_package_json_version, today,
};
use hoist::release::planner::NextVersion;
use hoist::release::semver::Version;

fn nv(to: &str, entries: Vec<ChangeEntry>, first: bool) -> NextVersion {
    NextVersion {
        package: "pkg".to_string(),
        from: Version::parse("1.0.0").unwrap(),
        to: Version::parse(to).unwrap(),
        reason: String::new(),
        entries,
        first_release: first,
    }
}

fn entry(section: Section, text: &str, sha: &str) -> ChangeEntry {
    ChangeEntry {
        section,
        text: text.to_string(),
        short_sha: sha.to_string(),
    }
}

#[test]
fn json_version_edit_preserves_bytes() {
    let input = "{\r\n\t\"name\": \"caf\u{e9}-\u{1f980}\",\r\n\t\"engines\": {\"version\": \"9.9.9\", \"node\": \">=24\"},\r\n\t\"version\" :  \"1.0.0\" ,\r\n\t\"note\": \"\\\"version\\\": \\\"1.0.0\\\"\"\r\n}\r\n";
    let out = set_package_json_version(input, "1.1.0").unwrap();
    assert_eq!(out, input.replacen("\"1.0.0\" ,", "\"1.1.0\" ,", 1));
}

#[test]
fn json_without_version_errors() {
    let input = "{\"name\": \"a\", \"engines\": {\"version\": \"1.0.0\"}}";
    assert!(set_package_json_version(input, "2.0.0").is_err());
}

#[test]
fn json_dep_edit_skips_workspace_protocol() {
    let input = "{\n\t\"dependencies\": {\n\t\t\"a\": \"^1.0.0\",\n\t\t\"b\": \"workspace:*\"\n\t},\n\t\"devDependencies\": {\"a\": \"^1.0.0\"}\n}\n";
    let out = set_package_json_dep(input, "a", "^1.1.0").unwrap();
    assert_eq!(
        out,
        input.replacen("\"a\": \"^1.0.0\",", "\"a\": \"^1.1.0\",", 1)
    );
    assert_eq!(set_package_json_dep(input, "b", "^2.0.0").unwrap(), input);
    assert!(set_package_json_dep(input, "zzz", "^2.0.0").is_err());
}

#[test]
fn cargo_edit_keeps_comments() {
    let input = "# top\n[package]\nname = \"x\" # the name\nversion = \"1.0.0\" # bump me\n\n[dependencies]\nserde = \"1\"\n";
    let out = set_cargo_version(input, "1.2.0").unwrap();
    assert_eq!(out, input.replace("\"1.0.0\"", "\"1.2.0\""));
    let plain = set_cargo_version("[dependencies]\n", "1.0.0").unwrap_err();
    assert!(!plain.contains("inherited"));
    // inherited: only a distinct error, the root file is never touched here
    let inherited = "[package]\nname = \"x\"\nversion.workspace = true\n\n[workspace.package]\nversion = \"1.0.0\"\n";
    let err = set_cargo_version(inherited, "2.0.0").unwrap_err();
    assert!(err.contains("inherited from the workspace") && err.contains("root Cargo.toml"));
}

#[test]
fn cargo_path_dep_version_updated() {
    let input = "[package]\nname = \"x\"\nversion = \"1.0.0\"\n\n[dependencies]\ncore = { path = \"../core\", version = \"1.0.0\" } # keep\nserde = { version = \"1.0.0\" }\n";
    let out = set_cargo_dep_version(input, "core", "1.0.1").unwrap();
    assert_eq!(
        out,
        input.replace(
            "path = \"../core\", version = \"1.0.0\"",
            "path = \"../core\", version = \"1.0.1\""
        )
    );
    assert!(set_cargo_dep_version(input, "serde", "2.0.0").is_err());
}

#[test]
fn changelog_sections_and_order() {
    let n = nv(
        "2.0.0",
        vec![
            entry(Section::Fixes, "fix a", "aaaaaaa"),
            entry(Section::Features, "add b", "bbbbbbb"),
            entry(Section::Breaking, "drop c", "ccccccc"),
        ],
        false,
    );
    assert_eq!(
        prepend_changelog(None, &n, "2026-10-17"),
        "# Changelog\n\n## 2.0.0 (2026-10-17)\n\n### Breaking changes\n\n- drop c (ccccccc)\n\n### Features\n\n- add b (bbbbbbb)\n\n### Fixes\n\n- fix a (aaaaaaa)\n"
    );
    let only_fix = nv("1.0.1", vec![entry(Section::Fixes, "x", "1234567")], false);
    let out = prepend_changelog(None, &only_fix, "2026-10-17");
    assert!(!out.contains("### Features") && !out.contains("### Breaking"));
}

#[test]
fn changelog_first_release_line() {
    let out = prepend_changelog(None, &nv("1.0.0", vec![], true), "2026-10-17");
    assert_eq!(
        out,
        "# Changelog\n\n## 1.0.0 (2026-10-17)\n\n- First release.\n"
    );
}

#[test]
fn changelog_prepends_above_previous() {
    let n = nv(
        "1.1.0",
        vec![entry(Section::Features, "new", "abcdef0")],
        false,
    );
    let first = prepend_changelog(None, &n, "2026-10-17");
    let n2 = nv(
        "1.2.0",
        vec![entry(Section::Fixes, "bug", "abcdef1")],
        false,
    );
    let second = prepend_changelog(Some(&first), &n2, "2026-10-18");
    assert_eq!(
        second,
        "# Changelog\n\n## 1.2.0 (2026-10-18)\n\n### Fixes\n\n- bug (abcdef1)\n\n## 1.1.0 (2026-10-17)\n\n### Features\n\n- new (abcdef0)\n"
    );
    // no header in the old file; CRLF kept
    let crlf = prepend_changelog(Some("## 1.0.0 (x)\r\n\r\n- old\r\n"), &n, "2026-10-17");
    assert!(crlf.starts_with("# Changelog\r\n\r\n## 1.1.0 (2026-10-17)\r\n"));
    assert!(crlf.ends_with("## 1.0.0 (x)\r\n\r\n- old\r\n"));
    assert!(!crlf.replace("\r\n", "").contains('\n'));
}

#[test]
fn today_has_date_shape() {
    let t = today();
    assert_eq!(t.len(), 10);
    assert_eq!(&t[4..5], "-");
}

#[test]
fn malformed_json_errors_instead_of_hanging() {
    for bad in [
        "{\"version\":\"1.0.0\",\"files\":[}",
        "{\"version\":\"1.0.0\"",
        "{\"version\":\"1.0.0\",}",
        "{\"version\":\"1.0",
        "{\"version\":\"1.0.0\"}}",
        "{\"version\":\"1.0.0\"} x",
        "[\"version\"]",
        "{\"version\":\"1.0.0\",\"version\":\"2.0.0\"}",
        "{\"a\":[1 2]}",
    ] {
        assert!(set_package_json_version(bad, "2.0.0").is_err(), "{bad}");
        assert!(set_package_json_dep(bad, "a", "1").is_err(), "{bad}");
    }
}

#[test]
fn cargo_dep_matches_renamed_package_and_workspace_table() {
    let input = "[workspace.dependencies]\ncore = { path = \"core\", version = \"1.0.0\" }\n\n[dependencies]\nother = { path = \"../o\", version = \"1.0.0\" }\nalias = { package = \"core\", path = \"../c\", version = \"1.0.0\" }\n";
    let out = set_cargo_dep_version(input, "core", "1.0.1").unwrap();
    assert_eq!(out.matches("\"1.0.1\"").count(), 2);
    assert!(out.contains("other = { path = \"../o\", version = \"1.0.0\" }"));
}

#[test]
fn changelog_dependents_only_and_preamble() {
    let out = prepend_changelog(None, &nv("1.0.1", vec![], false), "2026-10-17");
    assert_eq!(
        out,
        "# Changelog\n\n## 1.0.1 (2026-10-17)\n\n- Dependency updates.\n"
    );
    let old = "# Changelog\n\nAll notable changes.\n\n## 1.0.0 (x)\n\n- old\n";
    let out = prepend_changelog(Some(old), &nv("1.0.1", vec![], false), "2026-10-17");
    assert_eq!(
        out,
        "# Changelog\n\nAll notable changes.\n\n## 1.0.1 (2026-10-17)\n\n- Dependency updates.\n\n## 1.0.0 (x)\n\n- old\n"
    );
    // a heading that merely starts with the words is not the header
    let out = prepend_changelog(Some("# Changelogs\n"), &nv("1.0.1", vec![], false), "d");
    assert!(out.starts_with("# Changelog\n\n# Changelogs"));
}

#[test]
fn changelog_section_is_the_body_of_that_version() {
    let text = "# Changelog\n\n## 1.1.0 (2026-10-17)\n\n### Features\n\n- a (abc1234)\n\n## 1.0.0 (2026-10-01)\n\n- First release.\n";
    assert_eq!(
        changelog_section(text, "1.1.0").as_deref(),
        Some("### Features\n\n- a (abc1234)")
    );
    assert_eq!(
        changelog_section(text, "1.0.0").as_deref(),
        Some("- First release.")
    );
    assert_eq!(changelog_section(text, "1.0.1"), None);
    // `## 1.0.0 (` must match exactly: 11.0.0 is another version.
    assert_eq!(changelog_section("## 11.0.0 (x)\n\n- y\n", "1.0.0"), None);
    let crlf = "# Changelog\r\n\r\n## 2.0.0 (d)\r\n\r\n- b (1)\r\n\r\n## 1.0.0 (d)\r\n";
    assert_eq!(changelog_section(crlf, "2.0.0").as_deref(), Some("- b (1)"));
    assert_eq!(changelog_section("## 3.0.0 (d)\n\n", "3.0.0"), None);
}
