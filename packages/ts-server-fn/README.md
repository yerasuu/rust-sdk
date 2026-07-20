# ts-server-fn

`#[get]/#[post]/#[put]/#[patch]/#[delete]` attribute macros with **two
expansions**, selected by *your crate's* `server` feature:

- `server` **on** (the service itself): a pure axum route (inventory-registered)
  — plus an optional type-safe TypeScript client emitted from the signature.
- `server` **off** (another service depending on you with
  `default-features = false`): a typed Rust HTTP client fn with the same name
  and return type, backed by `ts-server-fn-client`.

```rust
use axum::extract::{Path, Json, Query};
use ts_server_fn::{get, post};

#[get("/api/posts/{id}")]
pub async fn get_post(Path(id): Path<String>, user: User) -> ApiResult<Post> { ... }

#[post("/api/posts")]
pub async fn create_post(Json(req): Json<CreatePost>) -> ApiResult<Post> { ... }
```

expands to (illustrative):

```rust
async fn __ts_impl_get_post(Path(id): Path<String>, user: User) -> ApiResult<Post> { ... }

pub async fn get_post(__a0: Path<String>, __a1: User) -> axum::response::Response {
    match __ts_impl_get_post(__a0, __a1).await {
        Ok(v)  => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (e.as_status_code(), Json(e)).into_response(),
    }
}
ts_server_fn_axum::inventory::submit! { ApiRoute { method: "GET", path: "/api/posts/{id}",
    register: |r| r.route("/api/posts/{id}", axum::routing::get(get_post)) } }
```

and, when `TS_SERVER_FN_PACKAGE_DIR` is set at build time, writes:

```typescript
// $DIR/src/handlers/getPost.ts
import type { Post } from "../types/Post";
import { apiGet } from "../runtime/client";
export async function getPost(id: string): Promise<Post> {
  return apiGet<Post>(`/api/posts/${encodeURIComponent(String(id))}`);
}
```

## Dual mode

Declare the feature in your service crate (this is the v0.3 breaking change —
a feature block plus the `ts-server-fn-client` dependency):

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
[dependencies]
search = { version = "...", default-features = false }
```

```rust
// once at startup — key is the Cargo package name verbatim
ts_server_fn_client::set_endpoint("search", "https://search.example.com");

let post = search::get_post(id).await?;                 // registry provider
let post = search::get_post_with(&provider, id).await?; // explicit provider
```

`raw` handlers, handlers returning a plain (non-`Result`) type, and handlers
taking a `Form<B>` body are server-only: no client fn is generated for them
(the Rust client transport is JSON-only in v1).

## Runtime

Two pieces the consumer wires up:

- **`ts-server-fn-axum`** (sibling crate) — `ApiRoute`, `api_router()`
  (collects every handler via `inventory`), and the `AsStatusCode` trait your
  error enums implement.
- **`runtime/client.ts`** — copy into your TS package at `src/runtime/client.ts`.
  Owns transport (base URL, cookies, status→throw, `204`→void, querystring).

```rust
let app = ts_server_fn_axum::api_router();          // every #[get]/#[post]/... handler
```

## Classification (signature-type-based)

| Signature arg            | Becomes        | TS                                   |
|--------------------------|----------------|--------------------------------------|
| `Path(x): Path<T>`       | path arg       | interpolated, urlencoded             |
| `Path((a,b)): Path<(A,B)>` | multiple path args | one per tuple element          |
| `Query(q): Query<T>`     | query arg      | `T` serialized to `?k=v`             |
| `Json(b): Json<T>` / `Form<T>` | body     | DTO **directly** (no `{ "b": ... }`) |
| anything else (`User`, `Session`, `State<_>`) | server extractor | stripped from TS |

Return type: `Result<T, E>` and any `*Result<T, ..>` alias (e.g.
`CollabResult<T>`) unwrap to `T`; `()`/`Result<(), _>` → `void` (no body parse).

## What this fixes vs the spike

- **Type-based classify** — no longer matches arg *names* against the route;
  reads `Path`/`Query`/`Json` from the signature → immune to rename drift.
- **`*Result` aliases** — `CollabResult<X>` now unwraps to `X` (the spike
  rendered the alias name as a bogus TS type).
- **Body un-wrapped (D2)** — request body is the DTO JSON itself.
- **Unit/204** — `Result<(), _>` → `Promise<void>`; runtime treats 204/empty
  as `undefined`.
- **Loud validation** — `Json` body on GET/DELETE, two bodies, or a
  path-placeholder/arg-count mismatch is a compile error.
- **Pure axum** — expands to inventory-registered axum routes; no
  `dioxus::fullstack`.

## Known limits

- Wrapper detection is by **last path segment ident** — import `Path`/`Query`/
  `Json` unaliased (`use axum::extract::Path as P` is not recognized).
- DTO type *files* (`../types/*`) are produced by `ts-rs` on the DTOs; this
  crate only references them by name. `ts-rs`'s serde-compat caveats
  (`flatten`, custom (de)serialize) apply to those files.
- The generated fn ↔ `runtime/client.ts` form one wire contract; the macro's
  snapshot tests pin the generated call shape — change both together.

## Test

```bash
cargo test -p ts-server-fn        # classify + TS render (incl. validation errors)
cargo test -p ts-server-fn-axum   # end-to-end: macro → axum routes compile + register
```
