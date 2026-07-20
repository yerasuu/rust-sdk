# ts-server-fn dual mode: axum handler / generated Rust client

**Date:** 2026-07-20
**Status:** Approved (design), pending implementation plan

## Goal

Make `#[get]/#[post]/#[put]/#[patch]/#[delete]` handlers usable in two modes,
selected by the **consumer service crate's own `server` feature**:

- **`server` feature ON** — expand exactly as today: original fn + axum
  wrapper + `inventory` registration (+ optional TS client emission).
- **`server` feature OFF** — expand to a **typed Rust HTTP client function**
  with the same name and return type, so another MSA service can depend on the
  service crate with `default-features = false` and call its API over HTTP.

Motivating scenario: `search` and `ai` services implement their APIs with
ts-server-fn. An `evaluation` service depends on both with
`default-features = false`; the macro gives it typed clients automatically.
Each remote service has its own domain, configured once at startup through a
per-crate provider registry.

## Key decisions

1. **Feature lives on the service crate, not the proc-macro crate.** The macro
   emits *both* expansions, gated with `#[cfg(feature = "server")]` /
   `#[cfg(not(feature = "server"))]`, evaluated in the consumer crate's
   context. Putting the feature on `ts-server-fn` itself would break under
   Cargo feature unification: a service that is a server for its own API *and*
   a client of another service would force `server` on for everything.

2. **Providers are managed in a global registry keyed by package name.**
   `ts-server-fn-client` holds one `RwLock<HashMap<String, Arc<dyn
   RequestProvider>>>`. Generated client fns look themselves up with
   `env!("CARGO_PKG_NAME")` — the token is emitted into the service crate, so
   it evaluates to *that crate's* package name at its own compile time. The
   consumer initializes once at startup:

   ```rust
   ts_server_fn_client::set_endpoint("search", "https://search.example.com");
   ts_server_fn_client::set_endpoint("ai", "https://ai.example.com");

   let post = search::get_post(id).await?;   // no provider argument
   ```

   Service crates declare nothing extra (no install macro, no per-crate
   static). Each crate gets its own key automatically, so per-service domains
   fall out for free.

3. **`RequestProvider` is an object-safe transport-level trait** (it must live
   behind `dyn` in the registry). It moves bytes; all serde happens in the
   generated code. `HttpProvider` (a reqwest wrapper holding the base URL) is
   the default implementation. Decorators (rate limiting, retry, auth
   injection) wrap the trait at the byte level.

4. **Client fns keep the server signature's return type verbatim**
   (`Result<T, E>`, including `*Result<T>` aliases). Non-2xx responses
   deserialize the body into `E` (mirror of the server's `(status, Json(E))`).
   Transport/decode failures convert via `E: From<ClientError>`. Type
   inference on `Err(...)` / `.map_err(Into::into)` means the macro never
   needs to name `E` explicitly — this is what keeps alias support working.

5. **Two client fns per handler.** `get_post(id)` resolves the provider from
   the registry; `get_post_with(&provider, id)` takes it explicitly (tests
   with mocks, or using one service under two domains). The registry variant
   is a one-line delegation to `_with`.

## Macro expansion

For:

```rust
#[get("/api/posts/{id}")]
pub async fn get_post(Path(id): Path<String>, user: User) -> ApiResult<Post> { ... }
```

the macro emits:

```rust
// server mode — identical to today, but cfg-gated
#[cfg(feature = "server")]
pub async fn get_post(Path(id): Path<String>, user: User) -> ApiResult<Post> { /* original body */ }
#[cfg(feature = "server")]
async fn __get_post_ts_route(...) -> ::axum::response::Response { ... }
#[cfg(feature = "server")]
::ts_server_fn_axum::inventory::submit! { ... }

// client mode — generated
#[cfg(not(feature = "server"))]
pub async fn get_post_with(
    provider: &dyn ::ts_server_fn_client::RequestProvider,
    id: String,
) -> ApiResult<Post> {
    // serialize path/query/body → provider.request_raw(...) → deserialize T or E;
    // transport/decode errors via `E: From<ClientError>` + inference
}

#[cfg(not(feature = "server"))]
pub async fn get_post(id: String) -> ApiResult<Post> {
    let __p = ::ts_server_fn_client::provider_for(env!("CARGO_PKG_NAME"));
    get_post_with(&*__p, id).await
}
```

Parameter mapping **reuses the existing `classify` pipeline** (the one that
drives TS generation):

| Signature arg | Client fn |
|---|---|
| `Path(x): Path<T>` | positional arg `x: T`, interpolated + urlencoded into the path |
| `Path((a, b)): Path<(A, B)>` | one positional arg per tuple element |
| `Query(q): Query<Q>` | arg `q: Q`, serialized to the querystring |
| `Json(b): Json<B>` / `Form<B>` | arg `b: B`, sent as the request body (DTO directly, no wrapping) |
| server extractors (`User`, `State<_>`, …) | **stripped** — they exist only behind the `server` gate |

Existing validation (body on GET/DELETE, two bodies, placeholder/arg-count
mismatch) applies unchanged and fires in both modes.

### Out of scope for v1

- `raw` handlers (`#[get("…", raw)]` — downloads/redirects): server mode only.
  No client fn is emitted for them (the response can't be typed). Handlers
  returning a plain (non-`Result`-like) type are likewise server-only — no
  client fn is emitted (transport errors would have nowhere to go). Revisit if
  needed.
- Rate limiting / retry providers — enabled by the trait, not implemented.

### Unchanged

- TS client emission (`TS_SERVER_FN_PACKAGE_DIR`) works exactly as before.
- `ts-server-fn-axum` (ApiRoute / api_router / AsStatusCode) unchanged; it is
  simply only referenced from the server-gated expansion.

## New crate: `ts-server-fn-client`

Runtime support for the client expansion, sibling of `ts-server-fn-axum`
(`packages/ts-server-fn-client`):

```rust
/// Object-safe transport. All serde lives in the generated code.
pub trait RequestProvider: Send + Sync {
    fn request_raw(
        &self,
        method: Method,                 // re-exported so generated code needs no direct dep
        path_and_query: &str,           // already interpolated + urlencoded
        body: Option<Vec<u8>>,          // JSON bytes (or form-encoded for Form)
    ) -> BoxFuture<'_, Result<RawResponse, ClientError>>;
}

pub struct RawResponse { pub status: u16, pub body: bytes::Bytes }

pub struct HttpProvider { /* base_url, reqwest::Client */ }
impl HttpProvider {
    pub fn new(base_url: impl Into<String>) -> Self;
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self;
}

#[derive(Debug)]
pub enum ClientError {
    Transport(reqwest::Error),
    Encode(String),
    Decode { status: u16, source: serde_json::Error },
}

// ── provider registry ────────────────────────────────────────────────
pub fn set_provider(pkg: &str, provider: impl RequestProvider + 'static);
pub fn set_endpoint(pkg: &str, base_url: &str);   // = set_provider(pkg, HttpProvider::new(base_url))
#[doc(hidden)]
pub fn provider_for(pkg: &str) -> Arc<dyn RequestProvider>;
```

Registry semantics:

- Backing store: `RwLock<HashMap<String, Arc<dyn RequestProvider>>>` —
  re-settable (tests can swap providers), lock held only for lookup/insert.
- `provider_for` on a missing key **panics** with an actionable message
  (requested key + currently registered keys + "call
  `ts_server_fn_client::set_endpoint(\"<pkg>\", …)` at startup"). Misuse is a
  wiring bug, not a recoverable condition.
- The registry key is the **Cargo package name verbatim** (dashes included,
  e.g. `"search-service"`), because generated code keys by
  `env!("CARGO_PKG_NAME")`.

Response handling in the generated code (shared helper in this crate):

- 2xx → deserialize body into `T`; `204` / empty body maps to `()` returns
  (mirror of the TS runtime's `void` handling).
- non-2xx → deserialize body into `E`, return `Err(e)`; if the error body
  itself fails to decode → `Err(ClientError::Decode { .. }.into())`.
- connection/timeout/etc. → `Err(ClientError::Transport(..).into())`.

## Service-crate migration (breaking change, v0.3)

```toml
# search service's Cargo.toml
[features]
default = ["server"]
server = ["dep:axum", "dep:ts-server-fn-axum", "dep:sqlx"]  # all server-only deps

[dependencies]
ts-server-fn = "0.3"
ts-server-fn-client = "0.1"   # unconditional (Cargo can't gate a dep on cfg(not(feature)))
```

- DTOs derive both `Serialize` and `Deserialize` (requests: server
  deserializes / client serializes; responses: the reverse).
- Error enums additionally derive `Deserialize` and implement
  `From<ts_server_fn_client::ClientError>`.
- Server-only modules (DB access, etc.) are gated by the service itself with
  `#[cfg(feature = "server")]`.

Consumer side:

```toml
# evaluation service
search = { version = "...", default-features = false }
ai     = { version = "...", default-features = false }
```

```rust
// once at startup
ts_server_fn_client::set_endpoint("search", "https://search.example.com");
ts_server_fn_client::set_endpoint("ai", "https://ai.example.com");

// anywhere after
let post = search::get_post(id).await?;
```

**Accepted trade-offs:**

- `ts-server-fn-client` (and thus reqwest) stays a dependency of server builds
  too. An opt-in `client` feature could avoid that but complicates every
  consumer; most servers already depend on reqwest.
- Registry keys are strings: a typo in `set_endpoint` surfaces as the
  first-call panic, not a compile error. The panic message makes it a
  ten-second fix.
- The registry is a static in `ts-server-fn-client`, so every crate in the
  binary must resolve to one semver-compatible copy of it. Two incompatible
  major versions would mean two registries. (Standard Rust ecosystem rule;
  documented, not mitigated.)

## Testing

- **Snapshot tests** (in `ts-server-fn`, same style as the TS-render tests):
  classify → client-expansion render for the same matrix (path-only, body,
  query, tuple paths, unit return, alias returns), both the registry fn and
  the `_with` fn, plus "raw emits no client fn".
- **End-to-end** (new test crate or extension of `ts-server-fn-axum/tests`):
  spin up the real axum router from a server-feature build, call it through
  `HttpProvider` from a client-mode build of the same handler definitions —
  via `set_endpoint` + registry fn and via `_with` with a mock provider.
  Verify round-trip of `Ok`, `Err(E)` (status + JSON body → restored `Err`),
  `()`/204, transport-error conversion, and the missing-provider panic
  message.
