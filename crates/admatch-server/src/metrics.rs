//! Prometheus metrics: names, recording helpers and the recorder setup.
//!
//! The `metrics` crate works like a logging facade (think SLF4J): code calls
//! `counter!`/`histogram!`/`gauge!` anywhere, and whichever recorder is
//! installed globally stores the values. Here that recorder is the
//! Prometheus exporter, and `GET /metrics` renders it as text.
//!
//! Label values are always from a small fixed set (route templates such as
//! `/v1/match`, never raw URLs or queries), so the number of time series
//! stays bounded no matter what clients send.

use std::time::Duration;

use admatch_core::engine::{AuctionOutcome, Snapshot};
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusRecorder};

/// Requests served, labelled by route, method and status code.
pub const HTTP_REQUESTS: &str = "http_requests_total";
/// Request latency in seconds, labelled by route and method.
pub const HTTP_DURATION: &str = "http_request_duration_seconds";
/// Auctions run, labelled `outcome="win"` or `outcome="no_ad"`.
pub const AUCTIONS: &str = "auction_outcomes_total";
/// Candidates skipped because their daily budget could not cover the price.
pub const BUDGET_REJECTIONS: &str = "budget_rejections_total";
/// Campaigns in the currently served index.
pub const INDEX_CAMPAIGNS: &str = "index_campaigns";
/// Keywords in the currently served index.
pub const INDEX_KEYWORDS: &str = "index_keywords";
/// Time to load and build the index, in seconds.
pub const INDEX_BUILD: &str = "index_build_duration_seconds";
/// Index loads that failed (the previous index keeps serving).
pub const INDEX_BUILD_FAILURES: &str = "index_build_failures_total";

/// Histogram bucket bounds in seconds, from 50 µs to 1 s. The match
/// endpoint is expected to answer in well under a millisecond, so the low
/// end is fine-grained; the high end catches pathological requests.
const LATENCY_BUCKETS: &[f64] = &[
    0.000_05, 0.000_1, 0.000_25, 0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5,
    1.0,
];

/// Builds the Prometheus recorder, with real histogram buckets for every
/// metric whose name ends in `_seconds` (without buckets the exporter would
/// publish summaries, which cannot be aggregated across replicas).
///
/// It is not installed here: `main` installs it globally, while tests keep
/// their own so they never fight over the one global slot.
pub fn build_recorder() -> Result<PrometheusRecorder, BuildError> {
    Ok(PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_seconds".to_owned()), LATENCY_BUCKETS)?
        .build_recorder())
}

/// Records one finished HTTP request.
pub fn record_request(route: String, method: String, status: u16, took: Duration) {
    histogram!(HTTP_DURATION, "route" => route.clone(), "method" => method.clone())
        .record(took.as_secs_f64());
    counter!(HTTP_REQUESTS, "route" => route, "method" => method, "status" => status.to_string())
        .increment(1);
}

/// Records the result of one auction.
pub fn record_auction(outcome: &AuctionOutcome) {
    let label = if outcome.winner.is_some() {
        "win"
    } else {
        "no_ad"
    };
    counter!(AUCTIONS, "outcome" => label).increment(1);

    // Use the engine's full count, not the debug ranking: the ranking is
    // cut to ten rows, so counting its "budget" rows would undercount.
    let skipped = outcome.budget_skipped;
    if skipped > 0 {
        counter!(BUDGET_REJECTIONS).increment(u64::try_from(skipped).unwrap_or(u64::MAX));
    }
}

/// Publishes the size of a newly loaded index.
pub fn record_index_size(snapshot: &Snapshot) {
    // Gauges hold f64. Counts here are far below 2^53, where f64 stops
    // representing every integer exactly, so the conversion is exact.
    gauge!(INDEX_CAMPAIGNS).set(snapshot.campaign_count() as f64);
    gauge!(INDEX_KEYWORDS).set(snapshot.keyword_count() as f64);
}

/// Records how long an index load took.
pub fn record_index_build(took: Duration) {
    histogram!(INDEX_BUILD).record(took.as_secs_f64());
}

/// Records a failed index load.
pub fn record_index_build_failure() {
    counter!(INDEX_BUILD_FAILURES).increment(1);
}
