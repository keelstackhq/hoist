//! A secrets repo's `access.toml` (spec §15.2): recipients and path rules. `.sops.yaml` is
//! generated from it by [`sops_yaml`], deterministically.

use super::config_err;
use super::manifest::{NAME as ENV, REPO};
use crate::error::HoistError;
use crate::timeutil::{add_months, parse_date};
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;
use toml::Spanned;

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+$").unwrap());
pub(crate) static AGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^age1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{58}$").unwrap());
/// `<env>/<glob>`: a rule never covers the repo's own files (access.toml, .sops.yaml, README).
static GLOB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*/[a-z0-9._*?/-]+$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Human,
    Ci,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Circleci,
}

#[derive(Debug, Clone)]
pub struct Recipient {
    pub name: String,
    pub kind: Kind,
    pub age: String,
    pub repo: Option<String>,
    pub env: Option<String>,
    pub provider: Option<Provider>,
    expires_day: i64,
}

impl Recipient {
    /// Whole days from `now` (Unix seconds) to `expires`; negative once expired.
    pub fn days_left(&self, now: i64) -> i64 {
        self.expires_day - now.div_euclid(86_400)
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// The glob as written: `**` any path, `*`/`?` within one segment.
    pub path: String,
    pub recipients: Vec<String>,
    regex: Regex,
}

impl Rule {
    /// Whether this rule's glob matches `path`; never for a path `rule_for` refuses.
    pub fn matches(&self, path: &str) -> bool {
        clean(path) && self.regex.is_match(path)
    }
}

#[derive(Debug, Clone)]
pub struct Access {
    pub recipients: Vec<Recipient>,
    pub rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Spanned<i64>,
    #[serde(default)]
    recipient: Vec<RawRecipient>,
    #[serde(default)]
    rule: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipient {
    name: Spanned<String>,
    kind: Kind,
    age: Spanned<String>,
    repo: Option<Spanned<String>>,
    env: Option<Spanned<String>>,
    provider: Option<Provider>,
    added: Option<Spanned<String>>,
    expires: Spanned<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    path: Spanned<String>,
    recipients: Vec<String>,
}

/// Relative, with no empty, `.` or `..` segment and no `\` or `:` (Windows separators, drive
/// letters, alternate data streams), so a path can never leave its env directory.
fn clean(path: &str) -> bool {
    !path.contains(['\\', ':']) && !path.split('/').any(|seg| matches!(seg, "" | "." | ".."))
}

/// Glob to an anchored regex: `**` any characters, `*` and `?` within one path segment.
fn glob_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                out.push_str(".*");
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '.' => out.push_str("\\."),
            c => out.push(c),
        }
    }
    out.push('$');
    out
}

impl Access {
    /// Parse and validate `text`. `now` (Unix seconds) bounds `expires` when `added` is missing.
    pub fn parse(text: &str, file: &str, now: i64) -> Result<Access, HoistError> {
        let raw: Raw = toml::from_str(text).map_err(|e| super::toml_err(file, text, e))?;
        let at = |s: &Spanned<String>| Some(super::line_at(text, s.span().start));
        let err = |line: Option<usize>, msg: String| config_err(file, line, msg);
        if *raw.version.get_ref() != 1 {
            let line = super::line_at(text, raw.version.span().start);
            return Err(err(Some(line), "version must be 1".into()));
        }
        let today = now.div_euclid(86_400);
        let mut recipients: Vec<Recipient> = Vec::new();
        for r in raw.recipient {
            let name = r.name.get_ref().clone();
            let fail = |s: &Spanned<String>, msg: String| Err(err(at(s), msg));
            if !NAME.is_match(&name) {
                return fail(&r.name, "recipient name must match [a-z0-9-]+".into());
            }
            if recipients.iter().any(|x| x.name == name) {
                return fail(&r.name, format!("duplicate recipient `{name}`"));
            }
            let fail = |s: &Spanned<String>, msg: &str| Err(err(at(s), format!("`{name}`: {msg}")));
            if !AGE.is_match(r.age.get_ref()) {
                return fail(&r.age, "`age` is not an age public key (age1…)");
            }
            let ci = r.kind == Kind::Ci;
            if [r.repo.is_some(), r.env.is_some(), r.provider.is_some()] != [ci; 3] {
                let rule = match ci {
                    true => "kind ci needs repo, env and provider",
                    false => "kind human takes no repo, env or provider",
                };
                return fail(&r.name, rule);
            }
            if let Some(repo) = r.repo.as_ref().filter(|x| !REPO.is_match(x.get_ref())) {
                return fail(repo, "repo must be `owner/name`");
            }
            if let Some(env) = r.env.as_ref().filter(|x| !ENV.is_match(x.get_ref())) {
                return fail(env, "env must match [a-z0-9][a-z0-9-]*");
            }
            let date = |field: &str, s: &Spanned<String>| {
                let msg = format!("`{name}`: `{field}` is not a date YYYY-MM-DD");
                parse_date(s.get_ref()).ok_or_else(|| err(at(s), msg))
            };
            let expires_day = date("expires", &r.expires)?;
            let (base_day, from) = match &r.added {
                Some(a) => match date("added", a)? {
                    d if d > today => return fail(a, "`added` is in the future"),
                    d if expires_day < d => return fail(&r.expires, "`expires` is before `added`"),
                    d => (d, "`added`"),
                },
                None => (today, "today"),
            };
            if expires_day > add_months(base_day, 13) {
                let msg = format!("`expires` is more than 13 months after {from}");
                return fail(&r.expires, &msg);
            }
            recipients.push(Recipient {
                name,
                kind: r.kind,
                age: r.age.into_inner(),
                repo: r.repo.map(Spanned::into_inner),
                env: r.env.map(Spanned::into_inner),
                provider: r.provider,
                expires_day,
            });
        }
        if raw.rule.is_empty() {
            return Err(err(None, "no [[rule]]: add at least one".into()));
        }
        let mut rules: Vec<Rule> = Vec::new();
        for r in raw.rule {
            let path = r.path.get_ref().clone();
            if !GLOB.is_match(&path) || !clean(&path) {
                let msg = "rule path must be `<env>/<glob>` of [a-z0-9._*?/-], no `.`/`..`".into();
                return Err(err(at(&r.path), msg));
            }
            if rules.iter().any(|x| x.path == path) {
                return Err(err(at(&r.path), format!("rule `{path}` is defined twice")));
            }
            let fail = |msg: String| Err(err(at(&r.path), format!("rule `{path}`: {msg}")));
            let mut human = false;
            for name in &r.recipients {
                let Some(rec) = recipients.iter().find(|x| &x.name == name) else {
                    return fail("a `recipients` entry is not a recipient name".into());
                };
                if r.recipients.iter().filter(|n| *n == name).count() > 1 {
                    return fail(format!("`{name}` is listed twice"));
                }
                human |= rec.kind == Kind::Human;
                let env = rec.env.as_deref().unwrap_or_default();
                if rec.kind == Kind::Ci && !path.starts_with(&format!("{env}/")) {
                    return fail(format!(
                        "ci recipient `{name}` (env {env}) is only allowed under `{env}/`"
                    ));
                }
            }
            if !human {
                return fail("no human recipient".into());
            }
            rules.push(Rule {
                regex: Regex::new(&glob_regex(&path)).expect("escaped glob is a valid regex"),
                path,
                recipients: r.recipients,
            });
        }
        Ok(Access { recipients, rules })
    }

    /// The first rule whose glob matches `path` (relative to the repo root), as sops picks it.
    /// A path with a leading `/` or an empty, `.` or `..` segment matches no rule.
    pub fn rule_for(&self, path: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.matches(path))
    }

    /// The rule's recipients' public keys, in rule order.
    pub fn public_keys(&self, rule: &Rule) -> Vec<&str> {
        let key = |n: &String| self.recipients.iter().find(|r| &r.name == n);
        rule.recipients
            .iter()
            .filter_map(key)
            .map(|r| r.age.as_str())
            .collect()
    }
}

/// `.sops.yaml` for `access`: one creation rule per `[[rule]]`, in file order.
pub fn sops_yaml(access: &Access) -> String {
    let mut out = String::from(
        "# Generated by `hoist secrets sync` from access.toml. Do not edit.\ncreation_rules:\n",
    );
    for rule in &access.rules {
        let keys = access.public_keys(rule).join(",");
        out += &format!("  - path_regex: '{}'\n    age: {keys}\n", rule.regex);
    }
    out
}
