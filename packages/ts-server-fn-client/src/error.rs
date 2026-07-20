//! Transport-level failures of the generated client. API-level errors are the
//! handler's own `E` (decoded from the non-2xx body); `ClientError` covers
//! everything that happens *around* that, and reaches the caller through the
//! `E: From<ClientError>` conversion.

use std::fmt;

#[derive(Debug)]
pub enum ClientError {
    /// Connection / TLS / timeout — the request never produced a response.
    Transport(reqwest::Error),
    /// Query or body serialization failed before sending (programmer error).
    Encode(String),
    /// The response body could not be decoded as `T` (2xx) or `E` (non-2xx).
    Decode {
        status: u16,
        source: serde_json::Error,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Transport(e) => write!(f, "transport error: {e}"),
            ClientError::Encode(e) => write!(f, "request encode error: {e}"),
            ClientError::Decode { status, source } => {
                write!(f, "response decode error (status {status}): {source}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::Transport(e) => Some(e),
            ClientError::Encode(_) => None,
            ClientError::Decode { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_status_for_decode() {
        let e = ClientError::Decode {
            status: 500,
            source: serde_json::from_slice::<()>(b"nope").unwrap_err(),
        };
        assert!(e.to_string().contains("status 500"), "{e}");
    }
}
