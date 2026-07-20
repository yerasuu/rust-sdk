# ts-server-fn Dual Mode (server/client) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `#[get]/#[post]/...` handlers expand to axum routes when the consuming crate's `server` feature is on, and to typed reqwest-backed client functions (with a per-package provider registry) when it is off.

**Architecture:** The proc macro emits *both* expansions, gated by `#[cfg(feature = "server")]` / `#[cfg(not(feature = "server"))]`, evaluated against the consuming service crate's own features. A new runtime crate `ts-server-fn-client` holds the object-safe `RequestProvider` transport trait, the reqwest-based `HttpProvider`, a global provider registry keyed by `env!("CARGO_PKG_NAME")`, and a generic `call()` helper that does all serde. Spec: `docs/superpowers/specs/2026-07-20-ts-server-fn-dual-mode-design.md`.

**Tech Stack:** Rust proc-macro (syn 2 / quote), axum 0.8, reqwest 0.12, serde / serde_json / serde_urlencoded, percent-encoding, inventory.

## Global Constraints

- The feature name is literally `server` and is checked in the **consuming** crate (`#[cfg(feature = "server")]` emitted as tokens; never `cfg!()` inside the proc-macro crate).
- Registry key is the Cargo **package name verbatim** (`env!("CARGO_PKG_NAME")`, dashes included).
- Generated client fns: `<name>(args…)` (registry lookup) and `<name>_with(provider, args…)` (explicit). Return type is the handler's return type **verbatim** (alias-safe); the macro never names `E`.
- `raw` handlers and handlers whose return type is not Result-like (per `route::is_result_like`) get **no client expansion** (server-only); the server expansion is unchanged.
- New/changed crates use `edition = "2021"` (matches existing `packages/*`).
- Versions: `ts-server-fn` 0.2.0 → **0.3.0**; new `ts-server-fn-client` **0.1.0**; `ts-server-fn-axum` unchanged.
- E2E fixture crates live in `fixtures/ts-server-fn-e2e/` **outside the workspace** (each with its own empty `[workspace]` table, `publish = false`) — this avoids Cargo feature unification between server-mode and client-mode builds of the same package.
- All commands run from the repo root unless a `cd` is shown.

## File Structure

```
packages/ts-server-fn-client/          # NEW runtime crate
  Cargo.toml
  src/lib.rs                           # re-exports only
  src/error.rs                         # ClientError
  src/provider.rs                      # BoxFuture, RawResponse, RequestProvider, HttpProvider
  src/registry.rs                      # set_provider / set_endpoint / provider_for
  src/call.rs                          # urlencode + generic call<Q,B,T,E>()
packages/ts-server-fn/
  src/lib.rs                           # MODIFY: testable expand(), cfg-gate server items
  src/clientgen.rs                     # NEW: client-fn token generation
packages/ts-server-fn-axum/
  Cargo.toml                           # MODIFY: prune dev-deps
  tests/emit.rs                        # DELETE (migrates to fixtures e2e service)
fixtures/ts-server-fn-e2e/
  service/{Cargo.toml, src/lib.rs, src/api.rs, tests/server.rs}   # NEW
  client/{Cargo.toml, src/lib.rs, tests/roundtrip.rs}             # NEW
Makefile                               # MODIFY: PACKAGES += ts-server-fn-client; test.e2e target
.github/workflows/pr-workflow.yml      # MODIFY: run make test.e2e
packages/ts-server-fn/README.md        # MODIFY: dual-mode docs
packages/ts-server-fn-client/README.md # NEW
docs/superpowers/specs/2026-07-20-ts-server-fn-dual-mode-design.md  # MODIFY: small addenda
```

---

### Task 1: `ts-server-fn-client` scaffold — `ClientError` + `urlencode`

**Files:**
- Create: `packages/ts-server-fn-client/Cargo.toml`
- Create: `packages/ts-server-fn-client/src/lib.rs`
- Create: `packages/ts-server-fn-client/src/error.rs`
- Create: `packages/ts-server-fn-client/src/call.rs` (only `urlencode` in this task)

**Interfaces:**
- Produces: `ClientError { Transport(reqwest::Error), Encode(String), Decode { status: u16, source: serde_json::Error } }` implementing `Debug + Display + std::error::Error`; `pub fn urlencode(&str) -> String`; re-export `pub use http::Method;`.
- Note: `Encode` is a small spec addendum (query/body serialization failure); the spec is updated in Task 9.

- [ ] **Step 1: Create the crate manifest**

`packages/ts-server-fn-client/Cargo.toml`:

```toml
[package]
name = "ts-server-fn-client"
version = "0.1.0"
edition = "2021"
description = "Client runtime for ts-server-fn: RequestProvider transport trait, reqwest-based HttpProvider, and the per-package provider registry."
license = "Apache-2.0"
keywords = ["api", "client", "codegen"]
repository = "https://github.com/biyard/rust-sdk/tree/main/packages/ts-server-fn-client"

[dependencies]
bytes = "1"
http = "1"
percent-encoding = "2"
reqwest = "0.12"
serde = "1"
serde_json = "1"
serde_urlencoded = "0.7"

[dev-dependencies]
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

(The crate is picked up automatically by the workspace `members = ["packages/*"]` glob.)

- [ ] **Step 2: Write failing tests**

`packages/ts-server-fn-client/src/call.rs`:

```rust
//! URL-encoding helper + (in a later task) the generic `call()` the generated
//! client fns delegate to.

/// Percent-encode one path segment. Mirrors the TS runtime's
/// `encodeURIComponent` (slightly stricter — also encodes `-`, `.`, `_`, `~` —
/// which is safe: the server percent-decodes either way).
pub fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_escapes_separators_and_spaces() {
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
        assert_eq!(urlencode("plain42"), "plain42");
    }
}
```

`packages/ts-server-fn-client/src/error.rs`:

```rust
//! Transport-level failures of the generated client. API-level errors are the
//! handler's own `E` (decoded from the non-2xx body); `ClientError` covers
//! everything that happens *around* that, and reaches the caller through the
//! `E: From<ClientError>` conversion.

use std::fmt;

#[derive(Debug)]
pub enum ClientError {
    /// Connection / TLS / timeout — the request never produced a response.
    Transport(reqwest::Error),
    /// Query or body serialization failed before sending (programmer error).
    Encode(String),
    /// The response body could not be decoded as `T` (2xx) or `E` (non-2xx).
    Decode {
        status: u16,
        source: serde_json::Error,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Transport(e) => write!(f, "transport error: {e}"),
            ClientError::Encode(e) => write!(f, "request encode error: {e}"),
            ClientError::Decode { status, source } => {
                write!(f, "response decode error (status {status}): {source}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::Transport(e) => Some(e),
            ClientError::Encode(_) => None,
            ClientError::Decode { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_status_for_decode() {
        let e = ClientError::Decode {
            status: 500,
            source: serde_json::from_slice::<()>(b"nope").unwrap_err(),
        };
        assert!(e.to_string().contains("status 500"), "{e}");
    }
}
```

`packages/ts-server-fn-client/src/lib.rs`:

```rust
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
```

- [ ] **Step 3: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn-client`
Expected: PASS (`urlencode_escapes_separators_and_spaces`, `display_includes_status_for_decode`)

- [ ] **Step 4: Commit**

```bash
git add packages/ts-server-fn-client
git commit -m "feat(ts-server-fn-client): scaffold crate with ClientError and urlencode"
```

---

### Task 2: `RequestProvider` trait + `HttpProvider`

**Files:**
- Create: `packages/ts-server-fn-client/src/provider.rs`
- Modify: `packages/ts-server-fn-client/src/lib.rs`

**Interfaces:**
- Consumes: `ClientError` (Task 1).
- Produces:
  - `pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`
  - `pub struct RawResponse { pub status: u16, pub body: bytes::Bytes }`
  - `pub trait RequestProvider: Send + Sync { fn request_raw<'a>(&'a self, method: Method, path_and_query: &'a str, body: Option<Vec<u8>>) -> BoxFuture<'a, Result<RawResponse, ClientError>>; }`
  - `pub struct HttpProvider` with `new(base_url)`, `with_client(base_url, reqwest::Client)`, `base_url()`.

- [ ] **Step 1: Write the failing test**

Bottom of `packages/ts-server-fn-client/src/provider.rs` (file content in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_provider_normalizes_trailing_slash() {
        let p = HttpProvider::new("https://search.example.com/");
        assert_eq!(p.base_url(), "https://search.example.com");
        let p = HttpProvider::new("https://search.example.com");
        assert_eq!(p.base_url(), "https://search.example.com");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ts-server-fn-client`
Expected: FAIL to compile — `provider` module / `HttpProvider` not defined yet (add `mod provider;` to lib.rs first so the failure is the missing type, not a missing module).

- [ ] **Step 3: Write the implementation**

`packages/ts-server-fn-client/src/provider.rs`:

```rust
//! The transport layer. `RequestProvider` is deliberately **object-safe**
//! (it must live behind `dyn` in the provider registry), so it moves raw
//! bytes; all serde lives in [`crate::call`]. Decorators (rate limiting,
//! retry, auth injection) wrap a provider at the byte level.

use std::future::Future;
use std::pin::Pin;

use crate::error::ClientError;
use http::Method;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Raw HTTP response: status + undecoded body bytes.
pub struct RawResponse {
    pub status: u16,
    pub body: bytes::Bytes,
}

/// Object-safe transport used by every generated client fn.
///
/// `path_and_query` is already interpolated + percent-encoded (e.g.
/// `/api/posts/42?limit=5`); implementations prepend their base URL.
/// `body`, when `Some`, is JSON bytes — send with
/// `content-type: application/json`.
pub trait RequestProvider: Send + Sync {
    fn request_raw<'a>(
        &'a self,
        method: Method,
        path_and_query: &'a str,
        body: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<RawResponse, ClientError>>;
}

/// Default provider: a thin reqwest wrapper bound to one base URL.
pub struct HttpProvider {
    base_url: String,
    client: reqwest::Client,
}

impl HttpProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client(base_url, reqwest::Client::new())
    }

    /// Use a pre-configured `reqwest::Client` (timeouts, proxies, headers…).
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self { base_url, client }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl RequestProvider for HttpProvider {
    fn request_raw<'a>(
        &'a self,
        method: Method,
        path_and_query: &'a str,
        body: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<RawResponse, ClientError>> {
        Box::pin(async move {
            let url = format!("{}{}", self.base_url, path_and_query);
            let mut req = self.client.request(method, &url);
            if let Some(b) = body {
                req = req.header("content-type", "application/json").body(b);
            }
            let resp = req.send().await.map_err(ClientError::Transport)?;
            let status = resp.status().as_u16();
            let body = resp.bytes().await.map_err(ClientError::Transport)?;
            Ok(RawResponse { status, body })
        })
    }
}
```

In `packages/ts-server-fn-client/src/lib.rs`, add the module and exports:

```rust
mod call;
mod error;
mod provider;

pub use call::urlencode;
pub use error::ClientError;
pub use http::Method;
pub use provider::{BoxFuture, HttpProvider, RawResponse, RequestProvider};
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn-client`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add packages/ts-server-fn-client
git commit -m "feat(ts-server-fn-client): object-safe RequestProvider + reqwest HttpProvider"
```

---

### Task 3: Provider registry

**Files:**
- Create: `packages/ts-server-fn-client/src/registry.rs`
- Modify: `packages/ts-server-fn-client/src/lib.rs`

**Interfaces:**
- Consumes: `RequestProvider`, `HttpProvider` (Task 2).
- Produces: `pub fn set_provider(pkg: &str, provider: impl RequestProvider + 'static)`, `pub fn set_endpoint(pkg: &str, base_url: &str)`, `#[doc(hidden)] pub fn provider_for(pkg: &str) -> Arc<dyn RequestProvider>` (panics with an actionable message when missing).

- [ ] **Step 1: Write the failing tests**

Bottom of `packages/ts-server-fn-client/src/registry.rs` (file content in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_endpoint_then_provider_for_roundtrips_and_resets() {
        set_endpoint("reg-test-pkg", "http://one.example.com/");
        let _p = provider_for("reg-test-pkg"); // must not panic
        // re-set is allowed (tests swap providers)
        set_endpoint("reg-test-pkg", "http://two.example.com");
        let _p = provider_for("reg-test-pkg");
    }

    #[test]
    #[should_panic(expected = "no provider registered for package `missing-pkg`")]
    fn provider_for_missing_panics_with_hint() {
        provider_for("missing-pkg");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ts-server-fn-client`
Expected: FAIL to compile — `registry` module not defined (add `mod registry;` + exports to lib.rs first so the failure is the missing functions).

- [ ] **Step 3: Write the implementation**

`packages/ts-server-fn-client/src/registry.rs`:

```rust
//! Global provider registry, keyed by the *service crate's* Cargo package
//! name. Generated client fns look themselves up with
//! `env!("CARGO_PKG_NAME")` — the token is emitted into the service crate,
//! so each service gets its own key (and therefore its own domain) for free.
//!
//! One registry per linked copy of this crate: every crate in the final
//! binary must resolve to a single semver-compatible `ts-server-fn-client`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::provider::{HttpProvider, RequestProvider};

type Registry = RwLock<HashMap<String, Arc<dyn RequestProvider>>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the provider for a service package. `pkg` is the
/// Cargo package name **verbatim** (dashes included, e.g. `"search-service"`).
pub fn set_provider(pkg: &str, provider: impl RequestProvider + 'static) {
    registry()
        .write()
        .expect("ts-server-fn-client registry poisoned")
        .insert(pkg.to_string(), Arc::new(provider));
}

/// Convenience: register the default [`HttpProvider`] for a base URL.
pub fn set_endpoint(pkg: &str, base_url: &str) {
    set_provider(pkg, HttpProvider::new(base_url));
}

/// Look up the provider for a package. Called by generated code.
///
/// Panics when nothing is registered: a missing provider is a startup wiring
/// bug, not a recoverable condition.
#[doc(hidden)]
pub fn provider_for(pkg: &str) -> Arc<dyn RequestProvider> {
    let map = registry()
        .read()
        .expect("ts-server-fn-client registry poisoned");
    if let Some(p) = map.get(pkg) {
        return p.clone();
    }
    let mut known: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
    known.sort_unstable();
    panic!(
        "ts-server-fn-client: no provider registered for package `{pkg}`. \
         Call `ts_server_fn_client::set_endpoint(\"{pkg}\", \"https://…\")` \
         (or `set_provider`) once at startup. Registered packages: {known:?}"
    );
}
```

In `lib.rs` add:

```rust
mod registry;
pub use registry::{provider_for, set_endpoint, set_provider};
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn-client`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add packages/ts-server-fn-client
git commit -m "feat(ts-server-fn-client): per-package provider registry"
```

---

### Task 4: Generic `call()` helper

**Files:**
- Modify: `packages/ts-server-fn-client/src/call.rs`
- Modify: `packages/ts-server-fn-client/src/lib.rs` (export `call`)

**Interfaces:**
- Consumes: `RequestProvider`, `RawResponse`, `BoxFuture`, `ClientError`, `Method`.
- Produces (exact signature generated code relies on):

```rust
pub async fn call<Q, B, T, E>(
    provider: &dyn RequestProvider,
    method: Method,
    path: &str,
    query: Option<&Q>,
    body: Option<&B>,
) -> Result<T, E>
where
    Q: serde::Serialize + ?Sized,
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned + From<ClientError>;
```

- [ ] **Step 1: Write the failing tests**

Replace the `tests` module in `packages/ts-server-fn-client/src/call.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{BoxFuture, RawResponse, RequestProvider};
    use std::sync::Mutex;

    #[test]
    fn urlencode_escapes_separators_and_spaces() {
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
        assert_eq!(urlencode("plain42"), "plain42");
    }

    // ── mock transport: canned response + request recording ─────────
    struct Mock {
        status: u16,
        body: &'static [u8],
        seen: Mutex<Vec<(Method, String, Option<Vec<u8>>)>>,
    }
    impl Mock {
        fn new(status: u16, body: &'static [u8]) -> Self {
            Mock { status, body, seen: Mutex::new(Vec::new()) }
        }
    }
    impl RequestProvider for Mock {
        fn request_raw<'a>(
            &'a self,
            method: Method,
            path_and_query: &'a str,
            body: Option<Vec<u8>>,
        ) -> BoxFuture<'a, Result<RawResponse, ClientError>> {
            self.seen
                .lock()
                .unwrap()
                .push((method, path_and_query.to_owned(), body));
            let status = self.status;
            let bytes = bytes::Bytes::from_static(self.body);
            Box::pin(async move { Ok(RawResponse { status, body: bytes }) })
        }
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Dto {
        id: String,
    }
    #[derive(serde::Serialize)]
    struct Req {
        title: String,
    }
    #[derive(serde::Serialize)]
    struct Qs {
        limit: u32,
        tag: String,
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct ApiErr {
        code: String,
    }
    impl From<ClientError> for ApiErr {
        fn from(e: ClientError) -> Self {
            ApiErr { code: format!("client:{e}") }
        }
    }

    const NO_Q: Option<&()> = None;
    const NO_B: Option<&()> = None;

    #[tokio::test]
    async fn ok_2xx_decodes_success_type() {
        let mock = Mock::new(200, br#"{"id":"1"}"#);
        let r: Result<Dto, ApiErr> = call(&mock, Method::GET, "/x", NO_Q, NO_B).await;
        assert_eq!(r.unwrap(), Dto { id: "1".into() });
    }

    #[tokio::test]
    async fn empty_2xx_body_is_unit() {
        let mock = Mock::new(200, b"");
        let r: Result<(), ApiErr> = call(&mock, Method::DELETE, "/x", NO_Q, NO_B).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn non_2xx_decodes_api_error() {
        let mock = Mock::new(400, br#"{"code":"boom"}"#);
        let r: Result<Dto, ApiErr> = call(&mock, Method::GET, "/x", NO_Q, NO_B).await;
        assert_eq!(r.unwrap_err(), ApiErr { code: "boom".into() });
    }

    #[tokio::test]
    async fn garbage_2xx_is_decode_error() {
        let mock = Mock::new(200, b"not json");
        let r: Result<Dto, ApiErr> = call(&mock, Method::GET, "/x", NO_Q, NO_B).await;
        assert!(r.unwrap_err().code.starts_with("client:"), "expected ClientError conversion");
    }

    #[tokio::test]
    async fn garbage_non_2xx_is_decode_error() {
        let mock = Mock::new(500, b"<html>oops</html>");
        let r: Result<Dto, ApiErr> = call(&mock, Method::GET, "/x", NO_Q, NO_B).await;
        assert!(r.unwrap_err().code.starts_with("client:"), "expected ClientError conversion");
    }

    #[tokio::test]
    async fn query_is_serialized_onto_path() {
        let mock = Mock::new(200, b"null");
        let q = Qs { limit: 5, tag: "a b".into() };
        let _: Result<(), ApiErr> = call(&mock, Method::GET, "/x", Some(&q), NO_B).await;
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].1, "/x?limit=5&tag=a+b");
        assert!(seen[0].2.is_none());
    }

    #[tokio::test]
    async fn body_is_json_bytes() {
        let mock = Mock::new(200, br#"{"id":"1"}"#);
        let b = Req { title: "hello".into() };
        let _: Result<Dto, ApiErr> = call(&mock, Method::POST, "/x", NO_Q, Some(&b)).await;
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].0, Method::POST);
        assert_eq!(seen[0].2.as_deref(), Some(br#"{"title":"hello"}"#.as_slice()));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ts-server-fn-client`
Expected: FAIL to compile — `call` not defined.

- [ ] **Step 3: Write the implementation**

Add to `packages/ts-server-fn-client/src/call.rs` (above the tests):

```rust
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::ClientError;
use crate::provider::{RawResponse, RequestProvider};
use http::Method;

/// The single entry point generated client fns delegate to. Serializes the
/// query/body, sends through the provider, and decodes the response:
///
/// - 2xx → body as `T` (empty body reads as JSON `null`, so `Result<(), E>`
///   handlers map 200/204-with-no-body to `Ok(())` — mirror of the TS
///   runtime's `void` handling).
/// - non-2xx → body as `E` (mirror of the server's `(status, Json(E))`).
/// - transport/encode/decode failures → `E::from(ClientError)`.
///
/// `T` and `E` are *inferred* from the calling fn's return type, so
/// `*Result<T>` aliases work without the macro ever naming `E`.
pub async fn call<Q, B, T, E>(
    provider: &dyn RequestProvider,
    method: Method,
    path: &str,
    query: Option<&Q>,
    body: Option<&B>,
) -> Result<T, E>
where
    Q: Serialize + ?Sized,
    B: Serialize + ?Sized,
    T: DeserializeOwned,
    E: DeserializeOwned + From<ClientError>,
{
    let mut path_and_query = path.to_owned();
    if let Some(q) = query {
        let qs = serde_urlencoded::to_string(q)
            .map_err(|e| E::from(ClientError::Encode(e.to_string())))?;
        if !qs.is_empty() {
            path_and_query.push('?');
            path_and_query.push_str(&qs);
        }
    }
    let body_bytes = match body {
        Some(b) => Some(
            serde_json::to_vec(b).map_err(|e| E::from(ClientError::Encode(e.to_string())))?,
        ),
        None => None,
    };

    let RawResponse { status, body } = provider
        .request_raw(method, &path_and_query, body_bytes)
        .await
        .map_err(E::from)?;

    let bytes: &[u8] = if body.is_empty() { b"null" } else { &body };
    if (200..300).contains(&status) {
        serde_json::from_slice::<T>(bytes)
            .map_err(|e| E::from(ClientError::Decode { status, source: e }))
    } else {
        match serde_json::from_slice::<E>(bytes) {
            Ok(api_err) => Err(api_err),
            Err(e) => Err(E::from(ClientError::Decode { status, source: e })),
        }
    }
}
```

In `lib.rs` change the `call` export line to:

```rust
pub use call::{call, urlencode};
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn-client`
Expected: PASS (12 tests)

- [ ] **Step 5: Commit**

```bash
git add packages/ts-server-fn-client
git commit -m "feat(ts-server-fn-client): generic call() with serde + status handling"
```

---

### Task 5: Macro — testable `expand()` + cfg-gate the server expansion

**Files:**
- Modify: `packages/ts-server-fn/src/lib.rs`
- Delete: `packages/ts-server-fn-axum/tests/emit.rs` (its content is ported to the e2e fixture in Task 7 — do not lose the handler/test scenarios)
- Modify: `packages/ts-server-fn-axum/Cargo.toml` (prune now-unused dev-deps)

**Interfaces:**
- Consumes: existing `route::classify`, `emit_axum`, `write_ts`, `tsgen`.
- Produces: `fn expand(method: &str, attr: TokenStream2, item: TokenStream2) -> TokenStream2` (unit-testable full expansion; Task 6 appends client tokens to its output). Server items each carry `#[cfg(feature = "server")]`.

**Why delete emit.rs in this task:** once server items are cfg-gated, they vanish in `ts-server-fn-axum`'s test build (that crate has no `server` feature), so `emit.rs` stops compiling. Removing it in the same commit keeps the tree green; Task 7 restores the coverage in the fixture where the feature exists.

- [ ] **Step 1: Write the failing test**

Append inside the existing `mod snapshot_tests` in `packages/ts-server-fn/src/lib.rs`:

```rust
    // ── full-expansion snapshots (server cfg-gating) ─────────────────
    fn norm(ts: &proc_macro2::TokenStream) -> String {
        ts.to_string().chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn server_items_are_cfg_gated() {
        let out = crate::expand(
            "GET",
            quote::quote! { "/api/posts/{id}" },
            quote::quote! {
                pub async fn get_post(Path(id): Path<String>) -> ApiResult<Post> {
                    unreachable!()
                }
            },
        );
        let s = norm(&out);
        // original fn, wrapper, and inventory registration all server-gated
        assert!(s.contains(r##"#[cfg(feature="server")]pubasyncfnget_post"##), "{s}");
        assert!(s.contains(r##"#[cfg(feature="server")]#[doc(hidden)]"##), "{s}");
        assert!(s.contains(r##"#[cfg(feature="server")]::ts_server_fn_axum::inventory::submit!"##), "{s}");
    }
```

Also add `quote` usage is already imported at crate top; the test uses fully-qualified `quote::quote!` so no import changes are needed.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ts-server-fn server_items_are_cfg_gated`
Expected: FAIL to compile — `crate::expand` not defined.

- [ ] **Step 3: Restructure `server_fn_impl` into `expand` and gate server items**

In `packages/ts-server-fn/src/lib.rs`, replace `server_fn_impl` with:

```rust
/// Shared expansion for every HTTP-method attribute.
fn server_fn_impl(method: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    expand(method, attr.into(), item.into()).into()
}

/// Proc-macro-free core so the full expansion is unit-testable.
fn expand(method: &str, attr: TokenStream2, item: TokenStream2) -> TokenStream2 {
    let route: RouteAttr = match syn::parse2(attr) {
        Ok(r) => r,
        Err(e) => return e.to_compile_error(),
    };
    let func: ItemFn = match syn::parse2(item) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error(),
    };

    // ── 1. Classify + (optionally) emit TS. Validation errors surface as
    //    compile errors regardless of whether generation is enabled. ───
    let _meta = match route::classify(
        method,
        &route,
        &func.sig.ident,
        &func.sig.inputs,
        &func.sig.output,
    ) {
        Ok(meta) => {
            if let Some(dir) = write_ts::package_dir() {
                let rendered = tsgen::render(method, &meta);
                write_ts::write_handler(&dir, "", &rendered.fn_name_camel, &rendered.source);
            }
            meta
        }
        Err(e) => {
            // Keep the original fn in the output so downstream sees the symbol,
            // but attach the diagnostic.
            let err = e.to_compile_error();
            return quote! { #err #func };
        }
    };

    // ── 2. Emit the cfg-gated pure-axum route + impl. ────────────────
    match emit_axum(method, &route, &func) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error(),
    }
}
```

Change `emit_axum` to take `func: &ItemFn` (adjust the two `.clone()` lines accordingly: `let vis = func.vis.clone(); let name = func.sig.ident.clone();` already clone — just change the parameter and the `#func` interpolation still works with a reference), and gate every emitted item in its final `quote!`:

```rust
    let expanded = quote! {
        #[cfg(feature = "server")]
        #func

        #[cfg(feature = "server")]
        #[doc(hidden)]
        #vis async fn #route_name(#(#wrapper_params),*) -> ::axum::response::Response {
            #body
        }

        #[cfg(feature = "server")]
        ::ts_server_fn_axum::inventory::submit! {
            ::ts_server_fn_axum::ApiRoute {
                method: #method_str,
                path: #axum_path,
                register: |__r: ::axum::Router| __r.route(
                    #axum_path,
                    ::axum::routing::#method_router(#route_name),
                ),
            }
        }
    };
    Ok(expanded)
```

- [ ] **Step 4: Delete the now-uncompilable axum e2e test and prune dev-deps**

```bash
git rm packages/ts-server-fn-axum/tests/emit.rs
```

Replace the `[dev-dependencies]` section of `packages/ts-server-fn-axum/Cargo.toml` with nothing (delete the whole section — `ts-server-fn`, `serde`, `serde_json`, `tokio`, `tower` were only used by `emit.rs`).

- [ ] **Step 5: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn && cargo test -p ts-server-fn-axum && cargo build -p ts-server-fn-axum`
Expected: PASS — all previous snapshot tests plus `server_items_are_cfg_gated`; ts-server-fn-axum builds with no tests.

- [ ] **Step 6: Commit**

```bash
git add -A packages/ts-server-fn packages/ts-server-fn-axum
git commit -m "feat(ts-server-fn): cfg-gate server expansion behind consumer 'server' feature

BREAKING: consuming crates must declare a 'server' feature (default = [\"server\"])
to keep axum-handler behavior. emit.rs e2e moves to fixtures/ts-server-fn-e2e."
```

---

### Task 6: Macro — client expansion (`clientgen.rs`)

**Files:**
- Create: `packages/ts-server-fn/src/clientgen.rs`
- Modify: `packages/ts-server-fn/src/lib.rs` (register module, append client tokens in `expand`)

**Interfaces:**
- Consumes: `route::{HandlerMeta, ClientArg, ArgKind, RouteAttr, is_result_like}`, `expand` (Task 5), and at *generated-code runtime*: `ts_server_fn_client::{RequestProvider, Method, call, urlencode, provider_for}` (Tasks 1–4 signatures).
- Produces: `pub fn emit_client(method: &str, route: &RouteAttr, meta: &HandlerMeta, func: &ItemFn) -> TokenStream2` returning the two `#[cfg(not(feature = "server"))]` fns (empty stream for `raw` / non-Result-like returns).

- [ ] **Step 1: Write the failing tests**

Append inside `mod snapshot_tests` in `packages/ts-server-fn/src/lib.rs`:

```rust
    // ── client-expansion snapshots ───────────────────────────────────
    fn expand_norm(method: &str, attr: &str, f: &str) -> String {
        let attr: proc_macro2::TokenStream = attr.parse().unwrap();
        let item: proc_macro2::TokenStream = f.parse().unwrap();
        norm(&crate::expand(method, attr, item))
    }

    #[test]
    fn client_fns_generated_for_get_with_path() {
        let s = expand_norm(
            "GET",
            r#""/api/posts/{id}""#,
            r#"pub async fn get_post(Path(id): Path<String>, user: User) -> ApiResult<Post> { unreachable!() }"#,
        );
        // _with variant: provider first, Path unwrapped, server extractor stripped,
        // return type verbatim (alias preserved)
        assert!(s.contains(r##"#[cfg(not(feature="server"))]pubasyncfnget_post_with(provider:&dyn::ts_server_fn_client::RequestProvider,id:String)->ApiResult<Post>"##), "{s}");
        assert!(!s.contains("user:User"), "server extractor must be stripped: {s}");
        // path interpolation via format! + urlencode
        assert!(s.contains(r##"::std::format!("/api/posts/{}",::ts_server_fn_client::urlencode(&id.to_string()))"##), "{s}");
        // call with GET, no query, no body
        assert!(s.contains("::ts_server_fn_client::Method::GET"), "{s}");
        assert!(s.contains("::core::option::Option::<&()>::None"), "{s}");
        // registry variant delegates to _with via CARGO_PKG_NAME lookup
        assert!(s.contains(r##"#[cfg(not(feature="server"))]pubasyncfnget_post(id:String)->ApiResult<Post>"##), "{s}");
        assert!(s.contains(r##"::ts_server_fn_client::provider_for(::std::env!("CARGO_PKG_NAME"))"##), "{s}");
        assert!(s.contains("get_post_with(&*__p,id).await"), "{s}");
    }

    #[test]
    fn client_fns_pass_query_and_body() {
        let s = expand_norm(
            "POST",
            r#""/api/rooms/{room_id}/posts""#,
            r#"pub async fn create_post(Path(room_id): Path<String>, Query(q): Query<Opts>, Json(req): Json<CreatePost>) -> Result<Post, ApiError> { unreachable!() }"#,
        );
        // param order: path → query → body
        assert!(s.contains("create_post_with(provider:&dyn::ts_server_fn_client::RequestProvider,room_id:String,q:Opts,req:CreatePost)"), "{s}");
        assert!(s.contains("::ts_server_fn_client::Method::POST"), "{s}");
        assert!(s.contains("::core::option::Option::Some(&q)"), "{s}");
        assert!(s.contains("::core::option::Option::Some(&req)"), "{s}");
    }

    #[test]
    fn raw_handler_gets_no_client_fn() {
        let s = expand_norm(
            "GET",
            r#""/api/download/{id}",raw"#,
            r#"pub async fn download(Path(id): Path<String>) -> Result<Response, ApiError> { unreachable!() }"#,
        );
        assert!(!s.contains("download_with"), "raw must not emit a client fn: {s}");
        assert!(!s.contains("provider_for"), "{s}");
    }

    #[test]
    fn non_result_return_gets_no_client_fn() {
        let s = expand_norm(
            "GET",
            r#""/api/health""#,
            r#"pub async fn health() -> Health { unreachable!() }"#,
        );
        assert!(!s.contains("health_with"), "plain returns are server-only: {s}");
        // server side still emitted
        assert!(s.contains(r##"#[cfg(feature="server")]pubasyncfnhealth"##), "{s}");
    }

    #[test]
    fn client_unit_return_delete() {
        let s = expand_norm(
            "DELETE",
            r#""/api/posts/{id}""#,
            r#"pub async fn delete_post(Path(id): Path<String>) -> Result<(), ApiError> { unreachable!() }"#,
        );
        assert!(s.contains("delete_post_with(provider:&dyn::ts_server_fn_client::RequestProvider,id:String)->Result<(),ApiError>"), "{s}");
        assert!(s.contains("::ts_server_fn_client::Method::DELETE"), "{s}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ts-server-fn`
Expected: FAIL — the new assertions find no client tokens (`expand` currently emits only server items).

- [ ] **Step 3: Implement `clientgen.rs`**

`packages/ts-server-fn/src/clientgen.rs`:

```rust
//! Client-mode expansion: for each handler, emit (under
//! `#[cfg(not(feature = "server"))]`) two fns with the handler's name and
//! **verbatim** return type:
//!
//! - `<name>_with(provider, args…)` — explicit `&dyn RequestProvider`.
//! - `<name>(args…)` — resolves the provider from the global registry by
//!   `env!("CARGO_PKG_NAME")` (evaluated in the *service crate's* build, so
//!   each service keys its own entry) and delegates to `<name>_with`.
//!
//! Args mirror the TS client: path args unwrapped (interpolated +
//! urlencoded), the `Query<Q>` struct, the `Json<B>`/`Form<B>` DTO; server
//! extractors are stripped. All serde happens in
//! `ts_server_fn_client::call`, whose `T`/`E` are *inferred* from the
//! declared return type — which is what keeps `*Result<T>` aliases working.
//!
//! `raw` handlers and non-`Result`-like returns are server-only: no client
//! fn is emitted (there is nowhere for transport errors to go).

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{ItemFn, LitStr, ReturnType};

use crate::route::{is_result_like, ArgKind, HandlerMeta, RouteAttr};

pub fn emit_client(
    method: &str,
    route: &RouteAttr,
    meta: &HandlerMeta,
    func: &ItemFn,
) -> TokenStream2 {
    if route.raw {
        return TokenStream2::new();
    }
    let result_like = match &func.sig.output {
        ReturnType::Type(_, ty) => is_result_like(ty),
        ReturnType::Default => false,
    };
    if !result_like {
        return TokenStream2::new();
    }

    let vis = &func.vis;
    let name = &func.sig.ident;
    let with_name = format_ident!("{}_with", name);
    let output = &func.sig.output;

    let params: Vec<TokenStream2> = meta
        .client_args
        .iter()
        .map(|a| {
            let id = format_ident!("{}", a.name);
            let ty = &a.ty;
            quote! { #id: #ty }
        })
        .collect();
    let arg_names: Vec<_> = meta
        .client_args
        .iter()
        .map(|a| format_ident!("{}", a.name))
        .collect();

    // `path_template` already uses `{}` placeholders → direct `format!` fit.
    let template = LitStr::new(&meta.path_template, Span::call_site());
    let path_exprs: Vec<TokenStream2> = meta
        .path_names
        .iter()
        .map(|n| {
            let id = format_ident!("{}", n);
            quote! { ::ts_server_fn_client::urlencode(&#id.to_string()) }
        })
        .collect();

    let query = meta
        .client_args
        .iter()
        .find(|a| a.kind == ArgKind::Query)
        .map(|a| format_ident!("{}", a.name));
    let body = meta
        .client_args
        .iter()
        .find(|a| a.kind == ArgKind::Body)
        .map(|a| format_ident!("{}", a.name));
    let query_expr = match &query {
        Some(id) => quote! { ::core::option::Option::Some(&#id) },
        None => quote! { ::core::option::Option::<&()>::None },
    };
    let body_expr = match &body {
        Some(id) => quote! { ::core::option::Option::Some(&#id) },
        None => quote! { ::core::option::Option::<&()>::None },
    };

    let method_ident = format_ident!("{}", method);

    quote! {
        #[cfg(not(feature = "server"))]
        #vis async fn #with_name(
            provider: &dyn ::ts_server_fn_client::RequestProvider,
            #(#params),*
        ) #output {
            let __path = ::std::format!(#template, #(#path_exprs),*);
            ::ts_server_fn_client::call(
                provider,
                ::ts_server_fn_client::Method::#method_ident,
                &__path,
                #query_expr,
                #body_expr,
            ).await
        }

        #[cfg(not(feature = "server"))]
        #vis async fn #name(#(#params),*) #output {
            let __p = ::ts_server_fn_client::provider_for(::std::env!("CARGO_PKG_NAME"));
            #with_name(&*__p, #(#arg_names),*).await
        }
    }
}
```

In `packages/ts-server-fn/src/lib.rs`: add `mod clientgen;` next to the other mods, and in `expand` replace the final `match emit_axum(...)` block with:

```rust
    // ── 2. Emit cfg-gated server route + cfg-gated client fns. ───────
    let server = match emit_axum(method, &route, &func) {
        Ok(ts) => ts,
        Err(e) => return e.to_compile_error(),
    };
    let client = clientgen::emit_client(method, &route, &_meta, &func);
    quote! { #server #client }
```

(rename `_meta` to `meta` since it is now used.)

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p ts-server-fn`
Expected: PASS — all snapshot tests including the five new client ones.

- [ ] **Step 5: Commit**

```bash
git add packages/ts-server-fn
git commit -m "feat(ts-server-fn): generate typed HTTP client fns in non-server mode"
```

---

### Task 7: E2E fixture — service crate (server-mode round trip)

**Files:**
- Create: `fixtures/ts-server-fn-e2e/service/Cargo.toml`
- Create: `fixtures/ts-server-fn-e2e/service/src/lib.rs`
- Create: `fixtures/ts-server-fn-e2e/service/src/api.rs`
- Create: `fixtures/ts-server-fn-e2e/service/tests/server.rs`

**Interfaces:**
- Consumes: `ts-server-fn` macros, `ts-server-fn-axum`, `ts-server-fn-client::ClientError` (for the `From` impl that must compile in both modes).
- Produces: package `e2e-service` with handlers `list_posts`, `get_post` (Err on `id == "boom"`), `create_post`, `delete_post`; types `Post`, `CreatePost`, `ListQuery`, `ApiError`, `ApiResult<T>`. `src/api.rs` is **self-contained** (no `use crate::…`) because Task 8 textually re-includes it via `#[path]`.

- [ ] **Step 1: Create the crate**

`fixtures/ts-server-fn-e2e/service/Cargo.toml`:

```toml
[package]
name = "e2e-service"
version = "0.0.0"
edition = "2021"
publish = false

# standalone — deliberately NOT a member of the repo workspace, so this
# package's features never unify with anything else.
[workspace]

[features]
default = ["server"]
server = ["dep:axum", "dep:ts-server-fn-axum"]

[dependencies]
axum = { version = "0.8", optional = true }
serde = { version = "1", features = ["derive"] }
ts-server-fn = { path = "../../../packages/ts-server-fn" }
ts-server-fn-axum = { path = "../../../packages/ts-server-fn-axum", optional = true }
ts-server-fn-client = { path = "../../../packages/ts-server-fn-client" }

[dev-dependencies]
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tower = { version = "0.5", features = ["util"] }
```

`fixtures/ts-server-fn-e2e/service/src/lib.rs`:

```rust
//! E2E fixture service. Built two ways:
//! - default features (`server`): real axum routes — tested in `tests/server.rs`.
//! - `default-features = false`: generated HTTP client — consumed by the
//!   sibling `e2e-client` crate.

pub mod api;
pub use api::*;
```

`fixtures/ts-server-fn-e2e/service/src/api.rs`:

```rust
//! Handler definitions. SELF-CONTAINED on purpose: `e2e-client` re-includes
//! this exact file via `#[path]` to get the server-mode expansion in its own
//! crate (whose `server` feature is on) while consuming `e2e-service` as a
//! dependency in client mode. Don't add `use crate::…` paths here.

#[cfg(feature = "server")]
use axum::extract::{Json, Path, Query};
use serde::{Deserialize, Serialize};
use ts_server_fn::{delete, get, post};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Post {
    pub id: String,
    pub title: String,
}

#[derive(Serialize, Deserialize)]
pub struct CreatePost {
    pub title: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ApiError {
    pub code: String,
}

#[cfg(feature = "server")]
impl ts_server_fn_axum::AsStatusCode for ApiError {
    fn as_status_code(&self) -> axum::http::StatusCode {
        axum::http::StatusCode::BAD_REQUEST
    }
}

impl From<ts_server_fn_client::ClientError> for ApiError {
    fn from(e: ts_server_fn_client::ClientError) -> Self {
        ApiError { code: format!("client:{e}") }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[get("/api/posts")]
pub async fn list_posts(Query(q): Query<ListQuery>) -> ApiResult<Vec<Post>> {
    Ok((0..q.limit.unwrap_or(0))
        .map(|i| Post { id: i.to_string(), title: format!("t{i}") })
        .collect())
}

#[get("/api/posts/{id}")]
pub async fn get_post(Path(id): Path<String>) -> ApiResult<Post> {
    if id == "boom" {
        return Err(ApiError { code: "boom".into() });
    }
    Ok(Post { id, title: "hi".into() })
}

#[post("/api/posts")]
pub async fn create_post(Json(req): Json<CreatePost>) -> ApiResult<Post> {
    Ok(Post { id: "1".into(), title: req.title })
}

#[delete("/api/posts/{id}")]
pub async fn delete_post(Path(_id): Path<String>) -> ApiResult<()> {
    Ok(())
}
```

- [ ] **Step 2: Port the server-mode e2e tests (from the deleted `emit.rs`)**

`fixtures/ts-server-fn-e2e/service/tests/server.rs`:

```rust
//! Server-mode e2e (successor of the old `ts-server-fn-axum/tests/emit.rs`):
//! the macro must expand to compiling axum routes, register them via
//! inventory, and serve real requests — including the Err → status+JSON path.

use e2e_service::api::*;
use ts_server_fn_axum::{api_router, registered_routes};

#[test]
fn routes_register_and_router_builds() {
    // Reference a real symbol from the lib so its rlib (and inventory's
    // registration ctors) cannot be dropped by the linker.
    let _ = ApiError { code: String::new() };
    let routes = registered_routes();
    for expected in [
        ("GET", "/api/posts"),
        ("GET", "/api/posts/{id}"),
        ("POST", "/api/posts"),
        ("DELETE", "/api/posts/{id}"),
    ] {
        assert!(routes.contains(&expected), "missing {expected:?} in {routes:?}");
    }
    let _router: axum::Router = api_router();
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (axum::http::StatusCode, String) {
    use tower::ServiceExt;
    let builder = axum::http::Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(b.to_owned()))
            .unwrap(),
        None => builder.body(axum::body::Body::empty()).unwrap(),
    };
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn handlers_serve_real_requests() {
    let (status, body) = send(api_router(), "GET", "/api/posts/42", None).await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], "42");

    let (status, body) =
        send(api_router(), "POST", "/api/posts", Some(r#"{"title":"hello"}"#)).await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["title"], "hello");

    let (status, _body) = send(api_router(), "DELETE", "/api/posts/7", None).await;
    assert_eq!(status, 200);

    let (status, body) = send(api_router(), "GET", "/api/posts?limit=2", None).await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    // Err path: AsStatusCode → 400 with the serialized error body.
    let (status, body) = send(api_router(), "GET", "/api/posts/boom", None).await;
    assert_eq!(status, 400, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], "boom");
}
```

- [ ] **Step 3: Run tests, verify server mode AND that client mode compiles**

Run:
```bash
cd fixtures/ts-server-fn-e2e/service && cargo test && cargo check --no-default-features && cd -
```
Expected: tests PASS; `cargo check --no-default-features` compiles cleanly (client fns generated, no axum in the graph).

- [ ] **Step 4: Commit**

```bash
git add fixtures/ts-server-fn-e2e/service
git commit -m "test(e2e): fixture service crate exercising server-mode expansion"
```

---

### Task 8: E2E fixture — client crate (generated server ↔ generated client round trip)

**Files:**
- Create: `fixtures/ts-server-fn-e2e/client/Cargo.toml`
- Create: `fixtures/ts-server-fn-e2e/client/src/lib.rs`
- Create: `fixtures/ts-server-fn-e2e/client/tests/roundtrip.rs`

**Interfaces:**
- Consumes: `e2e-service` **with `default-features = false`** (client-mode fns: `get_post(id)`, `get_post_with(provider, id)`, `create_post(req)`, `list_posts(q)`, `delete_post(id)`), plus the same `api.rs` source included locally in server mode (this crate's own `server` feature is on) to host the real router.
- Produces: the definitive round-trip test suite.

**Key trick:** `e2e-client` has its own `server` feature (default on) and includes `../service/src/api.rs` via `#[path]`. The included copy expands in **server** mode (this crate's feature), giving real inventory-registered axum routes; the `e2e-service` *dependency* expands in **client** mode (its feature off). Two different packages → no feature unification. One process hosts the generated server and calls it through the generated client.

- [ ] **Step 1: Create the crate**

`fixtures/ts-server-fn-e2e/client/Cargo.toml`:

```toml
[package]
name = "e2e-client"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[features]
default = ["server"]
server = ["dep:axum", "dep:ts-server-fn-axum"]

[dependencies]
axum = { version = "0.8", optional = true }
e2e-service = { path = "../service", default-features = false }
serde = { version = "1", features = ["derive"] }
ts-server-fn = { path = "../../../packages/ts-server-fn" }
ts-server-fn-axum = { path = "../../../packages/ts-server-fn-axum", optional = true }
ts-server-fn-client = { path = "../../../packages/ts-server-fn-client" }

[dev-dependencies]
tokio = { version = "1", features = ["macros", "net", "rt-multi-thread"] }
```

`fixtures/ts-server-fn-e2e/client/src/lib.rs`:

```rust
//! Hosts the SAME handler source as `e2e-service`, but compiled here in
//! SERVER mode (this crate's `server` feature is on by default) — that
//! yields the real generated axum router. Meanwhile the `e2e-service`
//! dependency is built with `default-features = false`, so its copy of the
//! handlers is the generated HTTP client. `tests/roundtrip.rs` wires one to
//! the other over localhost.

#[path = "../../service/src/api.rs"]
pub mod server_api;
```

- [ ] **Step 2: Write the round-trip tests**

`fixtures/ts-server-fn-e2e/client/tests/roundtrip.rs`:

```rust
use e2e_service::api as client; // client-mode generated fns
use ts_server_fn_client::HttpProvider;

/// Serve the server-mode expansion (from `e2e_client::server_api`) on an
/// ephemeral port; returns the base URL.
async fn spawn_server() -> String {
    // Reference a real symbol from the lib crate so its rlib (and inventory's
    // registration ctors) cannot be dropped by the linker; then verify.
    let _ = e2e_client::server_api::ApiError { code: String::new() };
    let routes = ts_server_fn_axum::registered_routes();
    assert!(routes.len() >= 4, "inventory not linked: {routes:?}");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = ts_server_fn_axum::api_router();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// keep the unused-import lint honest about why the lib is referenced
#[allow(unused_imports)]
use e2e_client::server_api;

/// Single test for everything that goes through the *global registry* —
/// the registry is process-wide state, so registry configuration must not
/// race across parallel #[tokio::test]s.
#[tokio::test]
async fn registry_roundtrip() {
    let base = spawn_server().await;
    ts_server_fn_client::set_endpoint("e2e-service", &base);

    // Ok path: GET with path arg.
    let post = client::get_post("42".into()).await.unwrap();
    assert_eq!(post.id, "42");
    assert_eq!(post.title, "hi");

    // Err path: server 400 + JSON body → restored Err(ApiError).
    let err = client::get_post("boom".into()).await.unwrap_err();
    assert_eq!(err, client::ApiError { code: "boom".into() });

    // Body path: POST with DTO.
    let post = client::create_post(client::CreatePost { title: "hello".into() })
        .await
        .unwrap();
    assert_eq!(post.title, "hello");

    // Query path: GET with Query<T> struct.
    let posts = client::list_posts(client::ListQuery { limit: Some(2) })
        .await
        .unwrap();
    assert_eq!(posts.len(), 2);

    // Unit path: DELETE → Result<(), _> decodes the empty/`null` body.
    client::delete_post("7".into()).await.unwrap();
}

/// `_with` variant: explicit provider, no registry involved.
#[tokio::test]
async fn explicit_provider_with() {
    let base = spawn_server().await;
    let provider = HttpProvider::new(base);
    let post = client::get_post_with(&provider, "9".into()).await.unwrap();
    assert_eq!(post.id, "9");
}

/// Transport failure surfaces through `From<ClientError>`.
#[tokio::test]
async fn transport_error_maps_through_from() {
    // port 9 (discard) is never listening
    let provider = HttpProvider::new("http://127.0.0.1:9");
    let err = client::get_post_with(&provider, "1".into()).await.unwrap_err();
    assert!(err.code.starts_with("client:"), "{err:?}");
}
```

- [ ] **Step 3: Run tests, verify they pass**

Run:
```bash
cd fixtures/ts-server-fn-e2e/client && cargo test && cd -
```
Expected: PASS (3 tests). This is the definitive proof: generated server and generated client from the same source, talking over real HTTP.

- [ ] **Step 4: Commit**

```bash
git add fixtures/ts-server-fn-e2e/client
git commit -m "test(e2e): generated-server <-> generated-client HTTP round trip"
```

---

### Task 9: Docs, Makefile, CI, versions

**Files:**
- Modify: `Makefile`
- Modify: `.github/workflows/pr-workflow.yml`
- Modify: `packages/ts-server-fn/Cargo.toml` (version 0.3.0)
- Modify: `packages/ts-server-fn/README.md`
- Create: `packages/ts-server-fn-client/README.md`
- Modify: `docs/superpowers/specs/2026-07-20-ts-server-fn-dual-mode-design.md` (addenda)

- [ ] **Step 1: Makefile — publish list + e2e target**

In `Makefile`, change the first line and append a target:

```make
PACKAGES=by-macros dioxus-gtag-macro dioxus-gtag ts-server-fn-client ts-server-fn ts-server-fn-axum
```

```make
.PHONY: test.e2e
test.e2e:
	cd fixtures/ts-server-fn-e2e/service && cargo test && cargo check --no-default-features
	cd fixtures/ts-server-fn-e2e/client && cargo test
```

- [ ] **Step 2: CI — run the e2e suite**

In `.github/workflows/pr-workflow.yml`, in the `non-server-feature` job after the existing `test` step, add:

```yaml
      - name: ts-server-fn e2e (dual mode)
        env:
          RUSTFLAGS: "-A warnings"
        run: make test.e2e
```

- [ ] **Step 3: Version bump**

In `packages/ts-server-fn/Cargo.toml`: `version = "0.3.0"`.

- [ ] **Step 4: READMEs**

`packages/ts-server-fn/README.md` — replace the intro paragraph and add a "Dual mode" section after the expansion example (keep the classification table and the rest). New/changed content:

```markdown
# ts-server-fn

`#[get]/#[post]/#[put]/#[patch]/#[delete]` attribute macros with **two
expansions**, selected by *your crate's* `server` feature:

- `server` **on** (the service itself): a pure axum route (inventory-registered)
  — plus an optional type-safe TypeScript client emitted from the signature.
- `server` **off** (another service depending on you with
  `default-features = false`): a typed Rust HTTP client fn with the same name
  and return type, backed by `ts-server-fn-client`.

## Dual mode

Declare the feature in your service crate (this is the v0.3 breaking change —
one Cargo.toml edit):

```toml
[features]
default = ["server"]
server = ["dep:axum", "dep:ts-server-fn-axum"]  # + your other server-only deps

[dependencies]
ts-server-fn = "0.3"
ts-server-fn-client = "0.1"
```

Requirements in client mode: DTOs derive `Serialize` **and** `Deserialize`;
error enums additionally derive `Deserialize` and implement
`From<ts_server_fn_client::ClientError>`; server-only modules are gated with
`#[cfg(feature = "server")]`.

A consumer (e.g. an `evaluation` service using `search` and `ai`):

```toml
search = { version = "...", default-features = false }
```

```rust
// once at startup — key is the Cargo package name verbatim
ts_server_fn_client::set_endpoint("search", "https://search.example.com");

let post = search::get_post(id).await?;                 // registry provider
let post = search::get_post_with(&provider, id).await?; // explicit provider
```

`raw` handlers and handlers returning a plain (non-`Result`) type are
server-only: no client fn is generated for them.
```

`packages/ts-server-fn-client/README.md`:

```markdown
# ts-server-fn-client

Client runtime for [ts-server-fn](../ts-server-fn): when a service crate is
built without its `server` feature, its handlers expand to typed HTTP client
fns that run on this crate.

- `RequestProvider` — object-safe, byte-level transport trait. Implement it
  for mocks or wrap a provider for rate limiting / retry / auth.
- `HttpProvider` — default provider; a reqwest wrapper bound to one base URL
  (`HttpProvider::new(url)`, or `with_client` for a tuned `reqwest::Client`).
- Registry — `set_endpoint(pkg, url)` / `set_provider(pkg, provider)` once at
  startup, keyed by the service's Cargo **package name verbatim**; generated
  fns look themselves up via `env!("CARGO_PKG_NAME")`.
- `ClientError` — `Transport` / `Encode` / `Decode`; reaches the caller
  through the service error type's `From<ClientError>` impl, so client fns
  keep the exact server signature (`Result<T, E>`, aliases included).

Known limits: one semver-compatible copy of this crate per binary (the
registry is a static here); native targets only for now (the transport future
is `Send`, which wasm reqwest is not).
```

- [ ] **Step 5: Spec addenda**

In `docs/superpowers/specs/2026-07-20-ts-server-fn-dual-mode-design.md`:
- Add `Encode(String)` to the `ClientError` enum listing (query/body serialization failure).
- In "Out of scope for v1", extend the raw-handlers bullet: handlers returning a plain (non-`Result`-like) type are likewise server-only — no client fn is emitted (transport errors would have nowhere to go).

- [ ] **Step 6: Verify everything**

```bash
cargo test -p ts-server-fn && cargo test -p ts-server-fn-client && cargo build -p ts-server-fn-axum && make test.e2e
```
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add Makefile .github/workflows/pr-workflow.yml packages/ts-server-fn packages/ts-server-fn-client docs/superpowers/specs
git commit -m "docs+ci: dual-mode docs, e2e wiring, ts-server-fn 0.3.0"
```
