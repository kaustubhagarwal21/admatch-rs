//! API contract tests: every error code, the readiness check, the response
//! shape and the metrics endpoint. None of these depend on how the auction
//! ranks ads; `api_engine.rs` covers that.

mod common;

use std::sync::OnceLock;

use admatch_core::engine::EngineConfig;
use admatch_server::routes::{MAX_BODY_BYTES, router};
use admatch_server::state::AppState;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{app, assert_error, empty_state, get, loaded_state, post_match, send, send_raw};
use metrics_exporter_prometheus::PrometheusHandle;
use serde_json::json;

const VALID: &str = r#"{"query": "photo editor", "country": "IN", "user_id": 1}"#;

// ---------- readiness (503) ----------

#[tokio::test]
async fn healthz_is_503_until_the_index_is_loaded() {
    let state = empty_state();
    let (status, body) = send(router(state.clone()), get("/healthz")).await;
    assert_error(status, &body, StatusCode::SERVICE_UNAVAILABLE, "not_ready");

    state.publish(
        admatch_core::engine::Snapshot::build(common::test_campaigns(), EngineConfig::default())
            .unwrap(),
    );
    let (status, body) = send(router(state), get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["campaigns"], 3);
    assert_eq!(body["keywords"], 3);
}

#[tokio::test]
async fn match_is_503_until_the_index_is_loaded() {
    let (status, body) = send(router(empty_state()), post_match(VALID)).await;
    assert_error(status, &body, StatusCode::SERVICE_UNAVAILABLE, "not_ready");
}

// ---------- malformed requests (400) ----------

#[tokio::test]
async fn invalid_json_syntax_is_400() {
    let (status, body) = send(app(), post_match(r#"{"query": "photo"#)).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn wrong_field_types_are_400() {
    for bad in [
        json!({"query": 42, "country": "IN", "user_id": 1}),
        json!({"query": "photo", "country": "IN", "user_id": "one"}),
        json!({"query": "photo", "country": "IN", "user_id": 1, "personalized": "yes"}),
        json!({"query": "photo", "country": ["IN"], "user_id": 1}),
        json!(["photo", "IN"]),
    ] {
        let (status, body) = send(app(), post_match(&bad.to_string())).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "malformed_request");
    }
}

#[tokio::test]
async fn missing_required_fields_are_400() {
    for bad in [
        json!({"country": "IN", "user_id": 1}),
        json!({"query": "photo", "user_id": 1}),
        json!({"query": "photo", "country": "IN"}),
    ] {
        let (status, body) = send(app(), post_match(&bad.to_string())).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "malformed_request");
    }
}

#[tokio::test]
async fn missing_json_content_type_is_400() {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/match")
        .body(Body::from(VALID))
        .unwrap();
    let (status, body) = send(app(), req).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn error_messages_do_not_leak_serde_internals() {
    let (_, body) = send(
        app(),
        post_match(r#"{"query": 42, "country": "IN", "user_id": 1}"#),
    )
    .await;
    let message = body["error"]["message"].as_str().unwrap();
    assert!(!message.contains("invalid type"), "{message}");
    assert!(!message.contains("line"), "{message}");
}

// ---------- validation (422) ----------

#[tokio::test]
async fn unknown_country_is_422() {
    for country in ["XX", "in", ""] {
        let req = json!({"query": "photo", "country": country, "user_id": 1});
        let (status, body) = send(app(), post_match(&req.to_string())).await;
        assert_error(
            status,
            &body,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_country",
        );
    }
}

#[tokio::test]
async fn unknown_age_bucket_is_422_when_personalized() {
    let req = json!({
        "query": "photo", "country": "IN", "user_id": 1,
        "personalized": true, "age_bucket": "18-25"
    });
    let (status, body) = send(app(), post_match(&req.to_string())).await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_age_bucket",
    );
}

#[tokio::test]
async fn age_bucket_is_ignored_when_not_personalized() {
    let req = json!({
        "query": "photo", "country": "IN", "user_id": 1,
        "personalized": false, "age_bucket": "not-a-bucket"
    });
    let (status, _) = send(app(), post_match(&req.to_string())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn empty_queries_are_422() {
    for query in ["", "   ", "!!! ???"] {
        let req = json!({"query": query, "country": "IN", "user_id": 1});
        let (status, body) = send(app(), post_match(&req.to_string())).await;
        assert_error(
            status,
            &body,
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty_query",
        );
    }
}

#[tokio::test]
async fn query_over_200_characters_is_422() {
    let ok = json!({"query": "a".repeat(200), "country": "IN", "user_id": 1});
    let (status, _) = send(app(), post_match(&ok.to_string())).await;
    assert_eq!(status, StatusCode::OK);

    let long = json!({"query": "a".repeat(201), "country": "IN", "user_id": 1});
    let (status, body) = send(app(), post_match(&long.to_string())).await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "query_too_long",
    );
}

#[tokio::test]
async fn query_over_16_tokens_is_422() {
    let ok = json!({"query": vec!["a"; 16].join(" "), "country": "IN", "user_id": 1});
    let (status, _) = send(app(), post_match(&ok.to_string())).await;
    assert_eq!(status, StatusCode::OK);

    let many = json!({"query": vec!["a"; 17].join(" "), "country": "IN", "user_id": 1});
    let (status, body) = send(app(), post_match(&many.to_string())).await;
    assert_error(
        status,
        &body,
        StatusCode::UNPROCESSABLE_ENTITY,
        "too_many_tokens",
    );
}

// ---------- body limit (413), routing (404, 405) ----------

#[tokio::test]
async fn oversized_body_with_content_length_is_413_json() {
    let big = "x".repeat(MAX_BODY_BYTES + 1);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/match")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, big.len())
        .body(Body::from(big))
        .unwrap();
    let (status, body) = send(app(), req).await;
    assert_error(
        status,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
}

#[tokio::test]
async fn oversized_body_without_content_length_is_413_json() {
    // No Content-Length header: the limit is enforced while the body is
    // read, which surfaces as an extractor rejection instead.
    let big = format!(r#"{{"query": "{}"}}"#, "x".repeat(MAX_BODY_BYTES));
    let (status, body) = send(app(), post_match(&big)).await;
    assert_error(
        status,
        &body,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    );
}

#[tokio::test]
async fn unknown_route_is_404_json() {
    let (status, body) = send(app(), get("/v1/nope")).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn wrong_method_is_405_json() {
    let (status, body) = send(app(), get("/v1/match")).await;
    assert_error(
        status,
        &body,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
}

// ---------- happy paths that do not depend on ranking ----------

#[tokio::test]
async fn no_match_is_200_with_null_ad_and_impression() {
    let req = json!({"query": "zorbu quaplim", "country": "IN", "user_id": 7});
    let (status, body) = send(app(), post_match(&req.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["ad"].is_null(), "{body}");
    assert!(body["impression_id"].is_null(), "{body}");
    assert_eq!(body["candidates"], 0);
    assert!(body["took_us"].is_u64(), "{body}");
    // The ranking is only included when debug is requested.
    assert!(body.get("ranking").is_none(), "{body}");
    // user_id is never echoed back.
    assert!(!body.to_string().contains("user_id"), "{body}");
}

#[tokio::test]
async fn debug_adds_a_ranking_array() {
    let req = json!({"query": "zorbu", "country": "IN", "user_id": 7, "debug": true});
    let (status, body) = send(app(), post_match(&req.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["ranking"].is_array(), "{body}");
}

#[tokio::test]
async fn optional_fields_default_to_false() {
    let (status, body) = send(app(), post_match(VALID)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("ranking").is_none(), "{body}");
}

// ---------- metrics ----------

/// The global metrics recorder can be installed only once per process, so
/// the metrics test installs it lazily and shares the handle.
#[allow(
    clippy::unwrap_used,
    reason = "test helper: a panic here is a test failure, which is what we want"
)]
fn global_metrics() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let recorder = admatch_server::metrics::build_recorder().unwrap();
            let handle = recorder.handle();
            metrics::set_global_recorder(recorder).unwrap();
            handle
        })
        .clone()
}

#[tokio::test]
async fn metrics_endpoint_reports_requests_auctions_and_index_size() {
    let state = AppState::new(EngineConfig::default(), global_metrics());
    state.publish(
        admatch_core::engine::Snapshot::build(common::test_campaigns(), EngineConfig::default())
            .unwrap(),
    );
    let app = router(state);
    let (status, _) = send(app.clone(), post_match(VALID)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, text, content_type) = send_raw(app, get("/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().starts_with("text/plain"));
    for name in [
        "http_requests_total",
        "http_request_duration_seconds_bucket",
        "route=\"/v1/match\"",
        "auction_outcomes_total",
        "index_campaigns 3",
        "index_keywords 3",
    ] {
        assert!(text.contains(name), "missing {name} in:\n{text}");
    }
}

#[tokio::test]
async fn loaded_state_serves_healthz() {
    let (status, _) = send(router(loaded_state()), get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
}
