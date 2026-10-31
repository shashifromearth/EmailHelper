//! HTTP API for the email agent and the Chrome extension.
//!
//! - GET  /livez, /readyz, /health
//! - POST /v1/search                       hybrid search
//! - POST /v1/index/run, GET /v1/index      run / inspect the ETL
//! - GET  /v1/people, /v1/people/{id}/profile
//! - POST /v1/people/{id}/resume            pick the resume for a job (by skills/stack)
//! - GET  /v1/people/{id}/resumes/{file}    the resume file (for uploads)

use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use eh_knowledge::People;

use crate::error::{Error, Result};
use crate::index::Indexer;
use crate::search::{Query, Searcher};
use crate::store;

pub struct AppState {
    pub token: String,
    pub indexer: Arc<Indexer>,
    pub searcher: Searcher,
    pub people: RwLock<Arc<People>>,
}

impl AppState {
    pub fn people(&self) -> Arc<People> {
        self.people.read().map(|p| p.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn reload_people(&self) {
        let p = Arc::new(People::load(&self.indexer.dir));
        if let Ok(mut w) = self.people.write() {
            *w = p;
        }
    }

    /// Run the ETL and reload people (new resumes, profiles).
    pub async fn reindex(self: &Arc<Self>) -> Result<crate::index::Report> {
        let r = self.indexer.run().await?;
        self.reload_people();
        Ok(r)
    }
}

type S = State<Arc<AppState>>;

async fn auth(State(s): S, req: Request, next: Next) -> Response {
    let ok = s.token.is_empty()
        || req
            .headers()
            .get("x-knowledge-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == s.token);
    if ok {
        next.run(req).await
    } else {
        Error::Unauthorized.into_response()
    }
}

async fn livez() -> &'static str {
    "ok"
}

/// Ready when the database answers and something has been indexed.
async fn readyz(State(s): S) -> Response {
    match s.searcher.store.call(|c| store::status(c)).await {
        Ok(st) if st.chunks > 0 => (StatusCode::OK, Json(json!({ "status": "ready", "chunks": st.chunks }))).into_response(),
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "not_ready", "reason": "index is empty" }))).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "not_ready", "reason": e.to_string() }))).into_response(),
    }
}

async fn health(State(s): S) -> Result<Json<Value>> {
    let st = s.searcher.store.call(|c| store::status(c)).await?;
    let models: Vec<Value> = s.searcher.embedders.iter().map(|e| json!({ "id": e.id(), "model": e.model() })).collect();
    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "index": st,
        "indexing": s.indexer.is_running(),
        "embedders": models,
    })))
}

async fn search(State(s): S, Json(q): Json<Query>) -> Result<Json<Value>> {
    if q.query.trim().is_empty() {
        return Err(Error::Invalid("query is empty".into()));
    }
    Ok(Json(serde_json::to_value(s.searcher.search(&q).await?).unwrap_or_default()))
}

async fn index_run(State(s): S) -> Result<(StatusCode, Json<Value>)> {
    if s.indexer.is_running() {
        return Ok((StatusCode::CONFLICT, Json(json!({ "status": "already_running" }))));
    }
    let s2 = s.clone();
    tokio::spawn(async move {
        if let Err(e) = s2.reindex().await {
            tracing::error!(error = %e, "index run failed");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "started" }))))
}

async fn index_status(State(s): S) -> Result<Json<Value>> {
    let (st, report) = s
        .searcher
        .store
        .call(|c| Ok((store::status(c)?, store::get_meta(c, "last_etl_report")?)))
        .await?;
    let report: Value = report.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null);
    Ok(Json(json!({ "index": st, "running": s.indexer.is_running(), "last_report": report })))
}

async fn people(State(s): S) -> Json<Value> {
    let p = s.people();
    let list: Vec<Value> = p
        .persons
        .iter()
        .map(|x| {
            json!({
                "id": x.id,
                "name": x.name,
                "mailboxes": x.mailboxes.iter().map(|m| json!({ "address": m.address, "purposes": m.purposes })).collect::<Vec<_>>(),
                "resumes": x.resumes.iter().map(|r| json!({ "file": r.file_name, "stack": r.stack, "purpose": r.purpose, "default": r.default })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Json(json!({ "people": list }))
}

fn person(s: &AppState, id: &str) -> Result<Arc<eh_knowledge::Person>> {
    s.people().person(id).ok_or_else(|| Error::NotFound(format!("person {id}")))
}

/// The person's profile for form filling. Private facts (private.json) are never included.
async fn profile(State(s): S, Path(id): Path<String>) -> Result<Json<Value>> {
    let p = person(&s, &id)?;
    Ok(Json(json!({ "id": p.id, "name": p.name, "profile": &*p.profile })))
}

#[derive(Deserialize)]
struct ResumeReq {
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    purpose: Option<String>,
}

async fn pick_resume(State(s): S, Path(id): Path<String>, Json(r): Json<ResumeReq>) -> Result<Json<Value>> {
    let p = person(&s, &id)?;
    let (res, matched) = p
        .select_resume(&r.skills, r.purpose.as_deref())
        .ok_or_else(|| Error::NotFound(format!("no resume for {id}")))?;
    Ok(Json(json!({
        "file": res.file_name,
        "stack": res.stack,
        "purpose": res.purpose,
        "matched": matched,
        "default": res.default,
        "download": format!("/v1/people/{}/resumes/{}", p.id, res.file_name),
    })))
}

async fn resume_file(State(s): S, Path((id, file)): Path<(String, String)>) -> Result<Response> {
    let p = person(&s, &id)?;
    // only files the person actually has: no paths from the request reach the disk
    let r = p
        .resumes
        .iter()
        .find(|r| r.file_name == file)
        .ok_or_else(|| Error::NotFound(format!("resume {file}")))?;
    let bytes = tokio::fs::read(&r.path).await?;
    let ctype = match file.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        _ => "application/octet-stream",
    };
    Ok(([(header::CONTENT_TYPE, ctype)], bytes).into_response())
}

/// Chrome extensions and the local dashboard may call the API.
fn cors() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{AllowOrigin, CorsLayer};
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|o: &HeaderValue, _| {
            let o = o.to_str().unwrap_or("");
            o.starts_with("chrome-extension://") || o.starts_with("http://127.0.0.1:") || o.starts_with("http://localhost:")
        }))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([header::CONTENT_TYPE, header::HeaderName::from_static("x-knowledge-token")])
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/v1/search", post(search))
        .route("/v1/index", get(index_status))
        .route("/v1/index/run", post(index_run))
        .route("/v1/people", get(people))
        .route("/v1/people/{id}/profile", get(profile))
        .route("/v1/people/{id}/resume", post(pick_resume))
        .route("/v1/people/{id}/resumes/{file}", get(resume_file))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/health", get(health))
        .merge(api)
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(60)))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024))
        .layer(cors())
        .with_state(state)
}
