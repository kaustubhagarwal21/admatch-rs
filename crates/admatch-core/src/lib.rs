//! Core decision logic for AdMatch.
//!
//! This crate holds everything that decides *which* ad is shown and *what it
//! costs*: query normalisation, the keyword index, relevance scoring, the
//! auction, budget accounting and the privacy rules.
//!
//! It deliberately performs no I/O: no network, no database, no files and no
//! clock reads. Callers pass in everything the logic depends on (for example
//! the current day for budgets). Two reasons:
//!
//! * The logic can be unit- and property-tested as plain function calls,
//!   without mocks, containers or timing flakiness.
//! * The dependency arrow points one way: `admatch-server` depends on this
//!   crate, never the reverse, so HTTP or database concerns cannot leak into
//!   the rules that decide money.
//!
//! Module map:
//!
//! * [`model`]: the shared vocabulary (IDs, money, campaigns, keywords).
//! * [`normalize`]: turns a raw search query into tokens.
//! * [`privacy`]: the audience-size rule for personalised targeting.
//! * [`budget`]: per-campaign daily spend tracking.
//! * [`engine`]: the index snapshot and the auction that picks one ad.

pub mod budget;
pub mod engine;
pub mod model;
pub mod normalize;
pub mod privacy;
