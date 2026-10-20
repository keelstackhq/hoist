//! Running the external tools releases need (git, npm, cargo, pnpm). Paths come from env.

use crate::auth::Secret;
use crate::error::HoistError;
use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Git,
    Npm,
    Cargo,
    Pnpm,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Git => "git",
            Tool::Npm => "npm",
            Tool::Cargo => "cargo",
            Tool::Pnpm => "pnpm",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Tool::Git => "HOIST_GIT",
            Tool::Npm => "HOIST_NPM",
            Tool::Cargo => "HOIST_CARGO",
            Tool::Pnpm => "HOIST_PNPM",
        }
    }

    fn program(self) -> String {
        std::env::var(self.env_var())
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.name().to_string())
    }
}

/// A tool that could not run or exited non-zero. Holds the tool, the arguments and the exit
/// code, never environment values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub tool: Tool,
    pub args: Vec<String>,
    /// `None` when the tool did not start or was killed by a signal.
    pub code: Option<i32>,
    /// Why it did not start (the OS error), when it did not.
    pub spawn_error: Option<String>,
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cmd = format!("{} {}", self.tool.name(), self.args.join(" "));
        match (&self.spawn_error, self.code) {
            (Some(e), _) => write!(
                f,
                "could not run `{}`: {e}; install it or set {}",
                cmd.trim_end(),
                self.tool.env_var()
            ),
            (None, Some(c)) => write!(
                f,
                "`{}` failed (exit {c}); see its output above",
                cmd.trim_end()
            ),
            (None, None) => write!(f, "`{}` was killed by a signal", cmd.trim_end()),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<ToolError> for HoistError {
    fn from(e: ToolError) -> Self {
        HoistError::Usage(e.to_string())
    }
}

fn command(tool: Tool, dir: &Path, args: &[&str], env: &[(&str, &Secret)]) -> Command {
    let mut cmd = Command::new(tool.program());
    cmd.args(args).current_dir(dir).stdin(Stdio::null());
    if tool == Tool::Git {
        // Never wait on a credential prompt (a private origin in `ls-remote`).
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never");
    } else {
        // R27: npm, cargo and pnpm (and the scripts they run) never see the GitHub token.
        cmd.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    }
    for (name, value) in env {
        cmd.env(name, value.expose());
    }
    cmd
}

fn tool_error(
    tool: Tool,
    args: &[&str],
    code: Option<i32>,
    spawn_error: Option<String>,
) -> ToolError {
    ToolError {
        tool,
        args: args.iter().map(|a| a.to_string()).collect(),
        code,
        spawn_error,
    }
}

/// Run `tool` in `dir`. Returns its stdout; its stderr goes to hoist's stderr. When the tool
/// fails, its stdout is echoed to hoist's stderr too, so the error's "see its output above"
/// holds. `env` values are passed to the child only and never appear in an error.
pub fn run(
    tool: Tool,
    dir: &Path,
    args: &[&str],
    env: &[(&str, &Secret)],
) -> Result<String, ToolError> {
    let out = command(tool, dir, args, env)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| tool_error(tool, args, None, Some(e.to_string())))?;
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stdout));
        return Err(tool_error(tool, args, out.status.code(), None));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like `run` without env, but stderr is also captured: it is echoed to hoist's stderr and
/// returned next to the result (on failure too), for callers that must read the reason.
pub fn run_capture(tool: Tool, dir: &Path, args: &[&str]) -> (Result<String, ToolError>, String) {
    let out = match command(tool, dir, args, &[]).output() {
        Ok(out) => out,
        Err(e) => {
            return (
                Err(tool_error(tool, args, None, Some(e.to_string()))),
                String::new(),
            );
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprint!("{stderr}");
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stdout));
        return (Err(tool_error(tool, args, out.status.code(), None)), stderr);
    }
    (
        Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        stderr,
    )
}

/// Run `tool` in `dir` for a person at a terminal (R5): stdin is inherited (npm's 2FA OTP
/// prompt), stdout is streamed to hoist's stderr as it comes (an auth link shows at once and
/// `--json` stdout stays clean), stderr is inherited. Pass no secrets: nothing is redacted.
pub fn run_interactive(tool: Tool, dir: &Path, args: &[&str]) -> Result<(), ToolError> {
    let status = command(tool, dir, args, &[])
        .stdin(Stdio::inherit())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| tool_error(tool, args, None, Some(e.to_string())))?;
    if !status.success() {
        return Err(tool_error(tool, args, status.code(), None));
    }
    Ok(())
}

/// Replace every non-empty secret value in `text` with `***`.
fn redact(text: &str, env: &[(&str, &Secret)]) -> String {
    env.iter()
        .map(|(_, v)| v.expose())
        .filter(|v| !v.is_empty())
        .fold(text.to_string(), |t, v| t.replace(v, "***"))
}

/// Like `run`, for a tool whose output could carry a secret passed in `env` (R21): stdout and
/// stderr are captured, every `env` value is replaced by `***`, then both go to hoist's stderr.
/// Returns the redacted stdout.
pub fn run_redacted(
    tool: Tool,
    dir: &Path,
    args: &[&str],
    env: &[(&str, &Secret)],
) -> Result<String, ToolError> {
    let out = command(tool, dir, args, env)
        .output()
        .map_err(|e| tool_error(tool, args, None, Some(e.to_string())))?;
    let stdout = redact(&String::from_utf8_lossy(&out.stdout), env);
    eprint!("{stdout}");
    eprint!("{}", redact(&String::from_utf8_lossy(&out.stderr), env));
    if !out.status.success() {
        return Err(tool_error(tool, args, out.status.code(), None));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_replaces_every_secret_value() {
        let a = Secret::from("s3cr3t");
        let empty = Secret::from("");
        let env = [("A", &a), ("B", &empty)];
        assert_eq!(redact("x s3cr3t y s3cr3t", &env), "x *** y ***");
        assert_eq!(redact("nothing", &env), "nothing");
    }
}
