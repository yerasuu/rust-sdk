//! Hosts the SAME handler source as `e2e-service`, but compiled here in
//! SERVER mode (this crate's `server` feature is on by default) — that
//! yields the real generated axum router. Meanwhile the `e2e-service`
//! dependency is built with `default-features = false`, so its copy of the
//! handlers is the generated HTTP client. `tests/roundtrip.rs` wires one to
//! the other over localhost.

#[path = "../../service/src/api.rs"]
pub mod server_api;
