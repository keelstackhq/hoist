//! Secrets repos (sops + age; design: keelstackhq/hull
//! `docs/superpowers/specs/2026-10-27-secrets-and-ci-providers-design.md` §3).

pub mod access;
pub mod decrypt;
pub mod fetch;
pub mod guard;
pub mod manifest;
pub mod redact;
pub mod sopsfile;
pub mod stream;
pub mod token;

use crate::error::HoistError;

/// A config error for `file`; `line` (1-based) is put in front when known.
pub(crate) fn config_err(file: &str, line: Option<usize>, message: String) -> HoistError {
    let message = match line {
        Some(n) => format!("line {n}: {message}"),
        None => message,
    };
    HoistError::Config {
        file: file.to_string(),
        message,
    }
}

/// A toml parse or deserialize error as `line N: reason`. serde's messages quote the bad value
/// (`invalid type: string "…"`), so they are rebuilt from fixed parts and the value is dropped;
/// a key name is kept only when it looks like one. toml's syntax messages never quote input.
pub(crate) fn toml_err(file: &str, text: &str, e: toml::de::Error) -> HoistError {
    let line = e.span().map(|s| line_at(text, s.start));
    config_err(file, line, safe_reason(e.message().trim_end()))
}

const KINDS: &[&str] = &[
    "boolean",
    "integer",
    "floating point",
    "string",
    "sequence",
    "map",
];

fn safe_reason(msg: &str) -> String {
    let expected = msg.rfind(", expected ").map_or("", |i| &msg[i..]);
    for p in ["invalid type: ", "invalid value: "] {
        if let Some(rest) = msg.strip_prefix(p) {
            let kind = KINDS
                .iter()
                .find(|k| rest.starts_with(**k))
                .unwrap_or(&"value");
            return format!("{p}{kind}{expected}");
        }
    }
    if msg.starts_with("unknown variant ") {
        return format!("unknown variant{expected}");
    }
    for p in ["unknown field `", "missing field `", "duplicate field `"] {
        if let Some(rest) = msg.strip_prefix(p) {
            let name = rest.split('`').next().unwrap_or_default();
            let key = |b: u8| b.is_ascii_alphanumeric() || b"_.-".contains(&b);
            let ok = (1..=64).contains(&name.len()) && name.bytes().all(key);
            return format!("{p}{}`{expected}", if ok { name } else { "…" });
        }
    }
    msg.to_string()
}

/// 1-based line of byte offset `at` in `text`.
pub(crate) fn line_at(text: &str, at: usize) -> usize {
    text.bytes().take(at).filter(|&b| b == b'\n').count() + 1
}
