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
