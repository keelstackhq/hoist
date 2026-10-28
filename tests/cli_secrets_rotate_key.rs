//! `hoist secrets rotate-key <recipient> [--public AGE1]` in a secrets repo (spec §15.4, §15.5):
//! a routine rotation. A new key `<name>-YYYYMM` joins the old one's rules (sealed into the
//! consumer's SOPS_AGE_KEY, or given with --public), the old one leaves, and every file gets a
//! new data key. Real sops; keys made at runtime only.
#![cfg(unix)]
mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{FakeApi, hoist};
use crypto_box::SecretKey;
use crypto_box::aead::OsRng;
use hoist::secrets::decrypt::resolve;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

const FILE: &str = "prod/app.sops.yaml";
const ENV: &str = "GET /repos/acme/site/environments/prod";
const POLICIES: &str = "GET /repos/acme/site/environments/prod/deployment-branch-policies";
const HELD: &str = "GET /repos/acme/site/environments/prod/secrets/SOPS_AGE_KEY";
const KEY: &str = "GET /repos/acme/site/environments/prod/secrets/public-key";
const PUT: &str = "PUT /repos/acme/site/environments/prod/secrets/SOPS_AGE_KEY";
const LIMITED: &str = r#"{"name":"prod","deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}"#;
const MAIN: &str = r#"{"total_count":1,"branch_policies":[{"name":"main","type":"branch"}]}"#;

/// The stdout of `program args` in `dir` with only PATH (and `key` as SOPS_AGE_KEY), or `None`.
fn tool(dir: &Path, program: &str, args: &[&str], key: Option<&str>) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).env_clear();
    cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    key.map(|k| cmd.env("SOPS_AGE_KEY", k));
    let out = cmd.output().expect(program);
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap())
}

/// A throwaway age identity in `dir`: (private, public).
fn identity(dir: &Path, name: &str) -> (String, String) {
    tool(dir, "age-keygen", &["-o", name], None).unwrap();
    let text = std::fs::read_to_string(dir.join(name)).unwrap();
    let private = text.lines().find(|l| l.starts_with("AGE-SECRET-KEY-"));
    let public = tool(dir, "age-keygen", &["-y", name], None).unwrap();
    (private.unwrap().to_string(), public.trim().to_string())
}

struct Repo {
    dir: tempfile::TempDir,
    keys: tempfile::TempDir,
    owner: (String, String),
    ci: (String, String),
}

/// `None` without sops or age-keygen locally (CI must have them): a secrets repo whose
/// `prod/app.sops.yaml` is encrypted for the owner and `site-prod`, the ci key of acme/site.
fn repo() -> Option<Repo> {
    let path = std::env::var_os("PATH");
    let on_path = |p: &str| resolve(OsStr::new(p), path.as_deref()).is_some();
    if !on_path("sops") || !on_path("age-keygen") {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must install sops and age-keygen"
        );
        eprintln!("skipped: sops or age-keygen is not on PATH (spec §15.9.1)");
        return None;
    }
    let (dir, keys) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let owner = identity(keys.path(), "owner.txt");
    let ci = identity(keys.path(), "ci.txt");
    let put = |at: &Path, name: &str, text: &str| std::fs::write(at.join(name), text).unwrap();
    let (o, c) = (&owner.1, &ci.1);
    put(
        dir.path(),
        "access.toml",
        &format!(
            "version = 1\n\n[[recipient]]\nname = \"owner-2026\"\nkind = \"human\"\nage = \"{o}\"\nexpires = \"2027-10-27\"\n\n[[recipient]]\nname = \"site-prod\"\nkind = \"ci\"\nage = \"{c}\"\nrepo = \"acme/site\"\nenv = \"prod\"\nprovider = \"github\"\nexpires = \"2027-10-27\"\n\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\", \"site-prod\"]\n\n[[rule]]\npath = \"preview/**\"\nrecipients = [\"owner-2026\"]\n"
        ),
    );
    put(
        keys.path(),
        "both.yaml",
        &format!("creation_rules:\n  - path_regex: '.*'\n    age: {o},{c}\n"),
    );
    put(
        keys.path(),
        "plain.yaml",
        "API_TOKEN: hoist-test-value-1\nDB_URL: hoist-test-value-2\n",
    );
    let args = "--config both.yaml encrypt plain.yaml";
    let enc = tool(
        keys.path(),
        "sops",
        &args.split(' ').collect::<Vec<_>>(),
        None,
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("prod")).unwrap();
    put(dir.path(), FILE, &enc);
    Some(Repo {
        dir,
        keys,
        owner,
        ci,
    })
}

/// `hoist secrets rotate-key <args>` outside an agent session, the owner's key as SOPS_AGE_KEY;
/// `--json` unless `json` is false (then stderr has the hint).
fn rotate(
    repo: &Repo,
    api: &FakeApi,
    args: &[&str],
    extra: &[(&str, &str)],
    json: bool,
) -> (i32, Value, String) {
    let mut cmd = hoist();
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_REMOTE");
    cmd.env("HOME", repo.keys.path())
        .env("SOPS_AGE_KEY", &repo.owner.0);
    cmd.env("HOIST_NOW", "2026-10-27T00:00:00Z")
        .env("HOIST_GITHUB_API", api.url());
    cmd.env("GITHUB_TOKEN", "gh-test-token")
        .envs(extra.iter().copied());
    cmd.args(["secrets", "rotate-key", "-C"])
        .arg(repo.dir.path())
        .args(args);
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let all = format!("{json}{err}");
    assert!(
        !all.contains("AGE-SECRET-KEY") && !all.contains("hoist-test-value"),
        "{all}"
    );
    (out.status.code().unwrap(), json, err)
}

/// GitHub for a `prod` environment only `main` deploys to, holding SOPS_AGE_KEY or not.
fn github(held: bool) -> (SecretKey, FakeApi) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    let key = serde_json::json!({ "key_id": "k1", "key": b64 }).to_string();
    let held = if held { 200 } else { 404 };
    let api = FakeApi::start(&[
        (ENV, 200, LIMITED, &[]),
        (POLICIES, 200, MAIN, &[]),
        (HELD, held, r#"{"name":"SOPS_AGE_KEY"}"#, &[]),
        (KEY, 200, &key, &[]),
        (PUT, 201, "{}", &[]),
    ]);
    (sk, api)
}

fn sent(api: &FakeApi) -> Vec<String> {
    let reqs = api.requests().into_iter();
    reqs.map(|r| format!("{} {}", r.method, r.path)).collect()
}

fn decrypts(repo: &Repo, key: &str) -> bool {
    let file = repo.dir.path().join(FILE);
    tool(
        repo.keys.path(),
        "sops",
        &["decrypt", file.to_str().unwrap()],
        Some(key),
    )
    .is_some()
}

fn ids(report: &Value) -> Vec<String> {
    let steps = report["human_steps"].as_array().unwrap();
    steps
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn rotate_key_seals_a_new_key_in_the_old_ones_place() {
    let Some(repo) = repo() else { return };
    let token = |text: &str| {
        text.lines()
            .find(|l| l.starts_with("API_TOKEN:"))
            .unwrap()
            .to_string()
    };
    let before = token(&std::fs::read_to_string(repo.dir.path().join(FILE)).unwrap());
    let (sk, api) = github(true);
    let (code, report, err) = rotate(&repo, &api, &["site-prod"], &[], true);
    assert_eq!(code, 2, "{err} {report:#}");
    assert_eq!(sent(&api), [ENV, POLICIES, HELD, KEY, PUT]);
    // The new key, sealed, reads the file; the old one does not; the owner still does.
    let put = api
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    let sealed = STANDARD
        .decode(body["encrypted_value"].as_str().unwrap())
        .unwrap();
    let private = String::from_utf8(sk.unseal(&sealed).unwrap()).unwrap();
    assert!(decrypts(&repo, &private) && decrypts(&repo, &repo.owner.0));
    assert!(!decrypts(&repo, &repo.ci.0));
    // A new data key: the value's ciphertext changed, not only the recipients.
    assert_ne!(
        token(&std::fs::read_to_string(repo.dir.path().join(FILE)).unwrap()),
        before
    );
    let access = std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    assert!(
        access.contains("recipients = [\"owner-2026\", \"site-prod-202610\"]")
            && !access.contains("\"site-prod\"")
            && !access.contains(&repo.ci.1),
        "{access}"
    );
    // A routine rotation: no list of values to rotate; the other repos must take the new key.
    let ids = ids(&report);
    assert!(
        ids.contains(&"regrant:site-prod-202610".to_string()),
        "{ids:?}"
    );
    assert_eq!(ids, ["regrant:site-prod-202610"], "{report:#}");
}

#[test]
fn rotate_key_renames_by_month_and_refuses_twice_in_one() {
    let Some(repo) = repo() else { return };
    let path = repo.dir.path().join("access.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("site-prod", "site-prod-202510")).unwrap();
    let (_, api) = github(true);
    let (code, report, err) = rotate(&repo, &api, &["site-prod-202510"], &[], true);
    assert_eq!(code, 2, "{err} {report:#}");
    let access = std::fs::read_to_string(&path).unwrap();
    assert!(
        access.contains("\"site-prod-202610\"") && !access.contains("202510"),
        "{access}"
    );
    // Again this month: refused before any request.
    let api = FakeApi::start(&[]);
    let (code, _, err) = rotate(&repo, &api, &["site-prod-202610"], &[], true);
    assert!(code == 3 && err.contains("already rotated"), "{err}");
    assert!(api.requests().is_empty());
}

#[test]
fn rotate_key_public_takes_the_key_another_repo_made() {
    let Some(repo) = repo() else { return };
    let new = identity(repo.keys.path(), "new.txt");
    let (_, api) = github(true);
    let args = ["site-prod", "--public", new.1.as_str()];
    let (code, report, err) = rotate(&repo, &api, &args, &[], true);
    assert_eq!(code, 0, "{err} {report:#}");
    assert_eq!(sent(&api), [ENV, POLICIES, HELD]);
    assert!(decrypts(&repo, &new.0) && !decrypts(&repo, &repo.ci.0));
    assert!(ids(&report).is_empty(), "{report:#}");
}

/// Arguments, extra environment, the expected exit code and a text the output must contain.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], i32, &'a str);

#[test]
fn rotate_key_refuses_and_changes_nothing() {
    let Some(repo) = repo() else { return };
    let read = |name: &str| std::fs::read_to_string(repo.dir.path().join(name)).unwrap();
    let (access, file) = (read("access.toml"), read(FILE));
    let only = tempfile::tempdir().unwrap();
    let keygen = resolve(
        OsStr::new("age-keygen"),
        std::env::var_os("PATH").as_deref(),
    )
    .unwrap();
    std::os::unix::fs::symlink(keygen, only.path().join("age-keygen")).unwrap();
    let path = only.path().to_str().unwrap();
    let stranger = identity(repo.keys.path(), "stranger.txt");
    let cases: [Case; 7] = [
        (&["site-prod"], &[("CLAUDECODE", "1")], 2, "agent session"),
        (&["nobody"], &[], 3, "no recipient `nobody`"),
        (&["owner-2026"], &[], 3, "only a ci recipient"),
        (&["site-prod"], &[("PATH", path)], 3, "sops"),
        (
            &["site-prod", "--public", "age1nope"],
            &[],
            3,
            "--public `age1nope`",
        ),
        (&["site-prod", "--public", &repo.ci.1], &[], 3, "already"),
        // Without the owner's key the files could not be re-keyed after the PUT.
        (
            &["site-prod"],
            &[("SOPS_AGE_KEY", &stranger.0)],
            3,
            "cannot open prod/app.sops.yaml",
        ),
    ];
    for (args, extra, want, text) in cases {
        let api = FakeApi::start(&[]);
        let (code, report, err) = rotate(&repo, &api, args, extra, true);
        assert!(
            code == want && format!("{report}{err}").contains(text),
            "{args:?}: {code} {err} {report:#}"
        );
        assert!(api.requests().is_empty(), "{args:?}");
        assert_eq!(
            (read("access.toml"), read(FILE)),
            (access.clone(), file.clone())
        );
    }
    // An environment any branch deploys to, or --public with no key there: nothing written.
    let open = r#"{"name":"prod","deployment_branch_policy":null}"#;
    let api = FakeApi::start(&[(ENV, 200, open, &[])]);
    let (code, _, err) = rotate(&repo, &api, &["site-prod"], &[], true);
    assert!(code == 3 && err.contains("only branch `main`"), "{err}");
    let new = identity(repo.keys.path(), "new.txt");
    let (_, api) = github(false);
    let (code, _, err) = rotate(&repo, &api, &["site-prod", "--public", &new.1], &[], true);
    assert!(code == 3 && err.contains("no SOPS_AGE_KEY"), "{err}");
    assert_eq!(sent(&api), [ENV, POLICIES, HELD]);
    assert_eq!((read("access.toml"), read(FILE)), (access, file));
}

#[test]
fn rotate_key_prints_the_commands_to_commit() {
    let Some(repo) = repo() else { return };
    let (_, api) = github(false);
    let (code, _, err) = rotate(&repo, &api, &["site-prod"], &[], false);
    assert_eq!(code, 0, "{err}");
    let dir = repo.dir.path().display();
    for line in [
        format!("git -C '{dir}' switch -c hoist/rotate-key-site-prod-202610"),
        format!("git -C '{dir}' commit -m 'secrets: rotate site-prod to site-prod-202610'"),
    ] {
        assert!(err.contains(&line), "{line}\n{err}");
    }
}

#[test]
fn rotate_key_refuses_a_new_name_already_taken() {
    let Some(repo) = repo() else { return };
    let path = repo.dir.path().join("access.toml");
    let other = identity(repo.keys.path(), "other.txt");
    let text = std::fs::read_to_string(&path).unwrap().replacen(
        "[[rule]]",
        &format!(
            "[[recipient]]\nname = \"site-prod-202610\"\nkind = \"ci\"\nage = \"{}\"\nrepo = \"other/site\"\nenv = \"prod\"\nprovider = \"github\"\nexpires = \"2027-10-27\"\n\n[[rule]]",
            other.1
        ),
        1,
    );
    std::fs::write(&path, &text).unwrap();
    let api = FakeApi::start(&[]);
    let (code, _, err) = rotate(&repo, &api, &["site-prod"], &[], true);
    assert!(
        code == 3 && err.contains("`site-prod-202610` is already a recipient"),
        "{err}"
    );
    assert!(api.requests().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn rotate_key_refuses_while_a_file_is_out_of_sync() {
    let Some(repo) = repo() else { return };
    // An earlier change to access.toml (a removal whose sync never ran): the file still has
    // `site-prod` and the owner but the rule now lists another human too. Its own steps matter,
    // so rotate-key will not run sync for it.
    let path = repo.dir.path().join("access.toml");
    let other = identity(repo.keys.path(), "other.txt");
    let text = std::fs::read_to_string(&path).unwrap().replacen(
        "[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\", \"site-prod\"]",
        &format!(
            "[[recipient]]\nname = \"owner-safe\"\nkind = \"human\"\nage = \"{}\"\nexpires = \"2027-10-27\"\n\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\", \"owner-safe\", \"site-prod\"]",
            other.1
        ),
        1,
    );
    std::fs::write(&path, &text).unwrap();
    let api = FakeApi::start(&[]);
    let (code, _, err) = rotate(&repo, &api, &["site-prod"], &[], true);
    assert!(
        code == 3 && err.contains("run `hoist secrets sync` first"),
        "{err}"
    );
    assert!(api.requests().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}
