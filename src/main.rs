#![forbid(unsafe_code)]

use clap::Parser;
use clap::error::ErrorKind;
use hoist::{Cli, render_json, render_json_error, render_table};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 3,
            };
            let _ = err.print();
            if code == 3
                && args
                    .iter()
                    .take_while(|a| *a != "--")
                    .any(|a| a == "--json")
            {
                let command = command_name(&args);
                let message = err.to_string();
                let message = message.trim_end();
                let fix = format!("run `hoist {command} --help`").replace("  ", " ");
                println!("{}", render_json_error(&command, message, fix.trim_end()));
            }
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
                // `hoist:` tells hoist's own exit 2 or 3 from the command's in a CI log.
                eprintln!("hoist: error: {message}");
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
            if json {
                let command = command_name(&args);
                let fix = match err {
                    hoist::HoistError::Network(_) => {
                        "run `hoist auth check` to see which token is missing or too weak"
                    }
                    hoist::HoistError::Config { .. } => {
                        "fix the file or repository setting named in the message, then run again"
                    }
                    _ => "the message says what to change; see `hoist --help`",
                };
                println!("{}", render_json_error(&command, &err.to_string(), fix));
            }
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

/// The subcommand words of the command line (`plan`, `release plan`), skipping flags and the
/// value of `-C`. Empty when there is none.
fn command_name(args: &[String]) -> String {
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-C" {
            it.next();
        } else if a == "--" {
            break;
        } else if !a.starts_with('-') {
            if words.is_empty() && !KNOWN.contains(&a.as_str()) {
                break;
            }
            words.push(a);
            let group = matches!(words[0], "auth" | "release" | "secrets");
            if words.len() == 2 || !group {
                break;
            }
        }
    }
    words.join(" ")
}

const KNOWN: [&str; 9] = [
    "explain", "plan", "apply", "check", "import", "init", "auth", "release", "secrets",
];
