//! Conventional commits: which bump a commit asks for, and its changelog entry.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    None,
    Patch,
    Minor,
    Major,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
    pub body: String,
    /// Repo-relative paths the commit touched.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Breaking,
    Features,
    Fixes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub section: Section,
    /// The subject without its `type(scope)!: ` prefix.
    pub text: String,
    /// The first 7 characters of the sha.
    pub short_sha: String,
}

fn subject_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Za-z]+)(?:\([^)]*\))?(!)?: (.+)$").expect("static regex"))
}

fn body_is_breaking(body: &str) -> bool {
    body.lines()
        .any(|l| l.starts_with("BREAKING CHANGE:") || l.starts_with("BREAKING-CHANGE:"))
}

/// The bump a commit asks for and its changelog entry. Commits that are not conventional, or
/// whose type releases nothing on its own (and are not breaking), give `Bump::None`.
pub fn classify(c: &Commit) -> (Bump, Option<ChangeEntry>) {
    let Some(caps) = subject_re().captures(c.subject.trim()) else {
        return (Bump::None, None);
    };
    let kind = caps[1].to_ascii_lowercase();
    let breaking = caps.get(2).is_some() || body_is_breaking(&c.body);
    let (bump, section) = if breaking {
        (Bump::Major, Section::Breaking)
    } else {
        match kind.as_str() {
            "feat" => (Bump::Minor, Section::Features),
            "fix" => (Bump::Patch, Section::Fixes),
            _ => return (Bump::None, None),
        }
    };
    let entry = ChangeEntry {
        section,
        text: caps[3].to_string(),
        short_sha: c.sha.chars().take(7).collect(),
    };
    (bump, Some(entry))
}
