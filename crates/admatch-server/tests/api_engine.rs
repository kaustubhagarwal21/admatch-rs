//! End-to-end match tests through the HTTP API: real matching, targeting,
//! negative keywords and pricing, checked against the spec's semantics
//! (default reserve 100,000 micros, increment 10,000 micros).
//!
//! These need the real matching engine in `admatch-core`.

mod common;

use axum::http::StatusCode;
use common::{app, post_match, send};
use serde_json::{Value, json};
use uuid::Uuid;

async fn match_body(req: Value) -> Value {
    let (status, body) = send(app(), post_match(&req.to_string())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn exact_keyword_wins_and_pays_the_reserve() {
    let body = match_body(json!({"query": "photo editor", "country": "IN", "user_id": 1})).await;
    let ad = &body["ad"];
    assert_eq!(ad["campaign_id"], 1, "{body}");
    assert_eq!(ad["keyword_id"], 11);
    assert_eq!(ad["keyword"], "photo editor");
    assert_eq!(ad["match_type"], "exact");
    assert_eq!(ad["max_cpt_bid_micros"], 2_000_000);
    assert_eq!(ad["relevance_bp"], 10_000);
    // The only candidate: no runner-up, so the price is the reserve.
    assert_eq!(ad["price_micros"], 100_000);
    assert_eq!(body["candidates"], 1);

    let id: Uuid = body["impression_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(id.get_version_num(), 7, "impression_id must be a UUIDv7");
}

#[tokio::test]
async fn exact_match_ignores_word_order_and_case() {
    let body = match_body(json!({"query": "Editor PHOTO!", "country": "IN", "user_id": 1})).await;
    assert_eq!(body["ad"]["campaign_id"], 1, "{body}");
}

#[tokio::test]
async fn exact_keyword_does_not_match_extra_words() {
    let body =
        match_body(json!({"query": "free photo editor", "country": "IN", "user_id": 1})).await;
    assert!(body["ad"].is_null(), "{body}");
}

#[tokio::test]
async fn broad_keyword_matches_a_longer_query_with_scaled_relevance() {
    let body = match_body(json!({"query": "chess puzzles", "country": "US", "user_id": 1})).await;
    let ad = &body["ad"];
    assert_eq!(ad["campaign_id"], 2, "{body}");
    assert_eq!(ad["match_type"], "broad");
    // 7,000 × 1 keyword token / 2 query tokens.
    assert_eq!(ad["relevance_bp"], 3_500);
}

#[tokio::test]
async fn negative_keyword_blocks_the_campaign() {
    let body = match_body(json!({"query": "free chess", "country": "US", "user_id": 1})).await;
    assert!(body["ad"].is_null(), "{body}");
}

#[tokio::test]
async fn campaign_does_not_serve_outside_its_countries() {
    let body = match_body(json!({"query": "chess", "country": "IN", "user_id": 1})).await;
    assert!(body["ad"].is_null(), "{body}");
}

#[tokio::test]
async fn age_targeted_campaign_serves_only_matching_personalised_requests() {
    let hit = match_body(json!({
        "query": "fitness", "country": "IN", "user_id": 1,
        "personalized": true, "age_bucket": "25-34"
    }))
    .await;
    assert_eq!(hit["ad"]["campaign_id"], 3, "{hit}");

    // Same user, but personalised ads are off: never shown as a fallback.
    let not_personalized = match_body(json!({
        "query": "fitness", "country": "IN", "user_id": 1,
        "personalized": false, "age_bucket": "25-34"
    }))
    .await;
    assert!(not_personalized["ad"].is_null(), "{not_personalized}");

    let other_bucket = match_body(json!({
        "query": "fitness", "country": "IN", "user_id": 1,
        "personalized": true, "age_bucket": "45-54"
    }))
    .await;
    assert!(other_bucket["ad"].is_null(), "{other_bucket}");
}

#[tokio::test]
async fn debug_ranking_lists_the_candidate_with_its_score() {
    let body = match_body(json!({
        "query": "chess", "country": "US", "user_id": 1, "debug": true
    }))
    .await;
    let ranking = body["ranking"].as_array().unwrap();
    assert!(!ranking.is_empty(), "{body}");
    let first = &ranking[0];
    assert_eq!(first["campaign_id"], 2);
    assert_eq!(first["bid_micros"], 1_500_000);
    assert_eq!(first["relevance_bp"], 10_000 * 7 / 10);
    // score = bid × relevance.
    assert_eq!(first["score"], 1_500_000_u64 * 7_000);
}
