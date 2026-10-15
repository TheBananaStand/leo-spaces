//! One error type for the HTTP surface, and the status codes it maps to.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Space(#[from] crate::spaces::SpaceError),
    #[error("{0}")]
    Host(#[from] crate::host::HostError),
    #[error("{0}")]
    Other(String),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        use crate::spaces::SpaceError;
        // The status code carries meaning the client acts on: a 404 for a
        // space that is gone lets the UI drop the row, and a 409 for a refused
        // operation tells it to show the message rather than retry.
        let status = match &self {
            Self::Space(SpaceError::NotFound(_)) => StatusCode::NOT_FOUND,
            Self::Space(SpaceError::Refused(_)) => StatusCode::CONFLICT,
            Self::Space(SpaceError::NoLibvirt { .. }) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = self.to_string();
        tracing::warn!(%status, error = %body, "request failed");
        (status, Json(json!({ "error": body }))).into_response()
    }
}
