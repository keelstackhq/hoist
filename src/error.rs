use thiserror::Error;

/// Usage, config or auth errors. Every message says how to fix the problem.
#[derive(Debug, Error)]
pub enum HoistError {
    #[error("{0}")]
    Usage(String),
}

impl HoistError {
    pub fn exit_code(&self) -> i32 {
        3
    }
}
