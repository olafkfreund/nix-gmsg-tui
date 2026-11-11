//! Error types.

use crate::crypto::CryptoError;
use crate::types::PairingErrorCode;

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Network failure (connection, TLS, timeout of the HTTP request itself).
    #[error("network error: {0}")]
    Network(reqwest::Error),
    /// The server answered with an error status.
    #[error("server error: HTTP {status}, code {code:?}: {message}")]
    Server {
        /// HTTP status.
        status: u16,
        /// gRPC status code from the error body, if any.
        code: Option<i64>,
        /// Error message from the body, if any.
        message: String,
        /// Type URLs of the error details.
        details: Vec<String>,
    },
    /// The session is no longer valid (unpaired, logged out, credentials rejected):
    /// pair again.
    #[error("session no longer valid: pair again ({0})")]
    SessionGone(String),
    /// The phone did not answer in time. The relay accepted the call, so the phone may
    /// still carry it out later.
    #[error("the phone did not respond in time")]
    Timeout,
    /// The phone answered but refused or failed the request.
    #[error("the phone refused the request: {0}")]
    PhoneRefused(String),
    /// A reply could not be decoded.
    #[error("decode error: {0}")]
    Decode(String),
    /// Cryptographic failure.
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    /// The client has been shut down.
    #[error("client closed")]
    Closed,
    /// An argument or session field is invalid.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Pairing failed.
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingError),
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        // Request URLs can carry signed media tokens.
        Self::Network(error.without_url())
    }
}

impl Error {
    /// Whether the error means the session is gone and pairing again is needed.
    pub fn is_session_gone(&self) -> bool {
        match self {
            Error::SessionGone(_) => true,
            Error::Server { code, details, .. } => {
                *code == Some(16)
                    || (*code == Some(5) && details.iter().any(|d| d.ends_with("TachyonError")))
            }
            _ => false,
        }
    }

    /// Whether the error is the server's HTTP status `s`.
    pub fn is_http_status(&self, s: u16) -> bool {
        matches!(self, Error::Server { status, .. } if *status == s)
    }

    /// Whether retrying later may help (network errors, 5xx).
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::Timeout => true,
            Error::Server { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

impl From<prost::DecodeError> for Error {
    fn from(e: prost::DecodeError) -> Self {
        Error::Decode(e.to_string())
    }
}

impl From<crate::codec::PbLiteError> for Error {
    fn from(e: crate::codec::PbLiteError) -> Self {
        Error::Decode(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn network_error_discards_signed_url() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = client
            .get("http://127.0.0.1:0/media?token=SECRET")
            .send()
            .await
            .unwrap_err();
        assert!(error.url().is_some(), "the input error must carry a URL");

        let error = Error::from(error);
        assert!(!error.to_string().contains("SECRET"));
        assert!(matches!(error, Error::Network(inner) if inner.url().is_none()));
    }
}

/// Why a pairing failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PairingError {
    /// The user chose the wrong emoji on the phone.
    #[error("wrong emoji chosen on the phone")]
    WrongEmoji,
    /// The user cancelled on the phone (or answered "this is not me").
    #[error("cancelled on the phone")]
    CancelledOnPhone,
    /// Cancelled from this side ([`CancelToken`](crate::CancelToken)).
    #[error("cancelled")]
    Cancelled,
    /// The phone did not answer in time, or the verification timed out.
    #[error("timed out")]
    TimedOut,
    /// The Google account has no phone usable for pairing.
    #[error("no phone available for pairing on this account")]
    NoPhone,
    /// No QR code was scanned before the client gave up.
    #[error("QR code not scanned in time")]
    QrExpired,
    /// The pairing was revoked while pairing.
    #[error("pairing revoked")]
    Revoked,
    /// The Google account cookies are missing or rejected.
    #[error("Google account cookies missing or rejected: {0}")]
    BadCookies(String),
    /// The UKEY2 handshake failed locally (invalid server message).
    #[error("handshake failed: {0}")]
    Handshake(String),
    /// Another error reported by the phone.
    #[error("phone reported pairing error {0:?}")]
    Phone(PairingErrorCode),
}

impl PairingError {
    /// Maps a `PairingError` code of the protocol (spec §4) to a pairing error.
    pub fn from_code(code: PairingErrorCode) -> Self {
        match code.0 {
            5 => PairingError::WrongEmoji,
            7 | 27 => PairingError::CancelledOnPhone,
            2 | 3 | 6 | 17 | 18 => PairingError::TimedOut,
            20 | 21 | 22 | 26 => PairingError::Cancelled,
            _ => PairingError::Phone(code),
        }
    }
}

/// Result type of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
