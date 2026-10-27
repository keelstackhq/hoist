//! sops file metadata, read without decrypting (hoist spec §15.4, `secrets check`).

use hoist::secrets::sopsfile::{SopsFile, parse};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `tool` with an empty environment except PATH, and HOME/XDG_CONFIG_HOME inside `home`, so no
/// SOPS_* or age setting of the machine can reach the fixtures.
fn clean(tool: &str, home: &Path) -> Command {
    let mut cmd = Command::new(tool);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"));
    cmd
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/secrets/sopsfile")
}

fn ok_text() -> String {
    std::fs::read_to_string(fixtures().join("ok.sops.yaml")).expect("fixture")
}

/// Regenerates the fixtures with two throwaway age keys made in a temp dir and dropped with
/// it: only ciphertext and public keys are written to the repo.
#[test]
#[ignore = "regenerates fixtures; needs sops 3.13.3 and age-keygen on PATH"]
fn generate_fixtures() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut recipients = Vec::new();
    for i in 0..2 {
        let key = tmp.path().join(format!("k{i}.txt"));
        let made = clean("age-keygen", tmp.path()).arg("-o").arg(&key).output();
        assert!(made.expect("age-keygen on PATH").status.success());
        let public = clean("age-keygen", tmp.path())
            .arg("-y")
            .arg(&key)
            .output()
            .unwrap();
        recipients.push(String::from_utf8(public.stdout).unwrap().trim().to_string());
    }
    // Its own config, so no `.sops.yaml` up the tree can change what sops encrypts.
    let config = tmp.path().join("test.sops.yaml");
    let rule = format!("  - path_regex: '.*'\n    age: {}\n", recipients.join(","));
    std::fs::write(&config, format!("creation_rules:\n{rule}")).unwrap();
    let plain = tmp.path().join("plain.yaml");
    let text = "API_TOKEN: hoist-test-value-1\nmulti: |\n  hoist-test-value-2\n  second line\n\
                port_unencrypted: \"8080\"\n";
    std::fs::write(&plain, text).unwrap();
    let out = clean("sops", tmp.path())
        .current_dir(tmp.path())
        .arg("--config")
        .arg(&config)
        .args(["encrypt", "--input-type", "yaml", "--output-type", "yaml"])
        .args(["--unencrypted-suffix", "_unencrypted"])
        .arg(&plain)
        .output()
        .expect("sops on PATH");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::create_dir_all(fixtures()).unwrap();
    std::fs::write(fixtures().join("ok.sops.yaml"), out.stdout).unwrap();
    recipients.sort();
    std::fs::write(
        fixtures().join("recipients.txt"),
        recipients.join("\n") + "\n",
    )
    .unwrap();
}

#[test]
fn reads_keys_and_recipients_without_decrypting() {
    let f: SopsFile = parse(&ok_text()).expect("valid sops file");
    assert_eq!(f.keys, ["API_TOKEN", "multi", "port_unencrypted"]);
    let want = std::fs::read_to_string(fixtures().join("recipients.txt")).unwrap();
    assert_eq!(
        f.recipients.iter().cloned().collect::<Vec<_>>(),
        want.lines().collect::<Vec<_>>()
    );
    assert!(f.plaintext.is_empty(), "{:?}", f.plaintext);
}

#[test]
fn plaintext_values_are_listed_by_key_only() {
    let text = ok_text();
    let line = text.lines().find(|l| l.starts_with("API_TOKEN:")).unwrap();
    let bad_type = line
        .replace("API_TOKEN: ", "")
        .replace("type:str]", "type:zzz]");
    for value in [
        "hoist-test-value-MARK",
        "98765",
        "true",
        "\"ENC[AES256_GCM,MARK\"",
        "\"ENC[AES256_GCM,data:aHVudGVyMg==,iv:x,tag:x,type:zzz]\"", // base64 "hunter2"
        &bad_type,
    ] {
        let f = parse(&text.replace(line, &format!("API_TOKEN: {value}"))).unwrap();
        assert_eq!(f.plaintext, ["API_TOKEN"], "{value}");
        assert!(!format!("{f:?}").contains("MARK") && !format!("{f:?}").contains("98765"));
    }
}

#[test]
fn broken_files_are_errors_that_never_quote_a_value() {
    let text = ok_text();
    let mac = text
        .lines()
        .find(|l| l.trim_start().starts_with("mac:"))
        .unwrap();
    let age = text.find("    age:\n").unwrap();
    let after_age = text[age..].find("    lastmodified:").unwrap() + age;
    let cases = [
        (text.replace(mac, "    mac: MARK"), "no sops MAC"),
        (text.replace(mac, ""), "no sops MAC"),
        (
            format!("{}{}", &text[..age], &text[after_age..]),
            "no age recipients",
        ),
        ("API_TOKEN: MARK\n".to_string(), "no sops metadata"),
        (
            format!("nested:\n  a: MARK\n{text}"),
            "`nested` is not a flat value",
        ),
        ("a: [MARK\n".to_string(), "not valid YAML"),
        ("- MARK\n".to_string(), "not a YAML mapping"),
        (
            text.replace("sops:\n", "sops:\n    pgp:\n        - fp: MARK\n"),
            "`sops.pgp`",
        ),
        (
            text.replace("sops:\n", "sops:\n    kms:\n        - arn: MARK\n"),
            "`sops.kms`",
        ),
        (
            text.replace("sops:\n", "sops:\n    encrypted_regex: MARK\n"),
            "`sops.encrypted_regex`",
        ),
        (
            text.replace("    unencrypted_suffix: _unencrypted\n", ""),
            "unencrypted_suffix",
        ),
        (
            text.replacen("          recipient: ", "          x: ", 1),
            "`sops.age` entry 1",
        ),
        (
            text.replacen("          recipient: ", "          recipent: ", 1),
            "`sops.age` entry 1",
        ),
        (
            text.replacen(
                "          recipient: age1",
                "          recipient: [MARK] #",
                1,
            ),
            "entry 1",
        ),
        (
            text.replacen("          recipient: age1", "          recipient: MARK", 1),
            "entry 1",
        ),
        (
            text.replacen(
                "          recipient: age1",
                "          recipient: age1MARK",
                1,
            ),
            "entry 1",
        ),
        (
            text.replacen("-----BEGIN AGE", "-----MARK AGE", 1),
            "`sops.age` entry 1",
        ),
        (
            text.replacen(
                "        - enc: |",
                "        - other: 1\n          enc: |",
                1,
            ),
            "entry 1",
        ),
        (
            text.replace("    age:\n", "    age:\n        - MARK\n"),
            "`sops.age` entry 1",
        ),
        (
            format!("\"::warning::MARK\": x\n{text}"),
            "outside [A-Za-z0-9_.-]",
        ),
        (
            format!("\"a\\e[31mMARK\": x\n{text}"),
            "outside [A-Za-z0-9_.-]",
        ),
    ];
    for (input, want) in cases {
        let err = parse(&input).expect_err(want);
        assert!(err.contains(want) && !err.contains("MARK"), "{want}: {err}");
    }
}

#[test]
fn plaintext_comments_are_listed_by_line() {
    let text = ok_text().replacen("multi:", "# MARK leaked\nmulti:", 1);
    assert_eq!(parse(&text).unwrap().plaintext, ["comment on line 2"]);
    let text = ok_text().replacen("type:str]\n", "type:str] # MARK\n", 1);
    assert_eq!(parse(&text).unwrap().plaintext, ["comment on line 1"]);
    let text = ok_text().replacen("type:str]\n", "type:str]\t# MARK\n", 1);
    assert_eq!(parse(&text).unwrap().plaintext, ["comment on line 1"]);
}

/// No tracked file may contain an age private key (any case, X25519 or post-quantum).
#[test]
fn no_private_key_is_committed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output();
    let out = out.expect("git on PATH");
    assert!(out.status.success());
    let key =
        regex::Regex::new(r"(?i)age-secret-key-(pq-)?1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{50,}")
            .unwrap();
    let files: Vec<&[u8]> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .collect();
    assert!(
        files.len() > 50,
        "git ls-files listed only {} files",
        files.len()
    );
    for file in files {
        let path = root.join(String::from_utf8_lossy(file).as_ref());
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !key.is_match(&text),
            "{} contains an age private key",
            path.display()
        );
    }
}
