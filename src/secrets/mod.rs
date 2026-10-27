//! Secrets repos (sops + age; design: keelstackhq/hull
//! `docs/superpowers/specs/2026-10-27-secrets-and-ci-providers-design.md` §3).

pub mod manifest;

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

/// A toml parse or deserialize error as `line N: reason`. toml's own message quotes the
/// source line and string values; both are left out, so a value pasted by mistake is never shown.
pub(crate) fn toml_err(file: &str, text: &str, e: toml::de::Error) -> HoistError {
    let line = e.span().map(|s| line_at(text, s.start));
    let mut reason = String::new();
    let mut quoted = false;
    for c in e.message().trim_end().chars() {
        match c {
            '"' if quoted => {
                reason.push_str("…\"");
                quoted = false;
            }
            '"' => {
                reason.push('"');
                quoted = true;
            }
            _ if quoted => {}
            c => reason.push(c),
        }
    }
    config_err(file, line, reason)
}

/// 1-based line of byte offset `at` in `text`.
pub(crate) fn line_at(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].matches('\n').count() + 1
}
