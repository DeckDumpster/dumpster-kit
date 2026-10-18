use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("config: {0}")]
    Config(String),
    #[error("provider: {0}")]
    Provider(String),
}

/// A failed sign-in step, answered as plain text with the status that names whose fault it is.
#[derive(Debug)]
pub struct AuthError(pub StatusCode, pub &'static str);

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}
