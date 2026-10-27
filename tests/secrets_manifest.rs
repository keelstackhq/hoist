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
fn manifest_bad_ref_syntax_names_the_line() {
    let text = MANIFEST.replace("site:prod/sentry.sops.yaml#token", "site/prod/sentry.yaml");
    let msg = manifest_err(&text);
    assert!(msg.contains("line 12"), "{msg}");
    assert!(msg.contains("<source>:<path>#<key>"), "{msg}");
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
fn manifest_unknown_source_and_unknown_key_fail() {
    let msg = manifest_err(&MANIFEST.replace("\"site:prod/sentry", "\"nope:prod/sentry"));
    assert!(
        msg.contains("unknown source `nope`") && msg.contains("line 12"),
        "{msg}"
    );
    let msg = manifest_err(&MANIFEST.replace("version = 1", "version = 1\nextra = true"));
    assert!(msg.contains("extra"), "{msg}");
    let msg = manifest_err(&MANIFEST.replace("version = 1", "version = 2"));
    assert!(msg.contains("version"), "{msg}");
}

#[test]
fn manifest_same_var_in_values_and_files_fails() {
    let text = MANIFEST.replace("GCP_CREDENTIALS_FILE", "SENTRY_AUTH_TOKEN");
    assert!(manifest_err(&text).contains("SENTRY_AUTH_TOKEN"));
}

#[test]
fn manifest_errors_never_echo_a_pasted_value() {
    let text = MANIFEST.replace(
        "\"site:prod/sentry.sops.yaml#token\"",
        "\"ghp_PASTEDVALUE123\"",
    );
    let msg = manifest_err(&text);
    assert!(!msg.contains("PASTEDVALUE"), "{msg}");
    assert!(
        msg.contains("line 12") && msg.contains("SENTRY_AUTH_TOKEN"),
        "{msg}"
    );
    // toml's own errors (wrong type) must not quote the value or the line either.
    let text = MANIFEST.replace("[env.prod.files]\n", "").replace(
        "GCP_CREDENTIALS_FILE = \"site:prod/gcp.sops.yaml#service_account\"",
        "files = \"ghp_PASTEDVALUE123\"",
    );
    let msg = manifest_err(&text);
    assert!(!msg.contains("PASTEDVALUE"), "{msg}");
    assert!(msg.contains("line 14"), "{msg}");
}

#[test]
fn manifest_ref_errors_name_the_bad_part() {
    let bad = |r: &str| manifest_err(&MANIFEST.replace("site:prod/sentry.sops.yaml#token", r));
    assert!(bad("site:prod/sentry.sops.yaml#").contains("key"));
    assert!(bad("site:Prod/sentry.sops.yaml#token").contains("<env>/<bundle>.sops.yaml"));
    assert!(bad("site:prod/sentry.yaml#token").contains("<env>/<bundle>.sops.yaml"));
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
fn manifest_rejects_option_like_refs_repos_and_names() {
    for (from, to) in [
        ("ref = \"main\"", "ref = \"-x\""),
        ("ref = \"main\"", "ref = \"a.lock\""),
        ("ref = \"main\"", "ref = \"a//b\""),
        ("ref = \"main\"", "ref = \"a/../b\""),
        ("keelstackhq/secrets-shared", "-o/r"),
        ("keelstackhq/secrets-shared", "o/.."),
        ("[env.preview]", "[env.my_env]"),
        ("shared = {", "Shared = {"),
    ] {
        let msg = manifest_err(&MANIFEST.replace(from, to));
        assert!(msg.contains("line "), "{to}: {msg}");
    }
}
