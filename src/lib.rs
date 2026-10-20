#![forbid(unsafe_code)]
//! hoist: provision and verify a repo's GitHub and Cloudflare setup.

pub mod auth;
pub mod change;
pub mod cli;
pub mod cloudflare;
pub mod cloudflare_write;
pub mod config;
pub mod desired;
pub mod diff;
pub mod diff_release;
pub mod error;
pub mod github;
pub mod github_write;
pub mod http;
pub mod ops;
pub mod release;
pub mod repo;
pub mod report;
pub mod seal;
pub mod timeutil;
pub mod verify;

pub use cli::{Area, Cli, Command};
pub use error::HoistError;
pub use report::{Action, HumanStep, Item, Report, Verify, render_json, render_table};

/// Run the parsed command and return its report.
pub fn run(cli: Cli) -> Result<Report, HoistError> {
    let dir = cli
        .dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match cli.command {
        Command::Explain => ops::explain::run(&dir),
        Command::Plan { ref area } => ops::plan::plan(&dir, area, cli.verbose),
        Command::Apply {
            yes,
            prune,
            ref area,
        } => ops::apply::apply(&dir, area, yes, prune, cli.verbose),
        Command::Check { ref area } => ops::check::check(&dir, area, cli.verbose),
        Command::Import { write, ref area } => ops::import::import(&dir, area, write, cli.verbose),
        Command::Auth {
            command: cli::AuthCommand::Check,
        } => ops::auth_check::auth_check(&dir, cli.verbose),
        Command::Release {
            command: cli::ReleaseCommand::Plan,
        } => ops::release::plan::release_plan(&dir),
        Command::Release {
            command: cli::ReleaseCommand::Prepare,
        } => ops::release::prepare::release_prepare(&dir, cli.verbose),
        Command::Release {
            command:
                cli::ReleaseCommand::Publish {
                    ref assets,
                    dry_run,
                },
        } => ops::release::publish::release_publish(&dir, assets.as_deref(), dry_run, cli.verbose),
        Command::Release {
            command: cli::ReleaseCommand::Bootstrap { dry_run, yes },
        } => ops::release::bootstrap::release_bootstrap(
            &dir,
            ops::release::bootstrap::BootstrapOpts {
                dry_run,
                yes,
                tty: std::io::IsTerminal::is_terminal(&std::io::stdin()),
            },
            cli.verbose,
        ),
        Command::Secrets {
            command:
                cli::SecretsCommand::Set {
                    ref name,
                    ref extra,
                },
        } => ops::secrets::secrets_set(&dir, name, extra, cli.verbose),
        Command::Secrets {
            command: cli::SecretsCommand::Rotate { ref name },
        } => ops::secrets::secrets_rotate(&dir, name.as_deref(), cli.verbose),
    }
}
