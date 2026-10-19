use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Application error type shared by agents, storage and the HTTP layer.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("llm error: {0}")]
    Llm(#[from] crate::llm::LlmError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    fn code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::Other(format!("task join error: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.code();
        // Never leak internal error details to clients; they are logged instead.
        let message = match &self {
            Self::Validation(m) | Self::NotFound(m) => m.clone(),
            Self::Unauthorized => "missing or invalid X-EmailHelper-Token".to_string(),
            other => {
                tracing::error!(error = %other, "request failed");
                "internal error".to_string()
            }
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}
