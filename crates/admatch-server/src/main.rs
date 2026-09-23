//! AdMatch server binary.
//!
//! For now this only starts the async runtime and structured logging, so the
//! binary, the build and CI exist end to end. The HTTP service (routes,
//! configuration, graceful shutdown) is built on top of this entry point in a
//! later milestone.

use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Log level used when `RUST_LOG` is unset, empty or contains no valid
/// directive. `info` shows startup and lifecycle events without the noise of
/// per-request debug output.
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::INFO;

/// Entry point.
///
/// `#[tokio::main]` wraps this `async fn` in a synchronous `main` that builds
/// a multi-threaded tokio runtime and blocks on it: roughly a thread pool that
/// runs lightweight tasks instead of one OS thread per request.
#[tokio::main]
async fn main() {
    init_tracing();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "admatch-server starting"
    );
    tracing::info!("no HTTP listener yet (skeleton build), exiting");
}

/// Installs the global logger, filtered by the `RUST_LOG` environment
/// variable.
fn init_tracing() {
    // A missing or non-UTF-8 RUST_LOG is treated like an empty one, which
    // selects the default level.
    let raw = std::env::var("RUST_LOG").unwrap_or_default();
    // `init()` panics only if a global logger is already installed. This is
    // the first thing `main` does and nothing else installs one, so it
    // cannot fail here.
    tracing_subscriber::fmt()
        .with_env_filter(log_filter(&raw))
        .init();
}

/// Builds the log filter from a raw `RUST_LOG` value.
///
/// Invalid directives are skipped (tracing-subscriber reports each one on
/// stderr) instead of aborting startup, so a typo in `RUST_LOG` can never stop
/// the server from starting. If nothing valid remains, [`DEFAULT_LOG_LEVEL`]
/// applies.
///
/// It takes the value as a parameter rather than reading the environment
/// itself so tests can call it directly: changing environment variables at
/// runtime (`std::env::set_var`) is `unsafe` in edition 2024, because other
/// threads may be reading them at the same time.
fn log_filter(raw: &str) -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(DEFAULT_LOG_LEVEL.into())
        .parse_lossy(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rust_log_falls_back_to_info() {
        assert_eq!(log_filter("").max_level_hint(), Some(LevelFilter::INFO));
    }

    #[test]
    fn valid_rust_log_is_respected() {
        assert_eq!(
            log_filter("debug").max_level_hint(),
            Some(LevelFilter::DEBUG)
        );
    }

    #[test]
    fn invalid_rust_log_falls_back_to_info() {
        assert_eq!(
            log_filter("admatch=not_a_level").max_level_hint(),
            Some(LevelFilter::INFO)
        );
    }
}
