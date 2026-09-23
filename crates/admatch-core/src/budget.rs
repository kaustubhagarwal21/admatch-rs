//! Per-campaign daily budgets.
//!
//! [`BudgetStore`] is an enum over the budget backends rather than a trait.
//! With a small, fixed set of backends (in-memory now, Redis later) an enum
//! keeps dispatch static and simple: no `Box<dyn Trait>`, no generic
//! parameter spreading through the server, and `match` shows every backend
//! in one place.
//!
//! # Zero overspend in one process
//!
//! Each (campaign, day) has one `AtomicI64` holding what has been spent.
//! "Is there room for this amount? If so, add it" must be one indivisible
//! step, otherwise two threads could both see room for the last slice of
//! budget and both spend it. [`AtomicI64::fetch_update`] gives exactly that:
//! it is a compare-and-swap (CAS) loop that reads the current value, computes
//! the new one, and stores it only if nobody changed the value in between;
//! if someone did, it re-reads and tries again.
//!
//! # Memory ordering: `Relaxed`
//!
//! Atomicity, not ordering, is what prevents overspend. Every read-modify-
//! write on one atomic variable takes part in a single total order (its
//! "modification order"), and a successful CAS always acts on the latest
//! value in that order. So two CAS operations can never both succeed from
//! the same old value, whatever ordering is used. Stronger orderings
//! (`Acquire`/`Release`/`SeqCst`) only order *other* memory accesses around
//! the atomic, and this counter publishes no other data: it is the only
//! thing we read. `Relaxed` is therefore sufficient and the cheapest. The
//! map entry holding the counter is created under DashMap's shard lock,
//! which already synchronises its creation between threads.

use std::sync::atomic::{AtomicI64, Ordering};

use chrono::NaiveDate;
use dashmap::DashMap;
use thiserror::Error;

use crate::model::{CampaignId, Micros};

/// Why a budget operation failed (as opposed to "not enough budget left",
/// which is a normal outcome, not an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BudgetError {
    /// Spend amounts must be positive; a zero or negative amount would
    /// silently refund or no-op, so it is refused.
    #[error("spend amount must be positive, got {0:?}")]
    InvalidAmount(Micros),
}

/// In-process budget counters: one atomic per (campaign, UTC day).
///
/// `DashMap` is a hash map split into shards, each behind its own lock, so
/// threads touching different campaigns rarely wait for each other. The
/// lock is held only to find (or create) the counter; the spend itself is a
/// lock-free CAS on the counter.
#[derive(Debug, Default)]
pub struct MemoryBudgets {
    spent: DashMap<(CampaignId, NaiveDate), AtomicI64>,
}

impl MemoryBudgets {
    /// Atomically adds `amount` to the day's spend if the total stays within
    /// `daily_budget`. Returns `Ok(true)` if spent, `Ok(false)` if it would
    /// overspend (nothing is changed then).
    pub fn try_spend(
        &self,
        campaign: CampaignId,
        day: NaiveDate,
        amount: Micros,
        daily_budget: Micros,
    ) -> Result<bool, BudgetError> {
        if amount.0 <= 0 {
            return Err(BudgetError::InvalidAmount(amount));
        }
        let key = (campaign, day);
        // Fast path: the counter usually exists, and `get` takes only a
        // shared (read) lock on its shard.
        if let Some(counter) = self.spent.get(&key) {
            return Ok(Self::add_within(&counter, amount, daily_budget));
        }
        // First spend of the day for this campaign: create the counter.
        // `entry` takes the shard's write lock, so two threads racing here
        // still end up sharing one counter.
        let counter = self.spent.entry(key).or_default();
        Ok(Self::add_within(&counter, amount, daily_budget))
    }

    /// The CAS loop. `checked_add` returning `None` (overflow) or a total
    /// above the budget makes the closure return `None`, which makes
    /// `fetch_update` give up without writing.
    fn add_within(counter: &AtomicI64, amount: Micros, daily_budget: Micros) -> bool {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |spent| {
                spent
                    .checked_add(amount.0)
                    .filter(|&total| total <= daily_budget.0)
            })
            .is_ok()
    }

    /// Total spent by `campaign` on `day` (zero if nothing yet).
    pub fn spent(&self, campaign: CampaignId, day: NaiveDate) -> Micros {
        Micros(
            self.spent
                .get(&(campaign, day))
                .map_or(0, |c| c.load(Ordering::Relaxed)),
        )
    }

    /// Forgets every counter for days before `today`. Called during the
    /// periodic index rebuild so the map does not grow forever.
    pub fn drop_days_before(&self, today: NaiveDate) {
        self.spent.retain(|(_, day), _| *day >= today);
    }
}

/// The budget backend the auction charges winners against.
#[derive(Debug)]
pub enum BudgetStore {
    /// Counters in this process's memory. Correct for a single server.
    Memory(MemoryBudgets),
}

impl BudgetStore {
    /// A fresh in-memory store with nothing spent yet.
    pub fn in_memory() -> Self {
        BudgetStore::Memory(MemoryBudgets::default())
    }

    /// Spends `amount` for `campaign` on `day` if it fits in `daily_budget`.
    /// `Ok(false)` means "not enough budget left", a normal outcome.
    pub fn try_spend(
        &self,
        campaign: CampaignId,
        day: NaiveDate,
        amount: Micros,
        daily_budget: Micros,
    ) -> Result<bool, BudgetError> {
        match self {
            BudgetStore::Memory(m) => m.try_spend(campaign, day, amount, daily_budget),
        }
    }

    /// Total spent by `campaign` on `day`.
    pub fn spent(&self, campaign: CampaignId, day: NaiveDate) -> Micros {
        match self {
            BudgetStore::Memory(m) => m.spent(campaign, day),
        }
    }

    /// Drops counters for days before `today` (no-op for stores that expire
    /// keys by themselves).
    pub fn drop_days_before(&self, today: NaiveDate) {
        match self {
            BudgetStore::Memory(m) => m.drop_days_before(today),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn spends_up_to_the_budget_exactly_and_no_further() {
        let store = BudgetStore::in_memory();
        let c = CampaignId(1);
        assert_eq!(
            store.try_spend(c, day(1), Micros(600), Micros(1_000)),
            Ok(true)
        );
        assert_eq!(
            store.try_spend(c, day(1), Micros(401), Micros(1_000)),
            Ok(false)
        );
        assert_eq!(
            store.try_spend(c, day(1), Micros(400), Micros(1_000)),
            Ok(true)
        );
        assert_eq!(store.spent(c, day(1)), Micros(1_000));
        assert_eq!(
            store.try_spend(c, day(1), Micros(1), Micros(1_000)),
            Ok(false)
        );
    }

    #[test]
    fn days_and_campaigns_are_independent() {
        let store = BudgetStore::in_memory();
        assert_eq!(
            store.try_spend(CampaignId(1), day(1), Micros(10), Micros(10)),
            Ok(true)
        );
        assert_eq!(
            store.try_spend(CampaignId(1), day(2), Micros(10), Micros(10)),
            Ok(true)
        );
        assert_eq!(
            store.try_spend(CampaignId(2), day(1), Micros(10), Micros(10)),
            Ok(true)
        );
        assert_eq!(store.spent(CampaignId(3), day(1)), Micros(0));
    }

    #[test]
    fn rejects_non_positive_amounts_and_survives_overflow() {
        let store = BudgetStore::in_memory();
        let c = CampaignId(1);
        assert_eq!(
            store.try_spend(c, day(1), Micros(0), Micros(10)),
            Err(BudgetError::InvalidAmount(Micros(0)))
        );
        assert!(store.try_spend(c, day(1), Micros(-5), Micros(10)).is_err());
        assert_eq!(
            store.try_spend(c, day(1), Micros(i64::MAX), Micros(i64::MAX)),
            Ok(true)
        );
        // spent + 1 would overflow i64: refused, not wrapped around.
        assert_eq!(
            store.try_spend(c, day(1), Micros(1), Micros(i64::MAX)),
            Ok(false)
        );
        assert_eq!(store.spent(c, day(1)), Micros(i64::MAX));
    }

    #[test]
    fn zero_budget_never_spends() {
        let store = BudgetStore::in_memory();
        assert_eq!(
            store.try_spend(CampaignId(1), day(1), Micros(1), Micros(0)),
            Ok(false)
        );
    }

    #[test]
    fn drop_days_before_keeps_today() {
        let store = BudgetStore::in_memory();
        let c = CampaignId(1);
        store.try_spend(c, day(1), Micros(5), Micros(10)).unwrap();
        store.try_spend(c, day(2), Micros(7), Micros(10)).unwrap();
        store.drop_days_before(day(2));
        assert_eq!(store.spent(c, day(1)), Micros(0));
        assert_eq!(store.spent(c, day(2)), Micros(7));
    }

    /// 8 threads × 100,000 spends of 7 against a budget of 1,000,000:
    /// exactly floor(1_000_000 / 7) = 142,857 succeed, spending 999,999.
    /// Any lost update or double spend would change these numbers.
    #[test]
    fn concurrent_spends_never_overspend() {
        const THREADS: usize = 8;
        const CALLS: usize = 100_000;
        let store = BudgetStore::in_memory();
        let c = CampaignId(42);
        let successes: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    s.spawn(|| {
                        (0..CALLS)
                            .filter(|_| {
                                store
                                    .try_spend(c, day(1), Micros(7), Micros(1_000_000))
                                    .unwrap()
                            })
                            .count()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(successes, 142_857);
        assert_eq!(store.spent(c, day(1)), Micros(999_999));
    }
}
