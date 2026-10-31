//! Service errors and their HTTP mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("upstream: {0}")]
    Upstream(String),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("knowledge: {0}")]
    Knowledge(#[from] eh_knowledge::AppError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Error::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Error::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            Error::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Error::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }
        (status, axum::Json(json!({ "error": code, "message": self.to_string() }))).into_response()
    }
}
