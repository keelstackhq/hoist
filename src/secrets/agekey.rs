//! A new age key for `hoist secrets grant` (spec §15.4): `age-keygen` (found on the absolute
//! `PATH` entries) with an empty environment prints it on stdout, which hoist reads into zeroized
//! memory. The private key is never printed, logged or written to disk.

use super::access::AGE;
use super::decrypt::resolve;
use crate::auth::Secret;
use crate::error::HoistError;
use regex::Regex;
use std::ffi::OsStr;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use zeroize::Zeroizing;

static PRIVATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^AGE-SECRET-KEY-1[QPZRY9X8GF2TVDW0S3JN54KHCE6MUA7L]{58}$").unwrap()
});

/// What hoist reads of age-keygen's output; it prints about 190 bytes.
const CAP: usize = 8192;

pub struct AgeKey {
    pub private: Secret,
    pub public: String,
}

pub fn generate() -> Result<AgeKey, HoistError> {
    generate_from(std::env::var_os("PATH").as_deref())
}

/// [`generate`] with `age-keygen` looked up on `path`.
pub fn generate_from(path: Option<&OsStr>) -> Result<AgeKey, HoistError> {
    let fail = |why: &str| HoistError::Usage(format!("secrets: {why}"));
    let program = resolve("age-keygen".as_ref(), path)
        .ok_or_else(|| fail("age-keygen is not on PATH; install age 1.3.2 (spec §15.9.1)"))?;
    let mut child = Command::new(program)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(&format!("could not start age-keygen ({})", e.kind())))?;
    // Read short of the capacity, so the buffer never grows: a grown Vec frees the old one
    // without zeroizing it. The pipe closes after the read, so a longer output cannot block.
    let mut out = Zeroizing::new(Vec::with_capacity(CAP));
    let read = child
        .stdout
        .take()
        .map(|o| o.take(CAP as u64 - 1).read_to_end(&mut out));
    let status = child.wait();
    if out.len() >= CAP - 1 {
        return Err(fail("age-keygen printed too much; expected one key"));
    }
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            let code = s.code().map_or("a signal".into(), |c| format!("exit {c}"));
            return Err(fail(&format!("age-keygen failed ({code})")));
        }
        Err(e) => return Err(fail(&format!("age-keygen did not finish ({})", e.kind()))),
    }
    let text = std::str::from_utf8(&out)
        .ok()
        .filter(|_| matches!(read, Some(Ok(_))));
    let lines = || text.into_iter().flat_map(str::lines);
    let public = lines().find_map(|l| l.strip_prefix("# public key: "));
    let private = lines().find(|l| l.starts_with("AGE-SECRET-KEY-"));
    match (
        public.filter(|p| AGE.is_match(p)),
        private.filter(|p| PRIVATE.is_match(p)),
    ) {
        (Some(public), Some(private)) => Ok(AgeKey {
            private: Secret::from(private),
            public: public.to_string(),
        }),
        _ => Err(fail("age-keygen printed no key in the expected format")),
    }
}
