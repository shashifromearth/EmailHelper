//! HTTP API. Everything under `/v1` needs the `x-joblinks-token` header;
//! `/health`, `/livez` and `/readyz` are open. CORS admits
//! `chrome-extension://` origins only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::model::{ClaimRequest, LinkPatch, ListQuery, NewLink, ResultRequest};
use crate::store::{Store, StoreError};

pub const TOKEN_HEADER: &str = "x-joblinks-token";
pub const MAX_BATCH: usize = 500;
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub token: Arc<str>,
    pub default_lease: Duration,
    /// Checked by `/readyz`.
    pub snapshot_path: Arc<PathBuf>,
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.0.is_server_error() {
            tracing::error!(status = self.0.as_u16(), error = %self.1, "request failed");
        } else {
            tracing::warn!(status = self.0.as_u16(), error = %self.1, "request rejected");
        }
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        let code = match e {
            StoreError::NotFound => StatusCode::NOT_FOUND,
            StoreError::Conflict(_) => StatusCode::CONFLICT,
            StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
        };
        Self(code, e.to_string())
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self(e.status(), e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self(StatusCode::BAD_REQUEST, e.body_text())
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn parse<T: DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// Byte-wise comparison that doesn't stop at the first difference.
fn token_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(TOKEN_HEADER)
        .is_some_and(|v| token_eq(v.as_bytes(), st.token.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "missing or wrong x-joblinks-token".into()).into_response()
    }
}

fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            origin.as_bytes().starts_with(b"chrome-extension://")
        }))
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([header::CONTENT_TYPE, HeaderName::from_static(TOKEN_HEADER)])
        .max_age(Duration::from_secs(600))
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/links", post(create).get(list))
        .route("/v1/links/claim", post(claim))
        .route("/v1/links/{id}", get(get_one).patch(patch).delete(delete))
        .route("/v1/links/{id}/result", post(result))
        .route("/v1/stats", get(stats))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/health", get(health))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .merge(v1)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(cors())
        .with_state(state)
}

async fn health(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "status": "ok", "links": st.store.len() }))
}

/// The process is up.
async fn livez() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready when the snapshot can be written.
async fn readyz(State(st): State<AppState>) -> Response {
    let path = st.snapshot_path.clone();
    let res = tokio::task::spawn_blocking(move || crate::snapshot::check_writable(&path))
        .await
        .map_err(std::io::Error::other)
        .and_then(|r| r);
    match res {
        Ok(()) => Json(json!({ "status": "ready" })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, path = %st.snapshot_path.display(), "snapshot not writable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "error": format!("snapshot not writable: {e}") })),
            )
                .into_response()
        }
    }
}

/// One link (`{url,…}`), an array, or `{"links": [...]}`.
async fn create(
    State(st): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let now = Utc::now();
    let items = match body {
        Value::Array(a) => a,
        Value::Object(mut o) if o.get("links").is_some_and(Value::is_array) => {
            match o.remove("links") {
                Some(Value::Array(a)) => a,
                _ => unreachable!("checked above"),
            }
        }
        single => {
            let new: NewLink = parse(single).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
            let up = st.store.upsert(new, now)?;
            let code = if up.created { StatusCode::CREATED } else { StatusCode::OK };
            return Ok((code, Json(up)).into_response());
        }
    };
    if items.len() > MAX_BATCH {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_BATCH} links per request"),
        ));
    }
    let (mut created, mut updated, mut failed) = (0usize, 0usize, 0usize);
    let results: Vec<Value> = items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            match parse::<NewLink>(item).and_then(|n| st.store.upsert(n, now).map_err(|e| e.to_string())) {
                Ok(up) => {
                    if up.created {
                        created += 1;
                    } else {
                        updated += 1;
                    }
                    json!({ "index": index, "created": up.created, "link": up.link })
                }
                Err(error) => {
                    failed += 1;
                    json!({ "index": index, "error": error })
                }
            }
        })
        .collect();
    Ok(Json(json!({ "created": created, "updated": updated, "failed": failed, "results": results })).into_response())
}

async fn list(
    State(st): State<AppState>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> ApiResult<Json<Value>> {
    let Query(q) = q?;
    let (total, links) = st.store.list(&q, Utc::now());
    Ok(Json(json!({
        "total": total,
        "offset": q.offset.unwrap_or(0),
        "links": links,
    })))
}

async fn get_one(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    Ok(Json(st.store.get(&id, Utc::now())?).into_response())
}

async fn patch(
    State(st): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<LinkPatch>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(p) = body?;
    Ok(Json(st.store.patch(&id, p, Utc::now())?).into_response())
}

async fn delete(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    st.store.delete(&id, Utc::now())?;
    Ok(StatusCode::NO_CONTENT)
}

async fn claim(
    State(st): State<AppState>,
    body: Result<Json<ClaimRequest>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(req) = body?;
    let (links, lease_expires_at) = st.store.claim(req, st.default_lease, Utc::now())?;
    Ok(Json(json!({ "links": links, "lease_expires_at": lease_expires_at })))
}

async fn result(
    State(st): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<ResultRequest>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(req) = body?;
    Ok(Json(st.store.result(&id, req, Utc::now())?).into_response())
}

async fn stats(State(st): State<AppState>) -> Json<Value> {
    Json(json!(st.store.stats(Utc::now())))
}
