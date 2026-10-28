use clap::{Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Area {
    Github,
    Cloudflare,
    /// npm trusted publishers and first publishes.
    Npm,
    /// crates.io trusted publishers and first publishes.
    Crates,
    /// The release environment and the release workflow.
    Release,
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
    /// Derive hoist.toml overrides from the live state; exit 1 when there are some
    Import {
        /// Write the overrides into the hoist.toml files (comments and other keys kept)
        #[arg(long)]
        write: bool,
        /// Limit to an area (repeatable)
        #[arg(long, value_enum)]
        area: Vec<Area>,
    },
    /// Check the permissions each token has against what it needs
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Plan, prepare and publish releases
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
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
        /// Catches a value typed after the name so it is refused without being echoed (R30).
        #[arg(hide = true, num_args = 0.., allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Check a secrets repo (access.toml) or a consumer's secrets.toml, without decrypting
    Check {
        /// Consumer repo: also look up every referenced file and key on GitHub (GET only)
        #[arg(long)]
        remote: bool,
    },
    /// In a secrets repo: a new age key for a consumer's environment (sealed into its
    /// SOPS_AGE_KEY), added as a recipient of every rule under the environment
    Grant {
        /// The consumer repo (owner/name)
        repo: String,
        /// Its GitHub environment, also the rules' path prefix (`<env>/`)
        #[arg(long)]
        env: String,
        /// Where the private key goes
        #[arg(long, default_value = "github", value_parser = ["github", "circleci"])]
        provider: String,
        /// The recipient's name in access.toml (default `<repo name>-<env>`)
        #[arg(long)]
        name: Option<String>,
        /// Overwrite the environment's SOPS_AGE_KEY; other secrets repos that list it must re-grant
        #[arg(long, conflicts_with = "public")]
        replace: bool,
        /// Add the key the environment already holds (its public half, age1…) instead of a new one
        #[arg(long, value_name = "AGE1")]
        public: Option<String>,
    },
    /// In a secrets repo: write .sops.yaml from access.toml and re-key files whose recipients changed
    Sync {
        /// Also give every file a new data key (`sops rotate -i`); after a revoke, and yearly
        #[arg(long)]
        rotate: bool,
    },
    /// Run a command with values from secrets repos, in CI only (secrets.toml; spec §15.7)
    Exec {
        /// The `[env.<E>]` table of secrets.toml
        #[arg(long, value_name = "E")]
        env: String,
        /// The keys of that table to pass, comma-separated; nothing else is fetched
        #[arg(long, value_name = "A,B", value_delimiter = ',', required = true)]
        keys: Vec<String>,
        /// The workflow file a `workflow_run` may be triggered by (.github/workflows/x.yml)
        #[arg(long, value_name = "PATH")]
        workflow_run_path: Option<String>,
        /// The command and its arguments, after `--`
        #[arg(last = true, required = true, num_args = 1.., value_name = "CMD")]
        cmd: Vec<OsString>,
    },
    /// Rotate a secret hoist minted (CLOUDFLARE_API_TOKEN): mint, verify, seal, revoke the old
    Rotate {
        /// Secret to rotate; all rotatable secrets when omitted
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
    /// Next versions from the commit history; read-only, always exit 0 (a preview)
    Plan,
    /// Open or update the release PR (branch `hoist/release`); closes it when nothing releases
    Prepare,
    /// Tag and publish the GitHub Release of every unreleased version (CI, release environment)
    Publish {
        /// Directory with the built binaries (`<bin>-<target>`, `.exe` on Windows)
        #[arg(long, value_name = "DIR")]
        assets: Option<PathBuf>,
        /// Show what would be done; GET requests only, no preflight
        #[arg(long)]
        dry_run: bool,
    },
    /// First publish and trusted publishers of missing packages, locally with your own logins
    Bootstrap {
        /// Show what would be done; GET requests and read-only tool commands only
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation (required in non-interactive runs)
        #[arg(long)]
        yes: bool,
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
            Command::Import { .. } => "import",
            Command::Auth {
                command: AuthCommand::Check,
            } => "auth check",
            Command::Release {
                command: ReleaseCommand::Plan,
            } => "release plan",
            Command::Release {
                command: ReleaseCommand::Prepare,
            } => "release prepare",
            Command::Release {
                command: ReleaseCommand::Publish { .. },
            } => "release publish",
            Command::Release {
                command: ReleaseCommand::Bootstrap { .. },
            } => "release bootstrap",
            Command::Secrets {
                command: SecretsCommand::Set { .. },
            } => "secrets set",
            Command::Secrets {
                command: SecretsCommand::Rotate { .. },
            } => "secrets rotate",
            Command::Secrets {
                command: SecretsCommand::Check { .. },
            } => "secrets check",
            Command::Secrets {
                command: SecretsCommand::Exec { .. },
            } => "secrets exec",
            Command::Secrets {
                command: SecretsCommand::Sync { .. },
            } => "secrets sync",
            Command::Secrets {
                command: SecretsCommand::Grant { .. },
            } => "secrets grant",
        }
    }
}
