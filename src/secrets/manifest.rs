//! The consumer manifest `secrets.toml` (design §3.3): which secrets a repo needs, from which
//! secrets repos, per environment. Every error names the file and line (exit 3).

use super::config_err;
use crate::error::HoistError;
use regex::Regex;
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;
use toml::Spanned;

static VAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z_][A-Z0-9_]*$").unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").unwrap());
static REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9_][A-Za-z0-9._-]*$").unwrap()
});
/// Branch segments start with an alphanumeric (no `-x`, `//`, leading or trailing `/`).
static GIT_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*$").unwrap()
});
static REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([a-z0-9][a-z0-9-]*):([a-z0-9-]+/[a-z0-9_-][a-z0-9._-]*\.sops\.yaml)#([A-Za-z0-9_.-]+)$",
    )
    .unwrap()
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub sources: BTreeMap<String, Source>,
    pub env: BTreeMap<String, EnvSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// `owner/name` of the secrets repo.
    pub repo: String,
    /// Branch or commit SHA; `main` when not given.
    pub git_ref: String,
}

/// One `[env.<name>]` table: env var name -> secret, and `[env.<name>.files]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSpec {
    pub vars: BTreeMap<String, SecretRef>,
    pub files: BTreeMap<String, SecretRef>,
}

/// `<source>:<path>#<key>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub source: String,
    pub path: String,
    pub key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Spanned<i64>,
    #[serde(default)]
    sources: BTreeMap<Spanned<String>, RawSource>,
    #[serde(default)]
    env: BTreeMap<Spanned<String>, RawEnv>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    repo: Spanned<String>,
    #[serde(rename = "ref")]
    git_ref: Option<Spanned<String>>,
}

type Pairs = Vec<(Spanned<String>, Spanned<String>)>;

/// An env table keeps every key and value with its span, so errors can name the line.
#[derive(Default)]
struct RawEnv {
    vars: Pairs,
    files: Pairs,
}

impl<'de> Deserialize<'de> for RawEnv {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawEnv;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a table of ENV_VAR = \"<source>:<path>#<key>\"")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawEnv, A::Error> {
                let mut env = RawEnv::default();
                while let Some(key) = map.next_key::<Spanned<String>>()? {
                    if key.get_ref() == "files" {
                        let files: BTreeMap<Spanned<String>, Spanned<String>> = map.next_value()?;
                        env.files.extend(files);
                    } else {
                        env.vars.push((key, map.next_value()?));
                    }
                }
                Ok(env)
            }
        }
        d.deserialize_map(V)
    }
}

fn valid_git_ref(r: &str) -> bool {
    GIT_REF.is_match(r)
        && !r.contains("..")
        && r.split('/')
            .all(|c| !c.ends_with(".lock") && !c.ends_with('.'))
}

impl Manifest {
    /// Parse and validate `text`; `file` is the path shown in errors.
    pub fn parse(text: &str, file: &str) -> Result<Manifest, HoistError> {
        let raw: Raw = toml::from_str(text).map_err(|e| super::toml_err(file, text, e))?;
        let err = |at: usize, msg: String| config_err(file, Some(super::line_at(text, at)), msg);
        if *raw.version.get_ref() != 1 {
            return Err(err(raw.version.span().start, "version must be 1".into()));
        }
        let mut sources = BTreeMap::new();
        for (name, src) in raw.sources {
            let (at, name) = (name.span().start, name.into_inner());
            if !NAME.is_match(&name) {
                return Err(err(
                    at,
                    format!("source name `{name}` must match [a-z0-9][a-z0-9-]*"),
                ));
            }
            let repo = src.repo.get_ref();
            if !REPO.is_match(repo) || repo.contains("..") {
                let msg = format!("source `{name}`: repo must be `owner/name`");
                return Err(err(src.repo.span().start, msg));
            }
            let git_ref = match src.git_ref {
                None => "main".to_string(),
                Some(r) if valid_git_ref(r.get_ref()) => r.into_inner(),
                Some(r) => {
                    let msg = format!("source `{name}`: ref is not a branch name or commit SHA");
                    return Err(err(r.span().start, msg));
                }
            };
            let repo = src.repo.into_inner();
            sources.insert(name, Source { repo, git_ref });
        }
        let mut env = BTreeMap::new();
        for (name, raw_env) in raw.env {
            let env_name = name.get_ref().clone();
            if !NAME.is_match(&env_name) {
                let msg = format!("env name `{env_name}` must match [a-z0-9][a-z0-9-]*");
                return Err(err(name.span().start, msg));
            }
            let mut spec = EnvSpec::default();
            for (pairs, files) in [(raw_env.vars, false), (raw_env.files, true)] {
                for (var, value) in pairs {
                    let (at, var) = (var.span().start, var.into_inner());
                    if !VAR.is_match(&var) {
                        let msg = format!("env var name `{var}` must match [A-Z_][A-Z0-9_]*");
                        return Err(err(at, msg));
                    }
                    if spec.vars.contains_key(&var) || spec.files.contains_key(&var) {
                        return Err(err(at, format!("`{var}` is set twice in env `{env_name}`")));
                    }
                    let r = parse_ref(value.get_ref(), &env_name, &sources)
                        .map_err(|m| err(value.span().start, format!("`{var}`: {m}")))?;
                    let map = if files {
                        &mut spec.files
                    } else {
                        &mut spec.vars
                    };
                    map.insert(var, r);
                }
            }
            env.insert(env_name, spec);
        }
        Ok(Manifest { sources, env })
    }
}

type Sources = BTreeMap<String, Source>;

fn parse_ref(value: &str, env: &str, sources: &Sources) -> Result<SecretRef, String> {
    let Some(c) = REF.captures(value) else {
        return Err("the value is not `<source>:<path>#<key>` with path \
             `<env>/<bundle>.sops.yaml` (secret values never go in secrets.toml)"
            .to_string());
    };
    let (source, path, key) = (c[1].to_string(), c[2].to_string(), c[3].to_string());
    if !sources.contains_key(&source) {
        return Err("its source is not under [sources]".to_string());
    }
    if !path.starts_with(&format!("{env}/")) {
        return Err(format!("its path must start with `{env}/`"));
    }
    Ok(SecretRef { source, path, key })
}
