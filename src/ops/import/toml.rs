//! Editing `hoist.toml` with `toml_edit` (ruling R27): comments, key order and unrelated keys
//! survive; a replaced value keeps its decor; equal values leave the file byte-identical;
//! writes go through a temp file plus rename in the same directory.

use crate::error::HoistError;
use std::fs::Permissions;
use std::path::{Path, PathBuf};
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

/// A rendered file waiting to replace the original.
struct Pending {
    path: PathBuf,
    tmp: PathBuf,
    file: String,
    text: String,
    perms: Option<Permissions>,
}

fn io_err(file: &str, what: &str, e: std::io::Error) -> HoistError {
    HoistError::Config {
        file: file.to_string(),
        message: format!("cannot {what}: {e}; check the directory's permissions"),
    }
}

/// The current text and permissions of `path`; `""` and `None` when it is missing. A
/// symlinked `hoist.toml` is refused: renaming over it would replace the link with a file.
fn read_existing(path: &Path, file: &str) -> Result<(String, Option<Permissions>), HoistError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((String::new(), None)),
        Err(e) => return Err(io_err(file, "read file", e)),
    };
    if meta.file_type().is_symlink() {
        return Err(HoistError::Config {
            file: file.to_string(),
            message: "is a symlink; `hoist import --write` does not follow it, so write the \
                      printed overrides into the target by hand"
                .to_string(),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| io_err(file, "read file", e))?;
    Ok((text, Some(meta.permissions())))
}

fn remove_temps(pending: &[Pending]) {
    for p in pending {
        let _ = std::fs::remove_file(&p.tmp);
    }
}

/// Edit every file (path relative to `root`; created when missing). All files are rendered
/// before any is written, so a render failure writes nothing; then each new text goes to a
/// temp file in the same directory (with the original's permissions) and is renamed over the
/// original. Files whose values are all equal are not touched. One outcome list per file.
pub fn write_files(
    root: &Path,
    files: &[(String, Vec<Edit>)],
) -> Result<Vec<Vec<Outcome>>, HoistError> {
    let mut pending = Vec::new();
    let mut all = Vec::new();
    for (file, edits) in files {
        let path = root.join(file);
        let (text, perms) = read_existing(&path, file)?;
        let (new, outcomes) = apply_edits(&text, file, edits)?;
        all.push(outcomes);
        if let Some(text) = new {
            let tmp = path.with_file_name(format!(".hoist.toml.{}.tmp", std::process::id()));
            pending.push(Pending {
                path,
                tmp,
                file: file.clone(),
                text,
                perms,
            });
        }
    }
    for (i, p) in pending.iter().enumerate() {
        let written = std::fs::write(&p.tmp, &p.text).and_then(|()| match &p.perms {
            Some(perms) => std::fs::set_permissions(&p.tmp, perms.clone()),
            None => Ok(()),
        });
        if let Err(e) = written {
            remove_temps(&pending[..=i]);
            return Err(io_err(&p.file, "write temp file", e));
        }
    }
    for (i, p) in pending.iter().enumerate() {
        if let Err(e) = std::fs::rename(&p.tmp, &p.path) {
            remove_temps(&pending[i..]);
            return Err(io_err(&p.file, "replace file", e));
        }
    }
    Ok(all)
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

    fn files(list: &[(&str, Val)]) -> Vec<(String, Vec<Edit>)> {
        list.iter()
            .map(|(f, v)| (f.to_string(), vec![edit("github", "managed", v.clone())]))
            .collect()
    }

    fn no_temp_files(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().contains("tmp"))
    }

    #[test]
    fn a_render_failure_writes_no_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("b")).unwrap();
        std::fs::write(root.path().join("hoist.toml"), "[github]\n").unwrap();
        std::fs::write(root.path().join("b/hoist.toml"), "[[broken\n").unwrap();
        let err = write_files(
            root.path(),
            &files(&[
                ("hoist.toml", Val::Str("external".into())),
                ("b/hoist.toml", Val::Str("external".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("b/hoist.toml:"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("hoist.toml")).unwrap(),
            "[github]\n"
        );
        assert!(no_temp_files(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn replace_keeps_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hoist.toml");
        std::fs::write(&path, "[github]\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let out = write_files(
            root.path(),
            &files(&[("hoist.toml", Val::Str("external".into()))]),
        )
        .unwrap();
        assert_eq!(out, [vec![Outcome::Added]]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[github]\nmanaged = \"external\"\n"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(no_temp_files(root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("real.toml"), "[github]\n").unwrap();
        std::os::unix::fs::symlink("real.toml", root.path().join("hoist.toml")).unwrap();
        let err = write_files(
            root.path(),
            &files(&[("hoist.toml", Val::Str("external".into()))]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("real.toml")).unwrap(),
            "[github]\n"
        );
    }
}
