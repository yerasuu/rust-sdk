//! `ts-server-fn` — turn `#[get]/#[post]/#[put]/#[patch]/#[delete]` handlers
//! into **pure axum routes** and emit a type-safe TypeScript client.
//!
//! This is the post-Dioxus design. Each attribute:
//!
//! 1. **Renames the original fn to `__ts_impl_<name>`** (signature + body
//!    unchanged) and emits a public `<name>` axum handler that takes the same
//!    extractors, forwards to the impl, and adapts the return:
//!    - `Result<T, E>` → `Ok` ⇒ `(200, Json(T))`, `Err` ⇒
//!      `(E.as_status_code(), Json(E))` (via the `AsStatusCode` trait from
//!      `ts-server-fn-axum`).
//!    - plain `T` ⇒ `(200, Json(T))`.
//!    - `#[get("…", raw)]` ⇒ the impl's own `IntoResponse` verbatim
//!      (downloads / redirects).
//!    The route is registered through `inventory` so `api_router()` collects
//!    every handler with no hand-written route table.
//!
//! 2. **When `TS_SERVER_FN_PACKAGE_DIR` is set**, classifies the signature by
//!    extractor *type* (`Path`/`Query`/`Json`) and writes one `.ts` client fn
//!    per handler. Request body is the DTO directly (no `{ "arg": value }`
//!    wrapping); query is the `Query<T>` struct.
//!
//! Classification is signature-type-based and validated: a `Json` body on
//! GET/DELETE, two bodies, or a path-placeholder/arg-count mismatch is a hard
//! compile error.

mod clientgen;
mod route;
mod tsgen;
mod write_ts;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, ReturnType};

use route::{is_result_like, RouteAttr};

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
    let meta = match route::classify(
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

    // ── 2. Emit cfg-gated server route + cfg-gated client fns. ───────
    let server = match emit_axum(method, &route, &func) {
        Ok(ts) => ts,
        Err(e) => return e.to_compile_error(),
    };
    let client = clientgen::emit_client(method, &route, &meta, &func);
    quote! { #server #client }
}

/// Build the axum route-wrapper + inventory registration, keeping the original
/// handler fn **unchanged** (same name, same `Result` return) so it stays
/// directly callable (e.g. from unit/integration tests). Only the generated
/// `__<name>_ts_route` wrapper performs the `Result`→`Json` adaptation and is
/// what `inventory` registers.
fn emit_axum(method: &str, route: &RouteAttr, func: &ItemFn) -> syn::Result<TokenStream2> {
    let vis = func.vis.clone();
    let name = func.sig.ident.clone();
    let route_name = format_ident!("__{}_ts_route", name);

    // Wrapper params: clone each typed arg's TYPE, bind to `__aN`, forward to
    // the original fn (kept verbatim below).
    let mut wrapper_params: Vec<TokenStream2> = Vec::new();
    let mut forward: Vec<TokenStream2> = Vec::new();
    for (i, input) in func.sig.inputs.iter().enumerate() {
        match input {
            FnArg::Typed(pt) => {
                let id = format_ident!("__a{}", i);
                let ty = &pt.ty;
                wrapper_params.push(quote! { #id: #ty });
                forward.push(quote! { #id });
            }
            FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    "ts-server-fn handlers must be free functions (no `self`)",
                ));
            }
        }
    }

    // Response adapter.
    let is_result = match &func.sig.output {
        ReturnType::Type(_, ty) => is_result_like(ty),
        ReturnType::Default => false,
    };
    let call = quote! { #name(#(#forward),*).await };
    let body = if route.raw {
        quote! { ::axum::response::IntoResponse::into_response(#call) }
    } else if is_result {
        quote! {
            match #call {
                ::core::result::Result::Ok(__v) => ::axum::response::IntoResponse::into_response(
                    (::axum::http::StatusCode::OK, ::axum::Json(__v))
                ),
                ::core::result::Result::Err(__e) => {
                    let __code = ::ts_server_fn_axum::AsStatusCode::as_status_code(&__e);
                    ::axum::response::IntoResponse::into_response((__code, ::axum::Json(__e)))
                }
            }
        }
    } else {
        quote! {
            ::axum::response::IntoResponse::into_response(
                (::axum::http::StatusCode::OK, ::axum::Json(#call))
            )
        }
    };

    // inventory registration. axum route path is the path part (strip query).
    let axum_path = route.path.value();
    let axum_path = axum_path.split('?').next().unwrap_or("").to_string();
    let method_router = format_ident!("{}", method.to_lowercase());
    let method_str = method;

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
}

#[proc_macro_attribute]
pub fn get(attr: TokenStream, item: TokenStream) -> TokenStream {
    server_fn_impl("GET", attr, item)
}
#[proc_macro_attribute]
pub fn post(attr: TokenStream, item: TokenStream) -> TokenStream {
    server_fn_impl("POST", attr, item)
}
#[proc_macro_attribute]
pub fn put(attr: TokenStream, item: TokenStream) -> TokenStream {
    server_fn_impl("PUT", attr, item)
}
#[proc_macro_attribute]
pub fn patch(attr: TokenStream, item: TokenStream) -> TokenStream {
    server_fn_impl("PATCH", attr, item)
}
#[proc_macro_attribute]
pub fn delete(attr: TokenStream, item: TokenStream) -> TokenStream {
    server_fn_impl("DELETE", attr, item)
}

// ─── Unit tests: classify + render pipeline (no axum needed) ─────────
#[cfg(test)]
mod snapshot_tests {
    use crate::route::{classify, RouteAttr};
    use crate::tsgen::render;
    use syn::ItemFn;

    fn render_handler(method: &str, attr_src: &str, fn_src: &str) -> String {
        let route: RouteAttr = syn::parse_str(attr_src).expect("parse route attr");
        let func: ItemFn = syn::parse_str(fn_src).expect("parse fn");
        let meta = classify(method, &route, &func.sig.ident, &func.sig.inputs, &func.sig.output)
            .expect("classify ok");
        render(method, &meta).source
    }

    #[test]
    fn get_path_only() {
        let out = render_handler(
            "GET",
            r#""/api/data-room/{room_id}""#,
            r#"pub async fn get_data_room(Path(room_id): Path<String>, user: User)
                 -> CollabResult<DataRoomResponse> { unreachable!() }"#,
        );
        // alias `CollabResult<T>` unwraps to T (the old code rendered the alias!)
        assert!(out.contains("export async function getDataRoom(roomId: string): Promise<DataRoomResponse>"), "{out}");
        assert!(out.contains("`/api/data-room/${encodeURIComponent(String(roomId))}`"), "{out}");
        assert!(out.contains(r#"import type { DataRoomResponse } from "../types/DataRoomResponse";"#), "{out}");
        // server extractor `user: User` stripped
        assert!(!out.contains("user"), "{out}");
        assert!(!out.contains("CollabResult"), "alias must not leak into TS: {out}");
    }

    #[test]
    fn post_body_direct_no_wrapping() {
        let out = render_handler(
            "POST",
            r#""/api/posts""#,
            r#"pub async fn create_post(Json(req): Json<CreatePostRequest>)
                 -> Result<PostResponse, ApiError> { unreachable!() }"#,
        );
        // D2: body passed directly, not `{ "req": req }`
        assert!(out.contains("export async function createPost(req: CreatePostRequest): Promise<PostResponse>"), "{out}");
        assert!(out.contains("return apiPost<PostResponse>(`/api/posts`, req);"), "{out}");
        assert!(!out.contains(r#"{ "req""#), "must not wrap body: {out}");
        // explicit Result<T,E> → error doc
        assert!(out.contains("@throws ApiError"), "{out}");
    }

    #[test]
    fn path_tuple_and_query_struct() {
        let out = render_handler(
            "GET",
            r#""/api/rooms/{room_id}/files""#,
            r#"pub async fn list_files(Path(room_id): Path<String>, Query(q): Query<ListFilesQuery>)
                 -> Result<ListResponseFile, ApiError> { unreachable!() }"#,
        );
        assert!(out.contains("listFiles(roomId: string, q: ListFilesQuery)"), "{out}");
        // query struct forwarded as object to the runtime
        assert!(out.contains("return apiGet<ListResponseFile>(`/api/rooms/${encodeURIComponent(String(roomId))}/files`, q);"), "{out}");
    }

    #[test]
    fn delete_unit_return_is_void() {
        let out = render_handler(
            "DELETE",
            r#""/api/posts/{id}""#,
            r#"pub async fn delete_post(Path(id): Path<String>) -> Result<(), ApiError> { unreachable!() }"#,
        );
        assert!(out.contains("deletePost(id: string): Promise<void>"), "{out}");
        assert!(out.contains("return apiDelete<void>(`/api/posts/${encodeURIComponent(String(id))}`);"), "{out}");
    }

    #[test]
    fn multi_path_tuple() {
        let out = render_handler(
            "GET",
            r#""/api/rooms/{room_id}/files/{file_id}""#,
            r#"pub async fn get_file(Path((room_id, file_id)): Path<(String, String)>)
                 -> Result<FileResponse, ApiError> { unreachable!() }"#,
        );
        assert!(out.contains("getFile(roomId: string, fileId: string)"), "{out}");
        assert!(out.contains("/api/rooms/${encodeURIComponent(String(roomId))}/files/${encodeURIComponent(String(fileId))}`"), "{out}");
    }

    // ── validation errors (Risk #2) ──────────────────────────────────
    fn classify_err(method: &str, attr_src: &str, fn_src: &str) -> String {
        let route: RouteAttr = syn::parse_str(attr_src).unwrap();
        let func: ItemFn = syn::parse_str(fn_src).unwrap();
        classify(method, &route, &func.sig.ident, &func.sig.inputs, &func.sig.output)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn err_body_on_get() {
        let e = classify_err(
            "GET",
            r#""/api/x""#,
            r#"pub async fn x(Json(req): Json<Req>) -> Result<R, E> { unreachable!() }"#,
        );
        assert!(e.contains("cannot take a `Json`"), "{e}");
    }

    #[test]
    fn err_path_count_mismatch() {
        let e = classify_err(
            "GET",
            r#""/api/x/{a}/{b}""#,
            r#"pub async fn x(Path(a): Path<String>) -> Result<R, E> { unreachable!() }"#,
        );
        assert!(e.contains("path placeholder"), "{e}");
    }

    #[test]
    fn err_two_queries() {
        let e = classify_err(
            "GET",
            r#""/api/x""#,
            r#"pub async fn x(Query(a): Query<A>, Query(b): Query<B>) -> Result<R, E> { unreachable!() }"#,
        );
        assert!(e.contains("more than one `Query`"), "{e}");
    }

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
        assert!(s.contains(r##"#[cfg(not(feature="server"))]pubasyncfnget_post_with(__provider:&dyn::ts_server_fn_client::RequestProvider,id:String)->ApiResult<Post>"##), "{s}");
        // scoped to the client (`_with`) fn's own param list: the whole-output
        // string also legitimately contains "user:User" in the untouched
        // server-side original fn (`#[cfg(feature="server")] ... get_post(...,
        // user: User)`), which Task 5 preserves verbatim — that occurrence is
        // correct and must not make this assertion fail.
        let with_start = s.find("get_post_with(").expect("with fn present");
        let with_close = with_start + s[with_start..].find(')').expect("with fn params close");
        assert!(!s[with_start..with_close].contains("user"), "server extractor must be stripped: {s}");
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
        assert!(s.contains("create_post_with(__provider:&dyn::ts_server_fn_client::RequestProvider,room_id:String,q:Opts,req:CreatePost)"), "{s}");
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
        assert!(s.contains("delete_post_with(__provider:&dyn::ts_server_fn_client::RequestProvider,id:String)->Result<(),ApiError>"), "{s}");
        assert!(s.contains("::ts_server_fn_client::Method::DELETE"), "{s}");
    }

    #[test]
    fn form_body_gets_no_client_fn() {
        let s = expand_norm(
            "POST",
            r#""/api/login""#,
            r#"pub async fn login(Form(req): Form<LoginForm>) -> Result<Session, ApiError> { unreachable!() }"#,
        );
        assert!(!s.contains("login_with"), "Form must not emit a client fn: {s}");
        assert!(s.contains(r##"#[cfg(feature="server")]pubasyncfnlogin"##), "server side must remain: {s}");
    }
}
