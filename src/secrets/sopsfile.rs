//! A sops-encrypted YAML file read without decrypting (spec §15.4): its top-level keys, the age
//! recipients in `sops.age[].recipient`, and which values are not encrypted. Errors never quote
//! the file's content.

use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;
use yaml_serde::Value;

/// One sops-encrypted value: `ENC[AES256_GCM,data:…,iv:<12 bytes>,tag:<16 bytes>,type:…]`.
static ENC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^ENC\[AES256_GCM,data:[A-Za-z0-9+/=]*,iv:[A-Za-z0-9+/]{43}=,tag:[A-Za-z0-9+/]{22}==,",
        r"type:(str|int|float|bool|bytes|comment)\]$"
    ))
    .unwrap()
});

/// Values whose key ends with this are left in plaintext by sops on purpose; the file's
/// `sops.unencrypted_suffix` must say the same.
const UNENCRYPTED_SUFFIX: &str = "_unencrypted";

/// The only `sops:` metadata v1 accepts: age recipients, one suffix rule, no KMS/PGP/Vault.
const METADATA: [&str; 5] = [
    "age",
    "lastmodified",
    "mac",
    "unencrypted_suffix",
    "version",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SopsFile {
    /// Top-level keys in file order, without `sops`.
    pub keys: Vec<String>,
    /// The age public keys the data key is encrypted to.
    pub recipients: BTreeSet<String>,
    /// Keys whose value is not encrypted (and not `*_unencrypted`), then `comment on line N`
    /// for each plaintext comment: plaintext in the repo.
    pub plaintext: Vec<String>,
}

/// Read `text` as a flat sops YAML file. `Err` is a reason that never contains file content.
pub fn parse(text: &str) -> Result<SopsFile, String> {
    let doc: Value = yaml_serde::from_str(text).map_err(|e| match e.location() {
        Some(at) => format!("not valid YAML (line {})", at.line()),
        None => "not valid YAML".to_string(),
    })?;
    let Value::Mapping(map) = doc else {
        return Err("not a YAML mapping of KEY: value".into());
    };
    let Some(Value::Mapping(sops)) = map.get("sops") else {
        return Err("no sops metadata (`sops:` mapping); encrypt it with `sops`".into());
    };
    let is_enc = |v: Option<&Value>| matches!(v, Some(Value::String(s)) if ENC.is_match(s));
    if !is_enc(sops.get("mac")) {
        return Err("no sops MAC (`sops.mac`); the file was not written by sops".into());
    }
    for (name, value) in sops {
        let name = name
            .as_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        let empty = matches!(value, Value::Null) || value.as_sequence().is_some_and(Vec::is_empty);
        if !name.is_some_and(|n| METADATA.contains(&n)) && !empty {
            let what = name.map_or("a `sops` entry".into(), |n| format!("`sops.{n}`"));
            return Err(format!(
                "{what} is not supported in v1 (age recipients only)"
            ));
        }
    }
    if sops.get("unencrypted_suffix").and_then(Value::as_str) != Some(UNENCRYPTED_SUFFIX) {
        return Err(format!(
            "`sops.unencrypted_suffix` must be `{UNENCRYPTED_SUFFIX}`"
        ));
    }
    let mut recipients = BTreeSet::new();
    if let Some(Value::Sequence(age)) = sops.get("age") {
        for entry in age {
            if let Some(Value::String(r)) = entry.get("recipient") {
                recipients.insert(r.clone());
            }
        }
    }
    if recipients.is_empty() {
        return Err("no age recipients (`sops.age[].recipient`)".into());
    }
    let (mut keys, mut plaintext) = (Vec::new(), Vec::new());
    for (key, value) in &map {
        let Value::String(key) = key else {
            return Err("a top-level key is not a string".into());
        };
        if key == "sops" {
            continue;
        }
        if matches!(value, Value::Mapping(_) | Value::Sequence(_)) {
            return Err(format!(
                "`{key}` is not a flat value (secrets files are KEY: value)"
            ));
        }
        if !key.ends_with(UNENCRYPTED_SUFFIX) && !is_enc(Some(value)) {
            plaintext.push(key.clone());
        }
        keys.push(key.clone());
    }
    // sops leaves comments it did not encrypt in plaintext, outside the MAC.
    for (i, line) in text.lines().enumerate() {
        let comment = match line.trim_start() {
            l if l.starts_with('#') => Some(l),
            _ => line.find(" #").map(|at| &line[at + 1..]),
        };
        if comment.is_some_and(|c| !c.starts_with("#ENC[")) {
            plaintext.push(format!("comment on line {}", i + 1));
        }
    }
    Ok(SopsFile {
        keys,
        recipients,
        plaintext,
    })
}
