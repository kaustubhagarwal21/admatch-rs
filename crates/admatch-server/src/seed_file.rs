//! The JSON seed file: the campaign source while `CAMPAIGN_SOURCE=file`.
//!
//! The `seed` binary writes it and the server reads it, and both use the
//! types in this module, so the writer and the reader cannot drift apart.

use std::path::Path;

use admatch_core::model::{AgeBucket, Campaign, Country};
use admatch_core::privacy::AudienceCounts;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Contents of `data/seed.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedFile {
    /// The RNG seed the file was generated from, so it can be reproduced.
    pub seed: u64,
    /// Campaigns that passed validation (rejected ones are left out, just as
    /// the admin API would refuse them).
    pub campaigns: Vec<Campaign>,
    /// The synthetic population per (country, age bucket). The server does
    /// not need it yet; it is loaded into Postgres in a later milestone.
    pub audience_counts: Vec<AudienceCell>,
}

/// One row of the audience table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudienceCell {
    /// Storefront country.
    pub country: Country,
    /// Age bucket.
    pub age_bucket: AgeBucket,
    /// Number of synthetic users in this cell.
    pub users: i64,
}

impl SeedFile {
    /// The audience rows as the lookup table the privacy rules use.
    pub fn audience_table(&self) -> AudienceCounts {
        let mut table = AudienceCounts::new();
        for cell in &self.audience_counts {
            table.insert(cell.country, cell.age_bucket, cell.users);
        }
        table
    }
}

/// Why the seed file could not be loaded.
///
/// The messages leave out the underlying error on purpose: it is exposed as
/// the error's `source`, and the loader logs the whole chain with `{:#}`.
/// Repeating it in the message would print it twice.
#[derive(Debug, Error)]
pub enum SeedFileError {
    /// The file could not be read (missing, no permission, ...).
    #[error("cannot read seed file {path} (run `make seed` to create it)")]
    Read {
        /// The path that was tried.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file is not valid seed JSON.
    #[error("seed file {path} is not valid seed JSON")]
    Parse {
        /// The path that was tried.
        path: String,
        /// The underlying JSON error, with line and column.
        source: serde_json::Error,
    },
}

/// Reads and parses a seed file. This is blocking file I/O, so async code
/// calls it through `tokio::task::spawn_blocking`.
pub fn read_seed_file(path: &Path) -> Result<SeedFile, SeedFileError> {
    let display = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|source| SeedFileError::Read {
        path: display.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| SeedFileError::Parse {
        path: display,
        source,
    })
}
