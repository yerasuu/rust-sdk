//! URL-encoding helper + (in a later task) the generic `call()` the generated
//! client fns delegate to.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::ClientError;
use crate::provider::{RawResponse, RequestProvider};
use http::Method;

/// Percent-encode one path segment. Mirrors the TS runtime's
/// `encodeURIComponent` (slightly stricter — also encodes `-`, `.`, `_`, `~` —
/// which is safe: the server percent-decodes either way).
pub fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

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
        seen: Mutex<Vec<(http::Method, String, Option<Vec<u8>>)>>,
    }
    impl Mock {
        fn new(status: u16, body: &'static [u8]) -> Self {
            Mock { status, body, seen: Mutex::new(Vec::new()) }
        }
    }
    impl RequestProvider for Mock {
        fn request_raw<'a>(
            &'a self,
            method: http::Method,
            path_and_query: &'a str,
            body: Option<Vec<u8>>,
        ) -> BoxFuture<'a, Result<RawResponse, crate::error::ClientError>> {
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
    impl From<crate::error::ClientError> for ApiErr {
        fn from(e: crate::error::ClientError) -> Self {
            ApiErr { code: format!("client:{e}") }
        }
    }

    const NO_Q: Option<&()> = None;
    const NO_B: Option<&()> = None;

    #[tokio::test]
    async fn ok_2xx_decodes_success_type() {
        let mock = Mock::new(200, br#"{"id":"1"}"#);
        let r: Result<Dto, ApiErr> = call(&mock, http::Method::GET, "/x", NO_Q, NO_B).await;
        assert_eq!(r.unwrap(), Dto { id: "1".into() });
    }

    #[tokio::test]
    async fn empty_2xx_body_is_unit() {
        let mock = Mock::new(200, b"");
        let r: Result<(), ApiErr> = call(&mock, http::Method::DELETE, "/x", NO_Q, NO_B).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn non_2xx_decodes_api_error() {
        let mock = Mock::new(400, br#"{"code":"boom"}"#);
        let r: Result<Dto, ApiErr> = call(&mock, http::Method::GET, "/x", NO_Q, NO_B).await;
        assert_eq!(r.unwrap_err(), ApiErr { code: "boom".into() });
    }

    #[tokio::test]
    async fn garbage_2xx_is_decode_error() {
        let mock = Mock::new(200, b"not json");
        let r: Result<Dto, ApiErr> = call(&mock, http::Method::GET, "/x", NO_Q, NO_B).await;
        assert!(r.unwrap_err().code.starts_with("client:"), "expected ClientError conversion");
    }

    #[tokio::test]
    async fn garbage_non_2xx_is_decode_error() {
        let mock = Mock::new(500, b"<html>oops</html>");
        let r: Result<Dto, ApiErr> = call(&mock, http::Method::GET, "/x", NO_Q, NO_B).await;
        assert!(r.unwrap_err().code.starts_with("client:"), "expected ClientError conversion");
    }

    #[tokio::test]
    async fn query_is_serialized_onto_path() {
        let mock = Mock::new(200, b"null");
        let q = Qs { limit: 5, tag: "a b".into() };
        let _: Result<(), ApiErr> = call(&mock, http::Method::GET, "/x", Some(&q), NO_B).await;
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].1, "/x?limit=5&tag=a+b");
        assert!(seen[0].2.is_none());
    }

    #[tokio::test]
    async fn body_is_json_bytes() {
        let mock = Mock::new(200, br#"{"id":"1"}"#);
        let b = Req { title: "hello".into() };
        let _: Result<Dto, ApiErr> = call(&mock, http::Method::POST, "/x", NO_Q, Some(&b)).await;
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].0, http::Method::POST);
        assert_eq!(seen[0].2.as_deref(), Some(br#"{"title":"hello"}"#.as_slice()));
    }
}
