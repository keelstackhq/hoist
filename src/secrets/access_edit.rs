//! A new `ci` recipient in `access.toml` for `hoist secrets grant` (spec §15.4): appended as a
//! `[[recipient]]` table and to every rule under `<env>/`, the rest of the file kept as written.
//! Refused when it would be ambiguous: one recipient per consumer environment, one name and one
//! key per recipient.

use super::access::{Access, Kind};
use crate::error::HoistError;
use toml_edit::{DocumentMut, Item, Table, value};

pub struct NewCi<'a> {
    pub name: &'a str,
    /// `owner/name` of the consumer repository.
    pub repo: &'a str,
    pub env: &'a str,
    /// The public key, `age1…`.
    pub age: &'a str,
    pub added: &'a str,
    pub expires: &'a str,
    /// `rotate-key`: the consumer's current recipient, which the new key joins until revoked.
    pub rotating: Option<&'a str>,
}

/// `<repo name>-<env>`, lowercase, with anything but `[a-z0-9]` as `-`.
pub fn default_name(repo: &str, env: &str) -> String {
    let name = repo.rsplit('/').next().unwrap_or(repo);
    format!("{name}-{env}")
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `text` with `new` added, or why not. The result is checked as `access.toml` again.
pub fn add_ci_recipient(text: &str, now: i64, new: &NewCi) -> Result<String, HoistError> {
    let usage = |msg: String| Err(HoistError::Usage(format!("secrets grant: {msg}")));
    let access = Access::parse(text, "access.toml", now)?;
    let name = new.name;
    let valid = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
    if name.is_empty() || !name.bytes().all(valid) {
        return usage(format!("recipient name `{name}` must match [a-z0-9-]+"));
    }
    let prefix = format!("{}/", new.env);
    if !access.rules.iter().any(|r| r.path.starts_with(&prefix)) {
        return usage(format!(
            "no rule in access.toml covers `{prefix}`; add one first"
        ));
    }
    // GitHub repository names are case-insensitive.
    let consumer = |r: &&super::access::Recipient| {
        r.kind == Kind::Ci
            && r.repo
                .as_deref()
                .is_some_and(|x| x.eq_ignore_ascii_case(new.repo))
            && r.env.as_deref() == Some(new.env)
    };
    let mut current = access.recipients.iter().filter(consumer);
    if let Some(old) = new.rotating
        && !access
            .recipients
            .iter()
            .filter(consumer)
            .any(|r| r.name == old)
    {
        return usage(format!(
            "`{old}` is not a recipient of {} ({}): nothing to rotate",
            new.repo, new.env
        ));
    }
    // In a rotation the current key stays, and the new one joins exactly its rules.
    if let Some(r) = current.find(|r| Some(r.name.as_str()) != new.rotating) {
        let old = &r.name;
        return usage(format!(
            "{} ({}) already has recipient `{old}`; replace its key with `hoist secrets \
             rotate-key {old}`",
            new.repo, new.env
        ));
    }
    if let Some(r) = access.recipients.iter().find(|r| r.name == name) {
        let whose = match (&r.repo, &r.env) {
            (Some(repo), Some(env)) => format!("{repo} ({env})"),
            _ => "a human recipient".into(),
        };
        return usage(format!(
            "recipient name `{name}` is taken by {whose}; choose another with --name"
        ));
    }
    if let Some(r) = access.recipients.iter().find(|r| r.age == new.age) {
        return usage(format!(
            "that age key is already recipient `{}`: one key per consumer environment",
            r.name
        ));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|_| HoistError::Usage("secrets grant: access.toml does not parse".into()))?;
    let mut table = Table::new();
    let fields = [
        ("name", name),
        ("kind", "ci"),
        ("age", new.age),
        ("repo", new.repo),
        ("env", new.env),
        ("provider", "github"),
        ("added", new.added),
        ("expires", new.expires),
    ];
    for (k, v) in fields {
        table.insert(k, value(v));
    }
    // Checked before any rule changes, so an inline `recipient = [...]` gets this message.
    let Some(list) = doc
        .get_mut("recipient")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return usage(
            "write access.toml's recipients as [[recipient]] tables, so hoist can add one".into(),
        );
    };
    list.push(table);
    let joins = |path: &str, names: &[&str]| match new.rotating {
        Some(old) => names.contains(&old),
        None => path.starts_with(&prefix),
    };
    let rules = doc.get_mut("rule").and_then(Item::as_array_of_tables_mut);
    for rule in rules.into_iter().flat_map(|r| r.iter_mut()) {
        let path = rule.get("path").and_then(Item::as_str).unwrap_or_default();
        let under = match rule.get("recipients").and_then(Item::as_array) {
            Some(list) => joins(
                path,
                &list.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
            ),
            None => false,
        };
        if let Some(list) = rule.get_mut("recipients").and_then(Item::as_array_mut)
            && under
        {
            list.push(name);
        }
    }
    // A form hoist cannot edit (inline tables) leaves the recipient out somewhere: refused.
    let updated = doc.to_string();
    let check = Access::parse(&updated, "access.toml", now)?;
    let added = check.recipients.iter().any(|r| r.name == name);
    let placed = check.rules.iter().all(|r| {
        let names: Vec<&str> = r.recipients.iter().map(String::as_str).collect();
        names.contains(&name) == joins(&r.path, &names)
    });
    if !(added && placed) {
        return usage(
            "write access.toml's recipients and rules as [[recipient]] and [[rule]] tables, each \
             rule with a `recipients = [...]` list, so hoist can add one"
                .into(),
        );
    }
    Ok(updated)
}
