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
}

const DIR: &str = ".github/workflows";

fn key(name: &str) -> Value {
    Value::String(name.to_string())
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

pub fn read_workflows(dir: &Path) -> Result<Workflows, HoistError> {
    let secret_re =
        Regex::new(r"\$\{\{\s*secrets\.([A-Za-z_][A-Za-z0-9_]*)\s*\}\}").expect("static regex");
    let mut out = Workflows::default();
    for name in workflow_files(dir)? {
        let rel = format!("{DIR}/{name}");
        let text = fs::read_to_string(dir.join(DIR).join(&name))
            .map_err(|e| config_err(&rel, format!("cannot read: {e}")))?;
        let doc: Value = yaml_serde::from_str(&text)
            .map_err(|e| config_err(&rel, format!("invalid YAML: {e}")))?;

        for cap in secret_re.captures_iter(&text) {
            let secret = &cap[1];
            if secret == "GITHUB_TOKEN" {
                continue;
            }
            let at = cap.get(0).map_or(0, |m| m.start());
            push_unique(
                &mut out.secrets,
                secret.to_string(),
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
        }
        let Some(jobs) = top.get(key("jobs")) else {
            continue;
        };
        let Value::Mapping(jobs) = jobs else {
            return Err(config_err(&rel, "`jobs` must be a mapping of job ids"));
        };
        for (id, job) in jobs {
            let Value::String(id) = id else { continue };
            let line = job_line(&text, id);
            let source = format!("{rel}:{line}");
            push_unique(&mut out.jobs, id.clone(), source.clone());
            let Value::Mapping(job) = job else { continue };
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
            if let Some(env) = env {
                let env_line = text
                    .match_indices("environment:")
                    .map(|(i, _)| line_of(&text, i))
                    .find(|l| *l >= line)
                    .unwrap_or(line);
                push_unique(&mut out.environments, env, format!("{rel}:{env_line}"));
            }
        }
    }
    Ok(out)
}
