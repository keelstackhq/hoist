//! `hoist changeset ...` commands. None sends an HTTP request.

mod add;
mod init;

use crate::cli::ChangesetCommand;
use crate::error::HoistError;
use crate::report::Report;
use std::path::Path;

pub fn run(dir: &Path, command: &ChangesetCommand) -> Result<Report, HoistError> {
    match command {
        ChangesetCommand::Add {
            packages,
            message,
            empty,
            id,
        } => add::add(dir, packages, message.as_deref(), *empty, id.as_deref()),
        ChangesetCommand::Init => init::init(dir),
        ChangesetCommand::Status => Err(HoistError::Usage(
            "`hoist changeset status` is not yet available; until then check by hand that the PR \
             adds a file under .changeset/"
                .into(),
        )),
    }
}

/// A report with one `create` item for a file this command wrote.
fn created(kind: &str, key: &str, rel: &str, reason: String) -> crate::Item {
    crate::Item {
        area: "changeset".into(),
        kind: kind.into(),
        key: key.into(),
        action: crate::Action::Create,
        reason,
        source: rel.into(),
        verify: None,
    }
}

fn report(command: &str, items: Vec<crate::Item>) -> Report {
    Report {
        command: command.into(),
        exit_code: 0,
        items,
        human_steps: vec![],
        text: None,
        hint: None,
    }
}
