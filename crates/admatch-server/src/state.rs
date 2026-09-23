//! Shared state for request handlers, and the task that keeps the campaign
//! index fresh.
//!
//! The index is an immutable [`Snapshot`] behind an [`ArcSwapOption`]:
//!
//! * A handler calls `load()` and gets a cheap reference-counted pointer to
//!   the current snapshot. It never takes a lock, so a reload can never make
//!   a request wait.
//! * A reload builds a complete new snapshot on the side and publishes it
//!   with one atomic pointer swap. Requests already running keep using the
//!   old snapshot until they finish; the old one is freed when the last
//!   `Arc` to it is dropped.
//! * `None` means "nothing loaded yet", which `/healthz` reports as 503.
//!
//! Java analogy: like an `AtomicReference<Snapshot>` where the snapshot is
//! never mutated after publication.

use std::sync::Arc;
use std::time::{Duration, Instant};

use admatch_core::budget::BudgetStore;
use admatch_core::engine::{EngineConfig, Snapshot};
use arc_swap::ArcSwapOption;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::time::MissedTickBehavior;

use crate::config::CampaignSource;
use crate::metrics as m;
use crate::seed_file::read_seed_file;

/// Everything a request handler needs. Cloning it only clones `Arc`s, which
/// is how axum hands a copy to each request.
#[derive(Clone)]
pub struct AppState {
    /// The current campaign index, or `None` before the first load.
    snapshot: Arc<ArcSwapOption<Snapshot>>,
    /// Daily spend per campaign. It lives outside the snapshot on purpose:
    /// reloading campaigns must not reset what has already been spent today.
    budgets: Arc<BudgetStore>,
    /// Engine settings used for every snapshot built by this process.
    engine: EngineConfig,
    /// Renders the metrics registry for `GET /metrics`.
    metrics: PrometheusHandle,
}

impl AppState {
    /// A state with no index loaded yet and fresh in-memory budgets.
    pub fn new(engine: EngineConfig, metrics: PrometheusHandle) -> Self {
        Self {
            snapshot: Arc::new(ArcSwapOption::empty()),
            budgets: Arc::new(BudgetStore::in_memory()),
            engine,
            metrics,
        }
    }

    /// The current snapshot, if one has been loaded.
    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.snapshot.load_full()
    }

    /// Publishes a new snapshot; later requests see it immediately.
    pub fn publish(&self, snapshot: Snapshot) {
        m::record_index_size(&snapshot);
        self.snapshot.store(Some(Arc::new(snapshot)));
    }

    /// The budget store the auction charges winners against.
    pub fn budgets(&self) -> &BudgetStore {
        &self.budgets
    }

    /// Engine settings for building snapshots.
    pub fn engine(&self) -> EngineConfig {
        self.engine
    }

    /// The metrics renderer.
    pub fn metrics(&self) -> &PrometheusHandle {
        &self.metrics
    }
}

/// Loads campaigns from `source` and builds a snapshot.
///
/// This is blocking work (file I/O, then CPU-bound index building), so it
/// must not run directly on an async worker thread, where it would stall
/// every request scheduled on that thread. Callers use `spawn_blocking`.
pub fn build_snapshot(source: &CampaignSource, engine: EngineConfig) -> anyhow::Result<Snapshot> {
    let campaigns = match source {
        CampaignSource::File(path) => read_seed_file(path)?.campaigns,
    };
    Ok(Snapshot::build(campaigns, engine)?)
}

/// Loads the index now and then again every `every`, forever.
///
/// A failed load is logged and retried on the next tick. If the first load
/// fails, `/healthz` stays at 503 so no traffic is routed here; if a later
/// reload fails, the previous snapshot keeps serving. The process never
/// exits because of a bad seed file, which is the usual behaviour for a
/// service behind a load balancer.
pub async fn keep_index_fresh(state: AppState, source: CampaignSource, every: Duration) {
    let mut ticker = tokio::time::interval(every);
    // If a load takes longer than the interval, wait a full interval before
    // the next one instead of firing a burst of catch-up reloads.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // The first tick completes immediately, so the initial load happens
        // at startup.
        ticker.tick().await;
        let started = Instant::now();
        let source_for_task = source.clone();
        let engine = state.engine();
        let result =
            tokio::task::spawn_blocking(move || build_snapshot(&source_for_task, engine)).await;
        match result {
            Ok(Ok(snapshot)) => {
                let took = started.elapsed();
                m::record_index_build(took);
                tracing::info!(
                    campaigns = snapshot.campaign_count(),
                    keywords = snapshot.keyword_count(),
                    took_ms = took.as_millis(),
                    "index loaded"
                );
                state.publish(snapshot);
            }
            Ok(Err(err)) => {
                m::record_index_build_failure();
                tracing::error!(error = %format!("{err:#}"), "index load failed, will retry");
            }
            Err(join_err) => {
                m::record_index_build_failure();
                tracing::error!(error = %join_err, "index load task panicked, will retry");
            }
        }
    }
}
