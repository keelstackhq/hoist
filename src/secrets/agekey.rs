//! A new age key for `hoist secrets grant` (spec §15.4): `age-keygen` (found on the absolute
//! `PATH` entries) with an empty environment prints it on stdout, which hoist reads into zeroized
//! memory. The private key is never printed, logged or written to disk.

use super::access::AGE;
use super::decrypt::resolve;
use crate::auth::Secret;
use crate::error::HoistError;
use std::io::Read;
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

pub struct AgeKey {
    pub private: Secret,
    pub public: String,
}

pub fn generate() -> Result<AgeKey, HoistError> {
    let fail = |why: &str| HoistError::Usage(format!("secrets: {why}"));
    let program = resolve("age-keygen".as_ref(), std::env::var_os("PATH").as_deref())
        .ok_or_else(|| fail("age-keygen is not on PATH; install age 1.3.2 (spec §15.9.1)"))?;
    let mut child = Command::new(program)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(&format!("could not start age-keygen ({})", e.kind())))?;
    let mut out = Zeroizing::new(Vec::with_capacity(512));
    let read = child.stdout.take().map(|mut o| o.read_to_end(&mut out));
    let ok = child.wait().is_ok_and(|s| s.success()) && matches!(read, Some(Ok(_)));
    let text = std::str::from_utf8(&out).ok().filter(|_| ok);
    let lines = || text.into_iter().flat_map(str::lines);
    let public = lines().find_map(|l| l.strip_prefix("# public key: "));
    let private = lines().find(|l| l.starts_with("AGE-SECRET-KEY-1"));
    match (public.filter(|p| AGE.is_match(p)), private) {
        (Some(public), Some(private)) => Ok(AgeKey {
            private: Secret::from(private),
            public: public.to_string(),
        }),
        _ => Err(fail("age-keygen did not print a key")),
    }
}
