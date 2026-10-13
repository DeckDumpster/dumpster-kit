//! The one HTTP error shape the kernel's layers answer with.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// A status and a plain-text body. A consumer's own error type converts from it (`From` is the
/// whole integration); the layers never reveal more than the body says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError(pub StatusCode, pub String);

impl HttpError {
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        HttpError(StatusCode::UNAUTHORIZED, msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        HttpError(StatusCode::FORBIDDEN, msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        HttpError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}

impl std::error::Error for HttpError {}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}
