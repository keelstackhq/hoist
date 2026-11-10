//! One changeset file: parse (as `@changesets/parse`), render (as `@changesets/write`),
//! and read `.changeset/` (as `@changesets/read`).

use super::CsType;
use crate::HoistError;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// A parsed `.changeset/<id>.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changeset {
    pub id: String,
    /// Package name and bump, in file order.
    pub releases: Vec<(String, CsType)>,
    /// The text after the frontmatter, trimmed.
    pub summary: String,
}

const EXAMPLE: &str = "write `---`, one `\"<package>\": patch|minor|major|none` per line, `---`, \
                       then the summary";

static MD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)\A\s*---(.*?)\n\s*---(\s*(?:\n|\z).*)\z").expect("changeset regex")
});

/// Parse changeset text. The error names the line where it can (the caller adds the file).
pub fn parse(id: &str, text: &str) -> Result<Changeset, String> {
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    if text.trim().is_empty() {
        return Err(format!("file is empty; {EXAMPLE}"));
    }
    let caps = MD
        .captures(&text)
        .ok_or_else(|| format!("missing or invalid frontmatter; {EXAMPLE}"))?;
    let front = caps.get(1).expect("group 1");
    let first_line = text[..front.start()].matches('\n').count() + 1;
    let mut releases: Vec<(String, CsType)> = Vec::new();
    for (i, line) in front.as_str().split('\n').enumerate() {
        let n = first_line + i;
        let t = line.trim_end();
        if t.trim_start().is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        if i == 0 {
            return Err(format!("line {n}: text after `---`; {EXAMPLE}"));
        }
        let (name, ty) = entry(t).map_err(|m| format!("line {n}: {m}; {EXAMPLE}"))?;
        if releases.iter().any(|(r, _)| *r == name) {
            return Err(format!(
                "line {n}: duplicate package \"{name}\"; name it once"
            ));
        }
        releases.push((name, ty));
    }
    Ok(Changeset {
        id: id.to_string(),
        releases,
        summary: caps.get(2).expect("group 2").as_str().trim().to_string(),
    })
}

/// One frontmatter line: `name: type`, the name bare, `"…"` (only `\"` escaped) or `'…'`.
fn entry(line: &str) -> Result<(String, CsType), String> {
    if line.starts_with(char::is_whitespace) {
        return Err("nested YAML is not supported".into());
    }
    let (name, rest) = match line.chars().next() {
        Some(q @ ('"' | '\'')) => quoted(&line[1..], q)?,
        _ => match line
            .find(": ")
            .or_else(|| line.strip_suffix(':').map(|s| s.len()))
        {
            Some(i) => (line[..i].trim_end().to_string(), &line[i..]),
            None => return Err("expected `name: type`".into()),
        },
    };
    let ty = rest
        .trim_start()
        .strip_prefix(':')
        .ok_or("expected `:` after the package name")?;
    let ty = ty.split(" #").next().unwrap_or("");
    let ty = ty.split("\t#").next().unwrap_or("").trim();
    let ty = ty
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| ty.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
        .unwrap_or(ty);
    if name.trim().is_empty() {
        return Err("empty package name".into());
    }
    if ty.is_empty() {
        return Err(format!(
            "no type for \"{name}\" (nested YAML is not supported)"
        ));
    }
    let ty = CsType::parse(ty).ok_or_else(|| {
        format!("invalid type \"{ty}\" for \"{name}\" (major, minor, patch or none)")
    })?;
    Ok((name, ty))
}

fn quoted(s: &str, q: char) -> Result<(String, &str), String> {
    let mut name = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == q {
            return Ok((name, &s[i + 1..]));
        }
        if q == '"' && c == '\\' && s[i + 1..].starts_with('"') {
            chars.next();
            name.push('"');
        } else {
            name.push(c);
        }
    }
    Err(format!("unterminated {q} in the package name"))
}

/// Render as `changeset add` writes it (after prettier).
pub fn render(c: &Changeset) -> String {
    let mut out = String::from("---\n");
    for (name, ty) in &c.releases {
        out.push_str(&format!(
            "\"{}\": {}\n",
            name.replace('"', "\\\""),
            ty.as_str()
        ));
    }
    out.push_str("---\n");
    if !c.summary.is_empty() {
        out.push_str(&format!("\n{}\n", c.summary));
    }
    out
}

/// Read every changeset in `<root>/.changeset/`, sorted by id. No directory = none.
pub fn read_dir(root: &Path) -> Result<Vec<Changeset>, HoistError> {
    let dir = root.join(".changeset");
    let err = |file: String, message: String| HoistError::Config { file, message };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(err(".changeset".into(), format!("cannot read: {e}"))),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| err(".changeset".into(), format!("cannot read: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!(".changeset/{name}");
        let path = entry.path();
        if path.is_dir() {
            if path.join("changes.json").exists() {
                return Err(err(
                    rel,
                    "legacy changes.json changeset folder is not supported; rewrite it as \
                     .changeset/<id>.md and remove the folder"
                        .into(),
                ));
            }
            continue;
        }
        let Some(id) = name.strip_suffix(".md") else {
            continue;
        };
        if name.starts_with('.') || name.eq_ignore_ascii_case("README.md") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| err(rel.clone(), format!("cannot read: {e}")))?;
        out.push(parse(id, &text).map_err(|m| err(rel, m))?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
