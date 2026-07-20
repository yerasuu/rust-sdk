//! E2E fixture service. Built two ways:
//! - default features (`server`): real axum routes — tested in `tests/server.rs`.
//! - `default-features = false`: generated HTTP client — consumed by the
//!   sibling `e2e-client` crate.

pub mod api;
pub use api::*;
