//! Knowledge domain shared by the email agent and the knowledge service:
//! people and their mailboxes, profiles, resumes, and the structure-aware ETL
//! that turns the knowledge folder into chunks.

pub mod docx;
pub mod etl;
pub mod people;
pub mod profile;
pub mod text;

pub use people::{People, Person, Route};
pub use profile::Profile;

/// Errors from reading knowledge files.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

pub type AppResult<T> = Result<T, AppError>;
