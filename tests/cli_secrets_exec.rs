//! `hoist secrets exec` (spec §15.7): guards first, then one fetch per file, decrypt with sops, and
//! the command run with only the asked values, its output redacted. Real sops and a throwaway
//! age key made at runtime; GitHub is a fake server.
#![cfg(unix)]
mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::FakeApi;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const FILE: &str = "GET /repos/acme/secrets-shared/contents/prod/app.sops.yaml";
const MANIFEST: &str = r#"version = 1

[sources]
shared = { repo = "acme/secrets-shared" }

[env.prod]
API_TOKEN = "shared:prod/app.sops.yaml#API_TOKEN"
MULTI = "shared:prod/app.sops.yaml#multi"
OTHER = "shared:prod/app.sops.yaml#other"
ALIAS = "shared:prod/app.sops.yaml#API_TOKEN"
PATH = "shared:prod/app.sops.yaml#other"
PATH_PREFIX = "shared:prod/app.sops.yaml#other"

[env.prod.files]
CERT = "shared:prod/app.sops.yaml#multi"
"#;
const PLAIN: &str = "API_TOKEN: hoist-test-value-1\nmulti: |\n  hoist-test-line-one\n  hoist-test-line-two\nother: hoist-test-value-other\n";

/// A secrets file encrypted at runtime for a fresh age key, as a contents-API body.
struct Fixture {
    dir: tempfile::TempDir,
    private: String,
    body: String,
}

/// The stdout of `program args` in `dir`, which must succeed.
fn run_tool(dir: &Path, program: &str, args: &[&str]) -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", path);
    let out = cmd.output().expect(program);
    assert!(
        out.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `None` (skip, with a message) when sops or age-keygen is missing locally; CI must have them.
fn fixture() -> Option<Fixture> {
    let have = |p: &str| {
        run_tool(
            Path::new("/"),
            "sh",
            &["-c", &format!("command -v {p} || true")],
        )
    };
    if have("sops").is_empty() || have("age-keygen").is_empty() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must install sops and age-keygen"
        );
        eprintln!("skipped: sops or age-keygen is not on PATH (spec §15.9.1)");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secrets.toml"), MANIFEST).unwrap();
    run_tool(dir.path(), "age-keygen", &["-o", "k.txt"]);
    let key = std::fs::read_to_string(dir.path().join("k.txt")).unwrap();
    let private = key
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .to_string();
    let public = run_tool(dir.path(), "age-keygen", &["-y", "k.txt"]);
    let rules = format!(
        "creation_rules:\n  - path_regex: '.*'\n    age: {}\n",
        public.trim()
    );
    std::fs::write(dir.path().join("test.sops.yaml"), rules).unwrap();
    std::fs::write(dir.path().join("plain.yaml"), PLAIN).unwrap();
    let args = "--config test.sops.yaml encrypt --unencrypted-suffix _unencrypted plain.yaml";
    let sops = run_tool(dir.path(), "sops", &args.split(' ').collect::<Vec<_>>());
    let body = serde_json::json!({ "type": "file", "encoding": "base64", "content": STANDARD.encode(sops) });
    Some(Fixture {
        dir,
        private,
        body: body.to_string(),
    })
}

/// `hoist secrets exec --env prod --keys <keys> -- sh -c <script>` with a clean environment: a
/// CircleCI main-branch run, a given token and the age key, then `extra` (an empty value unsets).
fn exec(fx: &Fixture, api: &FakeApi, keys: &str, extra: &[(&str, &str)], script: &str) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("hoist"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let base = [
        ("HOIST_GITHUB_API", api.url()),
        ("HOIST_MAX_SLEEP_MS", "0".into()),
        ("CI", "true".into()),
        ("CIRCLECI", "true".into()),
        ("CIRCLE_BRANCH", "main".into()),
        ("SOPS_AGE_KEY", fx.private.clone()),
        ("HOIST_SECRETS_TOKEN", "ghs_given".into()),
        ("SECRETS_READ_APP_KEY", "not-a-key".into()),
    ];
    let extra = extra.iter().map(|(k, v)| (*k, v.to_string()));
    for (k, v) in base.into_iter().chain(extra) {
        if v.is_empty() {
            cmd.env_remove(k)
        } else {
            cmd.env(k, v)
        };
    }
    cmd.args(["secrets", "exec", "--env", "prod", "--keys", keys, "-C"])
        .arg(fx.dir.path());
    cmd.args(["--", "sh", "-c", script]).stdin(Stdio::null());
    cmd
}

fn text(out: &Output) -> (i32, String, String) {
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (
        out.status.code().unwrap_or(-1),
        s(&out.stdout),
        s(&out.stderr),
    )
}

fn no_secret(all: &str) {
    assert!(
        !all.contains("hoist-test-") && !all.contains("AGE-SECRET-KEY"),
        "{all}"
    );
}

#[test]
fn guards_refuse_before_any_request() {
    let fx = Fixture {
        dir: tempfile::tempdir().unwrap(),
        private: "AGE-SECRET-KEY-1X".into(),
        body: String::new(),
    };
    std::fs::write(fx.dir.path().join("secrets.toml"), MANIFEST).unwrap();
    let github = [
        ("CIRCLECI", ""),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "pull_request_target"),
    ];
    for extra in [
        &[("CI", "")][..],
        &[("CLAUDECODE", "1")],
        &[("CIRCLE_PR_USERNAME", "fork")],
        &github,
    ] {
        let api = FakeApi::start(&[]);
        let (code, out, err) = text(
            &exec(&fx, &api, "API_TOKEN", extra, "echo ran")
                .output()
                .unwrap(),
        );
        assert!(
            code == 2 && err.starts_with("error: secrets: ") && !out.contains("ran"),
            "{extra:?}: {err}"
        );
        assert!(api.requests().is_empty(), "{extra:?}");
    }
}

#[test]
fn the_command_gets_only_the_asked_values_and_its_exit_code_is_returned() {
    let Some(fx) = fixture() else { return };
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let script = r#"test "$API_TOKEN" = hoist-test-value-1 && test "$ALIAS" = "$API_TOKEN" && echo token-ok
printf '%s\n' "$MULTI" | grep -c hoist-test-line; env | cut -d= -f1 | sort; exit 7"#;
    let (code, out, err) = text(
        &exec(&fx, &api, "API_TOKEN,MULTI,ALIAS,PATH_PREFIX", &[], script)
            .output()
            .unwrap(),
    );
    assert_eq!(code, 7, "{err}");
    assert!(out.contains("token-ok\n2\n"), "{out}");
    let names: Vec<&str> = out.lines().collect();
    let set = ["API_TOKEN", "MULTI", "PATH_PREFIX"];
    assert!(set.iter().all(|n| names.contains(n)), "{out}");
    for gone in [
        "OTHER",
        "SOPS_AGE_KEY",
        "HOIST_SECRETS_TOKEN",
        "SECRETS_READ_APP_KEY",
    ] {
        assert!(!names.contains(&gone), "{gone} reached the command");
    }
    no_secret(&(out + &err));
    api.assert_only_get();
    let requests = api.requests();
    assert_eq!(requests.len(), 1, "one fetch per file");
    assert_eq!(requests[0].auth.as_deref(), Some("Bearer ghs_given"));
}

#[test]
fn output_is_redacted_and_values_are_masked_first_on_github() {
    let Some(fx) = fixture() else { return };
    let event = fx.dir.path().join("event.json");
    std::fs::write(&event, r#"{"repository":{"default_branch":"main","id":1}}"#).unwrap();
    let event = event.to_str().unwrap();
    let github = [
        ("CIRCLECI", ""),
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_EVENT_PATH", event),
    ];
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let script = r#"echo "token=$API_TOKEN"; printf %s "$API_TOKEN" | base64
echo "$API_TOKEN" | sed 's/-/%2D/g'; echo "$MULTI" >&2"#;
    let (code, out, err) = text(
        &exec(&fx, &api, "API_TOKEN,MULTI,ALIAS", &github, script)
            .output()
            .unwrap(),
    );
    assert_eq!(code, 0, "{err}");
    let masks: Vec<&str> = out
        .lines()
        .take_while(|l| l.starts_with("::add-mask::"))
        .collect();
    let want = [
        "hoist-test-value-1",
        "hoist-test-line-one",
        "hoist-test-line-two",
    ];
    assert_eq!(masks, want.map(|v| format!("::add-mask::{v}")), "{out}");
    let shown: String = out.lines().skip(masks.len()).collect::<Vec<_>>().join("\n");
    assert!(
        shown.starts_with("token=***") && shown.matches("***").count() >= 3,
        "{shown}"
    );
    assert!(err.matches("***").count() >= 2, "{err}");
    no_secret(&(shown + &err));
    assert!(
        !out.contains(&STANDARD.encode("hoist-test-value-1")),
        "{out}"
    );
}

#[test]
fn a_minted_token_is_revoked_before_the_command_runs() {
    let Some(fx) = fixture() else { return };
    let pem = fx.dir.path().join("app.pem");
    let pem_path = pem.to_str().unwrap();
    run_tool(
        fx.dir.path(),
        "openssl",
        &[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            pem_path,
        ],
    );
    let pem = std::fs::read_to_string(&pem).unwrap();
    let app = [
        ("HOIST_SECRETS_TOKEN", ""),
        ("SECRETS_READ_APP_ID", "12345"),
        ("SECRETS_READ_APP_KEY", pem.as_str()),
    ];
    let grant = r#"{"token":"ghs_minted","permissions":{"contents":"read","metadata":"read"},
      "repository_selection":"selected","repositories":[{"name":"secrets-shared"}]}"#;
    let ran = fx.dir.path().join("ran");
    let script = format!(
        "touch {}; env | cut -d= -f1; echo command-ran >&2",
        ran.display()
    );
    // A failed revoke is a warning, printed before the command starts; the command still runs.
    // A fetch that fails still revokes the token first (exit 3, the command never runs).
    for (status, file, warned) in [(204, 200, false), (500, 200, true), (204, 404, false)] {
        let api = FakeApi::start(&[
            (
                "GET /repos/acme/secrets-shared/installation",
                200,
                r#"{"id":42}"#,
                &[],
            ),
            ("POST /app/installations/42/access_tokens", 201, grant, &[]),
            (FILE, file, &fx.body, &[]),
            ("DELETE /installation/token", status, "", &[]),
        ]);
        let (got, out, err) = text(
            &exec(&fx, &api, "API_TOKEN", &app, &script)
                .output()
                .unwrap(),
        );
        assert_eq!(got, if file == 200 { 0 } else { 3 }, "{err}");
        let warning = err.find("could not revoke the App token");
        assert_eq!(warning.is_some(), warned, "{err}");
        assert!(file != 200 || warning < err.find("command-ran"), "{err}");
        let requests = api.requests();
        let revoke = requests
            .iter()
            .find(|r| r.method == "DELETE")
            .expect("revoked");
        assert_eq!(revoke.auth.as_deref(), Some("Bearer ghs_minted"));
        assert_eq!(ran.exists(), file == 200, "{err}");
        assert!(
            !out.lines().any(|l| l.starts_with("SECRETS_READ_APP")),
            "{out}"
        );
        let all = out + &err;
        assert!(
            !all.contains("ghs_minted") && !all.contains("PRIVATE KEY"),
            "{all}"
        );
        let _ = std::fs::remove_file(&ran);
    }
}

/// The command reads `/dev/null`, not hoist's stdin; when hoist cannot write its output, the
/// command is killed instead of blocking on a full pipe.
#[test]
fn stdin_is_closed_and_a_dead_stdout_stops_the_command() {
    let Some(fx) = fixture() else { return };
    // `cat` ends at once on /dev/null; on hoist's stdin (held open here) it would never end.
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut cmd = exec(&fx, &api, "API_TOKEN", &[], "cat; echo done");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _held = child.stdin.take();
    assert_eq!(
        exited(&mut child).and_then(|s| s.code()),
        Some(0),
        "the command read stdin"
    );
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    assert_eq!(out, "done\n");
    // A command that ignores SIGPIPE and write errors stops only when hoist kills it.
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let script = "trap '' PIPE; while :; do echo x; done 2>/dev/null";
    let mut cmd = exec(&fx, &api, "API_TOKEN", &[], script);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let status = exited(&mut child);
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(3),
        "hoist did not stop the command"
    );
    // A background process keeping the output open does not keep hoist after the command exits.
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut cmd = exec(&fx, &api, "API_TOKEN", &[], "sleep 30 & exit 5");
    let mut child = cmd.stdout(Stdio::null()).spawn().unwrap();
    let status = exited(&mut child);
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(5),
        "hoist waited for the background process"
    );
}

/// The exit status of `child` within 20 s, else `None` (and it is killed).
fn exited(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}

#[test]
fn errors_name_the_fix_and_never_a_value() {
    let Some(fx) = fixture() else { return };
    // A second throwaway key, not a recipient of the file.
    run_tool(fx.dir.path(), "age-keygen", &["-o", "other.txt"]);
    let other = std::fs::read_to_string(fx.dir.path().join("other.txt")).unwrap();
    let other = other
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap();
    let cases = [
        ("NOPE", vec![], "`NOPE` is not in [env.prod]"),
        ("API_TOKEN,API_TOKEN", vec![], "`API_TOKEN` is asked twice"),
        ("PATH", vec![], "`PATH` may not be set"),
        ("NODE_OPTIONS", vec![], "`NODE_OPTIONS` may not be set"),
        ("GITHUB_TOKEN", vec![], "`GITHUB_TOKEN` may not be set"),
        ("PYTHONPATH", vec![], "`PYTHONPATH` may not be set"),
        ("BASH_ENV", vec![], "`BASH_ENV` may not be set"),
        (
            "JAVA_TOOL_OPTIONS",
            vec![],
            "`JAVA_TOOL_OPTIONS` may not be set",
        ),
        ("HTTPS_PROXY", vec![], "`HTTPS_PROXY` may not be set"),
        (
            "NPM_CONFIG_REGISTRY",
            vec![],
            "`NPM_CONFIG_REGISTRY` may not be set",
        ),
        // Only hoist sets this: with the keys still in the environment, it is refused.
        (
            "API_TOKEN",
            vec![("HOIST_SECRETS_EXEC_STAGE", "1")],
            "HOIST_SECRETS_EXEC_STAGE",
        ),
        ("API_TOKEN", vec![("SOPS_AGE_KEY", "")], "SOPS_AGE_KEY"),
        (
            "API_TOKEN",
            vec![("SOPS_AGE_KEY", other)],
            "could not decrypt",
        ),
    ];
    for (keys, extra, want) in cases {
        let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
        let (code, out, err) = text(&exec(&fx, &api, keys, &extra, "echo ran").output().unwrap());
        assert!(
            code == 3 && err.contains(want) && !out.contains("ran"),
            "{want}: {err}"
        );
        no_secret(&(out + &err));
    }
}

/// hoist restarts itself with the age key, App key and token passed through a pipe, not its
/// environment, and makes itself non-dumpable: the command cannot read them from
/// `/proc/<hoist>/environ` (owned by root once hoist is not dumpable).
#[test]
#[cfg(target_os = "linux")]
fn hoist_keeps_no_key_or_token_in_its_environment() {
    let Some(fx) = fixture() else { return };
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let script =
        r#"stat -c 'owner=%u' /proc/$PPID/environ; tr '\0' '\n' < /proc/$PPID/environ || true"#;
    let extra = [("SECRETS_READ_APP_ID", "12345")];
    let (code, out, err) = text(
        &exec(&fx, &api, "API_TOKEN", &extra, script)
            .output()
            .unwrap(),
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("owner=0\n"), "{out}");
    for name in [
        "SOPS_AGE_KEY=",
        "SECRETS_READ_APP_KEY=",
        "SECRETS_READ_APP_ID=",
        "HOIST_SECRETS_TOKEN=",
    ] {
        assert!(
            !out.lines().any(|l| l.starts_with(name)),
            "{name} in hoist's environ: {out}"
        );
    }
    for value in [fx.private.as_str(), "ghs_given", "not-a-key"] {
        assert!(!out.contains(value), "a key or token in hoist's environ");
    }
}

/// A file secret is written under `/dev/shm/hoist-*` (dir 0700, file 0600); the variable holds
/// its path and the directory is gone after the command exits, also after a SIGTERM to hoist,
/// which is passed on to the command.
#[test]
#[cfg(target_os = "linux")]
fn file_secrets_live_in_shm_and_go_away_also_on_sigterm() {
    let Some(fx) = fixture() else { return };
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let script = r#"test "$(cat "$CERT")" = "$(printf 'hoist-test-line-one\nhoist-test-line-two')" && echo file-ok
stat -c '%a' "$CERT" "$(dirname "$CERT")"; echo "$CERT""#;
    let (code, out, err) = text(
        &exec(&fx, &api, "API_TOKEN,CERT", &[], script)
            .output()
            .unwrap(),
    );
    assert_eq!(code, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[..3], ["file-ok", "600", "700"], "{out}");
    assert!(lines[3].starts_with("/dev/shm/hoist-"), "{out}");
    assert!(
        !Path::new(lines[3]).exists(),
        "the file secret outlived the command"
    );
    // SIGTERM to hoist: the command gets it, and the directory still goes.
    let ready = fx.dir.path().join("ready");
    let script = format!(
        "trap 'echo got-term; exit 9' TERM; echo \"$CERT\" > {}; while :; do sleep 0.1; done",
        ready.display()
    );
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut cmd = exec(&fx, &api, "CERT", &[], &script);
    let mut child = cmd.stdout(Stdio::piped()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while !ready.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let path = std::fs::read_to_string(&ready).unwrap();
    let pid = child.id().to_string();
    Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert_eq!(exited(&mut child).and_then(|s| s.code()), Some(9));
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    assert_eq!(out, "got-term\n");
    assert!(
        !Path::new(path.trim()).exists(),
        "the file secret outlived a SIGTERM"
    );
    // A command that ignores the signal is killed a few seconds later; the files still go.
    std::fs::remove_file(&ready).unwrap();
    let script = format!(
        "trap '' TERM; echo \"$CERT\" > {}; while :; do sleep 0.1; done",
        ready.display()
    );
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut child = exec(&fx, &api, "CERT", &[], &script)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while !ready.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let path = std::fs::read_to_string(&ready).unwrap();
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(exited(&mut child).and_then(|s| s.code()), Some(137));
    assert!(
        !Path::new(path.trim()).exists(),
        "the file secret outlived an ignored SIGTERM"
    );
}

/// A command that closes its stdout and stderr (a daemon) and ignores SIGTERM: hoist still passes
/// the signal on, kills the group a few seconds later and removes the files.
#[test]
#[cfg(target_os = "linux")]
fn a_command_without_output_still_gets_signals() {
    let Some(fx) = fixture() else { return };
    let ready = fx.dir.path().join("ready");
    let script = format!(
        "echo \"$CERT\" > {}; exec >/dev/null 2>&1; trap '' TERM; while :; do sleep 0.1; done",
        ready.display()
    );
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut child = exec(&fx, &api, "CERT", &[], &script)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while !ready.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300)); // past `exec >/dev/null`
    let path = std::fs::read_to_string(&ready).unwrap();
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert_eq!(exited(&mut child).and_then(|s| s.code()), Some(137));
    assert!(
        !Path::new(path.trim()).exists(),
        "the file secret outlived the command"
    );
}

/// Only hoist sets the stage marker; set by hand (also empty) next to a key, it is refused.
#[test]
fn a_faked_stage_marker_is_refused() {
    let fx = Fixture {
        dir: tempfile::tempdir().unwrap(),
        private: "AGE-SECRET-KEY-1X".into(),
        body: String::new(),
    };
    std::fs::write(fx.dir.path().join("secrets.toml"), MANIFEST).unwrap();
    for value in ["1", ""] {
        let api = FakeApi::start(&[]);
        let mut cmd = exec(&fx, &api, "API_TOKEN", &[], "echo ran");
        let (code, out, err) = text(&cmd.env("HOIST_SECRETS_EXEC_STAGE", value).output().unwrap());
        assert!(
            code == 3 && err.contains("HOIST_SECRETS_EXEC_STAGE") && !out.contains("ran"),
            "{value:?}: {err}"
        );
        assert!(api.requests().is_empty());
    }
}

#[test]
fn file_secrets_need_shm() {
    let err = hoist::secrets::shm::Dir::create(Path::new("/nonexistent-shm"))
        .err()
        .unwrap();
    assert!(err.contains("/nonexistent-shm"), "{err}");
}

/// When hoist cannot write the output, the command's whole process group is killed, not only the
/// command: a process it started does not keep running.
#[test]
#[cfg(target_os = "linux")]
fn a_dead_stdout_kills_the_whole_command_group() {
    let Some(fx) = fixture() else { return };
    let pid = fx.dir.path().join("pid");
    let script = format!(
        "sh -c 'trap \"\" PIPE; while :; do echo x; done 2>/dev/null' & echo $! > {}; wait",
        pid.display()
    );
    let api = FakeApi::start(&[(FILE, 200, &fx.body, &[])]);
    let mut cmd = exec(&fx, &api, "API_TOKEN", &[], &script);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert_eq!(exited(&mut child).and_then(|s| s.code()), Some(3));
    let stat = format!(
        "/proc/{}/stat",
        std::fs::read_to_string(&pid).unwrap().trim()
    );
    let end = Instant::now() + Duration::from_secs(5);
    let gone = || std::fs::read_to_string(&stat).map_or(true, |s| s.contains(") Z "));
    while !gone() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(gone(), "a process of the command's group still runs");
}
