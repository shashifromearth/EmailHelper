//! Local REST API (127.0.0.1 only). See documents/API.md.

mod handlers;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::app::App;
use crate::error::AppError;

pub const TOKEN_HEADER: &str = "x-emailhelper-token";

/// Constant-time comparison to avoid leaking the token through timing.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let provided = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if app.cfg.api_token.is_empty()
        || !constant_time_eq(provided.as_bytes(), app.cfg.api_token.as_bytes())
    {
        app.metrics.failure("api_auth");
        return Err(AppError::Unauthorized);
    }
    // Reject browser-originated cross-site requests outright (defence in depth;
    // CORS is not enabled so browsers can't read responses anyway).
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let allowed = origin.starts_with("moz-extension://")
            || origin.starts_with("http://127.0.0.1")
            || origin.starts_with("http://localhost");
        if !allowed {
            return Err(AppError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert("x-frame-options", "DENY".parse().unwrap());
    h.insert("referrer-policy", "no-referrer".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    res
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/emails", post(handlers::submit_email))
        .route("/drafts/pending", get(handlers::pending_drafts))
        .route("/drafts/{id}/ack", post(handlers::ack_draft))
        .route("/status", get(handlers::status))
        .route("/dashboard", get(handlers::dashboard_data))
        .route("/jobs", get(handlers::list_jobs).post(handlers::submit_job))
        .route("/applications", get(handlers::list_applications))
        .route(
            "/applications/{id}/answer",
            post(handlers::answer_application),
        )
        .route(
            "/applications/{id}/resume",
            post(handlers::resume_application),
        )
        .route("/questions/{id}/answer", post(handlers::answer_question))
        .route("/recruiters", get(handlers::list_recruiters))
        .route("/research/run", post(handlers::run_research))
        .route("/audit", get(handlers::audit_log))
        .route_layer(middleware::from_fn_with_state(app.clone(), require_token));

    Router::new()
        .nest("/api/v1", api)
        .route("/health", get(handlers::health))
        .route("/liveness", get(|| async { (StatusCode::OK, "ok") }))
        .route("/readiness", get(handlers::readiness))
        .route("/metrics", get(handlers::metrics))
        .route("/dashboard", get(handlers::dashboard_page))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error":"not_found","message":"no such route"})),
            )
                .into_response()
        })
        .layer(DefaultBodyLimit::max(app.cfg.max_body_bytes))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(app)
}
