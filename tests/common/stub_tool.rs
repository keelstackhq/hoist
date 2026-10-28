use std::path::{Path, PathBuf};

/// Write an executable POSIX shell script `<dir>/<name>`. Each call appends one line to
/// `<dir>/<name>.log`: its arguments, then ` env:` and the names (never values) of the
/// environment variables that start with `HOIST_`, `NPM_`, `NODE_`, `CARGO_`, `GITHUB_`,
/// `ACTIONS_`, `GH_`, `GIT_` or `GCM_`, then ` pwd:` and the working directory; for
/// `npm publish` also ` files:` and the comma-separated names in the working directory. Then
/// it runs `script`, which should print a fixed stdout.
pub fn stub(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    let log = dir.join(format!("{name}.log"));
    let files = if name == "npm" {
        "files=\"\"; [ \"$1\" = publish ] && files=\" files:$(ls -A | tr '\\n' ',')\"\n"
    } else {
        "files=\"\"\n"
    };
    let body = format!(
        "#!/bin/sh\n\
         names=$(env | grep -E '^(HOIST_|NPM_|NODE_|CARGO_|GITHUB_|ACTIONS_|GH_|GIT_|GCM_)' | cut -d= -f1 | sort | tr '\\n' ',')\n\
         {files}\
         echo \"$@ env:$names pwd:$(pwd)$files\" >> '{}'\n\
         {script}\n",
        log.display()
    );
    write_script(&path, body.as_bytes(), 0o755);
    path
}

/// Writes `body` to `path` (mode `mode`) through a child process. Had this process opened it for
/// writing, a fork on another test thread could inherit that descriptor until its child execs,
/// and running the file meanwhile fails with "text file busy" (ETXTBSY).
pub fn write_script(path: &Path, body: &[u8], mode: u32) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exec /bin/cat > \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("start /bin/sh");
    let written = child.stdin.take().expect("stdin").write_all(body);
    let status = child.wait().expect("wait for /bin/sh");
    assert!(
        written.is_ok() && status.success(),
        "write {}",
        path.display()
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}
