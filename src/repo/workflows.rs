//! Infer jobs, secrets, environments and the merge queue trigger from `.github/workflows`.

use super::Sourced;
use crate::HoistError;
use regex::Regex;
use std::fs;
use std::path::Path;
use yaml_serde::Value;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workflows {
    pub jobs: Vec<Sourced<String>>,
    pub secrets: Vec<Sourced<String>>,
    pub environments: Vec<Sourced<String>>,
    pub merge_group: bool,
    /// `path:line` of the first workflow with a `merge_group` trigger.
    pub merge_group_source: Option<String>,
    /// Every job that declares an `environment`, in file then job order.
    pub job_environments: Vec<JobEnv>,
    /// Which job reads which secret (a workflow-level reference counts for each of its jobs).
    pub secret_uses: Vec<SecretUse>,
    /// `file:job` of each job that passes `secrets: inherit` to a workflow outside this repo:
    /// which secrets it reads cannot be seen.
    pub opaque_inherits: Vec<String>,
    /// `file:job` of each job with a `secrets` use the scan cannot name (`toJSON(secrets)`,
    /// `secrets[matrix.n]`).
    pub unresolved_secrets: Vec<String>,
    /// `file:job` of each job whose token may post commit statuses: `statuses: write` or
    /// `write-all` in its own `permissions`, else in the workflow's.
    pub status_writers: Vec<String>,
    /// `file:job` of each job with no `permissions` (its own or the workflow's): its token
    /// gets the repo's default workflow permissions.
    pub default_permission_jobs: Vec<String>,
}

/// Secret references in YAML text: inside each `${{ … }}`, every `secrets.X` or
/// `secrets['X']` (any case; names upper-cased, as GitHub keeps them), and whether a
/// `secrets` use was left that names none.
struct SecretScan {
    expr: Regex,
    named: Regex,
    word: Regex,
}

impl SecretScan {
    fn new() -> SecretScan {
        let re = |r: &str| Regex::new(r).expect("static regex");
        SecretScan {
            expr: re(r"(?s)\$\{\{(.*?)\}\}"),
            named: re(
                r#"(?i)\bsecrets\s*(?:\.\s*([A-Za-z_][A-Za-z0-9_]*)|\[\s*['"]([A-Za-z_][A-Za-z0-9_]*)['"]\s*\])"#,
            ),
            word: re(r"(?i)\bsecrets\b"),
        }
    }

    /// (name, byte offset in `text`) of each named reference, and whether one was unresolved.
    fn scan(&self, text: &str) -> (Vec<(String, usize)>, bool) {
        let mut names = Vec::new();
        let mut unresolved = false;
        for e in self.expr.captures_iter(text) {
            let body = e.get(1).expect("group");
            for c in self.named.captures_iter(body.as_str()) {
                let n = c.get(1).or(c.get(2)).expect("a name group");
                let name = n.as_str().to_ascii_uppercase();
                if name != "GITHUB_TOKEN" {
                    names.push((name, body.start() + c.get(0).expect("match").start()));
                }
            }
            let rest = self.named.replace_all(body.as_str(), "");
            unresolved |= self.word.is_match(&rest);
        }
        (names, unresolved)
    }
}

/// One job's reference to a secret, with the environment the job runs in.
#[derive(Debug, Clone, PartialEq)]
pub struct SecretUse {
    pub secret: String,
    /// `file:job` (`deploy.yml:deploy`).
    pub job: String,
    pub environment: Option<String>,
}

/// A workflow job that runs in a GitHub environment.
#[derive(Debug, Clone, PartialEq)]
pub struct JobEnv {
    /// File name under `.github/workflows` (`release.yml`).
    pub file: String,
    /// Job id.
    pub job: String,
    pub environment: String,
}

const DIR: &str = ".github/workflows";

fn key(name: &str) -> Value {
    Value::String(name.to_string())
}

/// Whether a `permissions` value grants `statuses: write`.
fn grants_statuses(permissions: &Value) -> bool {
    match permissions {
        Value::String(s) => s == "write-all",
        Value::Mapping(m) => m.get(key("statuses")) == Some(&key("write")),
        _ => false,
    }
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

/// 1-based line of `  <job>:` (first match), or 1.
fn job_line(text: &str, job: &str) -> usize {
    let needle = format!("\n  {job}:");
    match text.find(&needle) {
        Some(i) => line_of(text, i + 1),
        None => 1,
    }
}

fn has_merge_group(on: &Value) -> bool {
    match on {
        Value::String(s) => s == "merge_group",
        Value::Sequence(items) => items
            .iter()
            .any(|v| matches!(v, Value::String(s) if s == "merge_group")),
        Value::Mapping(map) => map.contains_key(key("merge_group")),
        _ => false,
    }
}

fn push_unique(list: &mut Vec<Sourced<String>>, value: String, source: String) {
    if !list.iter().any(|s| s.value == value) {
        list.push(Sourced { value, source });
    }
}

fn config_err(file: &str, message: impl Into<String>) -> HoistError {
    HoistError::Config {
        file: file.to_string(),
        message: message.into(),
    }
}

/// Workflow files directly under `<dir>/.github/workflows`, sorted by file name.
fn workflow_files(dir: &Path) -> Result<Vec<String>, HoistError> {
    let wf_dir = dir.join(DIR);
    if !wf_dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries =
        fs::read_dir(&wf_dir).map_err(|e| config_err(DIR, format!("cannot read: {e}")))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| config_err(DIR, format!("cannot read: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_yaml = name.ends_with(".yml") || name.ends_with(".yaml");
        if is_yaml && entry.path().is_file() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// The secret names a YAML value references, and whether a use was left unnamed.
fn secrets_in(scan: &SecretScan, v: &Value) -> (Vec<String>, bool) {
    let text = yaml_serde::to_string(v).unwrap_or_default();
    let (names, unresolved) = scan.scan(&text);
    (names.into_iter().map(|(n, _)| n).collect(), unresolved)
}

pub fn read_workflows(dir: &Path) -> Result<Workflows, HoistError> {
    let scan = SecretScan::new();
    let mut out = Workflows::default();
    for name in workflow_files(dir)? {
        let rel = format!("{DIR}/{name}");
        let text = fs::read_to_string(dir.join(DIR).join(&name))
            .map_err(|e| config_err(&rel, format!("cannot read: {e}")))?;
        let doc: Value = yaml_serde::from_str(&text)
            .map_err(|e| config_err(&rel, format!("invalid YAML: {e}")))?;

        for (secret, at) in scan.scan(&text).0 {
            push_unique(
                &mut out.secrets,
                secret,
                format!("{rel}:{}", line_of(&text, at)),
            );
        }

        let Value::Mapping(top) = &doc else { continue };
        // YAML 1.1 parsers read the key `on` as boolean true; accept both spellings.
        if [key("on"), Value::Bool(true)]
            .iter()
            .any(|k| top.get(k).is_some_and(has_merge_group))
        {
            out.merge_group = true;
            if out.merge_group_source.is_none() {
                let line = text.find("merge_group").map_or(1, |i| line_of(&text, i));
                out.merge_group_source = Some(format!("{rel}:{line}"));
            }
        }
        let Some(jobs) = top.get(key("jobs")) else {
            continue;
        };
        let Value::Mapping(jobs) = jobs else {
            return Err(config_err(&rel, "`jobs` must be a mapping of job ids"));
        };
        // Secrets named outside `jobs` (a workflow-level `env:`) reach every job.
        let mut outer = top.clone();
        outer.remove(key("jobs"));
        let (outer_secrets, outer_unresolved) = secrets_in(&scan, &Value::Mapping(outer));
        for (id, job) in jobs {
            let Value::String(id) = id else { continue };
            let line = job_line(&text, id);
            let source = format!("{rel}:{line}");
            push_unique(&mut out.jobs, id.clone(), source.clone());
            let Value::Mapping(job) = job else { continue };
            // A job's own `permissions` replace the workflow's entirely.
            // `permissions: ~` is no override.
            let set = |v: Option<&Value>| v.filter(|v| !v.is_null()).cloned();
            match set(job.get(key("permissions"))).or(set(top.get(key("permissions")))) {
                Some(p) if grants_statuses(&p) => out.status_writers.push(format!("{name}:{id}")),
                Some(_) => {}
                None => out.default_permission_jobs.push(format!("{name}:{id}")),
            }
            if let Some(Value::String(label)) = job.get(key("name")) {
                push_unique(&mut out.jobs, label.clone(), source.clone());
            }
            let env = match job.get(key("environment")) {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Mapping(m)) => match m.get(key("name")) {
                    Some(Value::String(s)) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            };
            let inherits = job.get(key("secrets")) == Some(&key("inherit"));
            let remote = match job.get(key("uses")) {
                Some(Value::String(u)) => !u.starts_with("./"),
                _ => false,
            };
            if inherits && remote {
                out.opaque_inherits.push(format!("{name}:{id}"));
            }
            let (mut used, unresolved) = secrets_in(&scan, &Value::Mapping(job.clone()));
            if unresolved || outer_unresolved {
                out.unresolved_secrets.push(format!("{name}:{id}"));
            }
            used.extend(outer_secrets.iter().cloned());
            used.sort();
            used.dedup();
            for secret in used {
                out.secret_uses.push(SecretUse {
                    secret,
                    job: format!("{name}:{id}"),
                    environment: env.clone(),
                });
            }
            if let Some(env) = env {
                let env_line = text
                    .match_indices("environment:")
                    .map(|(i, _)| line_of(&text, i))
                    .find(|l| *l >= line)
                    .unwrap_or(line);
                out.job_environments.push(JobEnv {
                    file: name.clone(),
                    job: id.clone(),
                    environment: env.clone(),
                });
                push_unique(&mut out.environments, env, format!("{rel}:{env_line}"));
            }
        }
    }
    Ok(out)
}
