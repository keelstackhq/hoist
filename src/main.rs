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
    if let hoist::Command::Secrets {
        command: hoist::cli::SecretsCommand::Exec { .. },
    } = cli.command
    {
        return match hoist::exec(cli) {
            Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
            Err((code, message)) => {
                eprintln!("error: {message}");
                ExitCode::from(code as u8)
            }
        };
    }
    let json = cli.json;
    match hoist::run(cli) {
        Ok(report) => {
            if json {
                println!("{}", render_json(&report));
            } else {
                print!("{}", render_table(&report));
                if let Some(hint) = &report.hint {
                    // After the table: the hint refers to the steps above it.
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    eprint!("{hint}");
                }
            }
            ExitCode::from(report.exit_code as u8)
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
