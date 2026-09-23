//! The JSON error format shared by every endpoint.
//!
//! Every error response has the same shape:
//!
//! ```json
//! {"error": {"code": "snake_case_code", "message": "human-readable text"}}
//! ```
//!
//! Clients branch on the stable `code`; the `message` is for people. Messages
//! are always written by us, never copied from an internal error (a serde or
//! I/O error could reveal type names, file paths or other internals).

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An error that is turned into an HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status code.
    pub status: StatusCode,
    /// Stable, machine-readable error code.
    pub code: &'static str,
    /// Human-readable explanation, safe to show to any client.
    pub message: String,
}

impl ApiError {
    /// Builds an error from its parts.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400: the body is not the JSON object we expect.
    pub fn malformed(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "malformed_request", message)
    }

    /// 422: the JSON is well formed but a value breaks a rule.
    pub fn validation(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    /// 503: the campaign index has not been loaded yet.
    pub fn not_ready() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "the campaign index is not loaded yet",
        )
    }

    /// A generic error for a bare status code, used when a middleware layer
    /// (timeout, body limit, routing) produced a response without a JSON body.
    pub fn from_status(status: StatusCode) -> Self {
        let (code, message) = match status {
            StatusCode::NOT_FOUND => ("not_found", "no such route"),
            StatusCode::METHOD_NOT_ALLOWED => (
                "method_not_allowed",
                "this route does not accept that method",
            ),
            StatusCode::PAYLOAD_TOO_LARGE => ("payload_too_large", "request body is too large"),
            StatusCode::REQUEST_TIMEOUT | StatusCode::SERVICE_UNAVAILABLE => (
                "unavailable",
                "the server is overloaded or timed out, retry later",
            ),
            s if s.is_client_error() => ("bad_request", "the request was rejected"),
            _ => ("internal_error", "internal server error"),
        };
        Self::new(status, code, message)
    }
}

/// The JSON body, `{"error": {...}}`.
#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code,
                message: &self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// axum's `Json` extractor fails with a `JsonRejection`. By default axum
/// answers 400, 415 or 422 with a plain-text body that includes serde's
/// message. This conversion keeps our contract instead: every body problem
/// (bad syntax, wrong types, missing fields, wrong content type) is a 400
/// with our own wording; only an oversized body gets 413.
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        match rejection {
            JsonRejection::JsonSyntaxError(_) => {
                ApiError::malformed("request body is not valid JSON")
            }
            JsonRejection::JsonDataError(_) => {
                ApiError::malformed("request body has a missing field or a field of the wrong type")
            }
            JsonRejection::MissingJsonContentType(_) => {
                ApiError::malformed("expected header Content-Type: application/json")
            }
            JsonRejection::BytesRejection(_)
                if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE =>
            {
                ApiError::from_status(StatusCode::PAYLOAD_TOO_LARGE)
            }
            _ => ApiError::malformed("request body could not be read"),
        }
    }
}
