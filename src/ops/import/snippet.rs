//! The TOML text `hoist import` prints after the table when it did not write.

use super::ROOT;
use crate::report::{Action, Item};

/// One table's lines in the printed snippet: (table, lines).
type Lines = (String, Vec<String>);

/// The TOML text: the `drift` override lines grouped by file and table, then the comments.
/// Empty when there is neither. The caller decides whether to show it (never after a write).
pub(super) fn snippet(items: &[Item]) -> String {
    let lines = items
        .iter()
        .filter(|i| i.kind == "override" && i.action == Action::Drift)
        .chain(items.iter().filter(|i| i.kind == "comment"));
    let mut files: Vec<(String, Vec<Lines>)> = Vec::new();
    for i in lines {
        let Some((file, rest)) = i.key.split_once(':') else {
            continue;
        };
        let table = rest.split_once('.').map_or(rest, |(t, _)| t);
        let pos = match files.iter().position(|(f, _)| f == file) {
            Some(pos) => pos,
            None => {
                files.push((file.to_string(), Vec::new()));
                files.len() - 1
            }
        };
        let tables = &mut files[pos].1;
        match tables.iter_mut().find(|(t, _)| t == table) {
            Some((_, l)) => l.push(i.reason.clone()),
            None => tables.push((table.to_string(), vec![i.reason.clone()])),
        }
    }
    files.sort_by_key(|(f, _)| (f != ROOT, f.clone()));
    let mut out = String::new();
    for (file, tables) in files {
        out.push_str(&format!("\n# {file}\n"));
        for (table, lines) in tables {
            out.push_str(&format!("[{table}]\n"));
            for l in lines {
                out.push_str(&l);
                out.push('\n');
            }
        }
    }
    out
}
