//! `hoist secrets exec` keeps no key or token in its environment (spec §15.7.1): the first run reads
//! them, hands them through a pipe to a fresh image of hoist started without them (`execve`, same
//! process), and that image reads them from the pipe. On Linux each image first makes itself
//! non-dumpable, so a same-user process (the command, or what it starts) cannot read its memory
//! or `/proc/<pid>/environ` either. The command still gets them in neither place.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::process::Command;
use zeroize::Zeroizing;

/// The variables that hold keys and tokens: the age key and what reads secrets repos.
pub const KEYS: [&str; 4] = [
    "SOPS_AGE_KEY",
    "SECRETS_READ_APP_KEY",
    "SECRETS_READ_APP_ID",
    "HOIST_SECRETS_TOKEN",
];

/// Set in the second image: its stdin is the pipe with the keys.
pub const STAGE: &str = "HOIST_SECRETS_EXEC_STAGE";

/// The handed-over pipe must not fill before the new image reads it: Linux may shrink a new
/// pipe to 8 KiB. A 4096-bit PEM and the rest take about 3.5 KB.
const MAX: usize = 8 * 1024;

pub type Keys = BTreeMap<&'static str, Zeroizing<String>>;

/// Makes this process non-dumpable (Linux), so same-user processes cannot read its memory or
/// environment. `execve` resets it, so each image calls this first.
pub fn harden() {
    #[cfg(target_os = "linux")]
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
}

/// Restarts hoist with the same arguments, without the key variables, and with them on stdin
/// (`NAME\0value\0` each). Returns only if that failed.
pub fn restart() -> String {
    let mut blob = Zeroizing::new(Vec::new());
    for name in KEYS {
        if let Some(value) = std::env::var_os(name) {
            let value = Zeroizing::new(value.into_vec());
            blob.extend_from_slice(name.as_bytes());
            blob.push(0);
            blob.extend_from_slice(&value);
            blob.push(0);
        }
    }
    if blob.len() > MAX {
        return "the key variables are over 8 KiB".into();
    }
    // On Linux the running binary itself, even if its path was replaced since.
    #[cfg(target_os = "linux")]
    let exe = std::io::Result::Ok(std::path::PathBuf::from("/proc/self/exe"));
    #[cfg(not(target_os = "linux"))]
    let exe = std::env::current_exe();
    let exe = match exe {
        Ok(exe) => exe,
        Err(e) => return format!("could not find hoist's own binary ({})", e.kind()),
    };
    let (reader, mut writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(e) => return format!("could not open a pipe ({})", e.kind()),
    };
    if let Err(e) = writer.write_all(&blob) {
        return format!("could not hand over the keys ({})", e.kind());
    }
    drop((writer, blob));
    let mut command = Command::new(exe);
    command
        .args(std::env::args_os().skip(1))
        .env(STAGE, "1")
        .stdin(reader);
    for name in KEYS {
        command.env_remove(name);
    }
    format!("could not restart hoist ({})", command.exec().kind())
}

/// The keys the first image handed over on stdin.
pub fn take() -> Result<Keys, String> {
    let mut blob = Zeroizing::new(Vec::with_capacity(MAX + 1));
    let read = std::io::stdin().take(MAX as u64 + 1).read_to_end(&mut blob);
    if read.is_err() || blob.len() > MAX {
        return Err("could not read the keys handed over by hoist".into());
    }
    let mut keys = Keys::new();
    let mut parts = blob.split(|b| *b == 0);
    while let (Some(name), Some(value)) = (parts.next(), parts.next()) {
        let Some(&name) = KEYS.iter().find(|k| k.as_bytes() == name) else {
            return Err("the keys handed over by hoist are malformed".into());
        };
        let value = std::str::from_utf8(value).map_err(|_| format!("{name} is not UTF-8"))?;
        keys.insert(name, Zeroizing::new(value.to_string()));
    }
    Ok(keys)
}
