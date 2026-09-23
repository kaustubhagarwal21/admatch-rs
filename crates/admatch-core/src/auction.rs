//! The generalised second-price (GSP) auction, as pure functions.
//!
//! Kept separate from the index and the budget store so the pricing rules
//! can be tested with hand-picked numbers (like the worked example below)
//! that real keyword matching could not produce.
//!
//! * `score = bid × relevance`, in `u128` integers. No floats touch money.
//! * Ranking: higher score first; ties go to the higher bid, then the lower
//!   campaign id, so identical inputs always give identical outputs.
//! * Price of the candidate at position `i`, against the next candidate
//!   `i + 1`: `min(bid, max(reserve, ceil(next_score / relevance) + increment))`.
//!   `ceil(next_score / relevance)` is the smallest bid that would still
//!   out-score the next candidate, so the winner pays roughly what it needed
//!   to win, not what it offered. With no next candidate, the price is the
//!   reserve.

use std::cmp::Ordering;

use crate::index::score;
use crate::model::{CampaignId, Micros, RelevanceBp};

/// One campaign in the auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bidder {
    /// The campaign bidding.
    pub campaign_id: CampaignId,
    /// Max CPT bid of its best keyword.
    pub bid: Micros,
    /// Relevance of that keyword, at least 1.
    pub relevance: RelevanceBp,
}

impl Bidder {
    /// `bid × relevance`.
    pub fn score(&self) -> u128 {
        score(self.bid, self.relevance)
    }
}

/// Ranking order: sorts the best bidder first (use with `sort_by`).
pub fn rank_order(a: &Bidder, b: &Bidder) -> Ordering {
    b.score()
        .cmp(&a.score())
        .then_with(|| b.bid.cmp(&a.bid))
        .then_with(|| a.campaign_id.cmp(&b.campaign_id))
}

/// GSP price for `bidder` when `next` is the next remaining candidate.
///
/// Never above the bid and, for any bid at or above the reserve, never
/// below the reserve. The maths cannot overflow (`next_score < 2^95`, and
/// the increment adds at most `2^63`), but it is checked anyway; on the
/// impossible overflow the price falls back to the bid, the highest price
/// the advertiser agreed to.
pub fn gsp_price(
    bidder: &Bidder,
    next: Option<&Bidder>,
    reserve: Micros,
    increment: Micros,
) -> Micros {
    let as_u128 = |m: Micros| u128::try_from(m.0).unwrap_or(0);
    let reserve_u = as_u128(reserve);
    let raw = match next {
        None => Some(reserve_u),
        Some(next) => {
            // Relevance is always >= 1 by construction; max(1) keeps a
            // hand-built zero from dividing by zero.
            let relevance = u128::from(bidder.relevance.0).max(1);
            next.score()
                .div_ceil(relevance)
                .checked_add(as_u128(increment))
                .map(|p| p.max(reserve_u))
        }
    };
    match raw {
        Some(price) if price < as_u128(bidder.bid) => {
            // price < bid <= i64::MAX, so the conversion always succeeds.
            Micros(i64::try_from(price).unwrap_or(bidder.bid.0))
        }
        _ => bidder.bid,
    }
}

/// What winner selection decided for a ranked list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Position of the winner in the ranked list, if anyone could pay.
    pub winner: Option<usize>,
    /// Price each position would pay if it won (against the next position).
    pub prices: Vec<Micros>,
    /// How many positions from the top were skipped because they could not
    /// pay. They are exactly `0..budget_skipped`.
    pub budget_skipped: usize,
}

/// Walks the ranked list from the top. Each candidate is priced against the
/// next one and asked to pay through `try_pay(position, price)`; the first
/// that pays wins. A candidate that cannot pay is skipped, and the one below
/// it is then priced against *its* next candidate.
///
/// `ranked` must already be sorted with [`rank_order`].
pub fn select_winner(
    ranked: &[Bidder],
    reserve: Micros,
    increment: Micros,
    mut try_pay: impl FnMut(usize, Micros) -> bool,
) -> Selection {
    let prices: Vec<Micros> = ranked
        .iter()
        .enumerate()
        .map(|(i, b)| gsp_price(b, ranked.get(i + 1), reserve, increment))
        .collect();

    let winner = prices
        .iter()
        .enumerate()
        .find(|&(i, &price)| try_pay(i, price))
        .map(|(i, _)| i);

    Selection {
        winner,
        budget_skipped: winner.unwrap_or(ranked.len()),
        prices,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use proptest::prelude::*;

    use super::*;
    use crate::budget::BudgetStore;

    const RESERVE: Micros = Micros(100_000);
    const INCREMENT: Micros = Micros(10_000);

    fn bidder(id: i64, bid: i64, relevance: u32) -> Bidder {
        Bidder {
            campaign_id: CampaignId(id),
            bid: Micros(bid),
            relevance: RelevanceBp(relevance),
        }
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()
    }

    /// Runs the auction with real in-memory budgets.
    fn auction(bidders: &[Bidder], budgets: &[i64]) -> (Vec<Bidder>, Selection, BudgetStore) {
        let store = BudgetStore::in_memory();
        let mut ranked = bidders.to_vec();
        ranked.sort_by(rank_order);
        let budget_of = |id: CampaignId| {
            let pos = bidders.iter().position(|b| b.campaign_id == id).unwrap();
            Micros(budgets[pos])
        };
        let sel = select_winner(&ranked, RESERVE, INCREMENT, |i, price| {
            let id = ranked[i].campaign_id;
            store
                .try_spend(id, day(), price, budget_of(id))
                .unwrap_or(false)
        });
        (ranked, sel, store)
    }

    /// The worked example from the spec, section 6.5.
    #[test]
    fn worked_example_relevance_beats_a_higher_bid() {
        let a = bidder(1, 2_000_000, 9_000); // score 18e9
        let b = bidder(2, 3_000_000, 5_000); // score 15e9
        let c = bidder(3, 1_000_000, 10_000); // score 10e9

        let (ranked, sel, store) = auction(&[a, b, c], &[10_000_000; 3]);
        assert_eq!(ranked, vec![a, b, c]);
        assert_eq!(sel.winner, Some(0));
        // ceil(15e9 / 9_000) + 10_000 = 1_666_667 + 10_000.
        assert_eq!(sel.prices[0], Micros(1_676_667));
        assert_eq!(store.spent(CampaignId(1), day()), Micros(1_676_667));

        // A's budget cannot cover 1,676,667: A is skipped and B wins
        // against C: ceil(10e9 / 5_000) + 10_000 = 2_010_000.
        let (_, sel, store) = auction(&[a, b, c], &[1_676_666, 10_000_000, 10_000_000]);
        assert_eq!(sel.winner, Some(1));
        assert_eq!(sel.budget_skipped, 1);
        assert_eq!(sel.prices[1], Micros(2_010_000));
        assert_eq!(store.spent(CampaignId(1), day()), Micros(0));
        assert_eq!(store.spent(CampaignId(2), day()), Micros(2_010_000));
    }

    #[test]
    fn single_candidate_pays_the_reserve() {
        let (_, sel, _) = auction(&[bidder(1, 500_000, 7_000)], &[1_000_000]);
        assert_eq!(sel.winner, Some(0));
        assert_eq!(sel.prices, vec![RESERVE]);
    }

    #[test]
    fn price_is_capped_at_the_bid() {
        // Same score, lower bid ranks second; the winner's GSP formula would
        // exceed its own bid (increment on top of a tie), so it pays its bid.
        let a = bidder(1, 1_000_000, 5_000);
        let b = bidder(2, 500_000, 10_000);
        let (ranked, sel, _) = auction(&[b, a], &[10_000_000; 2]);
        assert_eq!(ranked[0], a, "tie on score goes to the higher bid");
        assert_eq!(sel.prices[0], Micros(1_000_000));
    }

    #[test]
    fn ties_on_score_and_bid_go_to_the_lower_campaign_id() {
        let mut ranked = [bidder(9, 1_000_000, 5_000), bidder(3, 1_000_000, 5_000)];
        ranked.sort_by(rank_order);
        assert_eq!(ranked[0].campaign_id, CampaignId(3));
    }

    #[test]
    fn nobody_can_pay_means_no_winner() {
        let (_, sel, _) = auction(
            &[bidder(1, 500_000, 7_000), bidder(2, 400_000, 7_000)],
            &[0, 0],
        );
        assert_eq!(sel.winner, None);
        assert_eq!(sel.budget_skipped, 2);
    }

    #[test]
    fn empty_auction() {
        let sel = select_winner(&[], RESERVE, INCREMENT, |_, _| true);
        assert_eq!(sel.winner, None);
        assert!(sel.prices.is_empty());
    }

    fn bidders() -> impl Strategy<Value = (Vec<Bidder>, Vec<i64>)> {
        prop::collection::vec(
            (RESERVE.0..=5_000_000_i64, 1_u32..=10_000, 0_i64..=6_000_000),
            0..30,
        )
        .prop_map(|rows| {
            let mut out = Vec::new();
            let mut budgets = Vec::new();
            for (id, (bid, rel, budget)) in (1..).zip(rows) {
                out.push(bidder(id, bid, rel));
                budgets.push(budget);
            }
            (out, budgets)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Invariants from the spec: reserve <= price <= bid; the winner
        /// out-scores every remaining candidate; same input, same output.
        #[test]
        fn auction_invariants((bidders, budgets) in bidders()) {
            let (ranked, sel, _) = auction(&bidders, &budgets);
            for pair in ranked.windows(2) {
                prop_assert_ne!(rank_order(&pair[0], &pair[1]), Ordering::Greater);
            }
            for (b, &price) in ranked.iter().zip(&sel.prices) {
                prop_assert!(price <= b.bid);
                prop_assert!(price >= RESERVE);
            }
            if let Some(w) = sel.winner {
                prop_assert_eq!(sel.budget_skipped, w);
                for other in &ranked[w + 1..] {
                    prop_assert!(ranked[w].score() >= other.score());
                }
            } else {
                prop_assert_eq!(sel.budget_skipped, ranked.len());
            }
            let (ranked2, sel2, _) = auction(&bidders, &budgets);
            prop_assert_eq!(ranked, ranked2);
            prop_assert_eq!(sel, sel2);
        }
    }
}
