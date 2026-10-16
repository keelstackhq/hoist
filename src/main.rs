#![forbid(unsafe_code)]

use clap::Parser;
use clap::error::ErrorKind;
use hoist::{Cli, render_json, render_table};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 3,
            };
            let _ = err.print();
            return ExitCode::from(code);
        }
    };
    let json = cli.json;
    match hoist::run(cli) {
        Ok(report) => {
            if json {
                println!("{}", render_json(&report));
            } else {
                print!("{}", render_table(&report));
            }
            ExitCode::from(report.exit_code as u8)
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
