//! AdMatch HTTP service.
//!
//! The code lives in a library, with thin binaries on top (`main.rs` for the
//! server, `bin/seed.rs` for the data generator), so the API tests in
//! `tests/` can build the real router and call it in-process.
//!
//! Module map:
//!
//! * [`config`]: environment-variable settings, validated at startup.
//! * [`state`]: shared handler state and the index reload task.
//! * [`routes`]: the axum router, handlers and middleware.
//! * [`error`]: the JSON error format.
//! * [`metrics`]: Prometheus metric names and recording helpers.
//! * [`seed_file`]: the seed JSON format shared by the server and `seed`.
//! * [`seedgen`]: deterministic synthetic campaigns and requests.
//! * [`validate`]: campaign validation, including the privacy threshold.

pub mod config;
pub mod error;
pub mod metrics;
pub mod routes;
pub mod seed_file;
pub mod seedgen;
pub mod state;
pub mod validate;
