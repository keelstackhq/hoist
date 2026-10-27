//! A sops-encrypted YAML file read without decrypting (spec §15.4): its top-level keys, the age
//! recipients in `sops.age[].recipient`, and which values are not encrypted. Errors never quote
//! the file's content.

use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;
use yaml_serde::Value;

/// One sops-encrypted value: `ENC[AES256_GCM,data:…,iv:…,tag:…,type:…]`.
static ENC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ENC\[AES256_GCM,data:[A-Za-z0-9+/=]*,iv:[A-Za-z0-9+/=]+,tag:[A-Za-z0-9+/=]+,type:[a-z]+\]$")
        .unwrap()
});

/// Values whose key ends with this are left in plaintext by sops on purpose.
const UNENCRYPTED_SUFFIX: &str = "_unencrypted";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SopsFile {
    /// Top-level keys in file order, without `sops`.
    pub keys: Vec<String>,
    /// The age public keys the data key is encrypted to.
    pub recipients: BTreeSet<String>,
    /// Keys whose value is not encrypted (and not `*_unencrypted`): plaintext in the repo.
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
    Ok(SopsFile {
        keys,
        recipients,
        plaintext,
    })
}
