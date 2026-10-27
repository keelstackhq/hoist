//! Decrypting one secrets file for `hoist secrets exec` (spec §15.7.1 step 4): `sops decrypt` with
//! the file on stdin and the age key only in that child's environment; only the asked keys kept.
#![cfg(unix)]

use hoist::secrets::decrypt::{DEADLINE, decrypt, resolve};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Whether sops and age-keygen are on PATH. Missing locally, the test skips with a message; in CI
/// (which installs them) that is a failure.
fn tools() -> bool {
    let on_path = |p: &str| resolve(OsStr::new(p), std::env::var_os("PATH").as_deref()).is_some();
    let found = on_path("sops") && on_path("age-keygen");
    assert!(
        found || std::env::var_os("CI").is_none(),
        "CI must install sops and age-keygen"
    );
    if !found {
        eprintln!("skipped: sops or age-keygen is not on PATH (spec §15.9.1)");
    }
    found
}

/// The stdout of `program args`, which must succeed.
fn run(dir: &Path, program: &str, args: &[&str]) -> String {
    let mut cmd = Command::new(program);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let cmd = cmd
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", path);
    let out = cmd.output().expect(program);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{program}: {err}");
    String::from_utf8(out.stdout).unwrap()
}

/// A throwaway age identity made at runtime (never committed): (private, public).
fn identity(dir: &Path, name: &str) -> (String, String) {
    run(dir, "age-keygen", &["-o", name]);
    let text = std::fs::read_to_string(dir.join(name)).unwrap();
    let private = text.lines().find(|l| l.starts_with("AGE-SECRET-KEY-"));
    let public = run(dir, "age-keygen", &["-y", name]).trim().to_string();
    (private.unwrap().to_string(), public)
}

/// `plain` encrypted with sops for `public`, with its own config so nothing up the tree applies.
fn encrypt(dir: &Path, public: &str, plain: &str) -> String {
    let rules = format!("creation_rules:\n  - path_regex: '.*'\n    age: {public}\n");
    std::fs::write(dir.join("test.sops.yaml"), rules).unwrap();
    std::fs::write(dir.join("plain.yaml"), plain).unwrap();
    let args = "--config test.sops.yaml encrypt --unencrypted-suffix _unencrypted plain.yaml";
    run(dir, "sops", &args.split(' ').collect::<Vec<_>>())
}

const WHAT: &str = "shared:prod/app.sops.yaml";

fn dec(sops: &Path, text: &str, key: &str, keys: &[&str]) -> Result<Vec<(String, String)>, String> {
    let got = decrypt(sops.as_os_str(), text, key, keys, WHAT, DEADLINE);
    let got = got.map_err(|e| e.to_string())?;
    Ok(got
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect())
}

/// A valid sops file (its metadata is checked before sops runs).
fn ok_sops() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/secrets/sopsfile");
    std::fs::read_to_string(dir.join("ok.sops.yaml")).unwrap()
}

/// A fake `sops` in `dir` running `body` (mode 0755 unless `mode` says otherwise).
fn fake(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

const PLAIN: &str =
    "API_TOKEN: hoist-test-value-1\nmulti: |\n  line one\n  line two\nport_unencrypted: 8080\n";

#[test]
fn only_the_asked_keys_come_back() {
    if !tools() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (private, public) = identity(dir.path(), "k.txt");
    let file = encrypt(dir.path(), &public, PLAIN);
    let got = dec(
        Path::new("sops"),
        &file,
        &private,
        &["multi", "port_unencrypted"],
    );
    let want = [
        ("multi", "line one\nline two\n"),
        ("port_unencrypted", "8080"),
    ];
    assert_eq!(got.unwrap(), want.map(|(k, v)| (k.into(), v.into())));
    // sops prints `<` as `\u003c` (6 bytes for 1): a file full of them still fits the cap.
    let angles = "<".repeat(40_000);
    let file = encrypt(dir.path(), &public, &format!("a_unencrypted: '{angles}'\n"));
    let got = dec(Path::new("sops"), &file, &private, &["a_unencrypted"]);
    assert_eq!(got.unwrap(), [("a_unencrypted".into(), angles)]);
}

#[test]
fn failures_name_the_file_and_never_a_value_or_key() {
    if !tools() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (private, public) = identity(dir.path(), "k.txt");
    let (other, _) = identity(dir.path(), "other.txt");
    let file = encrypt(dir.path(), &public, PLAIN);
    let fails = |sops: &str, key: &str, keys: &[&str], want: &str| {
        let err = dec(Path::new(sops), &file, key, keys).unwrap_err();
        assert!(err.contains(want) && err.contains(WHAT), "{want}: {err}");
        assert!(
            !err.contains("hoist-test-value") && !err.contains("AGE-SECRET-KEY"),
            "{err}"
        );
    };
    fails("sops", &private, &["NOPE"], "key `NOPE`");
    fails("sops", &other, &["API_TOKEN"], "could not decrypt");
    fails(
        "/nonexistent/sops",
        &private,
        &["API_TOKEN"],
        "sops is not on PATH",
    );
}

/// The child gets the age key and nothing else of the parent's environment (no HOME, no
/// `SOPS_AGE_KEY_FILE`, no token), and the file on stdin.
#[test]
fn the_child_sees_only_the_age_key() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "#!/bin/sh\nenv > {0}/env\ncat > {0}/stdin\necho '{{\"A\": \"x\", \"B\": 1.5}}'\n",
        dir.path().display()
    );
    let fake = fake(dir.path(), "sops", &script, 0o755);
    let got = dec(&fake, &ok_sops(), "AGE-SECRET-KEY-1TEST", &["A", "B"]).unwrap();
    assert_eq!(got, [("A".into(), "x".into()), ("B".into(), "1.5".into())]);
    let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
    let names: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, _)| k)
        .collect();
    let allowed = ["SOPS_AGE_KEY", "PWD", "SHLVL", "_", "OLDPWD"]; // the shell adds the last ones
    assert!(names.iter().all(|n| allowed.contains(n)), "{names:?}");
    assert!(env.contains("SOPS_AGE_KEY=AGE-SECRET-KEY-1TEST"));
    let stdin = std::fs::read_to_string(dir.path().join("stdin")).unwrap();
    assert_eq!(stdin, ok_sops());
}

/// Metadata other than age (pgp, KMS, Vault) is refused before sops runs: sops would try it.
/// A sops that cannot start or is killed says so, without the recipient hint.
#[test]
fn the_file_is_checked_and_sops_failures_say_what_happened() {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran");
    let marker = format!("#!/bin/sh\ntouch {}\n", ran.display());
    let marker = fake(dir.path(), "m/sops", &marker, 0o755);
    let killed = fake(dir.path(), "k/sops", "#!/bin/sh\nkill -9 $$\n", 0o755);
    let junk = fake(dir.path(), "j/sops", "\x7fELF junk", 0o755);
    let pgp = ok_sops().replace("sops:\n", "sops:\n    pgp:\n        - fp: ABCD\n");
    let err =
        |sops: &Path, text: &str| dec(sops, text, "AGE-SECRET-KEY-1TEST", &["A"]).unwrap_err();
    let got = err(&marker, &pgp);
    assert!(got.contains("not a valid sops file"), "{got}");
    assert!(!ran.exists(), "sops ran on a file with pgp metadata");
    let got = err(&killed, &ok_sops());
    assert!(
        got.contains("killed by a signal") && !got.contains("recipient"),
        "{got}"
    );
    assert!(err(&junk, &ok_sops()).contains("could not start sops"));
    // A fake printing `out`: past the first buffer, a nested key not asked for, trailing data.
    let out = dir.path().join("out.json");
    let cat = format!("#!/bin/sh\ncat {}\n", out.display());
    let cat = fake(dir.path(), "c/sops", &cat, 0o755);
    let pad = "p".repeat(20_000);
    let json = format!(r#"{{"Z":{{"n":[1]}},"P":"{pad}","A":"x"}}"#);
    std::fs::write(&out, &json).unwrap();
    let got = dec(&cat, &ok_sops(), "AGE-SECRET-KEY-1TEST", &["A"]);
    assert_eq!(got.unwrap(), [("A".into(), "x".into())]);
    std::fs::write(&out, json + " x").unwrap();
    assert!(err(&cat, &ok_sops()).contains("did not print a JSON object"));
    std::fs::write(&out, r#"{"A": {"a": "b"}}"#).unwrap();
    assert!(err(&cat, &ok_sops()).contains("key `A` is not a single value"));
}

/// sops is looked up on absolute PATH entries only and must be executable; an absolute path is
/// used as it is.
#[test]
fn sops_is_found_on_absolute_executable_paths_only() {
    let dir = tempfile::tempdir().unwrap();
    let skipped = fake(dir.path(), "a/sops", "", 0o644);
    let found = fake(dir.path(), "b/sops", "", 0o755);
    // A relative entry naming a directory with an executable sops is still skipped.
    let relative = fake(dir.path(), "r/sops", "", 0o755);
    let up = std::env::current_dir().unwrap().components().count() - 1;
    let relative = "../".repeat(up) + relative.parent().unwrap().to_str().unwrap();
    assert!(Path::new(&relative).join("sops").is_file());
    let path = std::env::join_paths([
        Path::new(&relative),
        Path::new(""),
        skipped.parent().unwrap(),
        found.parent().unwrap(),
    ])
    .unwrap();
    let sops = OsStr::new("sops");
    assert_eq!(resolve(sops, Some(&path)), Some(found.clone()));
    assert_eq!(resolve(found.as_os_str(), None), Some(found));
    assert_eq!(resolve(skipped.as_os_str(), Some(&path)), None);
    assert_eq!(resolve(sops, None), None);
}

/// A sops that runs past the deadline, floods its output, or leaves a process holding its output
/// is killed with its whole process group, and the decrypt fails.
#[test]
fn a_stuck_or_runaway_sops_is_killed_with_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let slow = fake(dir.path(), "s/sops", "#!/bin/sh\nexec sleep 30\n", 0o755);
    let flood = fake(dir.path(), "f/sops", "#!/bin/sh\nexec yes\n", 0o755);
    let held = format!("#!/bin/sh\nsleep 30 &\necho $! > {}\n", pid.display());
    let held = fake(
        dir.path(),
        "h/sops",
        &(held + "echo '{\"A\": \"x\"}'\n"),
        0o755,
    );
    let closed = "#!/bin/sh\necho '{\"A\": \"x\"}'\nexec >&-\nexec sleep 30\n";
    let closed = fake(dir.path(), "c/sops", closed, 0o755);
    // The flood gets a long deadline, so the cap (not the clock) stops it even on a slow runner.
    let (short, long) = (Duration::from_millis(500), Duration::from_secs(8));
    for (sops, want, deadline) in [
        (&slow, "did not finish within", short),
        (&held, "did not finish within", short),
        (&closed, "did not finish within", short),
        (&flood, "printed more than", long),
    ] {
        let start = Instant::now();
        let got = decrypt(
            sops.as_os_str(),
            &ok_sops(),
            "AGE-SECRET-KEY-1TEST",
            &["A"],
            WHAT,
            deadline,
        );
        let err = got.unwrap_err().to_string();
        assert!(err.contains(want) && err.contains("killed"), "{err}");
        assert!(
            start.elapsed() < deadline + Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }
    // The grandchild that held the output was killed too: gone, or a zombie waiting for init.
    let pid = std::fs::read_to_string(&pid).unwrap();
    let stat = format!("/proc/{}/stat", pid.trim());
    let gone = || std::fs::read_to_string(&stat).map_or(true, |s| s.contains(") Z "));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gone() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !cfg!(target_os = "linux") || gone(),
        "the grandchild still runs"
    );
}
