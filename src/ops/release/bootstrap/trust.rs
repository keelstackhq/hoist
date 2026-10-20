//! npm trusted publishers: reading `npm trust list <pkg> --json` and registering with
//! `npm trust github` (spec §9.6 step 4).
//!
//! Verified shape (npm 11.15.0 and 12.2.0, `lib/trust-cmd.js` `logOptions`, spec §13): with
//! `--json` npm prints one pretty-printed object per config, separated by blank lines (a stream
//! of objects, not an array), each flat:
//! `{"id", "type": "github", "file": "release.yml", "repository": "o/r", "environment",
//! "permissions"}`; no configs print nothing. The parser reads the whole stream, still accepts
//! an array, a wrapper object or the `claims` form, and treats anything else as "not trusted":
//! the worst case is a registration that already exists, never a skipped one.

use super::Trust;
use crate::error::HoistError;
use crate::release::tools::{Tool, run, run_interactive};
use crate::repo::git::GithubRepo;
use serde_json::Value;
use std::path::Path;

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

/// Whether `list_json` (the stdout of `npm trust list <pkg> --json`: one object per config)
/// holds a GitHub entry for `repo`, `workflow` (file name) and `env`. False when nothing
/// parses or no entry matches.
pub(crate) fn trusted(list_json: &str, repo: &GithubRepo, workflow: &str, env: &str) -> bool {
    // Every value of the stream up to the first parse error (a broken tail keeps the configs
    // read before it).
    let values = serde_json::Deserializer::from_str(list_json)
        .into_iter::<Value>()
        .map_while(Result::ok);
    let mut entries: Vec<Value> = Vec::new();
    for v in values {
        match v {
            Value::Array(a) => entries.extend(a),
            // One entry, or an object wrapping the list.
            Value::Object(ref o) => {
                let inner: Vec<Value> = o
                    .values()
                    .filter_map(Value::as_array)
                    .flatten()
                    .cloned()
                    .collect();
                entries.push(v);
                entries.extend(inner);
            }
            _ => {}
        }
    }
    entries.iter().any(|e| matches(e, repo, workflow, env))
}

/// The arguments of `npm trust github <pkg> --file <workflow> --repo <o>/<r> --env <env>
/// --allow-publish`.
pub(crate) fn trust_args(pkg: &str, t: &Trust) -> Vec<String> {
    let repo = t.repo.to_string();
    [
        "trust",
        "github",
        pkg,
        "--file",
        t.workflow.as_str(),
        "--repo",
        repo.as_str(),
        "--env",
        t.env.as_str(),
        "--allow-publish",
    ]
    .map(str::to_string)
    .to_vec()
}

/// The full command, as a human step shows it.
pub(crate) fn trust_command(pkg: &str, t: &Trust) -> String {
    format!("npm {}", trust_args(pkg, t).join(" "))
}

/// `npm trust list <pkg> --json`; a failed call or an unparsable answer is "not trusted".
pub(crate) fn npm_trusted(dir: &Path, pkg: &str, t: &Trust) -> bool {
    run(Tool::Npm, dir, &["trust", "list", pkg, "--json"], &[])
        .is_ok_and(|out| trusted(&out, &t.repo, &t.workflow, &t.env))
}

/// How a registration ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Registered {
    /// `npm trust github` ran and `npm trust list` now shows the entry.
    Done,
    /// `npm trust list` already showed the entry: nothing written.
    AlreadyTrusted,
    /// `npm trust github` exited 0 but `npm trust list` still lacks the entry: a failed
    /// verification (the message says what to check), not an API or tool error.
    Unverified(String),
}

/// Register `pkg`'s trusted publisher: read `npm trust list` first (a re-run or a race skips
/// the write), run `npm trust github` with the terminal's stdin, stdout and stderr (npm
/// prompts for 2FA), then read the list again to verify. A non-zero exit is an error; an
/// entry still missing is [`Registered::Unverified`].
pub(crate) fn register(dir: &Path, pkg: &str, t: &Trust) -> Result<Registered, HoistError> {
    if npm_trusted(dir, pkg, t) {
        return Ok(Registered::AlreadyTrusted);
    }
    let args = trust_args(pkg, t);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_interactive(Tool::Npm, dir, &args)?;
    if !npm_trusted(dir, pkg, t) {
        return Ok(Registered::Unverified(format!(
            "`npm trust github` exited 0 but `npm trust list {pkg} --json` does not show {}; \
             check https://www.npmjs.com/package/{pkg}/access, then run bootstrap again",
            t.describe()
        )));
    }
    Ok(Registered::Done)
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
    fn trust_command_is_the_exact_command() {
        let t = Trust {
            repo: GithubRepo {
                owner: "acme".to_string(),
                name: "x".to_string(),
            },
            workflow: "release.yml".to_string(),
            env: "release".to_string(),
        };
        assert_eq!(
            trust_command("@acme/lib", &t),
            "npm trust github @acme/lib --file release.yml --repo acme/x --env release --allow-publish"
        );
    }

    /// npm 11.15.0 / 12.2.0 print one pretty-printed object per config (`lib/trust-cmd.js`
    /// `logOptions`), separated by blank lines.
    fn npm_config(file: &str, env: &str) -> String {
        format!(
            "{{\n  \"id\": \"0d4c\",\n  \"type\": \"github\",\n  \"file\": \"{file}\",\n  \
             \"repository\": \"o/r\",\n  \"environment\": \"{env}\",\n  \
             \"permissions\": [\n    \"publish\"\n  ]\n}}"
        )
    }

    #[test]
    fn trusted_reads_npms_flat_object() {
        let one = format!("{}\n", npm_config("release.yml", "release"));
        assert!(trusted(&one, &repo(), "release.yml", "release"));
        let other = format!("{}\n", npm_config("release.yml", "staging"));
        assert!(!trusted(&other, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_reads_every_config_in_the_stream() {
        let two = format!(
            "{}\n\n{}\n",
            npm_config("old.yml", "release"),
            npm_config("release.yml", "release")
        );
        assert!(trusted(&two, &repo(), "release.yml", "release"));
        let none = format!(
            "{}\n\n{}\n",
            npm_config("old.yml", "release"),
            npm_config("release.yml", "staging")
        );
        assert!(!trusted(&none, &repo(), "release.yml", "release"));
        // A broken tail keeps the configs read before it.
        let tail = format!("{}\n\n{{\"id\":", npm_config("release.yml", "release"));
        assert!(trusted(&tail, &repo(), "release.yml", "release"));
    }

    #[test]
    fn trusted_false_for_an_empty_answer() {
        for text in ["", "\n", "  \n\n"] {
            assert!(
                !trusted(text, &repo(), "release.yml", "release"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn trusted_false_for_empty_list() {
        assert!(!trusted("[]", &repo(), "release.yml", "release"));
    }
}
