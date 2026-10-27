//! `hoist secrets check` in a secrets repo (spec §15.4): `access.toml`, the generated
//! `.sops.yaml` and every file under a rule, read without decrypting and without network.

use crate::error::HoistError;
use crate::ops::check::exit_code;
use crate::report::{Action, Item, Report};
use crate::secrets::access::{Access, Kind, Rule, sops_yaml};
use crate::secrets::sopsfile;
use std::collections::BTreeSet;
use std::path::Path;

/// A recipient that expires within this many days gets a warning.
const WARN_DAYS: i64 = 30;

fn item(kind: &str, key: &str, action: Action, reason: impl Into<String>, source: &str) -> Item {
    Item {
        area: "secrets".into(),
        kind: kind.into(),
        key: key.into(),
        action,
        reason: reason.into(),
        source: source.into(),
        verify: None,
    }
}

pub fn secrets_check(dir: &Path) -> Result<Report, HoistError> {
    let Some(text) = read_regular(&dir.join("access.toml")) else {
        let msg = "no readable access.toml; run `hoist secrets check` in a secrets repo";
        return Err(HoistError::Usage(format!("{}: {msg}", dir.display())));
    };
    let now = crate::timeutil::now_secs();
    let access = Access::parse(&text, "access.toml", now)?;
    let mut items = vec![sops_config(dir, &access)];
    for r in &access.recipients {
        let rotate = format!("run `hoist secrets rotate-key {}`", r.name);
        let (action, reason) = match r.days_left(now) {
            d if d < 0 => (Action::Drift, format!("expired {} days ago; {rotate}", -d)),
            d if d <= WARN_DAYS => (
                Action::Ok,
                format!("warning: expires in {d} days; {rotate}"),
            ),
            d => (Action::Ok, format!("expires in {d} days")),
        };
        items.push(item("recipient", &r.name, action, reason, "access.toml"));
    }
    let human = |n: &String| {
        access
            .recipients
            .iter()
            .any(|r| &r.name == n && r.kind == Kind::Human)
    };
    for (i, rule) in access.rules.iter().enumerate() {
        let humans = rule.recipients.iter().filter(|n| human(n)).count();
        let reason = match access.rules[..i].iter().find(|a| shadows(a, rule)) {
            Some(a) => format!("warning: unreachable, rule `{}` matches first", a.path),
            None if humans == 1 => "warning: only one human recipient; add a second".into(),
            None => format!("{} recipients", rule.recipients.len()),
        };
        items.push(item("rule", &rule.path, Action::Ok, reason, "access.toml"));
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    for (rel, is_link) in files {
        if let Some(found) = check_file(dir, &access, &rel, is_link) {
            items.push(found);
        }
    }
    Ok(Report {
        command: "secrets check".into(),
        exit_code: exit_code(&items),
        items,
        human_steps: Vec::new(),
        text: None,
        hint: None,
    })
}

/// The text of a regular file (never through a symlink), or `None`.
fn read_regular(path: &Path) -> Option<String> {
    let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    regular
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

fn sops_config(dir: &Path, access: &Access) -> Item {
    let (action, reason) = match read_regular(&dir.join(".sops.yaml")) {
        Some(h) if h == sops_yaml(access) => (Action::Ok, "matches access.toml"),
        Some(_) => (Action::Drift, "differs; run `hoist secrets sync`"),
        None => (Action::Drift, "missing; run `hoist secrets sync`"),
    };
    item("sops-config", ".sops.yaml", action, reason, "access.toml")
}

/// Whether rule `a`, earlier in the file, matches every file `b` could: `b` is a plain path `a`
/// matches, or `a` is `<prefix>**` and `b` starts with that prefix.
fn shadows(a: &Rule, b: &Rule) -> bool {
    match a.path.strip_suffix("**") {
        _ if !b.path.contains(['*', '?']) => a.matches(&b.path),
        Some(prefix) => (prefix.is_empty() || prefix.ends_with('/')) && b.path.starts_with(prefix),
        None => false,
    }
}

/// Every file and symlink below `dir` as (`/`-joined relative path, is a symlink), sorted;
/// `.git` is skipped and symlinks are never followed.
fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, bool)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{rel}{}{name}", if rel.is_empty() { "" } else { "/" });
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            out.push((path, true));
        } else if kind.is_dir() && path != ".git" {
            walk(&entry.path(), &path, out);
        } else if kind.is_file() {
            out.push((path, false));
        }
    }
}

/// The item for one walked path, or `None` for a file outside every rule that is not a sops file.
fn check_file(dir: &Path, access: &Access, rel: &str, is_link: bool) -> Option<Item> {
    let rule = access.rule_for(rel);
    let sops = rel.ends_with(".sops.yaml") && rel != ".sops.yaml";
    let drift = |reason: String| Some(item("file", rel, Action::Drift, reason, rel));
    if is_link {
        return drift("symlink: not followed; a secrets repo holds regular files".into());
    }
    let Some(rule) = rule else {
        return drift("no rule in access.toml covers it".into()).filter(|_| sops);
    };
    if !sops {
        return drift(format!("not a .sops.yaml file, under rule `{}`", rule.path));
    }
    let text = read_regular(&dir.join(rel)).unwrap_or_default();
    let file = match sopsfile::parse(&text) {
        Ok(file) => file,
        Err(reason) => return drift(reason),
    };
    if !file.plaintext.is_empty() {
        let keys = file.plaintext.join(", ");
        return drift(format!("plaintext: {keys}; encrypt with `sops`"));
    }
    let want: BTreeSet<String> = access
        .public_keys(rule)
        .into_iter()
        .map(String::from)
        .collect();
    if file.recipients != want {
        let fix = "run `hoist secrets sync`";
        return drift(format!(
            "recipients differ from rule `{}`; {fix}",
            rule.path
        ));
    }
    let reason = format!("encrypted for rule `{}`", rule.path);
    Some(item("file", rel, Action::Ok, reason, rel))
}
