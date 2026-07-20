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
