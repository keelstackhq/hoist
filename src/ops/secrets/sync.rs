//! `hoist secrets sync [--rotate]` in a secrets repo (spec §15.4, §15.5): writes `.sops.yaml` from
//! `access.toml`, then runs `sops updatekeys -y` on every file whose recipients differ from its
//! rule; `--rotate` also gives every file a new data key (`sops rotate -i`). sops opens each data
//! key with the owner's own age key (its usual key file or `SOPS_AGE_KEY`); hoist never reads it.
//! Owner-only: refused in an agent session.

use super::check::{item, read_regular, walk};
use crate::error::HoistError;
use crate::report::{Action, HumanStep, Item, Report};
use crate::secrets::access::{Access, sops_yaml};
use crate::secrets::decrypt::{DEADLINE, resolve};
use crate::secrets::sopsfile;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

pub(super) fn report(command: &str, items: Vec<Item>, human_steps: Vec<HumanStep>) -> Report {
    // A write that worked is not drift: 1 only for what could not be synced.
    let drift = items.iter().any(|i| i.action == Action::Drift);
    let exit_code = if drift {
        1
    } else {
        2 * i32::from(!human_steps.is_empty())
    };
    Report {
        command: command.into(),
        exit_code,
        items,
        human_steps,
        text: None,
        hint: None,
    }
}

/// An owner-only command refused in an agent session (exit 2, a human step), or `None`.
pub(super) fn owner_only(command: &str) -> Option<Report> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let agent = set("CLAUDECODE") || set("CLAUDE_CODE_REMOTE");
    agent.then(|| {
        let step = HumanStep {
            id: "owner-only".into(),
            title: format!("run `hoist {command}` yourself, not in an agent session"),
            url: String::new(),
            status: "pending".into(),
        };
        report(command, Vec::new(), vec![step])
    })
}

pub fn secrets_sync(dir: &Path, rotate: bool) -> Result<Report, HoistError> {
    if let Some(refused) = owner_only("secrets sync") {
        return Ok(refused);
    }
    let usage = |msg: &str| Err(HoistError::Usage(format!("{}: {msg}", dir.display())));
    let Some(text) = read_regular(&dir.join("access.toml")) else {
        return usage("no access.toml here (a secrets repo); `secrets sync` writes its .sops.yaml");
    };
    let access = Access::parse(&text, "access.toml", crate::timeutil::now_secs())?;
    let Some(sops) = resolve("sops".as_ref(), std::env::var_os("PATH").as_deref()) else {
        return usage("sops is not on PATH; install sops 3.13.3 (spec §15.9.1)");
    };
    let mut items = vec![write_config(dir, &access)];
    let mut human = Vec::new();
    if items[0].action == Action::Drift {
        return Ok(report("secrets sync", items, Vec::new()));
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    for (rel, odd) in files {
        // Anything sync cannot open or enter (as `secrets check` says) may hold a sops file
        // that keeps its old recipients.
        if let Some(why) = odd {
            let why = format!("{why}; not re-keyed (`hoist secrets check`)");
            items.push(item("file", &rel, Action::Drift, why, &rel));
            continue;
        }
        let sops_file = odd.is_none() && rel.ends_with(".sops.yaml") && rel != ".sops.yaml";
        if !sops_file {
            continue; // `secrets check` reports the rest
        }
        // A file no rule covers would keep its old recipients and data key.
        let Some(rule) = access.rule_for(&rel) else {
            let why = "outside every rule; not re-keyed (`hoist secrets check`)";
            items.push(item("file", &rel, Action::Drift, why, &rel));
            continue;
        };
        let want: BTreeSet<&str> = access.public_keys(rule).into_iter().collect();
        // Whether the file's recipients differ from the rule, and how many it would lose.
        let differs = |text: &str| match sopsfile::parse(text) {
            Ok(f) if f.plaintext.is_empty() => Ok((
                f.recipients
                    .iter()
                    .map(String::as_str)
                    .ne(want.iter().copied()),
                f.recipients
                    .iter()
                    .filter(|r| !want.contains(r.as_str()))
                    .count(),
            )),
            Ok(f) => Err(format!("plaintext: {}", f.plaintext.join(", "))),
            Err(why) => Err(why),
        };
        let file = |action, why: String| item("file", &rel, action, why, &rel);
        let before = read_regular(&dir.join(&rel)).ok_or_else(|| "cannot read it".to_string());
        let (changed, removed) = match before.and_then(|t| differs(&t)) {
            Ok(found) => found,
            Err(why) => {
                items.push(file(Action::Drift, format!("not changed: {why}")));
                continue;
            }
        };
        let steps = [(changed, ["updatekeys", "-y"]), (rotate, ["rotate", "-i"])];
        let steps: Vec<_> = steps
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, args)| args)
            .collect();
        let rule_name = &rule.path;
        if steps.is_empty() {
            items.push(file(
                Action::Ok,
                format!("recipients match rule `{rule_name}`"),
            ));
            continue;
        }
        let failed = steps
            .iter()
            .find_map(|args| run(&sops, dir, args, &rel).err());
        let after = read_regular(&dir.join(&rel)).map(|t| differs(&t));
        let did = ["recipients updated", "new data key"][usize::from(rotate)];
        items.push(match (failed, after) {
            (Some(why), _) => file(Action::Drift, why),
            (None, Some(Ok((false, _)))) => {
                // A removed recipient read these values (git keeps the old ciphertext). A new
                // data key is the usual follow-up to one, so it keeps the reminder too.
                if removed > 0 || rotate {
                    human.push(rotate_step(&rel, rotate, removed > 0));
                }
                file(Action::Update, format!("{did} for rule `{rule_name}`"))
            }
            _ => file(
                Action::Drift,
                "sops ran, but the recipients still differ from the rule".into(),
            ),
        });
    }
    Ok(report("secrets sync", items, human))
}

/// A removed recipient could read the file's values (git keeps the old ciphertext): they must be
/// rotated, after a new data key when this run did not make one. A new data key alone says the
/// same, as the run that removed the recipient may have been an earlier one.
fn rotate_step(rel: &str, rotated: bool, removed: bool) -> HumanStep {
    let title = match (rotated, removed) {
        (true, false) => format!(
            "{rel} has a new data key: if a recipient was removed, rotate the values it could read"
        ),
        (true, true) => {
            format!("a recipient was removed from {rel}: rotate the values it could read")
        }
        (false, _) => format!(
            "a recipient was removed from {rel}: run `hoist secrets sync --rotate`, then rotate \
             the values it could read"
        ),
    };
    HumanStep {
        id: format!("rotate:{rel}"),
        title,
        url: String::new(),
        status: "pending".into(),
    }
}

/// `.sops.yaml` generated from `access.toml`: written unless it already matches.
fn write_config(dir: &Path, access: &Access) -> Item {
    let path = dir.join(".sops.yaml");
    let want = sops_yaml(access);
    let item = |action, why: &str| item("sops-config", ".sops.yaml", action, why, "access.toml");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return item(Action::Drift, "not a regular file; not written");
    }
    let action = match read_regular(&path) {
        Some(have) if have == want => return item(Action::Ok, "matches access.toml"),
        Some(_) => Action::Update,
        None => Action::Create,
    };
    match replace(&path, want.as_bytes()) {
        Ok(()) => item(action, "written from access.toml"),
        Err(e) => item(Action::Drift, &format!("could not write it ({})", e.kind())),
    }
}

/// Writes `text` to a new file next to `path` (0644), then renames it over `path`: a reader sees
/// the old file or the new one, and a link to the old one keeps its content.
pub(super) fn replace(path: &Path, text: &[u8]) -> std::io::Result<()> {
    use ring::rand::{SecureRandom, SystemRandom};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut random = [0u8; 8];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| std::io::Error::other("no randomness"))?;
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!("{name}.tmp-{hex}"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&tmp)?;
    let written = file.write_all(text).and_then(|()| file.sync_all());
    match written.and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// `sops --config .sops.yaml <args> <rel>` in `dir` with the owner's environment (their key);
/// its output is not shown: sops' errors can quote the file.
fn run(sops: &Path, dir: &Path, args: &[&str; 2], rel: &str) -> Result<(), String> {
    let status = Command::new(sops)
        .args(["--config", ".sops.yaml"])
        .args(args)
        .arg("--")
        .arg(rel)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "sops {} failed (exit {}); is your own age key a recipient? (SOPS_AGE_KEY or its key file)",
            args[0],
            s.code().map_or("a signal".into(), |c| c.to_string())
        )),
        Err(e) => Err(format!("could not start sops ({})", e.kind())),
    }
}

/// `sync` after a command wrote access.toml: its items and steps, or, when it fails, one drift
/// item, since what was written stays written.
pub(super) fn sync_after_write(dir: &Path, rotate: bool) -> (Vec<Item>, Vec<HumanStep>) {
    match secrets_sync(dir, rotate) {
        Ok(r) => (r.items, r.human_steps),
        Err(e) => {
            let run = ["hoist secrets sync", "hoist secrets sync --rotate"][usize::from(rotate)];
            let why = format!("{e}; then run `{run}` and commit");
            let failed = item("sync", ".sops.yaml", Action::Drift, why, "access.toml");
            (vec![failed], Vec::new())
        }
    }
}

/// What to run next: the git commands to commit access.toml, .sops.yaml and every file `items`
/// re-keyed on `branch`, then a PR (hoist runs no git); with drift in `items`, fix it first.
pub(super) fn commit_hint(dir: &Path, branch: &str, message: &str, items: &[Item]) -> Box<str> {
    if items.iter().any(|i| i.action == Action::Drift) {
        return "next: fix the drift above and run `hoist secrets sync`; then commit access.toml, \
                .sops.yaml and the re-keyed files on a branch and open a PR\n"
            .into();
    }
    let files: Vec<String> = items
        .iter()
        .filter(|i| i.kind == "file" && i.action == Action::Update)
        .map(|i| sh(&i.key))
        .collect();
    let git = format!("git -C {}", sh(&dir.display().to_string()));
    let message = sh(message);
    format!(
        "next: hoist ran no git; commit (with every file sync re-keyed) and open a PR \
         yourself:\n  {git} switch -c {branch}\n  {git} add access.toml .sops.yaml {}\n  \
         {git} commit -m {message}\n  {git} push -u origin {branch}\nthen open a PR from \
         {branch}\n",
        files.join(" ")
    )
    .into()
}

/// `s` in single quotes for a POSIX shell.
fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Whether the owner's own age key (SOPS_AGE_KEY or its key file) opens `rel`: checked before a
/// command writes anything it could only finish by re-keying. The plaintext goes nowhere.
pub(super) fn owner_opens(dir: &Path, rel: &str) -> bool {
    let sops = resolve("sops".as_ref(), std::env::var_os("PATH").as_deref());
    sops.is_some_and(|sops| crate::secrets::decrypt::opens(&sops, dir, rel, DEADLINE))
}
