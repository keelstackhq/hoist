use std::path::{Path, PathBuf};

/// Write an executable POSIX shell script `<dir>/<name>`. Each call appends one line to
/// `<dir>/<name>.log`: its arguments, then ` env:` and the names (never values) of the
/// environment variables that start with `HOIST_`, `NPM_`, `NODE_`, `CARGO_`, `GITHUB_`,
/// `ACTIONS_`, `GH_`, `GIT_` or `GCM_`. Then it runs `script`, which should print a fixed stdout.
pub fn stub(dir: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let log = dir.join(format!("{name}.log"));
    let body = format!(
        "#!/bin/sh\n\
         names=$(env | grep -E '^(HOIST_|NPM_|NODE_|CARGO_|GITHUB_|ACTIONS_|GH_|GIT_|GCM_)' | cut -d= -f1 | sort | tr '\\n' ',')\n\
         echo \"$@ env:$names\" >> '{}'\n\
         {script}\n",
        log.display()
    );
    std::fs::write(&path, body).expect("write stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    path
}
