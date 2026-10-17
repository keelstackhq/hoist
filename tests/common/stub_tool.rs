use std::path::{Path, PathBuf};

/// Write an executable script `<dir>/<name>` that appends its arguments and the NAMES of the
/// env vars listed in `HOIST_STUB_ENV_NAMES`-style checks (never values) to `<dir>/<name>.log`,
/// then runs `script` (shell). `script` should print a fixed stdout. Returns the script path.
///
/// Each log line is `<args>` followed by ` env:<NAME>,<NAME>` for every `HOIST_*`, `NPM_*`,
/// `CARGO_*` and `NODE_*` variable present, so tests can assert a secret was handed over
/// without the value ever being written.
pub fn stub(dir: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let log = dir.join(format!("{name}.log"));
    let body = format!(
        "#!/bin/sh\n\
         names=$(env | sed -n 's/^\\(\\(HOIST\\|NPM\\|CARGO\\|NODE\\)_[A-Za-z0-9_]*\\)=.*/\\1/p' | sort | tr '\\n' ',')\n\
         echo \"$@ env:$names\" >> '{}'\n\
         {script}\n",
        log.display()
    );
    std::fs::write(&path, body).expect("write stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    path
}
