//! What hoist learns about the repository it runs in.

pub mod git;
pub mod workflows;

pub use git::GithubRepo;
use serde::Serialize;

/// A value inferred from the repo, with where it was found (`path:line`, path relative to the
/// repo root with `/` separators, line 1-based).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: String,
}
