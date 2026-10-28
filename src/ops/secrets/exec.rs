//! `hoist secrets exec` (spec §15.7): hoist restarts itself without the keys in its environment,
//! then the guards, the token, one fetch per file, the minted token revoked, each file decrypted
//! for the asked keys only, and the command run with those values (file secrets under
//! `/dev/shm`). The command runs in its own process group, with stdin `/dev/null` (it is not
//! interactive) and stdout and stderr through the redactor. Its environment is hoist's minus the
//! keys and tokens, plus the values. SIGTERM, SIGINT and SIGHUP are passed on to its group.

use super::ExecArgs;
use super::check::read_regular;
use crate::error::HoistError;
use crate::http::{Http, Method, Service};
use crate::secrets::decrypt::{DEADLINE, decrypt};
use crate::secrets::fetch::{Reader, fetch};
use crate::secrets::guard::{self, Ci};
use crate::secrets::handoff::{self, KEYS, Keys, STAGE};
use crate::secrets::manifest::Manifest;
use crate::secrets::redact::Redactor;
use crate::secrets::{shm, stream, token};
use rustix::process::{Pid, Signal, kill_process_group};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufReader, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// How long output may still arrive after the command exits (from what it left running).
const GRACE: Duration = Duration::from_secs(5);

/// Variables a secret may not set: what changes how the command, its shell or its runtime
/// behaves, or what CI and git read. Exact names, then prefixes.
const RESERVED: &[&str] = &[
    "PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "HOME",
    "CI",
    "SHELL",
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "PROMPT_COMMAND",
    "CDPATH",
    "ZDOTDIR",
    "TMPDIR",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_EXTRA_CA_CERTS",
    "RUBYOPT",
    "RUBYLIB",
    "PERLLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];
const RESERVED_PREFIXES: &[&str] = &[
    "LD_",
    "DYLD_",
    "GITHUB_",
    "RUNNER_",
    "ACTIONS_",
    "CIRCLE_",
    "GIT_",
    "PYTHON",
    "PERL5",
    "NPM_CONFIG_",
    "HOIST_",
];

fn reserved(var: &str) -> bool {
    KEYS.contains(&var)
        || RESERVED.contains(&var)
        || RESERVED_PREFIXES.iter().any(|p| var.starts_with(p))
}

fn usage(why: String) -> (i32, String) {
    (3, format!("secrets: {why}"))
}

fn error(e: HoistError) -> (i32, String) {
    (e.exit_code(), e.to_string())
}

/// One asked variable: its name, the key it reads, and whether it is a file secret.
type Wanted<'a> = (&'a str, &'a str, bool);

pub fn secrets_exec(dir: &Path, args: &ExecArgs, verbose: bool) -> Result<i32, (i32, String)> {
    handoff::harden();
    let keys: Keys = match std::env::var_os(STAGE) {
        None => return Err(usage(handoff::restart())),
        // Only hoist sets STAGE, after removing the keys: set by hand, the keys are still there.
        Some(_) if KEYS.iter().any(|k| std::env::var_os(k).is_some()) => {
            return Err(usage(format!(
                "{STAGE} is set only by hoist itself; unset it"
            )));
        }
        Some(_) => handoff::take().map_err(usage)?,
    };
    // The keys come from the handover only; everything else from the environment.
    let env = |name: &str| match keys.get(name) {
        Some(value) => Some(value.to_string()),
        None if KEYS.contains(&name) => None,
        None => std::env::var(name).ok(),
    };
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
    // The asked keys by file: (source, path) -> [(variable, key, file?)].
    let mut files: BTreeMap<(&str, &str), Vec<Wanted>> = BTreeMap::new();
    for (i, var) in args.keys.iter().enumerate() {
        if args.keys[..i].contains(var) {
            return Err(usage(format!("`{var}` is asked twice in --keys")));
        }
        if reserved(var) {
            return Err(usage(format!("`{var}` may not be set by `secrets exec`")));
        }
        let (r, file) = match (spec.vars.get(var), spec.files.get(var)) {
            (Some(r), _) => (r, false),
            (None, Some(r)) => (r, true),
            (None, None) => return Err(usage(format!("`{var}` is not in [env.{}]", args.env))),
        };
        files
            .entry((&r.source, &r.path))
            .or_default()
            .push((var, &r.key, file));
    }
    let Some(age_key) = keys.get("SOPS_AGE_KEY").filter(|k| !k.trim().is_empty()) else {
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
    // The token lives only in this block: it is gone before the command starts.
    let fetched: Vec<_> = {
        let secret = token::secrets_token(&env, &repos, now, &client).map_err(error)?;
        let minted = keys
            .get("HOIST_SECRETS_TOKEN")
            .is_none_or(|t| t.trim().is_empty());
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
                eprintln!("hoist: warning: secrets: {why}");
            }
        }
        fetched
    };
    let mut texts = Vec::new();
    for (source, path, text) in fetched {
        let text = text.map_err(error)?;
        texts.push(text.map_err(|why| usage(format!("{source}:{path}: {why}")))?);
    }

    let mut values: Vec<(&str, Zeroizing<String>, bool)> = Vec::new();
    for ((&(source, path), wanted), text) in files.iter().zip(&texts) {
        let asked: Vec<&str> = wanted.iter().map(|(_, k, _)| *k).collect();
        let what = format!("{source}:{path}");
        let got =
            decrypt(OsStr::new("sops"), text, age_key, &asked, &what, DEADLINE).map_err(error)?;
        for &(var, key, file) in wanted {
            // Two variables may name the same key (an alias): each gets the value.
            let value = got.get(key).cloned();
            let value = value.ok_or_else(|| usage(format!("{what}: no `{key}`")))?;
            values.push((var, value, file));
        }
    }
    drop(keys);
    run(ci, args.cmd, &values)
}

/// Runs `cmd` with `values`, its output redacted; its exit code (128 + the signal if killed).
fn run(
    ci: Ci,
    cmd: &[OsString],
    values: &[(&str, Zeroizing<String>, bool)],
) -> Result<i32, (i32, String)> {
    // From here a signal is passed on to the command instead of ending hoist, so the file
    // secrets are always removed.
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register_usize(sig, Arc::clone(&signal), sig as usize)
            .map_err(|e| usage(format!("could not handle signals ({})", e.kind())))?;
    }
    // Each value once (an alias repeats one), in the order asked.
    let mut unique: Vec<&[u8]> = Vec::new();
    for (_, v, _) in values {
        if !unique.contains(&v.as_bytes()) {
            unique.push(v.as_bytes());
        }
    }
    if ci == Ci::Github {
        // GitHub masks each value in the log before the command can print it.
        let mut out = std::io::stdout().lock();
        for mask in Redactor::masks(unique.iter().copied()) {
            let _ = writeln!(out, "{}", mask.as_str());
        }
        let _ = out.flush();
    }
    let redactor = Arc::new(Redactor::new(unique.iter().copied()));
    let mut command = Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .env_remove(STAGE);
    for k in KEYS {
        command.env_remove(k);
    }
    let shm = match values.iter().any(|(_, _, file)| *file) {
        true => Some(
            shm::Dir::create(Path::new("/dev/shm"))
                .map_err(|why| (2, format!("secrets: {why}")))?,
        ),
        false => None,
    };
    for (k, v, file) in values {
        match (&shm, file) {
            (Some(dir), true) => command.env(k, dir.write(k, v.as_bytes()).map_err(usage)?),
            _ => command.env(k, v.as_str()),
        };
    }
    let name = cmd[0].to_string_lossy();
    if signal.load(Ordering::SeqCst) != 0 {
        return Err(usage(format!(
            "stopped by a signal before `{name}` started"
        )));
    }
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
    // When a signal was passed on, the time the command is killed if it still runs.
    let mut kill_at: Option<Instant> = None;
    // Polls until the command exits, also when it closed its output early (a daemon): signals are
    // passed on the whole time, and hoist never blocks where the runner's signal cannot reach it.
    let poll = Duration::from_millis(50);
    let status = loop {
        if open > 0 {
            match rx.recv_timeout(poll) {
                Ok(written) => {
                    open -= 1;
                    // A stream that cannot be written stops the command (it would block on it).
                    if !written && !failed {
                        failed = true;
                        signal_group(&mut child, Signal::KILL);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => open = 0,
            }
        } else {
            std::thread::sleep(poll);
        }
        // Signals within one poll are merged: the last one is passed on.
        let sig = signal.swap(0, Ordering::SeqCst);
        if let Some(sig) = i32::try_from(sig).ok().and_then(Signal::from_named_raw) {
            signal_group(&mut child, sig);
            kill_at.get_or_insert(Instant::now() + GRACE);
        }
        // A command that ignores the signal is killed, so hoist can still remove the files
        // before the runner kills it.
        if exited.is_none() && kill_at.is_some_and(|at| Instant::now() >= at) {
            signal_group(&mut child, Signal::KILL);
            kill_at = None;
        }
        if exited.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => exited = Some((status, Instant::now() + GRACE)),
                Ok(None) => {}
                Err(_) => {
                    signal_group(&mut child, Signal::KILL);
                    return Err(usage(format!("`{name}` did not finish")));
                }
            }
        }
        // Done once the command exited and its output ended, or after the grace for what it left
        // running with a pipe open.
        if let Some((status, end)) = exited
            && (open == 0 || Instant::now() >= end)
        {
            break status;
        }
    };
    drop(shm);
    if failed {
        return Err(usage(format!(
            "could not write the output of `{name}`; it was stopped"
        )));
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// Sends `sig` to the command's process group (it leads one), and kills the command itself if a
/// kill failed. Once the command is reaped, its group id stays reserved while any member lives.
fn signal_group(child: &mut Child, sig: Signal) {
    let sent = Pid::from_raw(child.id() as i32).map(|pid| kill_process_group(pid, sig));
    if !matches!(sent, Some(Ok(()))) && sig == Signal::KILL {
        let _ = child.kill();
    }
}
