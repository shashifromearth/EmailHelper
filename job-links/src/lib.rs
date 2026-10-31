//! Job-link queue: a small local service that collects job links (from the
//! email and research agents, or added by hand) and hands them out to the
//! Chrome extension that fills the applications.

pub mod api;
pub mod canon;
pub mod config;
pub mod model;
pub mod snapshot;
pub mod store;
