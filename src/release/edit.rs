//! Pure text editors for release preparation: manifests, dependents and changelogs.

use super::commits::{ChangeEntry, Section};
use super::planner::NextVersion;
use crate::timeutil::{civil_from_days, now_secs};

/// A string member of a JSON object: its key path and the byte range of its content.
struct StrMember {
    path: Vec<String>,
    start: usize,
    end: usize,
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
    out: Vec<StrMember>,
}

impl Scanner<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// Skip a string starting at the opening quote; returns the content range.
    fn string(&mut self) -> Result<(usize, usize), String> {
        let start = self.i + 1;
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return Ok((start, self.i - 1));
                }
                _ => self.i += 1,
            }
        }
        Err("invalid JSON: unterminated string".to_string())
    }

    fn value(&mut self, path: &mut Vec<String>, as_member: bool) -> Result<(), String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(());
                        }
                        Some(b',') => self.i += 1,
                        Some(b'"') => {
                            let (s, e) = self.string()?;
                            let key = String::from_utf8_lossy(&self.b[s..e]).into_owned();
                            self.ws();
                            if self.b.get(self.i) != Some(&b':') {
                                return Err("invalid JSON: expected `:`".to_string());
                            }
                            self.i += 1;
                            path.push(key);
                            self.value(path, true)?;
                            path.pop();
                        }
                        _ => return Err("invalid JSON: bad object".to_string()),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b']') => {
                            self.i += 1;
                            return Ok(());
                        }
                        Some(b',') => self.i += 1,
                        Some(_) => self.value(path, false)?,
                        None => return Err("invalid JSON: unterminated array".to_string()),
                    }
                }
            }
            Some(b'"') => {
                let (start, end) = self.string()?;
                if as_member {
                    self.out.push(StrMember {
                        path: path.clone(),
                        start,
                        end,
                    });
                }
                Ok(())
            }
            Some(_) => {
                while self.i < self.b.len()
                    && !matches!(self.b[self.i], b',' | b'}' | b']')
                    && !self.b[self.i].is_ascii_whitespace()
                {
                    self.i += 1;
                }
                Ok(())
            }
            None => Err("invalid JSON: unexpected end".to_string()),
        }
    }
}

fn json_strings(text: &str) -> Result<Vec<StrMember>, String> {
    let mut s = Scanner {
        b: text.as_bytes(),
        i: 0,
        out: Vec::new(),
    };
    s.value(&mut Vec::new(), false)?;
    Ok(s.out)
}

fn json_escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

fn splice(text: &str, edits: &[(usize, usize, String)]) -> String {
    let mut out = text.to_string();
    for (s, e, v) in edits.iter().rev() {
        out.replace_range(*s..*e, v);
    }
    out
}

/// Replace the top-level `"version"` string of a package.json; every other byte stays.
pub fn set_package_json_version(text: &str, v: &str) -> Result<String, String> {
    let members = json_strings(text)?;
    let m = members
        .iter()
        .find(|m| m.path == ["version"])
        .ok_or_else(|| "package.json has no top-level string `version`".to_string())?;
    Ok(splice(text, &[(m.start, m.end, json_escape(v))]))
}

/// Set the requirement of `dep` in top-level `dependencies`, `optionalDependencies` and
/// `peerDependencies`; `workspace:` specifiers stay as they are.
pub fn set_package_json_dep(text: &str, dep: &str, req: &str) -> Result<String, String> {
    let members = json_strings(text)?;
    let mut found = false;
    let mut edits = Vec::new();
    for m in &members {
        if m.path.len() == 2
            && m.path[1] == dep
            && matches!(
                m.path[0].as_str(),
                "dependencies" | "optionalDependencies" | "peerDependencies"
            )
        {
            found = true;
            if !text[m.start..m.end].starts_with("workspace:") {
                edits.push((m.start, m.end, json_escape(req)));
            }
        }
    }
    if !found {
        return Err(format!("package.json does not depend on `{dep}`"));
    }
    Ok(splice(text, &edits))
}

fn toml_doc(text: &str) -> Result<toml_edit::DocumentMut, String> {
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("invalid TOML: {e}"))
}

fn set_string(item: &mut toml_edit::Item, v: &str) {
    // Keep the trailing comment and spacing of the old value.
    let decor = item.as_value().map(|o| o.decor().clone());
    let mut value = toml_edit::Value::from(v);
    if let Some(d) = decor {
        *value.decor_mut() = d;
    }
    *item = toml_edit::Item::Value(value);
}

/// Set `[package] version`; with an inherited version, `[workspace.package] version`.
pub fn set_cargo_version(text: &str, v: &str) -> Result<String, String> {
    let mut doc = toml_doc(text)?;
    if let Some(item) = doc
        .get_mut("package")
        .and_then(|p| p.get_mut("version"))
        .filter(|i| i.is_str())
    {
        set_string(item, v);
    } else if let Some(item) = doc
        .get_mut("workspace")
        .and_then(|w| w.get_mut("package"))
        .and_then(|p| p.get_mut("version"))
        .filter(|i| i.is_str())
    {
        set_string(item, v);
    } else {
        return Err(
            "Cargo.toml has no `[package] version` or `[workspace.package] version`".into(),
        );
    }
    Ok(doc.to_string())
}

fn update_dep_tables(table: &mut dyn toml_edit::TableLike, dep: &str, v: &str, found: &mut bool) {
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(deps) = table.get_mut(kind).and_then(|d| d.as_table_like_mut()) else {
            continue;
        };
        for (key, item) in deps.iter_mut() {
            let Some(t) = item.as_table_like_mut() else {
                continue;
            };
            let real = t.get("package").and_then(|p| p.as_str()).unwrap_or(&key);
            if real != dep && key != dep {
                continue;
            }
            let ws = t.get("workspace").and_then(|w| w.as_bool()) == Some(true);
            if t.contains_key("path") || ws {
                *found = true;
                if let Some(ver) = t.get_mut("version") {
                    set_string(ver, v);
                }
            }
        }
    }
}

/// Set `version` of the path dependency `dep` (all dependency tables, `[workspace.dependencies]`
/// and `[target.*]` tables). A dependency without `version` or with `workspace = true` is kept.
pub fn set_cargo_dep_version(text: &str, dep: &str, v: &str) -> Result<String, String> {
    let mut doc = toml_doc(text)?;
    let mut found = false;
    update_dep_tables(doc.as_table_mut(), dep, v, &mut found);
    if let Some(ws) = doc.get_mut("workspace").and_then(|w| w.as_table_like_mut()) {
        // `[workspace.dependencies]` has the same shape as `[dependencies]`.
        if let Some(deps) = ws
            .get_mut("dependencies")
            .and_then(|d| d.as_table_like_mut())
        {
            for (key, item) in deps.iter_mut() {
                if let Some(t) = item.as_table_like_mut()
                    && (key == dep || t.get("package").and_then(|p| p.as_str()) == Some(dep))
                    && t.contains_key("path")
                {
                    found = true;
                    if let Some(ver) = t.get_mut("version") {
                        set_string(ver, v);
                    }
                }
            }
        }
    }
    if let Some(targets) = doc.get_mut("target").and_then(|t| t.as_table_like_mut()) {
        for (_, cfg) in targets.iter_mut() {
            if let Some(t) = cfg.as_table_like_mut() {
                update_dep_tables(t, dep, v, &mut found);
            }
        }
    }
    if !found {
        return Err(format!("Cargo.toml has no path dependency on `{dep}`"));
    }
    Ok(doc.to_string())
}

/// Today as `YYYY-MM-DD` (UTC; honours `HOIST_NOW`).
pub fn today() -> String {
    let (y, m, d) = civil_from_days(now_secs().div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn section_block(title: &str, section: Section, entries: &[ChangeEntry], nl: &str) -> String {
    let lines: Vec<String> = entries
        .iter()
        .filter(|e| e.section == section)
        .map(|e| format!("- {} ({}){nl}", e.text, e.short_sha))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!("### {title}{nl}{nl}{}", lines.concat())
}

/// Put the section of `nv` above the previous ones; adds the `# Changelog` header if missing.
pub fn prepend_changelog(existing: Option<&str>, nv: &NextVersion, date: &str) -> String {
    let existing = existing.unwrap_or("");
    let nl = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut blocks = Vec::new();
    for (title, section) in [
        ("Breaking changes", Section::Breaking),
        ("Features", Section::Features),
        ("Fixes", Section::Fixes),
    ] {
        let b = section_block(title, section, &nv.entries, nl);
        if !b.is_empty() {
            blocks.push(b);
        }
    }
    if blocks.is_empty() && nv.first_release {
        blocks.push(format!("- First release.{nl}"));
    }
    let new = format!("## {} ({date}){nl}{nl}{}", nv.to, blocks.join(nl));
    let rest = existing.trim_start_matches(['\r', '\n']);
    let rest = match rest.strip_prefix("# Changelog") {
        Some(r) => r.trim_start_matches(['\r', '\n']),
        None => rest,
    };
    if rest.is_empty() {
        format!("# Changelog{nl}{nl}{new}")
    } else {
        format!("# Changelog{nl}{nl}{new}{nl}{rest}")
    }
}
