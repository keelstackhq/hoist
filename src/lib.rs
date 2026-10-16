#![forbid(unsafe_code)]
//! hoist: provision and verify a repo's GitHub and Cloudflare setup.

pub mod cli;
pub mod config;
pub mod error;
pub mod repo;
pub mod report;

pub use cli::{Area, Cli, Command};
pub use error::HoistError;
pub use report::{Action, HumanStep, Item, Report, Verify, render_json, render_table};

/// Run the parsed command and return its report. M1 commands return an empty report.
pub fn run(cli: Cli) -> Result<Report, HoistError> {
    Ok(Report {
        command: cli.command.name().to_string(),
        items: Vec::new(),
        human_steps: Vec::new(),
        exit_code: 0,
    })
}
