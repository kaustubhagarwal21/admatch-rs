//! The HTTP router: endpoints, request validation and middleware.
//!
//! Endpoints:
//!
//! * `GET /healthz`: 200 once the campaign index is loaded, otherwise 503.
//! * `POST /v1/match`: runs one auction for a search query.
//! * `GET /metrics`: Prometheus text format.

use std::time::{Duration, Instant};

use admatch_core::engine::{AuctionRequest, RankedEntry, Winner};
use admatch_core::model::{AgeBucket, Country};
use admatch_core::normalize::QueryError;
use axum::extract::rejection::JsonRejection;
use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::error::ApiError;
use crate::metrics as m;
use crate::state::AppState;

/// Largest accepted request body. A match request is a few hundred bytes;
/// the cap stops a client from making the server buffer megabytes.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Longest a request may take before it is answered with 503. The auction
/// itself takes microseconds, so hitting this means the server is
/// overloaded; failing fast is better than letting requests pile up.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Builds the application router with all middleware attached.
pub fn router(state: AppState) -> Router {
    // `.layer` calls wrap everything added before them, so the layer added
    // LAST is the OUTERMOST: a request passes json_errors, trace, metrics,
    // timeout and body limit, in that order, before reaching a handler.
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/match", post(match_ad))
        .route("/metrics", get(metrics))
        .fallback(not_found)
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        .layer(middleware::from_fn(track_metrics))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(json_errors))
}

/// Body of `POST /v1/match`.
///
/// `country` and `age_bucket` arrive as plain strings and are checked by
/// hand, so an unknown value is a 422 validation error with a clear code,
/// while a wrong JSON type (say, a number) stays a 400.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchRequest {
    /// Raw search text: 1 to 200 characters, at most 16 tokens.
    pub query: String,
    /// Storefront country code, e.g. `"IN"`.
    pub country: String,
    /// Synthetic pseudonymous user ID. Never returned by any endpoint; it is
    /// stored with impression events in a later milestone.
    pub user_id: i64,
    /// Whether the user allows personalised ads. Defaults to false.
    #[serde(default)]
    pub personalized: bool,
    /// Age bucket such as `"25-34"`. Ignored unless `personalized` is true.
    #[serde(default)]
    pub age_bucket: Option<String>,
    /// When true, the response includes the auction ranking.
    #[serde(default)]
    pub debug: bool,
}

/// Response of `POST /v1/match`.
#[derive(Debug, Clone, Serialize)]
pub struct MatchResponse {
    /// UUIDv7 of this impression, or `null` when no ad is shown.
    pub impression_id: Option<Uuid>,
    /// The ad shown, or `null`.
    pub ad: Option<Winner>,
    /// Campaigns that entered the auction.
    pub candidates: usize,
    /// Server-side processing time in microseconds.
    pub took_us: u64,
    /// Top candidates with scores; present only when `debug` was true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<Vec<RankedEntry>>,
}

/// Body of a healthy `GET /healthz`.
#[derive(Debug, Clone, Serialize)]
struct Health {
    status: &'static str,
    campaigns: usize,
    keywords: usize,
}

/// `GET /healthz`: ready once an index is loaded.
async fn healthz(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    let snapshot = state.snapshot().ok_or_else(ApiError::not_ready)?;
    Ok(Json(Health {
        status: "ok",
        campaigns: snapshot.campaign_count(),
        keywords: snapshot.keyword_count(),
    }))
}

/// `POST /v1/match`: validate, run the auction, respond.
///
/// The body is taken as `Result<Json<_>, JsonRejection>` rather than plain
/// `Json<_>` so that a bad body reaches this function and is turned into
/// our JSON error format, instead of axum's plain-text default.
async fn match_ad(
    State(state): State<AppState>,
    body: Result<Json<MatchRequest>, JsonRejection>,
) -> Result<Json<MatchResponse>, ApiError> {
    let started = Instant::now();
    let Json(req) = body?;

    let country: Country = req.country.parse().map_err(|_| {
        ApiError::validation(
            "invalid_country",
            "country must be one of IN, US, GB, AU, SG, AE, CA, DE, JP, NZ",
        )
    })?;
    // Privacy: the age bucket is only looked at for personalised requests.
    let age_bucket: Option<AgeBucket> = match (&req.age_bucket, req.personalized) {
        (Some(raw), true) => Some(raw.parse().map_err(|_| {
            ApiError::validation(
                "invalid_age_bucket",
                "age_bucket must be one of 18-24, 25-34, 35-44, 45-54, 55-64, 65+",
            )
        })?),
        _ => None,
    };

    let snapshot = state.snapshot().ok_or_else(ApiError::not_ready)?;
    let auction = AuctionRequest {
        query: &req.query,
        country,
        personalized: req.personalized,
        age_bucket,
        // The ranking is built (and sorted) only when the client asks for
        // it; the budget-skip metric comes from `outcome.budget_skipped`.
        debug: req.debug,
    };
    // Budgets are per UTC day, so "today" is the UTC date.
    let today = chrono::Utc::now().date_naive();
    let outcome = snapshot
        .run(&auction, state.budgets(), today)
        .map_err(query_error)?;
    m::record_auction(&outcome);

    // An impression ID exists only when an ad is actually shown.
    let impression_id = outcome.winner.as_ref().map(|_| Uuid::now_v7());
    Ok(Json(MatchResponse {
        impression_id,
        ad: outcome.winner,
        candidates: outcome.candidates,
        took_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        ranking: outcome.ranking,
    }))
}

/// Maps a query validation error to a 422 response.
fn query_error(err: QueryError) -> ApiError {
    let code = match err {
        QueryError::Empty => "empty_query",
        QueryError::TooLong => "query_too_long",
        QueryError::TooManyTokens => "too_many_tokens",
    };
    // QueryError's Display text is our own wording, safe to return.
    ApiError::validation(code, err.to_string())
}

/// `GET /metrics`: the Prometheus text exposition format.
async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics().render(),
    )
}

/// Any unknown route.
async fn not_found() -> ApiError {
    ApiError::from_status(StatusCode::NOT_FOUND)
}

/// Middleware: counts every request and records its latency, labelled by
/// the route template (e.g. `/v1/match`) rather than the raw path, so a
/// client probing random URLs cannot create unbounded metric series.
async fn track_metrics(req: Request, next: Next) -> Response {
    let started = Instant::now();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let response = next.run(req).await;
    m::record_request(route, method, response.status().as_u16(), started.elapsed());
    response
}

/// Middleware: guarantees that every error response uses the JSON error
/// format. Our handlers already do; this catches the responses produced
/// by layers and by axum itself (timeout, body limit, wrong method), which
/// are plain text or empty.
async fn json_errors(req: Request, next: Next) -> Response {
    let response = next.run(req).await;
    let status = response.status();
    let is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if (status.is_client_error() || status.is_server_error()) && !is_json {
        return ApiError::from_status(status).into_response();
    }
    response
}
