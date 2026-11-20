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

#[test]
fn environment_tables_load_with_defaults() {
    let dir = dir_with(
        "[github.environments.production]\nbranches = [\"main\", \"release/*\"]\n[github.environments.splice-bot]\nmanage = false\n[github.environments.open]\nbranches = []\n",
    );
    let cfg = load_root(dir.path()).unwrap();
    let envs = &cfg.github.environments;
    assert_eq!(
        envs["production"].branches.as_deref(),
        Some(&["main".to_string(), "release/*".to_string()][..])
    );
    assert!(envs["production"].manage);
    assert!(!envs["splice-bot"].manage);
    assert_eq!(envs["splice-bot"].branches, None);
    assert_eq!(envs["open"].branches.as_deref(), Some(&[][..]));
}

#[test]
fn an_unknown_environment_key_names_its_path() {
    let dir = dir_with("[github.environments.production]\nbranch = \"main\"\n");
    let msg = load_root(dir.path()).unwrap_err().to_string();
    assert!(msg.contains("hoist.toml"), "{msg}");
    assert!(
        msg.contains("github.environments.production.branch"),
        "{msg}"
    );
}

fn variable_error(toml: &str) -> String {
    load_root(dir_with(toml).path()).unwrap_err().to_string()
}

#[test]
fn variable_names_are_validated_at_load() {
    for (toml, key) in [
        ("[github.variables]\n\"BAD-NAME\" = \"x\"\n", "BAD-NAME"),
        ("[github.variables]\n\"1ST\" = \"x\"\n", "1ST"),
        ("[github.variables]\ngithub_sha = \"x\"\n", "github_sha"),
        ("[github.variables]\nAPI = \"x\"\napi = \"y\"\n", "api"),
        (
            "[github]\noptional_variables = [\"bad name\"]\n",
            "bad name",
        ),
        (
            "[github]\noptional_variables = [\"GITHUB_X\"]\n",
            "GITHUB_X",
        ),
    ] {
        let msg = variable_error(toml);
        assert!(
            msg.contains("hoist.toml") && msg.contains(key),
            "{toml}: {msg}"
        );
    }
    let ok = load_root(
        dir_with("[github.variables]\nAPI_URL = \"x\"\n_x1 = \"y\"\n[github]\noptional_variables = [\"REGION\"]\n").path(),
    );
    assert!(ok.is_ok());
}
