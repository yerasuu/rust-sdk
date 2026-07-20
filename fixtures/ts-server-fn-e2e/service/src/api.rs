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
