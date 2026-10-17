use hoist::config::{Managed, load_root, load_target};

fn dir_with(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hoist.toml"), content).unwrap();
    dir
}

#[test]
fn missing_file_gives_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = load_root(dir.path()).unwrap();
    assert_eq!(cfg.github.actions_permissions, "read");
    assert_eq!(cfg.github.managed, Managed::Hoist);
    assert!(!cfg.github.merge_queue);
    assert!(cfg.github.allow_auto_merge);
    assert_eq!(
        cfg.release.versioning,
        hoist::config::Versioning::PerPackage
    );
    assert_eq!(cfg.release.environment, "release");
}

#[test]
fn unknown_key_is_error() {
    let dir = dir_with("[github]\nfoo = 1\n");
    let msg = load_root(dir.path()).unwrap_err().to_string();
    assert!(msg.contains("hoist.toml"), "{msg}");
    assert!(msg.contains("github.foo"), "{msg}");
}

#[test]
fn unknown_section_is_error() {
    let dir = dir_with("[nope]\n");
    let msg = load_root(dir.path()).unwrap_err().to_string();
    assert!(msg.contains("hoist.toml") && msg.contains("nope"), "{msg}");
}

#[test]
fn invalid_toml_names_line() {
    let dir = dir_with("[github");
    let msg = load_root(dir.path()).unwrap_err().to_string();
    assert!(msg.contains("hoist.toml"), "{msg}");
    assert!(msg.contains("line 1"), "{msg}");
}

#[test]
fn target_deploy_parses() {
    let dir = dir_with("[deploy]\nprovider=\"cloudflare-pages\"\nproject=\"keelstack-landing\"\n");
    let cfg = load_target(dir.path()).unwrap();
    let deploy = cfg.deploy.unwrap();
    assert_eq!(deploy.provider.as_deref(), Some("cloudflare-pages"));
    assert_eq!(deploy.project.as_deref(), Some("keelstack-landing"));
    assert_eq!(deploy.on, "push");
    assert!(cfg.publish.is_none());
}

#[test]
fn managed_external_parses() {
    let dir = dir_with("[github]\nmanaged = \"external\"\n");
    assert_eq!(
        load_root(dir.path()).unwrap().github.managed,
        Managed::External
    );
}
