//! Client runtime for `ts-server-fn`.
//!
//! When a service crate is built **without** its `server` feature, every
//! `#[get]/#[post]/...` handler expands to a typed HTTP client fn that
//! delegates here: [`call`] does the serde, an object-safe
//! [`RequestProvider`] moves the bytes, and the provider registry
//! ([`set_endpoint`] / [`set_provider`]) maps each service package name to
//! its provider.
//!
//! ```ignore
//! // once at startup:
//! ts_server_fn_client::set_endpoint("search", "https://search.example.com");
//! // anywhere after:
//! let post = search::get_post(id).await?;
//! ```

mod call;
mod error;

pub use call::urlencode;
pub use error::ClientError;
pub use http::Method;
