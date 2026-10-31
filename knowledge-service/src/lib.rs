//! Knowledge service: indexes the knowledge folder (structure-aware ETL,
//! Gemini + local embeddings, sqlite-vec) and answers hybrid searches for the
//! email agent and the Chrome extension.

pub mod api;
pub mod config;
pub mod embed;
pub mod error;
pub mod index;
pub mod search;
pub mod store;
