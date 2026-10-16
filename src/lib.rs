#![forbid(unsafe_code)]
//! hoist: provision and verify a repo's GitHub and Cloudflare setup.

pub mod auth;
pub mod cli;
pub mod cloudflare;
pub mod config;
pub mod desired;
pub mod diff;
pub mod error;
pub mod github;
pub mod http;
pub mod ops;
pub mod repo;
pub mod report;
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
        Command::Check { ref area } => ops::check::check(&dir, area, cli.verbose),
        Command::Auth {
            command: cli::AuthCommand::Check,
        } => ops::auth_check::auth_check(&dir, cli.verbose),
    }
}
