//! AdMatch server binary: reads the configuration, starts the index loader
//! and serves HTTP until ctrl-c or SIGTERM.

use admatch_server::config::Config;
use admatch_server::routes::router;
use admatch_server::state::{AppState, keep_index_fresh};
use anyhow::Context;
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
///
/// Returning `anyhow::Result` means a startup error (bad configuration, port
/// already in use) is printed with its context and the process exits with a
/// non-zero status.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cfg = Config::from_env().context("invalid configuration")?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        bind_addr = %cfg.bind_addr,
        "admatch-server starting"
    );

    // Install the Prometheus recorder globally, so every counter!/histogram!
    // call in the process is recorded, and keep a handle to render it.
    let recorder =
        admatch_server::metrics::build_recorder().context("invalid metrics configuration")?;
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).context("a metrics recorder was already installed")?;

    let state = AppState::new(cfg.engine, handle);

    // The listener starts before the index is loaded: until the first load
    // finishes, /healthz answers 503 and /v1/match answers not_ready.
    tokio::spawn(keep_index_fresh(
        state.clone(),
        cfg.campaign_source.clone(),
        cfg.index_refresh,
    ));

    let listener = tokio::net::TcpListener::bind(cfg.bind_addr)
        .await
        .with_context(|| format!("cannot listen on {}", cfg.bind_addr))?;
    tracing::info!(addr = %cfg.bind_addr, "listening");

    // Graceful shutdown: on a signal, stop accepting new connections and
    // let in-flight requests finish before returning.
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    tracing::info!("shut down cleanly");
    Ok(())
}

/// Completes when the process receives ctrl-c (SIGINT) or, on Unix, SIGTERM
/// (what `docker stop` and Kubernetes send).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "cannot listen for ctrl-c");
            // Without a working handler, never trigger shutdown from here.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // Whichever signal arrives first wins.
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
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
