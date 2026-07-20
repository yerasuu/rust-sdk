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
