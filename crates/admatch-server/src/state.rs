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
use chrono::NaiveDate;
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
///
/// Every tick also drops budget counters for old days (see
/// `prune_budget_days`).
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
        // Runs whether or not the load succeeded: a broken seed file must
        // not stop old budget counters from being cleaned up.
        prune_budget_days(state.budgets(), chrono::Utc::now().date_naive());
    }
}

/// Drops budget counters for days before YESTERDAY (UTC), so the in-memory
/// map holds at most two days per campaign instead of growing forever.
///
/// Why keep yesterday and not only today: a request can read the date just
/// before UTC midnight (day D) and call `try_spend(D)` a moment after a
/// prune that already saw day D+1. If D's counter had been removed, the
/// store would recreate it at zero and let the campaign spend its whole
/// budget on D a second time. One day of slack is far longer than any
/// request, so this cannot happen.
///
/// `today` is a parameter (not read from the clock here) so tests can pass
/// a fixed date.
fn prune_budget_days(budgets: &BudgetStore, today: NaiveDate) {
    // `pred_opt` is only `None` for the earliest date chrono can represent;
    // falling back to `today` there simply keeps a little more.
    budgets.drop_days_before(today.pred_opt().unwrap_or(today));
}

#[cfg(test)]
mod tests {
    use super::*;
    use admatch_core::model::{CampaignId, Micros};

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    /// Regression: the index-refresh loop never pruned old budget days, so
    /// the map kept one entry per (campaign, day) for the life of the
    /// process.
    #[test]
    fn prune_keeps_today_and_yesterday_and_drops_older_days() {
        let budgets = BudgetStore::in_memory();
        let campaign = CampaignId(1);
        let budget = Micros(1_000_000);
        for d in [20, 21, 22, 23] {
            assert!(
                budgets
                    .try_spend(campaign, day(d), Micros(100_000), budget)
                    .unwrap()
            );
        }

        prune_budget_days(&budgets, day(23));

        assert_eq!(budgets.spent(campaign, day(20)), Micros(0));
        assert_eq!(budgets.spent(campaign, day(21)), Micros(0));
        assert_eq!(budgets.spent(campaign, day(22)), Micros(100_000));
        assert_eq!(budgets.spent(campaign, day(23)), Micros(100_000));
    }

    /// The midnight race the one-day slack guards against: a late request
    /// for yesterday still sees yesterday's spend after the prune, so it
    /// cannot overspend that day.
    #[test]
    fn a_late_spend_for_yesterday_still_respects_yesterdays_budget() {
        let budgets = BudgetStore::in_memory();
        let campaign = CampaignId(1);
        let budget = Micros(1_000_000);
        assert!(
            budgets
                .try_spend(campaign, day(22), budget, budget)
                .unwrap()
        );

        prune_budget_days(&budgets, day(23));

        assert!(
            !budgets
                .try_spend(campaign, day(22), Micros(100_000), budget)
                .unwrap()
        );
    }
}
