mod common;

use common::{fixture_repo, git, hoist};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

/// release-ws with b at 1.0.0, committed and tagged a@1.2.0, b@1.0.0, tool@0.3.0.
fn tagged_repo() -> TempDir {
    let repo = fixture_repo("release-ws", "https://github.com/acme/x.git");
    let d = repo.path();
    write(
        d,
        "packages/b/package.json",
        "{\n  \"name\": \"b\",\n  \"version\": \"1.0.0\",\n  \"private\": false\n}\n",
    );
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "chore: init"]);
    for t in ["a@1.2.0", "b@1.0.0", "tool@0.3.0"] {
        git(d, &["tag", t]);
    }
    repo
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn commit_in(d: &Path, rel: &str, msg: &str) {
    write(d, rel, &format!("{msg}\n"));
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", msg]);
}

fn init(d: &Path, extra: &[&str]) -> std::process::Output {
    hoist()
        .args(["-C", d.to_str().unwrap(), "--json", "changeset", "init"])
        .args(extra)
        .output()
        .expect("run hoist")
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn migrate(d: &Path) -> String {
    std::fs::read_to_string(d.join(".changeset/hoist-migrate.md")).expect("hoist-migrate.md")
}

/// `release plan --json` items as (key, reason-prefix "from -> to") for releasing packages.
fn plan_versions(d: &Path) -> Vec<(String, String)> {
    let o = hoist()
        .args(["-C", d.to_str().unwrap(), "--json", "release", "plan"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == "create")
        .map(|i| {
            let r = i["reason"].as_str().unwrap();
            (
                i["key"].as_str().unwrap().to_string(),
                r.split(' ').take(3).collect::<Vec<_>>().join(" "),
            )
        })
        .collect()
}

#[test]
fn from_commits_writes_pending_bumps() {
    let repo = tagged_repo();
    let d = repo.path();
    commit_in(d, "packages/b/x.ts", "feat: add thing");
    commit_in(d, "packages/b/y.ts", "fix: stop crash");
    commit_in(d, "packages/a/z.ts", "fix(a): tidy");
    // Below 1.0, a breaking change is a minor bump (§9.1).
    commit_in(d, "crates/tool/w.rs", "feat!: new flags");
    let o = init(d, &["--from-commits"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let text = migrate(d);
    let (front, summary) = text.split_once("\n---\n").unwrap();
    assert_eq!(front, "---\n\"a\": patch\n\"b\": minor\n\"tool\": minor");
    assert_eq!(
        summary, "\nnew flags\nadd thing\ntidy\nstop crash\n",
        "{text}"
    );
    assert!(d.join(".changeset/config.json").exists());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let i = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "hoist-migrate")
        .expect("migrate item");
    assert_eq!(i["kind"], "changeset");
    assert_eq!(i["action"], "create");
    assert_eq!(i["source"], ".changeset/hoist-migrate.md");
}

#[test]
fn from_commits_breaking_at_1_0_is_major() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/x.ts", "fix!: drop old api");
    let o = init(repo.path(), &["--from-commits"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(migrate(repo.path()).starts_with("---\n\"b\": major\n---\n"));
}

#[test]
fn from_commits_nothing_pending_writes_no_changeset() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/notes.md", "docs: notes");
    let o = init(repo.path(), &["--from-commits"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(repo.path().join(".changeset/config.json").exists());
    assert!(!repo.path().join(".changeset/hoist-migrate.md").exists());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["key"] == "hoist-migrate" && i["action"] == "ok")
    );
}

#[test]
fn plain_init_writes_no_changeset() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/x.ts", "feat: add thing");
    assert_eq!(init(repo.path(), &[]).status.code(), Some(0));
    assert!(!repo.path().join(".changeset/hoist-migrate.md").exists());
}

#[test]
fn from_commits_shallow_clone_exit_3_and_writes_nothing() {
    let repo = tagged_repo();
    commit_in(repo.path(), "packages/b/x.ts", "feat: add thing");
    let clone = TempDir::new().unwrap();
    let dst = clone.path().join("s");
    git(
        clone.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", repo.path().display()),
            dst.to_str().unwrap(),
        ],
    );
    let o = init(&dst, &["--from-commits"]);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(!dst.join(".changeset").exists());
}

#[test]
fn then_plan_gives_same_versions() {
    // Commits mode before == the versions the written types give. (The changesets-mode planner
    // is CS-3; until it lands this applies the written types with semver by hand.)
    let repo = tagged_repo();
    let d = repo.path();
    commit_in(d, "packages/b/x.ts", "feat: add thing");
    commit_in(d, "packages/a/z.ts", "fix: tidy");
    commit_in(d, "crates/tool/w.rs", "feat!: new flags");
    let before = plan_versions(d);
    assert_eq!(init(d, &["--from-commits"]).status.code(), Some(0));
    let text = migrate(d);
    let cs = hoist::changeset::parse("hoist-migrate", &text).unwrap();
    let base = [("a", "1.2.0"), ("b", "1.0.0"), ("tool", "0.3.0")];
    let mut after: Vec<(String, String)> = Vec::new();
    for (name, ty) in &cs.releases {
        let from = base.iter().find(|(n, _)| n == name).unwrap().1;
        let v = hoist::release::semver::Version::parse(from).unwrap();
        let bump = match ty {
            hoist::changeset::CsType::Major => hoist::release::commits::Bump::Major,
            hoist::changeset::CsType::Minor => hoist::release::commits::Bump::Minor,
            hoist::changeset::CsType::Patch => hoist::release::commits::Bump::Patch,
            hoist::changeset::CsType::None => hoist::release::commits::Bump::None,
        };
        after.push((name.clone(), format!("{from} -> {}", v.bump(bump))));
    }
    // Plan lists dependents too (a depends on b); compare the packages with own commits.
    for (name, change) in &after {
        assert!(
            before.iter().any(|(n, c)| n == name && c == change),
            "{name}: {change} not in {before:?}"
        );
    }
    assert_eq!(after.len(), 3);
}
