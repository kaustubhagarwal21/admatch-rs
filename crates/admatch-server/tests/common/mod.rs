//! Helpers shared by the API test files.
//!
//! Each test builds the real router around its own state and sends requests
//! with `tower::ServiceExt::oneshot`: the request goes straight into the
//! router as a function call, with no TCP socket, so tests are fast and
//! cannot collide on ports.

#![allow(
    dead_code,
    reason = "each test file compiles this module separately and uses a different subset of it"
)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic here is a test failure, which is what we want"
)]

use admatch_core::engine::{EngineConfig, Snapshot};
use admatch_core::model::{
    AdvertiserId, AgeBucket, Campaign, CampaignId, CampaignStatus, Country, Keyword, KeywordId,
    MatchType, Micros, NegativeKeyword,
};
use admatch_server::routes::router;
use admatch_server::state::AppState;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::Value;
use tower::ServiceExt;

/// A campaign with one keyword and sensible defaults.
pub fn campaign(
    id: i64,
    countries: Vec<Country>,
    keyword: (i64, &str, MatchType, i64),
) -> Campaign {
    let (keyword_id, text, match_type, bid) = keyword;
    Campaign {
        id: CampaignId(id),
        advertiser_id: AdvertiserId(id),
        name: format!("campaign {id}"),
        status: CampaignStatus::Active,
        daily_budget: Micros(50_000_000),
        countries,
        age_buckets: None,
        audience_size: None,
        keywords: vec![Keyword {
            id: KeywordId(keyword_id),
            text: text.to_owned(),
            match_type,
            max_cpt_bid: Micros(bid),
        }],
        negative_keywords: vec![],
    }
}

/// The campaigns every API test runs against:
///
/// * 1: exact "photo editor", IN, bid 2.00
/// * 2: broad "chess", US, bid 1.50, negative broad "free"
/// * 3: broad "fitness", IN, bid 1.00, age-targeted to 25-34 with an
///   audience of 10,000 (above the 5,000 threshold)
pub fn test_campaigns() -> Vec<Campaign> {
    let photo = campaign(
        1,
        vec![Country::IN],
        (11, "photo editor", MatchType::Exact, 2_000_000),
    );
    let mut chess = campaign(
        2,
        vec![Country::US],
        (21, "chess", MatchType::Broad, 1_500_000),
    );
    chess.negative_keywords = vec![NegativeKeyword {
        text: "free".to_owned(),
        match_type: MatchType::Broad,
    }];
    let mut fitness = campaign(
        3,
        vec![Country::IN],
        (31, "fitness", MatchType::Broad, 1_000_000),
    );
    fitness.age_buckets = Some(vec![AgeBucket::Age25To34]);
    fitness.audience_size = Some(10_000);
    vec![photo, chess, fitness]
}

/// State with nothing loaded (as right after startup).
pub fn empty_state() -> AppState {
    let handle = PrometheusBuilder::new().build_recorder().handle();
    AppState::new(EngineConfig::default(), handle)
}

/// State with the test campaigns loaded.
pub fn loaded_state() -> AppState {
    let state = empty_state();
    let snapshot = Snapshot::build(test_campaigns(), EngineConfig::default()).unwrap();
    state.publish(snapshot);
    state
}

/// A router over the loaded test campaigns.
pub fn app() -> Router {
    router(loaded_state())
}

/// `POST /v1/match` with a JSON body given as text.
pub fn post_match(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/match")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

/// A GET request.
pub fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// Sends one request and returns the status and the raw body.
pub async fn send_raw(app: Router, req: Request<Body>) -> (StatusCode, String, Option<String>) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap(),
        content_type,
    )
}

/// Sends one request and parses the body as JSON.
pub async fn send(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body, content_type) = send_raw(app, req).await;
    assert_eq!(
        content_type.as_deref(),
        Some("application/json"),
        "every response here must be JSON, got body {body:?}"
    );
    (status, serde_json::from_str(&body).unwrap())
}

/// Asserts the standard error shape and returns nothing on success.
pub fn assert_error(status: StatusCode, body: &Value, want_status: StatusCode, want_code: &str) {
    assert_eq!(status, want_status, "body: {body}");
    assert_eq!(body["error"]["code"], want_code, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "error message missing: {body}"
    );
}
