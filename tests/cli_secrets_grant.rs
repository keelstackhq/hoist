//! `hoist secrets grant <repo> --env E` in a secrets repo (spec §15.4): a new age key, its private
//! half sealed into the consumer's environment secret `SOPS_AGE_KEY`, its public half a `ci`
//! recipient of every rule under `E/`, then `sync`. Real sops; keys made at runtime only.
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
}

/// `None` without sops or age-keygen locally (CI must have them): a secrets repo whose
/// `prod/app.sops.yaml` is encrypted for the owner, the only recipient of rule `prod/**`.
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
    let put = |at: &Path, name: &str, text: &str| std::fs::write(at.join(name), text).unwrap();
    let o = &owner.1;
    put(
        dir.path(),
        "access.toml",
        &format!(
            "version = 1\n\n[[recipient]]\nname = \"owner-2026\"\nkind = \"human\"\nage = \"{o}\"\nexpires = \"2027-10-27\"\n\n[[rule]]\npath = \"prod/**\"\nrecipients = [\"owner-2026\"]\n\n[[rule]]\npath = \"preview/**\"\nrecipients = [\"owner-2026\"]\n"
        ),
    );
    put(
        keys.path(),
        "only-owner.yaml",
        &format!("creation_rules:\n  - path_regex: '.*'\n    age: {o}\n"),
    );
    put(keys.path(), "plain.yaml", "API_TOKEN: hoist-test-value-1\n");
    let args = "--config only-owner.yaml encrypt --unencrypted-suffix _unencrypted plain.yaml";
    let enc = tool(
        keys.path(),
        "sops",
        &args.split(' ').collect::<Vec<_>>(),
        None,
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("prod")).unwrap();
    put(dir.path(), FILE, &enc);
    Some(Repo { dir, keys, owner })
}

/// The consumer's environment public key, as GitHub returns it.
fn env_key() -> (SecretKey, String) {
    let sk = SecretKey::generate(&mut OsRng);
    let b64 = STANDARD.encode(sk.public_key().as_bytes());
    (
        sk,
        serde_json::json!({ "key_id": "k1", "key": b64 }).to_string(),
    )
}

/// `hoist secrets grant --json <args>` outside an agent session, the owner's key as SOPS_AGE_KEY.
fn grant(
    repo: &Repo,
    api: &FakeApi,
    args: &[&str],
    extra: &[(&str, &str)],
) -> (i32, Value, String) {
    run(repo, api, args, extra, true)
}

/// The same, as a table (`json` false), which prints the hint on stderr.
fn run(
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
    cmd.args(["secrets", "grant", "-C"])
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

/// GitHub for a `prod` environment only `main` deploys to; `held` is whether it has SOPS_AGE_KEY.
fn github(key: &str, held: bool) -> FakeApi {
    let held = if held { 200 } else { 404 };
    FakeApi::start(&[
        (ENV, 200, LIMITED, &[]),
        (POLICIES, 200, MAIN, &[]),
        (HELD, held, r#"{"name":"SOPS_AGE_KEY"}"#, &[]),
        (KEY, 200, key, &[]),
        (PUT, 201, "{}", &[]),
    ])
}

fn sent(api: &FakeApi) -> Vec<String> {
    let reqs = api.requests().into_iter();
    reqs.map(|r| format!("{} {}", r.method, r.path)).collect()
}

/// The private key in the PUT, opened with the environment's key.
fn sealed(api: &FakeApi, sk: &SecretKey) -> String {
    let put = api
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("a PUT");
    let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
    assert_eq!(body["key_id"], "k1");
    let box_ = STANDARD
        .decode(body["encrypted_value"].as_str().unwrap())
        .unwrap();
    String::from_utf8(sk.unseal(&box_).unwrap()).unwrap()
}

#[test]
fn grant_seals_a_new_key_and_makes_it_a_recipient() {
    let Some(repo) = repo() else { return };
    let (sk, key) = env_key();
    let api = github(&key, false);
    let (code, _, err) = run(&repo, &api, &["acme/site", "--env", "prod"], &[], false);
    assert_eq!(code, 0, "{err}");
    assert_eq!(sent(&api), [ENV, POLICIES, HELD, KEY, PUT]);
    let private = sealed(&api, &sk);
    assert!(
        private.starts_with("AGE-SECRET-KEY-1") && !private.contains('\n'),
        "not one identity"
    );
    // Its public key is recipient `site-prod` of the prod rule only, and it reads the file.
    let access = std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    std::fs::write(repo.keys.path().join("granted.txt"), &private).unwrap();
    let public = tool(repo.keys.path(), "age-keygen", &["-y", "granted.txt"], None).unwrap();
    assert!(access.contains(public.trim()) && access.contains("name = \"site-prod\""));
    assert!(access.contains("repo = \"acme/site\"") && access.contains("expires = \"2027-10-27\""));
    assert!(access.contains("recipients = [\"owner-2026\", \"site-prod\"]\n\n[[rule]]\npath = \"preview/**\"\nrecipients = [\"owner-2026\"]"), "{access}");
    let file = repo.dir.path().join(FILE);
    let read = [
        "decrypt",
        "--extract",
        "[\"API_TOKEN\"]",
        file.to_str().unwrap(),
    ];
    assert_eq!(
        tool(repo.keys.path(), "sops", &read, Some(&private)).as_deref(),
        Some("hoist-test-value-1")
    );
    // The hint: git commands to run, the owner commits; hoist ran none.
    let dir = repo.dir.path().display();
    for line in [
        format!("git -C '{dir}' switch -c hoist/grant-site-prod"),
        format!("git -C '{dir}' add access.toml .sops.yaml '{FILE}'"),
        format!("git -C '{dir}' commit -m 'secrets: grant site-prod (acme/site prod)'"),
        format!("git -C '{dir}' push -u origin hoist/grant-site-prod"),
    ] {
        assert!(err.contains(&line), "{line}\n{err}");
    }
    assert!(
        err.contains("open a PR from hoist/grant-site-prod"),
        "{err}"
    );
}

/// Arguments, extra environment, the expected exit code and a text the output must contain.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], i32, &'a str);

#[test]
fn grant_refuses_before_any_request_and_changes_nothing() {
    let Some(repo) = repo() else { return };
    let access = || std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    let before = access();
    let cases: [Case; 8] = [
        (
            &["acme/site", "--env", "prod"],
            &[("CLAUDECODE", "1")],
            2,
            "agent session",
        ),
        (&["acme/site", "--env", "staging"], &[], 3, "no rule"),
        (
            &["acme/site", "--env", "prod", "--provider", "circleci"],
            &[],
            3,
            "CircleCI",
        ),
        (&["acme", "--env", "prod"], &[], 3, "owner/name"),
        (
            &["acme/site", "--env", "prod", "--name", "Site"],
            &[],
            3,
            "[a-z0-9-]",
        ),
        (
            &["acme/site", "--env", "prod", "--name", "owner-2026"],
            &[],
            3,
            "--name",
        ),
        (
            &["acme/site", "--env", "prod", "--public", "age1nope"],
            &[],
            3,
            "--public `age1nope`",
        ),
        (
            &[
                "acme/site",
                "--env",
                "prod",
                "--replace",
                "--public",
                "age1x",
            ],
            &[],
            3,
            "--public",
        ),
    ];
    for (args, extra, want, text) in cases {
        let api = FakeApi::start(&[]);
        let (code, report, err) = grant(&repo, &api, args, extra);
        assert!(
            code == want && format!("{report}{err}").contains(text),
            "{args:?}: {code} {err} {report:#}"
        );
        assert!(api.requests().is_empty(), "{args:?}");
        assert_eq!(access(), before, "{args:?}");
    }
}

#[test]
fn grant_refuses_an_open_environment_or_a_key_it_would_overwrite() {
    let Some(repo) = repo() else { return };
    let access = || std::fs::read_to_string(repo.dir.path().join("access.toml")).unwrap();
    let before = access();
    let (sk, key) = env_key();
    // Any branch can deploy: refused after the GET, nothing written.
    let open = r#"{"name":"prod","deployment_branch_policy":null}"#;
    let api = FakeApi::start(&[(ENV, 200, open, &[])]);
    let (code, _, err) = grant(&repo, &api, &["acme/site", "--env", "prod"], &[]);
    assert!(code == 3 && err.contains("only branch `main`"), "{err}");
    assert!(api.requests().iter().all(|r| r.method == "GET"));
    // A key is there already (another secrets repo's): refused unless --replace or --public.
    let api = github(&key, true);
    let (code, _, err) = grant(&repo, &api, &["acme/site", "--env", "prod"], &[]);
    assert!(
        code == 3 && err.contains("--public") && err.contains("--replace"),
        "{err}"
    );
    assert_eq!(sent(&api), [ENV, POLICIES, HELD]);
    assert_eq!(access(), before);
    // --public adds the key it holds and writes no secret.
    let other = identity(repo.keys.path(), "other.txt");
    let api = github(&key, true);
    let args = ["acme/site", "--env", "prod", "--public", other.1.as_str()];
    let (code, report, err) = grant(&repo, &api, &args, &[]);
    assert_eq!(code, 0, "{err} {report:#}");
    assert_eq!(sent(&api), [ENV, POLICIES, HELD]);
    assert!(access().contains(&other.1));
    // --public needs the key to be there.
    std::fs::write(repo.dir.path().join("access.toml"), &before).unwrap();
    let api = github(&key, false);
    let (code, _, err) = grant(&repo, &api, &args, &[]);
    assert!(code == 3 && err.contains("no SOPS_AGE_KEY"), "{err}");
    assert_eq!(access(), before);
    // --replace overwrites it: other secrets repos that list this consumer must re-grant (exit 2).
    let api = github(&key, true);
    let args = ["acme/site", "--env", "prod", "--replace"];
    let (code, report, err) = grant(&repo, &api, &args, &[]);
    assert_eq!(code, 2, "{err} {report:#}");
    assert!(sealed(&api, &sk).starts_with("AGE-SECRET-KEY-1"));
    assert_eq!(report["items"][0]["action"], "update", "{report:#}");
    // (Restoring access.toml above left the file re-keyed for `other`: sync adds its own step.)
    let steps = report["human_steps"].as_array().unwrap();
    let step = steps.iter().find(|s| s["id"] == "regrant:site-prod");
    let step = step.and_then(|s| s["title"].as_str()).unwrap_or_default();
    assert!(step.contains("every other secrets repo"), "{report:#}");
    // GitHub refuses the write: exit 3, access.toml as it was.
    std::fs::write(repo.dir.path().join("access.toml"), &before).unwrap();
    let api = FakeApi::start(&[
        (ENV, 200, LIMITED, &[]),
        (POLICIES, 200, MAIN, &[]),
        (HELD, 404, "{}", &[]),
        (KEY, 404, "{}", &[]),
    ]);
    let (code, _, err) = grant(&repo, &api, &["acme/site", "--env", "prod"], &[]);
    assert!(
        code == 3 && err.contains("environment prod of acme/site"),
        "{err}"
    );
    assert_eq!(access(), before);
}
