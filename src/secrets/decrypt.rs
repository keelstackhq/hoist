//! Decrypting one secrets file for `hoist secrets exec` (spec §15.7.1 step 4). The file's metadata
//! is checked first (age only, so sops tries no PGP, KMS or Vault). `sops decrypt` then reads the
//! file on stdin and gets `SOPS_AGE_KEY` as its only environment variable (no `HOME`, so no key
//! file or config of the machine applies). Its JSON output is read into a buffer that is wiped
//! when it grows or drops, and only the asked keys are copied out, zeroized on drop; the other
//! values are skipped, never copied. Not wiped: the pipes, serde_json's escape buffer, and the
//! copy of the key that the OS keeps for the child. sops' own messages are not shown: they could
//! quote the file.

use crate::error::HoistError;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

/// `program` as an absolute path: as it is, or found on the absolute entries of `path` (the
/// child's environment is empty, so it cannot be searched there). It must be an executable file.
pub fn resolve(program: &OsStr, path: Option<&OsStr>) -> Option<PathBuf> {
    let exec = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let program = Path::new(program);
    if program.is_absolute() {
        return exec(program).then(|| program.to_path_buf());
    }
    std::env::split_paths(path?)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(program))
        .find(|p| exec(p))
}

fn fail(what: &str, why: &str) -> HoistError {
    HoistError::Usage(format!("secrets: {what}: {why}"))
}

/// The values of `keys` in the sops file `text` (named `what` in errors), decrypted by `sops` with
/// `age_key`. A scalar that is not a string (a number, `true`) comes back as its JSON text.
pub fn decrypt(
    sops: &OsStr,
    text: &str,
    age_key: &str,
    keys: &[&str],
    what: &str,
) -> Result<BTreeMap<String, Zeroizing<String>>, HoistError> {
    super::sopsfile::parse(text)
        .map_err(|why| fail(what, &format!("not a valid sops file: {why}")))?;
    let program = resolve(sops, std::env::var_os("PATH").as_deref()).ok_or_else(|| {
        fail(
            what,
            "sops is not on PATH; install sops 3.13.3 (spec §15.9.1)",
        )
    })?;
    let mut child = Command::new(program)
        .args([
            "decrypt",
            "--input-type",
            "yaml",
            "--output-type",
            "json",
            "/dev/stdin",
        ])
        .env_clear()
        .env("SOPS_AGE_KEY", age_key)
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail(what, &format!("could not start sops ({})", e.kind())))?;
    let (mut stdin, mut stdout) = (child.stdin.take(), child.stdout.take());
    // Feed stdin while reading stdout, so a large file cannot fill both pipes.
    let read = std::thread::scope(|s| {
        s.spawn(|| stdin.take().map(|mut i| i.write_all(text.as_bytes())));
        stdout.as_mut().map(|o| read_all(o, 2 * text.len() + 4096))
    });
    let status = child
        .wait()
        .map_err(|_| fail(what, "sops did not finish"))?;
    let Some(code) = status.code() else {
        return Err(fail(what, "sops was killed by a signal"));
    };
    let out = match read {
        Some(Ok(out)) if code == 0 => out,
        _ if code == 0 => return Err(fail(what, "could not read sops' output")),
        _ => {
            let hint = "is this runner's key a recipient? (`hoist secrets check`)";
            let why = format!("sops could not decrypt it (exit {code}); {hint}");
            return Err(fail(what, &why));
        }
    };
    let mut de = serde_json::Deserializer::from_slice(&out);
    let picked = (&mut de)
        .deserialize_map(Pick(keys))
        .and_then(|p| de.end().map(|()| p))
        .map_err(|_| fail(what, "sops did not print a JSON object"))?;
    let mut values = BTreeMap::new();
    for &key in keys {
        let value = match picked.get(key) {
            Some(Some(v)) => v.clone(),
            Some(None) => return Err(fail(what, &format!("key `{key}` is not a single value"))),
            None => return Err(fail(what, &format!("key `{key}` is not in it"))),
        };
        values.insert(key.to_string(), value);
    }
    Ok(values)
}

/// All of `r`, in a buffer that is wiped whenever it grows (a `Vec` would free the old one as is).
fn read_all(r: &mut impl Read, cap: usize) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut buf = Zeroizing::new(vec![0; cap.max(1)]);
    let mut len = 0;
    loop {
        if len == buf.len() {
            let mut bigger = Zeroizing::new(vec![0; 2 * len]);
            bigger[..len].copy_from_slice(&buf[..len]);
            buf = bigger;
        }
        match r.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(len);
    Ok(buf)
}

/// The asked keys of a JSON object: `Some` value for a scalar, `None` for null, a list or a map.
struct Pick<'k>(&'k [&'k str]);
type Picked = BTreeMap<String, Option<Zeroizing<String>>>;

impl<'de> Visitor<'de> for Pick<'_> {
    type Value = Picked;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Picked, A::Error> {
        let mut picked = Picked::new();
        while let Some(key) = map.next_key::<String>()? {
            if self.0.contains(&key.as_str()) {
                picked.insert(key, map.next_value_seed(Scalar)?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(picked)
    }
}

/// `visit_*` methods that keep the value as its text.
macro_rules! text {
    ($($visit:ident: $t:ty),*) => {$(
        fn $visit<E>(self, v: $t) -> Result<Self::Value, E> {
            Ok(Some(Zeroizing::new(v.to_string())))
        }
    )*};
}

/// One value as text, zeroized; `None` for anything but a string, number or bool.
struct Scalar;

impl<'de> DeserializeSeed<'de> for Scalar {
    type Value = Option<Zeroizing<String>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Scalar {
    type Value = Option<Zeroizing<String>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a value")
    }
    text!(visit_str: &str, visit_bool: bool, visit_i64: i64, visit_u64: u64);
    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        // The same text serde_json's `Value` printed.
        Ok(serde_json::Number::from_f64(v).map(|n| Zeroizing::new(n.to_string())))
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}
