//! The GitHub action `keelstackhq/hoist/secrets` (`secrets/action.yml`, spec §15.8): the inputs
//! hull's workflows pass, no `${{ }}` inside a script, sops pinned like CI, and the arguments the
//! exec step gives `hoist secrets exec` (run against a stub `hoist`, nothing downloaded).

mod common;

use std::path::Path;
use std::process::Command;
use yaml_serde::Value;

fn action() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("secrets/action.yml");
    yaml_serde::from_str(&std::fs::read_to_string(path).expect("secrets/action.yml")).unwrap()
}

fn steps(a: &Value) -> Vec<Value> {
    a["runs"]["steps"].as_sequence().expect("steps").clone()
}

#[test]
fn the_action_takes_what_hull_passes_and_keeps_inputs_out_of_scripts() {
    let a = action();
    assert_eq!(a["runs"]["using"].as_str(), Some("composite"));
    let inputs: Vec<&str> = a["inputs"]
        .as_mapping()
        .expect("inputs")
        .keys()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(
        inputs,
        [
            "env",
            "keys",
            "run",
            "app-id",
            "app-key",
            "age-key",
            "workflow-run-path"
        ]
    );
    for s in steps(&a) {
        // Inputs reach scripts through `env:` only, and no other action runs with the key.
        let run = s["run"].as_str().expect("every step is a script");
        assert!(!run.contains("${{"), "{run}");
        assert!(s.get("uses").is_none());
    }
    // The age key reaches only the exec step.
    let with_key: Vec<_> = steps(&a)
        .iter()
        .filter(|s| s["env"].get("SOPS_AGE_KEY").is_some())
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(with_key, ["Run with secrets"]);
}

#[test]
fn sops_is_pinned_as_ci_pins_it() {
    let install = steps(&action())[0]["run"].as_str().unwrap().to_string();
    let ci = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
    )
    .unwrap();
    let amd64 = "e5bec3346a873ae91d871550f3e698c1aad962aff462a080e40f25fde17fef6b";
    assert!(ci.contains(amd64) && install.contains(amd64));
    assert!(install.contains("v3.13.3"));
    // arm64, from sops-v3.13.3.checksums.txt.
    assert!(install.contains("53b0abacd38ef1b12a66d6c100956691b9cefce018d91f81e73ddf7438b94d77"));
}

/// Runs the exec step's script with a stub `hoist` that prints its arguments, one per line.
fn exec_args(workflow_run_path: &str, run: &str) -> Vec<String> {
    let a = action();
    let script = steps(&a)
        .into_iter()
        .find(|s| s["name"] == "Run with secrets")
        .expect("exec step")["run"]
        .as_str()
        .unwrap()
        .to_string();
    let dir = tempfile::tempdir().unwrap();
    common::write_script(
        &dir.path().join("hoist"),
        b"#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
        0o755,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HOIST_BIN_DIR", dir.path())
        .env("INPUT_ENV", "preview")
        .env("INPUT_KEYS", "A,B")
        .env("INPUT_RUN", run)
        .env("INPUT_WORKFLOW_RUN_PATH", workflow_run_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_exec_step_passes_the_inputs_as_arguments_never_as_code() {
    let run = "node x.mjs deploy \"$(id)\"; echo done";
    let args = exec_args("", run);
    let want = [
        "secrets", "exec", "--env", "preview", "--keys", "A,B", "--", "bash", "-eo", "pipefail",
        "-c", run,
    ];
    assert_eq!(args, want);
    let args = exec_args(".github/workflows/preview.yml", "x");
    assert_eq!(
        args[6..8],
        ["--workflow-run-path", ".github/workflows/preview.yml"]
    );
}
