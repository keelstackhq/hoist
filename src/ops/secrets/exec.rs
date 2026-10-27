//! `hoist secrets exec` (spec §15.7): the guards, then the token, one fetch per file, the minted
//! token revoked, each file decrypted for the asked keys only, and the command run with those
//! values. Its stdin is `/dev/null` (it is not interactive) and its stdout and stderr pass through
//! the redactor. Its environment is hoist's minus the keys and tokens, plus the values.

use super::ExecArgs;
use super::check::read_regular;
use crate::error::HoistError;
use crate::http::{Http, Method, Service};
use crate::secrets::decrypt::{DEADLINE, decrypt};
use crate::secrets::fetch::{Reader, fetch};
use crate::secrets::guard::{self, Ci};
use crate::secrets::manifest::{Manifest, SecretRef};
use crate::secrets::redact::Redactor;
use crate::secrets::{stream, token};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Kept from the command: the age key and what reads secrets repos.
const HIDDEN: [&str; 4] = [
    "SOPS_AGE_KEY",
    "SECRETS_READ_APP_KEY",
    "SECRETS_READ_APP_ID",
    "HOIST_SECRETS_TOKEN",
];

/// How long output may still arrive after the command exits (from what it left running).
const GRACE: Duration = Duration::from_secs(5);

fn usage(why: String) -> (i32, String) {
    (3, format!("secrets: {why}"))
}

fn error(e: HoistError) -> (i32, String) {
    (e.exit_code(), e.to_string())
}

pub fn secrets_exec(dir: &Path, args: &ExecArgs, verbose: bool) -> Result<i32, (i32, String)> {
    let env = |name: &str| std::env::var(name).ok();
    let event = env("GITHUB_EVENT_PATH")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    let ci = guard::check(&env, event.as_ref(), args.run_path).map_err(|why| (2, why))?;

    let Some(text) = read_regular(&dir.join("secrets.toml")) else {
        return Err(usage(format!(
            "{}: no readable secrets.toml",
            dir.display()
        )));
    };
    let manifest = Manifest::parse(&text, "secrets.toml").map_err(error)?;
    let Some(spec) = manifest.env.get(args.env) else {
        return Err(usage(format!("no [env.{}] in secrets.toml", args.env)));
    };
    // The asked keys by file: (source, path) -> [(variable, key)].
    let mut files: BTreeMap<(&str, &str), Vec<(&str, &str)>> = BTreeMap::new();
    for (i, var) in args.keys.iter().enumerate() {
        if args.keys[..i].contains(var) {
            return Err(usage(format!("`{var}` is asked twice in --keys")));
        }
        let reserved = var == "PATH" || ["LD_", "DYLD_"].iter().any(|p| var.starts_with(p));
        if reserved || HIDDEN.contains(&var.as_str()) {
            return Err(usage(format!("`{var}` may not be set by `secrets exec`")));
        }
        let r: &SecretRef = match (spec.vars.get(var), spec.files.contains_key(var)) {
            (Some(r), _) => r,
            (None, true) => {
                return Err(usage(format!(
                    "`{var}` is a file secret; not supported yet"
                )));
            }
            (None, false) => return Err(usage(format!("`{var}` is not in [env.{}]", args.env))),
        };
        files
            .entry((&r.source, &r.path))
            .or_default()
            .push((var, &r.key));
    }
    let Some(age_key) = env("SOPS_AGE_KEY")
        .map(Zeroizing::new)
        .filter(|k| !k.trim().is_empty())
    else {
        return Err(usage(
            "SOPS_AGE_KEY is not set; it comes from the environment's secret".into(),
        ));
    };

    // Read every file, then revoke a minted token before anything runs.
    let repos: Vec<&str> = files
        .keys()
        .map(|(s, _)| manifest.sources[*s].repo.as_str())
        .collect();
    let now = u64::try_from(crate::timeutil::now_secs()).unwrap_or_default();
    let client = |t| Http::new(Service::Github, t).verbose(verbose);
    let secret = token::secrets_token(&env, &repos, now, &client).map_err(error)?;
    let minted = env("HOIST_SECRETS_TOKEN").is_none_or(|t| t.trim().is_empty());
    let reader = Reader::new(client(secret.clone()));
    let fetched: Vec<_> = (files.keys())
        .map(|&(source, path)| {
            (
                source,
                path,
                fetch(&reader, &manifest.sources[source], path),
            )
        })
        .collect();
    // Revoked whether or not every fetch worked.
    if minted {
        // The token is contents: read on ciphertext and expires within the hour: a failed revoke
        // is a warning, not a failed deploy. It never reaches the command either way.
        let revoke = client(secret).send_json(Method::Delete, "/installation/token", None);
        if revoke.is_err() {
            let why = "could not revoke the App token; it expires within the hour";
            match ci {
                Ci::Github => println!("::warning::hoist: {why}"),
                Ci::Circleci => {}
            }
            eprintln!("warning: secrets: {why}");
        }
    }
    let mut texts = Vec::new();
    for (source, path, text) in fetched {
        let text = text.map_err(error)?;
        texts.push(text.map_err(|why| usage(format!("{source}:{path}: {why}")))?);
    }

    let mut values: Vec<(&str, Zeroizing<String>)> = Vec::new();
    for ((&(source, path), wanted), text) in files.iter().zip(&texts) {
        let keys: Vec<&str> = wanted.iter().map(|(_, k)| *k).collect();
        let what = format!("{source}:{path}");
        let got =
            decrypt(OsStr::new("sops"), text, &age_key, &keys, &what, DEADLINE).map_err(error)?;
        for (var, key) in wanted {
            // Two variables may name the same key (an alias): each gets the value.
            let value = got.get(*key).cloned();
            values.push((
                var,
                value.ok_or_else(|| usage(format!("{what}: no `{key}`")))?,
            ));
        }
    }
    drop(age_key);
    run(ci, args.cmd, &values)
}

/// Runs `cmd` with `values`, its output redacted; its exit code (128 + the signal if killed).
fn run(
    ci: Ci,
    cmd: &[OsString],
    values: &[(&str, Zeroizing<String>)],
) -> Result<i32, (i32, String)> {
    let bytes = || values.iter().map(|(_, v)| v.as_bytes());
    if ci == Ci::Github {
        // GitHub masks each value in the log before the command can print it.
        let mut out = std::io::stdout().lock();
        for mask in Redactor::masks(bytes()) {
            let _ = writeln!(out, "{}", mask.as_str());
        }
        let _ = out.flush();
    }
    let redactor = Arc::new(Redactor::new(bytes()));
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in HIDDEN {
        command.env_remove(k);
    }
    for (k, v) in values {
        command.env(k, v.as_str());
    }
    let name = cmd[0].to_string_lossy();
    let mut child = command
        .spawn()
        .map_err(|e| usage(format!("could not start `{name}` ({})", e.kind())))?;
    // The readers are not joined: a process the command left behind may hold a pipe for ever.
    let (tx, rx) = mpsc::channel();
    let (r, tx2) = (Arc::clone(&redactor), tx.clone());
    let out = child.stdout.take().map(BufReader::new);
    let err = child.stderr.take().map(BufReader::new);
    std::thread::spawn(move || {
        tx.send(out.is_none_or(|o| stream::copy(&r, o, std::io::stdout()).is_ok()))
    });
    std::thread::spawn(move || {
        tx2.send(err.is_none_or(|e| stream::copy(&redactor, e, std::io::stderr()).is_ok()))
    });
    let (mut open, mut failed, mut exited) = (2, false, None);
    while open > 0 {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(written) => {
                open -= 1;
                // A stream that cannot be written stops the command (it would block on the pipe).
                if !written && !failed {
                    failed = true;
                    let _ = child.kill();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if exited.is_none() {
            exited = child
                .try_wait()
                .ok()
                .flatten()
                .map(|s| (s, Instant::now() + GRACE));
        }
        if exited.is_some_and(|(_, end)| Instant::now() >= end) {
            break;
        }
    }
    let status = match exited {
        Some((status, _)) => status,
        None => child
            .wait()
            .map_err(|_| usage(format!("`{name}` did not finish")))?,
    };
    if failed {
        return Err(usage(format!(
            "could not write the output of `{name}`; it was stopped"
        )));
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}
