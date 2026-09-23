//! Privacy rule for personalised (age-targeted) campaigns.
//!
//! A campaign may target age buckets only if **more than** `k_targeting`
//! people (default 5,000) fall inside its targeting. The rule is modelled on
//! Apple's public advertising privacy wording; the implementation is our own.
//! It stops an advertiser from aiming an ad at a group so small that serving
//! it would single people out.

use std::collections::{BTreeSet, HashMap};

use thiserror::Error;

use crate::model::{AgeBucket, Country};

/// Why a targeting setup was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PrivacyError {
    /// The audience is at or below the threshold. Deliberately carries no
    /// count: telling the advertiser the exact size would itself leak how
    /// many people are in a small group.
    #[error("audience too small for personalised targeting")]
    AudienceTooSmall,
}

/// Number of (synthetic) users in each (country, age bucket) cell.
///
/// A thin wrapper instead of a bare `HashMap`, so callers use a small,
/// named API and the storage can change without touching them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudienceCounts {
    cells: HashMap<(Country, AgeBucket), i64>,
}

impl AudienceCounts {
    /// Creates an empty table (every cell counts as 0 users).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the user count for one cell, replacing any previous value.
    pub fn insert(&mut self, country: Country, bucket: AgeBucket, users: i64) {
        self.cells.insert((country, bucket), users);
    }

    /// The user count for one cell, or `None` if it was never set.
    pub fn get(&self, country: Country, bucket: AgeBucket) -> Option<i64> {
        self.cells.get(&(country, bucket)).copied()
    }
}

/// Total users across every (country, bucket) pair a campaign targets.
///
/// Duplicates in either list are ignored. Counting a cell twice would
/// inflate the audience and could let a too-narrow campaign pass the
/// threshold, so de-duplicating errs on the safe side. Missing cells count
/// as 0 for the same reason. Addition saturates instead of overflowing.
pub fn audience_size(counts: &AudienceCounts, countries: &[Country], buckets: &[AgeBucket]) -> i64 {
    let countries: BTreeSet<Country> = countries.iter().copied().collect();
    let buckets: BTreeSet<AgeBucket> = buckets.iter().copied().collect();

    let mut total: i64 = 0;
    for &country in &countries {
        for &bucket in &buckets {
            let users = counts.get(country, bucket).unwrap_or(0);
            total = total.saturating_add(users);
        }
    }
    total
}

/// Checks the targeting threshold: allowed only when `size > k_targeting`.
///
/// Strictly greater, because the public rule says "more than 5,000 people";
/// an audience of exactly 5,000 is refused.
pub fn check_targeting(size: i64, k_targeting: i64) -> Result<(), PrivacyError> {
    if size > k_targeting {
        Ok(())
    } else {
        Err(PrivacyError::AudienceTooSmall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_strictly_greater_than_k() {
        assert_eq!(check_targeting(5_001, 5_000), Ok(()));
        assert_eq!(
            check_targeting(5_000, 5_000),
            Err(PrivacyError::AudienceTooSmall)
        );
        assert_eq!(
            check_targeting(0, 5_000),
            Err(PrivacyError::AudienceTooSmall)
        );
    }

    #[test]
    fn audience_size_sums_targeted_cells_once() {
        let mut counts = AudienceCounts::new();
        counts.insert(Country::IN, AgeBucket::Age18To24, 3_000);
        counts.insert(Country::IN, AgeBucket::Age25To34, 2_500);
        counts.insert(Country::US, AgeBucket::Age18To24, 10_000);

        let size = audience_size(
            &counts,
            &[Country::IN, Country::IN],
            &[
                AgeBucket::Age18To24,
                AgeBucket::Age25To34,
                AgeBucket::Age65Plus,
            ],
        );
        // US is not targeted; the duplicate IN and the missing 65+ cell add 0.
        assert_eq!(size, 5_500);
    }
}
