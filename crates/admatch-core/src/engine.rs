//! The matching engine: an immutable [`Snapshot`] of all campaigns plus the
//! auction that picks at most one ad per query.
//!
//! A snapshot is built once and then only read. The server shares it between
//! request handlers and swaps in a whole new one when campaigns change, so
//! readers never wait on a lock.
//!
//! `build` creates the keyword index ([`crate::index`]); `run` matches,
//! filters, ranks and prices ([`crate::auction`]) and charges the winner
//! against a [`BudgetStore`].

use std::collections::HashSet;

use chrono::NaiveDate;
use serde::Serialize;
use thiserror::Error;

use crate::auction::{Bidder, Selection, rank_order, select_winner};
use crate::budget::BudgetStore;
use crate::index::{KeywordIndex, KeywordMatch};
use crate::model::{
    AgeBucket, Campaign, CampaignId, CampaignStatus, Country, KeywordId, MatchType, Micros,
    RelevanceBp,
};
use crate::normalize::{QueryError, normalize};
use crate::privacy::check_targeting;

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
    /// Steps: normalise → index lookup (best keyword per campaign) →
    /// eligibility filters in spec order → rank → walk the ranking, charging
    /// each candidate its GSP price until one can pay.
    ///
    /// `day` is the current UTC date, passed in (not read from a clock) so
    /// the core stays deterministic and testable. An invalid query is an
    /// error; "no ad" is a normal outcome with `winner: None`.
    ///
    /// A budget *error* (as opposed to "not enough budget") is treated like
    /// "cannot pay": the candidate is skipped. Failing closed means we never
    /// show an ad we could not charge for.
    pub fn run(
        &self,
        req: &AuctionRequest<'_>,
        budgets: &BudgetStore,
        day: NaiveDate,
    ) -> Result<AuctionOutcome, QueryError> {
        let tokens = normalize(req.query)?;
        let prepared = self.index.prepare(&tokens);

        let mut eligible: Vec<KeywordMatch> = Vec::new();
        let mut excluded: Vec<(KeywordMatch, ExclusionReason)> = Vec::new();
        for m in self.index.best_matches(&prepared) {
            let Some(campaign) = self.campaigns.get(m.campaign_pos) else {
                continue;
            };
            match self.eligibility(campaign, &m, req, || {
                self.index.negative_matches(m.campaign_pos, &prepared)
            }) {
                Eligibility::Paused => {}
                Eligibility::Excluded(reason) => excluded.push((m, reason)),
                Eligibility::Eligible => eligible.push(m),
            }
        }

        eligible.sort_by(|a, b| rank_order(&a.bidder(), &b.bidder()));
        let ranked: Vec<Bidder> = eligible.iter().map(KeywordMatch::bidder).collect();
        let selection = select_winner(&ranked, self.cfg.reserve, self.cfg.increment, |i, price| {
            eligible
                .get(i)
                .and_then(|m| self.campaigns.get(m.campaign_pos))
                .is_some_and(|c| {
                    budgets
                        .try_spend(c.id, day, price, c.daily_budget)
                        .unwrap_or(false)
                })
        });

        let winner = selection.winner.and_then(|i| {
            let m = eligible.get(i)?;
            let price = *selection.prices.get(i)?;
            let campaign = self.campaigns.get(m.campaign_pos)?;
            let keyword = campaign.keywords.iter().find(|k| k.id == m.keyword_id)?;
            Some(Winner {
                campaign_id: m.campaign_id,
                keyword_id: m.keyword_id,
                keyword_text: keyword.text.clone(),
                match_type: m.match_type,
                max_cpt_bid: m.bid,
                price,
                relevance: m.relevance,
            })
        });

        let ranking = req
            .debug
            .then(|| debug_ranking(&eligible, &selection, excluded));

        Ok(AuctionOutcome {
            winner,
            candidates: eligible.len(),
            ranking,
        })
    }

    /// The eligibility checks, in the order the spec lists them. The first
    /// failing check is the reported reason. `negative_hit` is a closure so
    /// the negative-keyword scan only runs if the cheaper checks pass.
    fn eligibility(
        &self,
        campaign: &Campaign,
        m: &KeywordMatch,
        req: &AuctionRequest<'_>,
        negative_hit: impl FnOnce() -> bool,
    ) -> Eligibility {
        // 1. Paused campaigns do not enter the auction at all.
        if campaign.status != CampaignStatus::Active {
            return Eligibility::Paused;
        }
        // 2. Country.
        if !campaign.countries.contains(&req.country) {
            return Eligibility::Excluded(ExclusionReason::Targeting);
        }
        // 3. Age targeting: personalised request, bucket targeted, and an
        //    audience above the privacy threshold. Never a fallback.
        if let Some(buckets) = &campaign.age_buckets {
            let bucket_ok =
                req.personalized && req.age_bucket.is_some_and(|b| buckets.contains(&b));
            let audience_ok = campaign
                .audience_size
                .is_some_and(|size| check_targeting(size, self.cfg.k_targeting).is_ok());
            if !(bucket_ok && audience_ok) {
                return Eligibility::Excluded(ExclusionReason::Targeting);
            }
        }
        // 4. Negative keywords.
        if negative_hit() {
            return Eligibility::Excluded(ExclusionReason::NegativeKeyword);
        }
        // 5. Reserve price.
        if m.bid < self.cfg.reserve {
            return Eligibility::Excluded(ExclusionReason::Reserve);
        }
        Eligibility::Eligible
    }
}

/// Most rows returned in the debug ranking.
pub const DEBUG_RANKING_LIMIT: usize = 10;

/// Outcome of the eligibility checks for one matched campaign.
enum Eligibility {
    /// Not active: silently left out (not a candidate, not in the ranking).
    Paused,
    /// Matched but may not take part, for this reason.
    Excluded(ExclusionReason),
    /// Enters the auction.
    Eligible,
}

/// Builds the debug ranking: auction candidates in rank order first (so the
/// winner is always visible), then excluded campaigns by score, top 10.
fn debug_ranking(
    eligible: &[KeywordMatch],
    selection: &Selection,
    mut excluded: Vec<(KeywordMatch, ExclusionReason)>,
) -> Vec<RankedEntry> {
    let entry = |m: &KeywordMatch, would_pay, reason| RankedEntry {
        campaign_id: m.campaign_id,
        keyword_id: m.keyword_id,
        bid: m.bid,
        relevance: m.relevance,
        score: m.score(),
        would_pay,
        excluded: reason,
    };
    let mut rows: Vec<RankedEntry> = eligible
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let reason = (i < selection.budget_skipped).then_some(ExclusionReason::Budget);
            entry(m, selection.prices.get(i).copied(), reason)
        })
        .collect();
    excluded.sort_by(|(a, _), (b, _)| rank_order(&a.bidder(), &b.bidder()));
    rows.extend(excluded.iter().map(|(m, r)| entry(m, None, Some(*r))));
    rows.truncate(DEBUG_RANKING_LIMIT);
    rows
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

    /// A campaign bidding `bid` on broad "photo editor" with a large budget.
    fn bidding(id: i64, bid: i64) -> Campaign {
        let mut c = campaign(id, id * 10);
        c.keywords[0].max_cpt_bid = Micros(bid);
        c.daily_budget = Micros(100_000_000);
        c
    }

    fn debug(query: &str) -> AuctionRequest<'_> {
        AuctionRequest {
            debug: true,
            ..request(query)
        }
    }

    fn reasons(outcome: &AuctionOutcome) -> Vec<(CampaignId, Option<ExclusionReason>)> {
        outcome
            .ranking
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| (r.campaign_id, r.excluded))
            .collect()
    }

    #[test]
    fn run_picks_the_top_score_and_charges_the_second_price() {
        let snap = Snapshot::build(
            vec![bidding(1, 1_000_000), bidding(2, 2_000_000)],
            EngineConfig::default(),
        )
        .unwrap();
        let budgets = BudgetStore::in_memory();
        let out = snap
            .run(&request("free photo editor"), &budgets, day())
            .unwrap();
        let winner = out.winner.unwrap();
        assert_eq!(out.candidates, 2);
        assert_eq!(winner.campaign_id, CampaignId(2));
        assert_eq!(winner.keyword_text, "photo editor");
        assert_eq!(winner.relevance, RelevanceBp(4_666));
        // Same relevance, so the price is the runner-up's bid + increment.
        assert_eq!(winner.price, Micros(1_010_000));
        assert_eq!(budgets.spent(CampaignId(2), day()), Micros(1_010_000));
        assert_eq!(out.ranking, None);
    }

    #[test]
    fn no_match_means_no_winner_and_no_charge() {
        let snap = Snapshot::build(vec![bidding(1, 1_000_000)], EngineConfig::default()).unwrap();
        let budgets = BudgetStore::in_memory();
        let out = snap.run(&request("chess"), &budgets, day()).unwrap();
        assert_eq!(out.winner, None);
        assert_eq!(out.candidates, 0);
    }

    #[test]
    fn eligibility_filters_report_their_reasons() {
        let paused = Campaign {
            status: CampaignStatus::Paused,
            ..bidding(1, 9_000_000)
        };
        let wrong_country = Campaign {
            countries: vec![Country::US],
            ..bidding(2, 8_000_000)
        };
        let negative = Campaign {
            negative_keywords: vec![crate::model::NegativeKeyword {
                text: "free".to_owned(),
                match_type: MatchType::Broad,
            }],
            ..bidding(3, 7_000_000)
        };
        let below_reserve = bidding(4, 99_999);
        let ok = bidding(5, 1_000_000);
        let snap = Snapshot::build(
            vec![paused, wrong_country, negative, below_reserve, ok],
            EngineConfig::default(),
        )
        .unwrap();
        let out = snap
            .run(
                &debug("free photo editor"),
                &BudgetStore::in_memory(),
                day(),
            )
            .unwrap();
        assert_eq!(out.candidates, 1);
        assert_eq!(out.winner.as_ref().unwrap().campaign_id, CampaignId(5));
        // Paused campaigns are not listed; others show the first failed rule.
        assert_eq!(
            reasons(&out),
            vec![
                (CampaignId(5), None),
                (CampaignId(2), Some(ExclusionReason::Targeting)),
                (CampaignId(3), Some(ExclusionReason::NegativeKeyword)),
                (CampaignId(4), Some(ExclusionReason::Reserve)),
            ]
        );
    }

    #[test]
    fn age_targeting_needs_consent_bucket_and_a_large_audience() {
        let targeted = |id: i64, audience: i64| Campaign {
            age_buckets: Some(vec![AgeBucket::Age25To34]),
            audience_size: Some(audience),
            ..bidding(id, 1_000_000)
        };
        let run = |c: Campaign, personalized: bool, bucket: Option<AgeBucket>| {
            let snap = Snapshot::build(vec![c], EngineConfig::default()).unwrap();
            let req = AuctionRequest {
                personalized,
                age_bucket: bucket,
                ..request("photo editor")
            };
            snap.run(&req, &BudgetStore::in_memory(), day())
                .unwrap()
                .winner
                .is_some()
        };
        let bucket = Some(AgeBucket::Age25To34);
        assert!(run(targeted(1, 5_001), true, bucket));
        // Exactly k is not "more than" k.
        assert!(!run(targeted(1, 5_000), true, bucket));
        // No consent, wrong bucket or no bucket: never shown as a fallback.
        assert!(!run(targeted(1, 50_000), false, bucket));
        assert!(!run(targeted(1, 50_000), true, Some(AgeBucket::Age65Plus)));
        assert!(!run(targeted(1, 50_000), true, None));
        // Contextual campaigns serve to everyone.
        assert!(run(bidding(1, 1_000_000), false, None));
    }

    #[test]
    fn exhausted_budget_falls_through_to_the_next_candidate() {
        let broke = Campaign {
            daily_budget: Micros(500_000),
            ..bidding(1, 2_000_000)
        };
        let snap =
            Snapshot::build(vec![broke, bidding(2, 1_000_000)], EngineConfig::default()).unwrap();
        let budgets = BudgetStore::in_memory();
        let out = snap.run(&debug("photo editor"), &budgets, day()).unwrap();
        let winner = out.winner.clone().unwrap();
        assert_eq!(winner.campaign_id, CampaignId(2));
        // No one left below campaign 2, so it pays the reserve.
        assert_eq!(winner.price, Micros(100_000));
        assert_eq!(
            reasons(&out),
            vec![
                (CampaignId(1), Some(ExclusionReason::Budget)),
                (CampaignId(2), None),
            ]
        );
        assert_eq!(budgets.spent(CampaignId(1), day()), Micros(0));
    }

    #[test]
    fn identical_inputs_give_identical_outcomes() {
        let campaigns: Vec<Campaign> = (1..=20)
            .map(|i| bidding(i, 100_000 * (i % 7 + 1)))
            .collect();
        let snap = Snapshot::build(campaigns, EngineConfig::default()).unwrap();
        let first = snap
            .run(&debug("photo editor pro"), &BudgetStore::in_memory(), day())
            .unwrap();
        let second = snap
            .run(&debug("photo editor pro"), &BudgetStore::in_memory(), day())
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.ranking.unwrap().len(), DEBUG_RANKING_LIMIT);
    }
}
