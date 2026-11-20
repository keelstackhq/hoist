//! Changesets (`.changeset/*.md`, the @changesets/cli 2.x format): read, write, config.

mod config;
mod file;

pub use config::{CsConfig, read_config};
pub use file::{Changeset, parse, read_dir, render};

/// A changeset bump type, ordered `None < Patch < Minor < Major`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CsType {
    None,
    Patch,
    Minor,
    Major,
}

impl CsType {
    /// `none`, `patch`, `minor` or `major` (exact, lower case).
    pub fn parse(s: &str) -> Option<CsType> {
        match s {
            "none" => Some(CsType::None),
            "patch" => Some(CsType::Patch),
            "minor" => Some(CsType::Minor),
            "major" => Some(CsType::Major),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CsType::None => "none",
            CsType::Patch => "patch",
            CsType::Minor => "minor",
            CsType::Major => "major",
        }
    }
}

/// Glob match: `*` and `?` stay within one `/` segment, `**` matches across segments
/// (`a/**/b` also matches `a/b`).
pub fn glob_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    glob(&p, &s)
}

fn glob(p: &[char], s: &[char]) -> bool {
    match p {
        [] => s.is_empty(),
        ['*', '*', rest @ ..] => {
            if let ['/', after @ ..] = rest
                && glob(after, s)
            {
                return true;
            }
            (0..=s.len()).any(|i| glob(rest, &s[i..]))
        }
        ['*', rest @ ..] => {
            let seg = s.iter().position(|&c| c == '/').unwrap_or(s.len());
            (0..=seg).any(|i| glob(rest, &s[i..]))
        }
        ['?', rest @ ..] => matches!(s, [c, ..] if *c != '/') && glob(rest, &s[1..]),
        [c, rest @ ..] => matches!(s, [d, ..] if d == c) && glob(rest, &s[1..]),
    }
}
