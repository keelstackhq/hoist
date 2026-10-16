use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Area {
    Github,
    Cloudflare,
}

#[derive(Debug, Parser)]
#[command(
    name = "hoist",
    version,
    about = "Provision and verify a repo's GitHub and Cloudflare setup"
)]
pub struct Cli {
    /// Run as if started in this directory
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Print exactly one JSON object on stdout
    #[arg(long, global = true)]
    pub json: bool,
    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Log more to stderr
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the desired model and the file each item came from
    Explain,
    /// Diff desired against live state, no writes
    Plan {
        /// Limit to an area (repeatable)
        #[arg(long, value_enum)]
        area: Vec<Area>,
    },
    /// Write the planned changes, then verify; asks for confirmation unless --yes
    Apply {
        /// Do not ask for confirmation (required in non-interactive runs)
        #[arg(long)]
        yes: bool,
        /// Also revoke this repo's stale hoist-minted Cloudflare tokens
        #[arg(long)]
        prune: bool,
        /// Limit to an area (repeatable)
        #[arg(long, value_enum)]
        area: Vec<Area>,
    },
    /// Plan plus verification, read-only; exit 1 on drift
    Check {
        /// Limit to an area (repeatable)
        #[arg(long, value_enum)]
        area: Vec<Area>,
    },
    /// Check the permissions each token has against what it needs
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Manage GitHub Actions secrets (values come from the environment or a hidden prompt)
    Secrets {
        #[command(subcommand)]
        command: SecretsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum SecretsCommand {
    /// Set a secret from env NAME or a hidden prompt; never from an argument
    Set {
        /// Secret name (A-Z, 0-9, `_`)
        name: String,
    },
    /// Rotate a secret hoist minted (CLOUDFLARE_API_TOKEN): mint, verify, seal, revoke the old
    Rotate {
        /// Secret to rotate; all rotatable secrets when omitted
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Compare token permissions with what hoist needs
    Check,
}

impl Command {
    /// Name used in the report's `command` field.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Explain => "explain",
            Command::Plan { .. } => "plan",
            Command::Apply { .. } => "apply",
            Command::Check { .. } => "check",
            Command::Auth {
                command: AuthCommand::Check,
            } => "auth check",
            Command::Secrets {
                command: SecretsCommand::Set { .. },
            } => "secrets set",
            Command::Secrets {
                command: SecretsCommand::Rotate { .. },
            } => "secrets rotate",
        }
    }
}
