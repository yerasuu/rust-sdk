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
Each remote service has its own domain, so the client fn takes a **provider**
(base URL + transport) as its first parameter.

## Key decisions

1. **Feature lives on the service crate, not the proc-macro crate.** The macro
   emits *both* expansions, gated with `#[cfg(feature = "server")]` /
   `#[cfg(not(feature = "server"))]`, evaluated in the consumer crate's
   context. Putting the feature on `ts-server-fn` itself would break under
   Cargo feature unification: a service that is a server for its own API *and*
   a client of another service would force `server` on for everything.

2. **Provider is a trait** (`RequestProvider`), first parameter of every
   generated client fn: `provider: &impl ts_server_fn_client::RequestProvider`.
   `HttpProvider` (a reqwest wrapper holding the base URL) is the default
   implementation. Trait allows mock providers in tests and future decorator
   composition (rate limiting, retry, auth injection) without API changes.

3. **Client fns keep the server signature's return type verbatim**
   (`Result<T, E>`, including `*Result<T>` aliases). Non-2xx responses
   deserialize the body into `E` (mirror of the server's `(status, Json(E))`).
   Transport/decode failures convert via `E: From<ClientError>`. Type
   inference on `Err(...)` / `.map_err(Into::into)` means the macro never
   needs to name `E` explicitly — this is what keeps alias support working.

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
pub async fn get_post(
    provider: &impl ::ts_server_fn_client::RequestProvider,
    id: String,
) -> ApiResult<Post> {
    provider
        .request(::ts_server_fn_client::Method::GET,
                 &format!("/api/posts/{}", /* urlencoded */ id),
                 ::core::option::Option::<&()>::None,   // no query
                 ::core::option::Option::<&()>::None)   // no body
        .await
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
  No client fn is emitted for them (the response can't be typed). Revisit if
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
pub trait RequestProvider {
    fn request<Q, B, T, E>(
        &self,
        method: Method,
        path: &str,
        query: Option<&Q>,
        body: Option<&B>,
    ) -> impl Future<Output = Result<T, E>>
    where
        Q: Serialize + ?Sized,
        B: Serialize + ?Sized,
        T: DeserializeOwned,
        E: DeserializeOwned + From<ClientError>;
}

pub struct HttpProvider { /* base_url, reqwest::Client */ }
impl HttpProvider {
    pub fn new(base_url: impl Into<String>) -> Self;
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self;
}

#[derive(Debug)]
pub enum ClientError {
    Transport(reqwest::Error),
    Decode { status: u16, source: serde_json::Error },
}
```

`HttpProvider` behavior:

- 2xx → deserialize body into `T`; `204` / empty body maps to `()` returns
  (mirror of the TS runtime's `void` handling).
- non-2xx → deserialize body into `E`, return `Err(e)`; if the error body
  itself fails to decode → `Err(ClientError::Decode { .. }.into())`.
- connection/timeout/etc. → `Err(ClientError::Transport(..).into())`.

`Method` is re-exported (from `http`/`reqwest`) so generated code doesn't need
a direct dependency.

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
let search = HttpProvider::new("https://search.example.com");
let post = search::get_post(&search, id).await?;
```

**Accepted trade-off:** `ts-server-fn-client` (and thus reqwest) stays a
dependency of server builds too. An opt-in `client` feature could avoid that
but complicates every consumer; most servers already depend on reqwest.

## Testing

- **Snapshot tests** (in `ts-server-fn`, same style as the TS-render tests):
  classify → client-body render for the same matrix (path-only, body, query,
  tuple paths, unit return, alias returns) plus "raw emits no client fn".
- **End-to-end** (new test crate or extension of `ts-server-fn-axum/tests`):
  spin up the real axum router from a server-feature build, call it through
  `HttpProvider` from a client-mode build of the same handler definitions;
  verify round-trip of `Ok`, `Err(E)` (status + JSON body → restored `Err`),
  `()`/204, and transport-error conversion.
