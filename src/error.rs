use thiserror::Error;

/// Usage, config or auth errors. Every message says how to fix the problem.
#[derive(Debug, Error)]
pub enum HoistError {
    #[error("{0}")]
    Usage(String),
    #[error("{file}: {message}")]
    Config { file: String, message: String },
    /// A failed API call; the message names the service and path, never headers or bodies.
    #[error("{0}")]
    Network(String),
}

impl HoistError {
    pub fn exit_code(&self) -> i32 {
        3
    }
}
