//! npm trusted publishers: reading `npm trust list <pkg> --json` (spec §9.6 step 4).
//!
//! The answer's shape is not verified against the real CLI (spec §13), so the parser accepts
//! the plausible forms and treats anything else as "not trusted": the worst case is a
//! registration that already exists, never a skipped one.

use crate::repo::git::GithubRepo;
use serde_json::Value;

const WORKFLOW_DIR: &str = ".github/workflows/";

/// A string field of `entry`, looked up in the entry itself, then in its `claims` object.
fn field<'a>(entry: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let scopes = [Some(entry), entry.get("claims")];
    scopes
        .into_iter()
        .flatten()
        .find_map(|s| keys.iter().find_map(|k| s.get(*k)))
}

/// `"o/r"` or `{"owner":"o","name":"r"}`.
fn repo_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => Some(format!(
            "{}/{}",
            v.get("owner")?.as_str()?,
            v.get("name")?.as_str()?
        )),
        _ => None,
    }
}

/// `"release.yml"`, `".github/workflows/release.yml"` or `{"file": ...}` (npm's `workflow_ref`).
fn workflow_of(v: &Value) -> Option<&str> {
    let s = match v {
        Value::String(s) => s.as_str(),
        Value::Object(_) => v.get("file")?.as_str()?,
        _ => return None,
    };
    Some(s.strip_prefix(WORKFLOW_DIR).unwrap_or(s))
}

fn matches(entry: &Value, repo: &GithubRepo, workflow: &str, env: &str) -> bool {
    if let Some(t) = entry.get("type").and_then(Value::as_str)
        && !t.eq_ignore_ascii_case("github")
    {
        return false;
    }
    let want_repo = repo.to_string();
    let want_file = workflow.strip_prefix(WORKFLOW_DIR).unwrap_or(workflow);
    let repo_ok = field(entry, &["repository", "repo"])
        .and_then(repo_of)
        .is_some_and(|r| r.eq_ignore_ascii_case(&want_repo));
    let file_ok = field(
        entry,
        &[
            "workflow_ref",
            "file",
            "workflow",
            "workflowFile",
            "workflow_file",
        ],
    )
    .and_then(workflow_of)
    .is_some_and(|f| f == want_file);
    let env_ok = field(entry, &["environment", "env"])
        .and_then(Value::as_str)
        .is_some_and(|e| e == env);
    repo_ok && file_ok && env_ok
}

/// Whether `list_json` (the stdout of `npm trust list <pkg> --json`) holds a GitHub entry for
/// `repo`, `workflow` (file name) and `env`. False on any parse error or unknown shape.
pub(crate) fn trusted(list_json: &str, repo: &GithubRepo, workflow: &str, env: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(list_json) else {
        return false;
    };
    let entries: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        // One entry, or an object wrapping the list.
        Value::Object(o) => std::iter::once(&v)
            .chain(o.values().filter_map(Value::as_array).flatten())
            .collect(),
        _ => Vec::new(),
    };
    entries.iter().any(|e| matches(e, repo, workflow, env))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> GithubRepo {
        GithubRepo {
            owner: "o".to_string(),
            name: "r".to_string(),
        }
    }

    #[test]
    fn trusted_matches_repo_string_and_plain_workflow() {
        let json = r#"[{"type":"github","repository":"o/r","workflow":"release.yml","environment":"release"}]"#;
        assert!(trusted(json, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_matches_repo_object_and_workflow_path() {
        let json = r#"[{"repository":{"owner":"o","name":"r"},"file":".github/workflows/release.yml","environment":"release"}]"#;
        assert!(trusted(json, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_matches_npm_claims_shape() {
        let json = r#"[{"id":"1","type":"github","claims":{"repository":"o/r","workflow_ref":{"file":"release.yml"},"environment":"release"}}]"#;
        assert!(trusted(json, &repo(), "release.yml", "release"));
        assert!(trusted(
            json,
            &repo(),
            ".github/workflows/release.yml",
            "release"
        ));
    }

    #[test]
    fn trusted_false_for_another_env() {
        let json = r#"[{"repository":"o/r","workflow":"release.yml","environment":"staging"}]"#;
        assert!(!trusted(json, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_false_for_another_repo_or_workflow() {
        let other_repo =
            r#"[{"repository":"o/x","workflow":"release.yml","environment":"release"}]"#;
        assert!(!trusted(other_repo, &repo(), "release.yml", "release"));
        let other_file = r#"[{"repository":"o/r","workflow":"ci.yml","environment":"release"}]"#;
        assert!(!trusted(other_file, &repo(), "release.yml", "release"));
        let gitlab = r#"[{"type":"gitlab","repository":"o/r","workflow":"release.yml","environment":"release"}]"#;
        assert!(!trusted(gitlab, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_false_for_garbage() {
        for text in [
            "",
            "not json",
            "npm ERR! 404",
            "42",
            "\"o/r\"",
            "[1,2]",
            "{}",
        ] {
            assert!(!trusted(text, &repo(), "release.yml", "release"), "{text}");
        }
    }

    #[test]
    fn trusted_false_for_empty_list() {
        assert!(!trusted("[]", &repo(), "release.yml", "release"));
    }
}
