//! The matching engine: an immutable [`Snapshot`] of all campaigns plus the
//! auction that picks at most one ad per query.
//!
//! A snapshot is built once and then only read. The server shares it between
//! request handlers and swaps in a whole new one when campaigns change, so
//! readers never wait on a lock.
//!
//! This milestone fixes the public API only: `build` stores the campaigns and
//! `run` validates the query and returns "no ad". The index and the auction
//! replace the internals later without changing these signatures.

use std::collections::HashSet;

use chrono::NaiveDate;
use serde::Serialize;
use thiserror::Error;

use crate::budget::BudgetStore;
use crate::index::{KeywordIndex, KeywordMatch};
use crate::model::{
    AgeBucket, Campaign, CampaignId, Country, KeywordId, MatchType, Micros, RelevanceBp,
};
use crate::normalize::{QueryError, normalize};

/// Tunable auction and privacy parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Age-targeted campaigns serve only if their audience is strictly larger
    /// than this.
    pub k_targeting: i64,
    /// Minimum price of a tap; bids below it never enter the auction.
    pub reserve: Micros,
    /// Added to the second-price so the winner pays slightly more than the
    /// runner-up's equivalent bid.
    pub increment: Micros,
}

impl Default for EngineConfig {
    /// The defaults from the configuration table: 5,000 people,
    /// 0.10 reserve, 0.01 increment.
    fn default() -> Self {
        Self {
            k_targeting: 5_000,
            reserve: Micros(100_000),
            increment: Micros(10_000),
        }
    }
}

/// Why a snapshot could not be built from the given campaigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BuildError {
    /// Two campaigns share an ID, so results would be ambiguous.
    #[error("duplicate campaign id {0:?}")]
    DuplicateCampaign(CampaignId),
    /// Two keywords share an ID, so a winning keyword could not be
    /// reported unambiguously.
    #[error("duplicate keyword id {0:?}")]
    DuplicateKeyword(KeywordId),
    /// More distinct tokens than a `u32` token id can number.
    #[error("too many distinct keyword tokens to intern")]
    VocabularyFull,
}

/// An immutable, query-ready view of every campaign.
#[derive(Debug, Clone)]
pub struct Snapshot {
    campaigns: Vec<Campaign>,
    index: KeywordIndex,
    cfg: EngineConfig,
}

impl Snapshot {
    /// Builds a snapshot, rejecting duplicate campaign or keyword IDs.
    pub fn build(campaigns: Vec<Campaign>, cfg: EngineConfig) -> Result<Snapshot, BuildError> {
        let mut campaign_ids = HashSet::new();
        let mut keyword_ids = HashSet::new();
        for campaign in &campaigns {
            // `insert` returns false when the value was already present.
            if !campaign_ids.insert(campaign.id) {
                return Err(BuildError::DuplicateCampaign(campaign.id));
            }
            for keyword in &campaign.keywords {
                if !keyword_ids.insert(keyword.id) {
                    return Err(BuildError::DuplicateKeyword(keyword.id));
                }
            }
        }
        let index = KeywordIndex::build(&campaigns).map_err(|_| BuildError::VocabularyFull)?;
        Ok(Snapshot {
            campaigns,
            index,
            cfg,
        })
    }

    /// Campaigns whose keywords match `query`, each with its single best
    /// keyword, after negative keywords are applied. Sorted by the
    /// campaign's position in the snapshot. Targeting and bids are not
    /// checked here; that is the auction's job.
    pub fn matches(&self, query: &str) -> Result<Vec<KeywordMatch>, QueryError> {
        let tokens = normalize(query)?;
        let prepared = self.index.prepare(&tokens);
        let mut matches = self.index.best_matches(&prepared);
        matches.retain(|m| !self.index.negative_matches(m.campaign_pos, &prepared));
        Ok(matches)
    }

    /// Number of campaigns in the snapshot (reported by `/metrics`).
    pub fn campaign_count(&self) -> usize {
        self.campaigns.len()
    }

    /// Number of keywords across all campaigns (reported by `/metrics`).
    pub fn keyword_count(&self) -> usize {
        self.campaigns.iter().map(|c| c.keywords.len()).sum()
    }

    /// The configuration this snapshot was built with.
    pub fn config(&self) -> EngineConfig {
        self.cfg
    }

    /// Runs one auction for a search request.
    ///
    /// `day` is the current UTC date, passed in (not read from a clock) so
    /// the core stays deterministic and testable. An invalid query is an
    /// error; "no ad" is a normal outcome with `winner: None`.
    pub fn run(
        &self,
        req: &AuctionRequest<'_>,
        _budgets: &BudgetStore,
        _day: NaiveDate,
    ) -> Result<AuctionOutcome, QueryError> {
        // Validate now so the error contract is real from day one; the
        // tokens feed the index once it exists.
        let _tokens = normalize(req.query)?;
        Ok(AuctionOutcome {
            winner: None,
            candidates: 0,
            ranking: req.debug.then(Vec::new),
        })
    }
}

/// One search request, as the auction sees it.
///
/// Borrows the query (`&'a str`) instead of owning a `String`: the HTTP
/// layer already holds the text, so the hot path copies nothing.
#[derive(Debug, Clone, Copy)]
pub struct AuctionRequest<'a> {
    /// Raw query text; normalised inside `run`.
    pub query: &'a str,
    /// The storefront the search came from.
    pub country: Country,
    /// Whether the user allows personalised ads. When false, age-targeted
    /// campaigns cannot serve.
    pub personalized: bool,
    /// The user's age bucket; ignored unless `personalized` is true.
    pub age_bucket: Option<AgeBucket>,
    /// When true, the outcome includes the ranking for inspection.
    pub debug: bool,
}

/// Result of one auction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuctionOutcome {
    /// The ad shown, if any campaign won and could pay.
    pub winner: Option<Winner>,
    /// Campaigns that entered the auction.
    pub candidates: usize,
    /// Top candidates with scores and exclusion reasons; only in debug mode.
    pub ranking: Option<Vec<RankedEntry>>,
}

/// The winning ad and what it pays. Field names on the wire match the
/// `ad` object of the match API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Winner {
    /// Winning campaign.
    pub campaign_id: CampaignId,
    /// The campaign's best matching keyword.
    pub keyword_id: KeywordId,
    /// That keyword's text.
    #[serde(rename = "keyword")]
    pub keyword_text: String,
    /// How that keyword matched.
    pub match_type: MatchType,
    /// The advertiser's maximum bid for that keyword.
    #[serde(rename = "max_cpt_bid_micros")]
    pub max_cpt_bid: Micros,
    /// What the winner is charged (second price, never above its bid).
    #[serde(rename = "price_micros")]
    pub price: Micros,
    /// Relevance used in the score.
    #[serde(rename = "relevance_bp")]
    pub relevance: RelevanceBp,
}

/// One row of the debug ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RankedEntry {
    /// Candidate campaign.
    pub campaign_id: CampaignId,
    /// Its best matching keyword.
    pub keyword_id: KeywordId,
    /// Bid of that keyword.
    #[serde(rename = "bid_micros")]
    pub bid: Micros,
    /// Relevance of that keyword for this query.
    #[serde(rename = "relevance_bp")]
    pub relevance: RelevanceBp,
    /// `bid × relevance`. `u128` so the product can never overflow.
    pub score: u128,
    /// The price this candidate would pay if it won.
    #[serde(rename = "would_pay_micros")]
    pub would_pay: Option<Micros>,
    /// Why the candidate could not win, if it was excluded.
    pub excluded: Option<ExclusionReason>,
}

/// Why a matching campaign was left out of the auction or skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// A negative keyword matched the query.
    NegativeKeyword,
    /// Country, age or audience-size targeting did not allow it.
    Targeting,
    /// Its bid is below the reserve price.
    Reserve,
    /// Not enough daily budget left to pay the price.
    Budget,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AdvertiserId, CampaignStatus, Keyword};

    fn campaign(id: i64, keyword_id: i64) -> Campaign {
        Campaign {
            id: CampaignId(id),
            advertiser_id: AdvertiserId(1),
            name: format!("campaign {id}"),
            status: CampaignStatus::Active,
            daily_budget: Micros(1_000_000),
            countries: vec![Country::IN],
            age_buckets: None,
            audience_size: None,
            keywords: vec![Keyword {
                id: KeywordId(keyword_id),
                text: "photo editor".to_owned(),
                match_type: MatchType::Broad,
                max_cpt_bid: Micros(2_000_000),
            }],
            negative_keywords: vec![],
        }
    }

    fn request(query: &str) -> AuctionRequest<'_> {
        AuctionRequest {
            query,
            country: Country::IN,
            personalized: false,
            age_bucket: None,
            debug: false,
        }
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()
    }

    #[test]
    fn build_counts_campaigns_and_keywords() {
        let snap = Snapshot::build(
            vec![campaign(1, 10), campaign(2, 20)],
            EngineConfig::default(),
        )
        .unwrap();
        assert_eq!(snap.campaign_count(), 2);
        assert_eq!(snap.keyword_count(), 2);
    }

    #[test]
    fn build_rejects_duplicate_ids() {
        let cfg = EngineConfig::default();
        assert_eq!(
            Snapshot::build(vec![campaign(1, 10), campaign(1, 20)], cfg).unwrap_err(),
            BuildError::DuplicateCampaign(CampaignId(1))
        );
        assert_eq!(
            Snapshot::build(vec![campaign(1, 10), campaign(2, 10)], cfg).unwrap_err(),
            BuildError::DuplicateKeyword(KeywordId(10))
        );
    }

    #[test]
    fn run_rejects_invalid_queries() {
        let snap = Snapshot::build(vec![], EngineConfig::default()).unwrap();
        let budgets = BudgetStore::in_memory();
        assert_eq!(
            snap.run(&request("  "), &budgets, day()),
            Err(QueryError::Empty)
        );
    }

    #[test]
    fn exclusion_reason_serialises_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ExclusionReason::NegativeKeyword).unwrap(),
            "\"negative_keyword\""
        );
    }
}
