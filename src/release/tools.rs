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

/// Run `tool` in `dir`. Returns its stdout; its stderr goes to hoist's stderr. `env` values
/// are passed to the child only and never appear in an error.
pub fn run(
    tool: Tool,
    dir: &Path,
    args: &[&str],
    env: &[(&str, &Secret)],
) -> Result<String, ToolError> {
    let err = |code, spawn_error| ToolError {
        tool,
        args: args.iter().map(|a| a.to_string()).collect(),
        code,
        spawn_error,
    };
    let mut cmd = Command::new(tool.program());
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    if tool == Tool::Git {
        // Never wait on a credential prompt (a private origin in `ls-remote`).
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never");
    }
    for (name, value) in env {
        cmd.env(name, value.expose());
    }
    let out = cmd.output().map_err(|e| err(None, Some(e.to_string())))?;
    if !out.status.success() {
        return Err(err(out.status.code(), None));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
