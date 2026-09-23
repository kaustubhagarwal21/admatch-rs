//! Per-campaign daily budgets.
//!
//! [`BudgetStore`] is an enum over the budget backends rather than a trait.
//! With a small, fixed set of backends (in-memory now, Redis later) an enum
//! keeps dispatch static and simple: no `Box<dyn Trait>`, no generic
//! parameter spreading through the server, and `match` shows every backend
//! in one place.
//!
//! This milestone only lays down the types. The spend logic (`try_spend`, an
//! atomic check-and-add per campaign and day) arrives with the auction.

use thiserror::Error;

use crate::model::Micros;

/// Why a budget operation failed (as opposed to "not enough budget left",
/// which is a normal outcome, not an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BudgetError {
    /// Spend amounts must be positive; a zero or negative amount would
    /// silently refund or no-op, so it is refused.
    #[error("spend amount must be positive, got {0:?}")]
    InvalidAmount(Micros),
}

/// In-process budget counters. Filled in with the auction milestone.
#[derive(Debug, Default)]
pub struct MemoryBudgets {
    /// Private field so the struct can only be built through
    /// [`BudgetStore::in_memory`] and its internals can change freely.
    _private: (),
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
}
