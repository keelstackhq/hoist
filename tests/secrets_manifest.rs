//! The secrets.toml model (design §3.3).

use hoist::secrets::manifest::Manifest;

const MANIFEST: &str = r#"version = 1

[sources]
shared = { repo = "keelstackhq/secrets-shared" }
site   = { repo = "keelstackhq/secrets-keelstack", ref = "main" }

[env.preview]
CLOUDFLARE_API_TOKEN = "shared:preview/cloudflare.sops.yaml#CLOUDFLARE_API_TOKEN"

[env.prod]
CLOUDFLARE_API_TOKEN = "shared:prod/cloudflare.sops.yaml#CLOUDFLARE_API_TOKEN"
SENTRY_AUTH_TOKEN    = "site:prod/sentry.sops.yaml#token"

[env.prod.files]
GCP_CREDENTIALS_FILE = "site:prod/gcp.sops.yaml#service_account"
"#;

fn manifest_err(text: &str) -> String {
    let err = Manifest::parse(text, "secrets.toml").expect_err("must fail");
    assert_eq!(err.exit_code(), 3);
    err.to_string()
}

#[test]
fn manifest_parses_the_spec_example() {
    let m = Manifest::parse(MANIFEST, "secrets.toml").expect("valid");
    assert_eq!(m.sources["shared"].repo, "keelstackhq/secrets-shared");
    assert_eq!(m.sources["shared"].git_ref, "main");
    let prod = &m.env["prod"];
    let sentry = &prod.vars["SENTRY_AUTH_TOKEN"];
    assert_eq!(
        (
            sentry.source.as_str(),
            sentry.path.as_str(),
            sentry.key.as_str()
        ),
        ("site", "prod/sentry.sops.yaml", "token")
    );
    assert_eq!(prod.files["GCP_CREDENTIALS_FILE"].key, "service_account");
    assert!(m.env["preview"].files.is_empty());
}

#[test]
fn manifest_bad_env_var_name_names_the_line() {
    let text = MANIFEST.replace("SENTRY_AUTH_TOKEN    =", "sentry-token =");
    let msg = manifest_err(&text);
    assert!(msg.starts_with("secrets.toml: "), "{msg}");
    assert!(msg.contains("line 12"), "{msg}");
    assert!(msg.contains("sentry-token"), "{msg}");
}

#[test]
fn manifest_path_outside_env_names_the_line() {
    let text = MANIFEST.replace(
        "shared:preview/cloudflare.sops.yaml",
        "shared:prod/cloudflare.sops.yaml",
    );
    let msg = manifest_err(&text);
    assert!(msg.contains("line 8"), "{msg}");
    assert!(msg.contains("must start with `preview/`"), "{msg}");
}

#[test]
fn manifest_errors_never_echo_a_value() {
    let gcp =
        "[env.prod.files]\nGCP_CREDENTIALS_FILE = \"site:prod/gcp.sops.yaml#service_account\"";
    let positions = [
        ("version = 1", "version = "),
        ("\"keelstackhq/secrets-shared\"", ""),
        ("\"main\"", ""),
        ("\"site:prod/sentry.sops.yaml#token\"", ""),
        (gcp, "files = "),
    ];
    let values = [
        r#""ghp_MARK\"q'1""#,
        "9876543217",
        "98765.43217",
        "true",
        "'ab\"MARK1'",
    ];
    for (from, prefix) in positions {
        for value in values {
            let msg = manifest_err(&MANIFEST.replacen(from, &format!("{prefix}{value}"), 1));
            for marker in ["MARK", "98765", "true"] {
                assert!(!msg.contains(marker), "{prefix}{value}: {msg}");
            }
            assert!(msg.contains("line "), "{prefix}{value}: {msg}");
        }
    }
}

#[test]
fn manifest_inline_files_and_explicit_ref() {
    let text = MANIFEST
        .replace("[env.prod.files]\n", "")
        .replace(
            "GCP_CREDENTIALS_FILE = \"site:prod/gcp.sops.yaml#service_account\"",
            "files = { GCP_CREDENTIALS_FILE = \"site:prod/gcp.sops.yaml#service_account\" }",
        )
        .replace("ref = \"main\"", "ref = \"release/v1\"");
    let m = Manifest::parse(&text, "secrets.toml").expect("valid");
    assert!(m.env["prod"].files.contains_key("GCP_CREDENTIALS_FILE"));
    assert_eq!(m.sources["site"].git_ref, "release/v1");
}

#[test]
fn manifest_rejects_bad_names_refs_and_keys() {
    for (from, to) in [
        ("ref = \"main\"", "ref = \"-x\""),
        ("ref = \"main\"", "ref = \"a.lock/b\""),
        ("site:prod/sentry.sops.yaml", "site:prod/..sops.yaml"),
        ("ref = \"main\"", "ref = \"a//b\""),
        ("ref = \"main\"", "ref = \"a/../b\""),
        ("keelstackhq/secrets-shared", "-o/r"),
        ("keelstackhq/secrets-shared", "o/.."),
        ("[env.preview]", "[env.my_env]"),
        ("shared = {", "Shared = {"),
        ("\"site:prod/sentry", "\"nope:prod/sentry"),
        ("version = 1", "version = 2"),
        ("version = 1", "version = 1\nextra = true"),
        ("GCP_CREDENTIALS_FILE", "SENTRY_AUTH_TOKEN"),
    ] {
        let msg = manifest_err(&MANIFEST.replace(from, to));
        assert!(msg.contains("line "), "{to}: {msg}");
    }
}
