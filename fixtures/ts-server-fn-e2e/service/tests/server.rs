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
