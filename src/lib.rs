#![forbid(unsafe_code)]
//! hoist: provision and verify a repo's GitHub and Cloudflare setup.

pub mod auth;
pub mod cli;
pub mod config;
pub mod desired;
pub mod diff;
pub mod error;
pub mod github;
pub mod http;
pub mod ops;
pub mod repo;
pub mod report;

pub use cli::{Area, Cli, Command};
pub use error::HoistError;
pub use report::{Action, HumanStep, Item, Report, Verify, render_json, render_table};

/// Run the parsed command and return its report. Only `explain` is implemented so far; the
/// other commands return an empty report until their tasks land.
pub fn run(cli: Cli) -> Result<Report, HoistError> {
    let dir = cli
        .dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match cli.command {
        Command::Explain => ops::explain::run(&dir),
        _ => Ok(Report {
            command: cli.command.name().to_string(),
            items: Vec::new(),
            human_steps: Vec::new(),
            exit_code: 0,
        }),
    }
}
