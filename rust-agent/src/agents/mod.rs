//! Email agents: classifier → context → drafter → validator, orchestrated by
//! [`pipeline::process_email`].

pub mod classifier;
pub mod context;
pub mod drafter;
pub mod facts;
pub mod pipeline;
pub mod questions;
pub mod validator;
