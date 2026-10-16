//! Editing `hoist.toml` with `toml_edit` (ruling R27): comments, key order and unrelated keys
//! survive; a replaced value keeps its decor; equal values leave the file byte-identical;
//! writes go through a temp file plus rename in the same directory.

use crate::error::HoistError;
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// A value import writes: only the shapes `config.rs` accepts for the imported keys.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Bool(bool),
    Str(String),
    List(Vec<String>),
}

impl Val {
    fn to_value(&self) -> Value {
        match self {
            Val::Bool(b) => Value::from(*b),
            Val::Str(s) => Value::from(s.as_str()),
            Val::List(l) => Value::Array(l.iter().map(String::as_str).collect::<Array>()),
        }
    }

    fn matches(&self, v: &Value) -> bool {
        match (self, v) {
            (Val::Bool(a), Value::Boolean(b)) => a == b.value(),
            (Val::Str(a), Value::String(b)) => a == b.value(),
            (Val::List(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| y.as_str() == Some(x))
            }
            _ => false,
        }
    }
}

/// The TOML line for one key, e.g. `required_checks = ["ci"]`.
pub fn line(key: &str, val: &Val) -> String {
    format!("{key} = {}", val.to_value().to_string().trim())
}

/// What an edit did to its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Added,
    Replaced,
    Same,
}

/// One key to set: (table, key, value).
pub type Edit = (String, String, Val);

/// Apply `edits` to the text of a `hoist.toml` (`""` for a missing file). `file` names the
/// file in errors. Returns the new text (`None` when nothing changed) and one outcome per edit.
pub fn apply_edits(
    text: &str,
    file: &str,
    edits: &[Edit],
) -> Result<(Option<String>, Vec<Outcome>), HoistError> {
    let err = |message: String| HoistError::Config {
        file: file.to_string(),
        message,
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| err(e.to_string().trim_end().to_string()))?;
    let mut outcomes = Vec::new();
    for (table, key, val) in edits {
        let entry = doc
            .entry(table)
            .or_insert_with(|| Item::Table(Table::new()));
        let Some(t) = entry.as_table_like_mut() else {
            return Err(err(format!("`{table}` is not a table; fix it by hand")));
        };
        let outcome = match t.get_mut(key) {
            None => {
                t.insert(key, Item::Value(val.to_value()));
                Outcome::Added
            }
            Some(item) => {
                let Some(v) = item.as_value_mut() else {
                    return Err(err(format!(
                        "`{table}.{key}` is not a value; fix it by hand"
                    )));
                };
                if val.matches(v) {
                    Outcome::Same
                } else {
                    let decor = v.decor().clone();
                    *v = val.to_value();
                    *v.decor_mut() = decor;
                    Outcome::Replaced
                }
            }
        };
        outcomes.push(outcome);
    }
    let changed = outcomes.iter().any(|o| *o != Outcome::Same);
    Ok((changed.then(|| doc.to_string()), outcomes))
}

/// Edit the file at `path` (created when missing); untouched when every value is equal.
pub fn edit_file(path: &Path, file: &str, edits: &[Edit]) -> Result<Vec<Outcome>, HoistError> {
    let io_err = |what: &str, e: std::io::Error| HoistError::Config {
        file: file.to_string(),
        message: format!("cannot {what}: {e}; check the directory's permissions"),
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err("read file", e)),
    };
    let (new, outcomes) = apply_edits(&text, file, edits)?;
    if let Some(new) = new {
        let tmp = path.with_file_name(format!(".hoist.toml.{}.tmp", std::process::id()));
        std::fs::write(&tmp, new).map_err(|e| io_err("write temp file", e))?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io_err("replace file", e));
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(table: &str, key: &str, val: Val) -> Edit {
        (table.to_string(), key.to_string(), val)
    }

    #[test]
    fn replaces_in_place_keeping_comments() {
        let text =
            "# top\n[github]\n# why\nvisibility = \"private\" # pinned\nmanaged = \"hoist\"\n";
        let (new, out) = apply_edits(
            text,
            "hoist.toml",
            &[edit("github", "visibility", Val::Str("public".into()))],
        )
        .unwrap();
        assert_eq!(out, [Outcome::Replaced]);
        assert_eq!(
            new.unwrap(),
            "# top\n[github]\n# why\nvisibility = \"public\" # pinned\nmanaged = \"hoist\"\n"
        );
    }

    #[test]
    fn equal_values_change_nothing() {
        let text = "[github]\nrequired_checks = [ \"ci\" ]  # x\nmerge_queue = false\n";
        let (new, out) = apply_edits(
            text,
            "hoist.toml",
            &[
                edit("github", "required_checks", Val::List(vec!["ci".into()])),
                edit("github", "merge_queue", Val::Bool(false)),
            ],
        )
        .unwrap();
        assert_eq!(out, [Outcome::Same, Outcome::Same]);
        assert!(new.is_none());
    }

    #[test]
    fn missing_file_gets_the_table() {
        let (new, out) = apply_edits(
            "",
            "apps/site/hoist.toml",
            &[edit("deploy", "project", Val::Str("site-landing".into()))],
        )
        .unwrap();
        assert_eq!(out, [Outcome::Added]);
        assert_eq!(new.unwrap(), "[deploy]\nproject = \"site-landing\"\n");
    }

    #[test]
    fn lines_render_as_toml() {
        assert_eq!(
            line(
                "required_checks",
                &Val::List(vec!["ci".into(), "lint".into()])
            ),
            "required_checks = [\"ci\", \"lint\"]"
        );
        assert_eq!(line("merge_queue", &Val::Bool(true)), "merge_queue = true");
    }
}
