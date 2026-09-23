//! Server configuration, read from environment variables.
//!
//! Every setting has a default (see `.env.example`), so the server starts
//! with no configuration at all. Values are parsed and checked once, at
//! startup: a typo such as `RESERVE_CPT_MICROS=abc` stops the process with a
//! clear message instead of surfacing later as odd auction prices.
//!
//! Parsing takes a lookup function rather than reading `std::env` directly,
//! so tests can pass a plain map. Changing real environment variables from a
//! test (`std::env::set_var`) is `unsafe` in edition 2024, because other test
//! threads may be reading them at the same time.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use admatch_core::engine::EngineConfig;
use admatch_core::model::Micros;
use thiserror::Error;

/// Default listen address: all interfaces, port 8080.
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
/// Default campaign seed file, written by the `seed` binary.
pub const DEFAULT_SEED_FILE: &str = "data/seed.json";
/// Default interval between index reloads, in seconds.
pub const DEFAULT_INDEX_REFRESH_SECS: u64 = 30;

/// Where campaigns are loaded from.
///
/// Only `file` exists so far. It is still an enum (not a bool or a bare
/// path) so adding `postgres` later is a new variant that the compiler
/// forces every `match` to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignSource {
    /// Read campaigns from a JSON seed file.
    File(PathBuf),
}

/// Validated server settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Address the HTTP listener binds to (`BIND_ADDR`).
    pub bind_addr: SocketAddr,
    /// Where campaigns come from (`CAMPAIGN_SOURCE` plus `SEED_FILE`).
    pub campaign_source: CampaignSource,
    /// Privacy threshold, reserve price and price increment for the auction
    /// (`K_TARGETING`, `RESERVE_CPT_MICROS`, `PRICE_INCREMENT_MICROS`).
    pub engine: EngineConfig,
    /// How often the index is rebuilt from the source (`INDEX_REFRESH_SECS`).
    pub index_refresh: Duration,
}

/// A setting that could not be used. The message names the variable, so the
/// operator knows exactly what to fix.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// The value does not parse as the expected type.
    #[error("{var}={value:?} is not a valid {expected}")]
    Invalid {
        /// Environment variable name.
        var: &'static str,
        /// The raw value that was rejected.
        value: String,
        /// What was expected, in words.
        expected: &'static str,
    },
    /// The value parses but is outside the allowed range.
    #[error("{var}={value} is out of range: {rule}")]
    OutOfRange {
        /// Environment variable name.
        var: &'static str,
        /// The parsed value.
        value: i64,
        /// The rule it breaks, in words.
        rule: &'static str,
    },
    /// `CAMPAIGN_SOURCE` names a source this build does not support.
    #[error("CAMPAIGN_SOURCE={0:?} is not supported yet (only \"file\")")]
    UnsupportedSource(String),
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Config, ConfigError> {
        Config::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup`, which returns a variable's
    /// value or `None` when it is unset. Empty values count as unset, so
    /// `RESERVE_CPT_MICROS=` in a `.env` file means "use the default".
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());

        let bind_addr = parse_or(
            get("BIND_ADDR"),
            "BIND_ADDR",
            DEFAULT_BIND_ADDR,
            "socket address (host:port)",
        )?;

        let source = get("CAMPAIGN_SOURCE").unwrap_or_else(|| "file".to_owned());
        let campaign_source = match source.as_str() {
            "file" => CampaignSource::File(PathBuf::from(
                get("SEED_FILE").unwrap_or_else(|| DEFAULT_SEED_FILE.to_owned()),
            )),
            _ => return Err(ConfigError::UnsupportedSource(source)),
        };

        let defaults = EngineConfig::default();
        let k_targeting: i64 = parse_or(
            get("K_TARGETING"),
            "K_TARGETING",
            &defaults.k_targeting.to_string(),
            "integer",
        )?;
        let reserve: i64 = parse_or(
            get("RESERVE_CPT_MICROS"),
            "RESERVE_CPT_MICROS",
            &defaults.reserve.0.to_string(),
            "integer number of micros",
        )?;
        let increment: i64 = parse_or(
            get("PRICE_INCREMENT_MICROS"),
            "PRICE_INCREMENT_MICROS",
            &defaults.increment.0.to_string(),
            "integer number of micros",
        )?;
        let refresh_secs: i64 = parse_or(
            get("INDEX_REFRESH_SECS"),
            "INDEX_REFRESH_SECS",
            &DEFAULT_INDEX_REFRESH_SECS.to_string(),
            "integer number of seconds",
        )?;

        // Range checks. A negative threshold would let every audience pass,
        // a zero reserve would let ads serve for free, and a zero refresh
        // interval would rebuild the index in a busy loop.
        ensure(k_targeting >= 0, "K_TARGETING", k_targeting, "must be >= 0")?;
        ensure(reserve > 0, "RESERVE_CPT_MICROS", reserve, "must be > 0")?;
        ensure(
            increment >= 0,
            "PRICE_INCREMENT_MICROS",
            increment,
            "must be >= 0",
        )?;
        ensure(
            (1..=86_400).contains(&refresh_secs),
            "INDEX_REFRESH_SECS",
            refresh_secs,
            "must be between 1 and 86400",
        )?;

        Ok(Config {
            bind_addr,
            campaign_source,
            engine: EngineConfig {
                k_targeting,
                reserve: Micros(reserve),
                increment: Micros(increment),
            },
            // `unsigned_abs` cannot lose information: the range check above
            // guarantees the value is positive.
            index_refresh: Duration::from_secs(refresh_secs.unsigned_abs()),
        })
    }
}

/// Parses `value`, or `default` when the variable is unset.
fn parse_or<T: FromStr>(
    value: Option<String>,
    var: &'static str,
    default: &str,
    expected: &'static str,
) -> Result<T, ConfigError> {
    let raw = value.unwrap_or_else(|| default.to_owned());
    raw.trim().parse().map_err(|_| ConfigError::Invalid {
        var,
        value: raw.clone(),
        expected,
    })
}

/// Returns an out-of-range error unless `ok` holds.
fn ensure(ok: bool, var: &'static str, value: i64, rule: &'static str) -> Result<(), ConfigError> {
    if ok {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange { var, value, rule })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let cfg = config(&[]).unwrap();
        assert_eq!(cfg.bind_addr, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(
            cfg.campaign_source,
            CampaignSource::File(PathBuf::from("data/seed.json"))
        );
        assert_eq!(cfg.engine, EngineConfig::default());
        assert_eq!(cfg.index_refresh, Duration::from_secs(30));
    }

    #[test]
    fn values_are_read_and_empty_means_default() {
        let cfg = config(&[
            ("BIND_ADDR", "127.0.0.1:9000"),
            ("SEED_FILE", "/tmp/s.json"),
            ("K_TARGETING", "100"),
            ("RESERVE_CPT_MICROS", "50000"),
            ("PRICE_INCREMENT_MICROS", ""),
            ("INDEX_REFRESH_SECS", "5"),
        ])
        .unwrap();
        assert_eq!(cfg.bind_addr, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(
            cfg.campaign_source,
            CampaignSource::File(PathBuf::from("/tmp/s.json"))
        );
        assert_eq!(cfg.engine.k_targeting, 100);
        assert_eq!(cfg.engine.reserve, Micros(50_000));
        assert_eq!(cfg.engine.increment, Micros(10_000));
        assert_eq!(cfg.index_refresh, Duration::from_secs(5));
    }

    #[test]
    fn rejects_unparsable_values() {
        assert!(matches!(
            config(&[("RESERVE_CPT_MICROS", "abc")]),
            Err(ConfigError::Invalid {
                var: "RESERVE_CPT_MICROS",
                ..
            })
        ));
        assert!(matches!(
            config(&[("BIND_ADDR", "localhost")]),
            Err(ConfigError::Invalid {
                var: "BIND_ADDR",
                ..
            })
        ));
    }

    #[test]
    fn rejects_out_of_range_values() {
        for (var, value) in [
            ("K_TARGETING", "-1"),
            ("RESERVE_CPT_MICROS", "0"),
            ("PRICE_INCREMENT_MICROS", "-5"),
            ("INDEX_REFRESH_SECS", "0"),
        ] {
            assert!(
                matches!(config(&[(var, value)]), Err(ConfigError::OutOfRange { .. })),
                "{var}={value} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_unknown_campaign_source() {
        assert_eq!(
            config(&[("CAMPAIGN_SOURCE", "postgres")]),
            Err(ConfigError::UnsupportedSource("postgres".to_owned()))
        );
    }
}
