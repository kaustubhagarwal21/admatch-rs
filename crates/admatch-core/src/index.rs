//! The inverted keyword index: which campaigns match a query, and with which
//! keyword.
//!
//! Built once per [`Snapshot`](crate::engine::Snapshot) and then only read.
//!
//! # Token interning
//!
//! Every distinct keyword and negative-keyword token is given a small
//! [`TokenId`] when the index is built. The hot path then compares and hashes
//! `u32`s instead of strings. A query token that is not in the vocabulary
//! cannot help any keyword match (no keyword contains it), and it makes an
//! exact match impossible (the query has a token no keyword has).
//!
//! # Token sets
//!
//! Keywords, negatives and queries are compared as *sets* of distinct tokens:
//! `"photo photo editor"` behaves like `"photo editor"`. Word order is already
//! ignored for exact match (the rearrangement close variant), and relevance
//! counts distinct tokens, so treating repeats the same way keeps all the
//! rules consistent.
//!
//! # Structures
//!
//! * `exact`: sorted token set -> keywords with exactly that set. One hash
//!   lookup answers "which exact keywords equal this query?".
//! * `broad`: token -> posting list of broad keywords containing it. For each
//!   distinct query token we walk its posting list and count a hit per
//!   keyword. A keyword matches when its hits equal its number of distinct
//!   tokens, i.e. every one of its tokens was in the query.
//! * Negatives are stored per campaign and checked only for campaigns that
//!   already matched, so their cost scales with matches, not with the corpus.
//!
//! # Ownership
//!
//! The index owns all of its data (`Box<[TokenId]>`, `Vec`s) and holds no
//! references into the campaigns, so it has no lifetime parameter and can be
//! moved into an `Arc` and shared between threads freely. Keywords refer to
//! their campaign by *position* (`usize`) in the snapshot's campaign list
//! instead of by reference.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::model::{Campaign, CampaignId, KeywordId, MatchType, Micros, RelevanceBp, TokenId};
use crate::normalize::tokenize;

/// Relevance of an exact match: a perfect 100.00%.
pub const EXACT_RELEVANCE_BP: u32 = 10_000;

/// Upper weight of a broad match. A broad keyword covering the whole query
/// scores 7,000 (70.00%), always below an exact match.
pub const BROAD_WEIGHT_BP: u32 = 7_000;

/// Lowest relevance any match can have, so a score is never zero.
pub const MIN_RELEVANCE_BP: u32 = 1;

/// Relevance of a broad match (our own model, not a published formula):
/// `max(1, 7_000 × keyword_tokens / query_tokens)` on distinct-token counts,
/// in integer maths (the division rounds down).
///
/// A keyword that covers more of the query is more relevant: `photo editor`
/// against `free photo editor` gives `7_000 × 2 / 3 = 4_666`.
pub fn broad_relevance(keyword_tokens: usize, query_tokens: usize) -> RelevanceBp {
    // u64 so the multiplication can never overflow, whatever the counts.
    let kw = u64::try_from(keyword_tokens).unwrap_or(u64::MAX);
    let q = u64::try_from(query_tokens).unwrap_or(u64::MAX).max(1);
    let raw = u64::from(BROAD_WEIGHT_BP).saturating_mul(kw) / q;
    // A broad match has keyword_tokens <= query_tokens, so raw <= 7_000; the
    // clamp only guards against misuse of this public function.
    let clamped = raw.clamp(u64::from(MIN_RELEVANCE_BP), u64::from(BROAD_WEIGHT_BP));
    RelevanceBp(u32::try_from(clamped).unwrap_or(BROAD_WEIGHT_BP))
}

/// The auction score of one keyword: `bid × relevance`.
///
/// `u128` because `i64::MAX × u32::MAX` is below 2^95, so the product can
/// never overflow. A negative bid (never valid) scores 0.
pub fn score(bid: Micros, relevance: RelevanceBp) -> u128 {
    let bid = u128::try_from(bid.0).unwrap_or(0);
    bid * u128::from(relevance.0)
}

/// A campaign's best matching keyword for one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordMatch {
    /// Position of the campaign in the snapshot (used to look it up again).
    pub(crate) campaign_pos: usize,
    /// The matching campaign.
    pub campaign_id: CampaignId,
    /// Its best matching keyword.
    pub keyword_id: KeywordId,
    /// How that keyword matched.
    pub match_type: MatchType,
    /// That keyword's max CPT bid.
    pub bid: Micros,
    /// Relevance of that keyword for this query.
    pub relevance: RelevanceBp,
}

impl KeywordMatch {
    /// `bid × relevance`, the value the auction ranks by.
    pub fn score(&self) -> u128 {
        score(self.bid, self.relevance)
    }

    /// True when `self` should be preferred over `other` as the campaign's
    /// keyword: higher score, then higher bid, then lower keyword id. The
    /// final tie-break makes the choice deterministic.
    fn beats(&self, other: &KeywordMatch) -> bool {
        (self.score(), self.bid, std::cmp::Reverse(self.keyword_id))
            > (
                other.score(),
                other.bid,
                std::cmp::Reverse(other.keyword_id),
            )
    }
}

/// A query after interning, ready for index lookups.
#[derive(Debug, Clone)]
pub struct PreparedQuery {
    /// Interned ids of the query tokens found in the vocabulary, sorted and
    /// de-duplicated (so it can be used directly as an exact-match key).
    known: Vec<TokenId>,
    /// Number of distinct query tokens, known or not (relevance denominator).
    distinct: usize,
}

impl PreparedQuery {
    /// True when every query token is in the vocabulary. If not, no exact
    /// keyword or exact negative can equal the query.
    fn all_known(&self) -> bool {
        self.known.len() == self.distinct
    }

    /// True when every token of `set` (sorted) appears in the query.
    fn contains_all(&self, set: &[TokenId]) -> bool {
        set.iter().all(|t| self.known.binary_search(t).is_ok())
    }
}

/// One keyword as stored in the index.
#[derive(Debug, Clone)]
struct IndexedKeyword {
    campaign_pos: usize,
    campaign_id: CampaignId,
    id: KeywordId,
    match_type: MatchType,
    bid: Micros,
    /// Sorted, distinct token ids.
    tokens: Box<[TokenId]>,
}

/// One negative keyword as stored in the index.
#[derive(Debug, Clone)]
struct IndexedNegative {
    match_type: MatchType,
    /// Sorted, distinct token ids.
    tokens: Box<[TokenId]>,
}

/// The vocabulary is full: more than `u32::MAX` distinct tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VocabularyFull;

/// Inverted index over every keyword of every campaign.
#[derive(Debug, Clone, Default)]
pub struct KeywordIndex {
    vocab: HashMap<String, TokenId>,
    keywords: Vec<IndexedKeyword>,
    exact: HashMap<Box<[TokenId]>, Vec<usize>>,
    broad: HashMap<TokenId, Vec<usize>>,
    /// Negatives per campaign position.
    negatives: Vec<Vec<IndexedNegative>>,
}

impl KeywordIndex {
    /// Builds the index. Keywords or negatives whose text has no letters or
    /// digits are skipped: an empty token set could never match as a
    /// keyword, and as a broad negative it would vacuously match (and so
    /// block) every query.
    pub(crate) fn build(campaigns: &[Campaign]) -> Result<Self, VocabularyFull> {
        let mut index = KeywordIndex::default();
        for (campaign_pos, campaign) in campaigns.iter().enumerate() {
            for keyword in &campaign.keywords {
                let tokens = index.intern_set(&keyword.text)?;
                if tokens.is_empty() {
                    continue;
                }
                let pos = index.keywords.len();
                match keyword.match_type {
                    MatchType::Exact => index.exact.entry(tokens.clone()).or_default().push(pos),
                    MatchType::Broad => {
                        for &token in tokens.iter() {
                            index.broad.entry(token).or_default().push(pos);
                        }
                    }
                }
                index.keywords.push(IndexedKeyword {
                    campaign_pos,
                    campaign_id: campaign.id,
                    id: keyword.id,
                    match_type: keyword.match_type,
                    bid: keyword.max_cpt_bid,
                    tokens,
                });
            }

            let mut negatives = Vec::new();
            for negative in &campaign.negative_keywords {
                let tokens = index.intern_set(&negative.text)?;
                if !tokens.is_empty() {
                    negatives.push(IndexedNegative {
                        match_type: negative.match_type,
                        tokens,
                    });
                }
            }
            index.negatives.push(negatives);
        }
        Ok(index)
    }

    /// Tokenises `text` and interns each token, returning the sorted,
    /// de-duplicated set of ids.
    fn intern_set(&mut self, text: &str) -> Result<Box<[TokenId]>, VocabularyFull> {
        let mut ids = Vec::new();
        for token in tokenize(text) {
            let next = TokenId(u32::try_from(self.vocab.len()).map_err(|_| VocabularyFull)?);
            let id = match self.vocab.entry(token) {
                Entry::Occupied(e) => *e.get(),
                Entry::Vacant(e) => *e.insert(next),
            };
            ids.push(id);
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids.into_boxed_slice())
    }

    /// Number of distinct interned tokens.
    pub fn vocabulary_size(&self) -> usize {
        self.vocab.len()
    }

    /// Interns a normalised query. Unknown tokens are counted but dropped.
    pub fn prepare(&self, tokens: &[String]) -> PreparedQuery {
        let mut distinct: Vec<&str> = tokens.iter().map(String::as_str).collect();
        distinct.sort_unstable();
        distinct.dedup();

        let mut known: Vec<TokenId> = distinct
            .iter()
            .filter_map(|t| self.vocab.get(*t).copied())
            .collect();
        known.sort_unstable();
        PreparedQuery {
            known,
            distinct: distinct.len(),
        }
    }

    /// Every matching campaign with its single best keyword, sorted by
    /// campaign position. Negatives are NOT applied here; see
    /// [`KeywordIndex::negative_matches`].
    pub fn best_matches(&self, query: &PreparedQuery) -> Vec<KeywordMatch> {
        // campaign position -> best match so far
        let mut best: HashMap<usize, KeywordMatch> = HashMap::new();
        let mut offer = |kw: &IndexedKeyword, relevance: RelevanceBp| {
            let candidate = KeywordMatch {
                campaign_pos: kw.campaign_pos,
                campaign_id: kw.campaign_id,
                keyword_id: kw.id,
                match_type: kw.match_type,
                bid: kw.bid,
                relevance,
            };
            match best.entry(kw.campaign_pos) {
                Entry::Vacant(e) => {
                    e.insert(candidate);
                }
                Entry::Occupied(mut e) => {
                    if candidate.beats(e.get()) {
                        e.insert(candidate);
                    }
                }
            }
        };

        // Exact: only possible when every query token is known.
        if query.all_known()
            && let Some(hits) = self.exact.get(query.known.as_slice())
        {
            for &pos in hits {
                if let Some(kw) = self.keywords.get(pos) {
                    offer(kw, RelevanceBp(EXACT_RELEVANCE_BP));
                }
            }
        }

        // Broad: count hits per keyword across the distinct known tokens.
        let mut hits: HashMap<usize, usize> = HashMap::new();
        for token in &query.known {
            if let Some(postings) = self.broad.get(token) {
                for &pos in postings {
                    *hits.entry(pos).or_insert(0) += 1;
                }
            }
        }
        for (pos, count) in hits {
            if let Some(kw) = self.keywords.get(pos)
                && count == kw.tokens.len()
            {
                offer(kw, broad_relevance(kw.tokens.len(), query.distinct));
            }
        }

        let mut out: Vec<KeywordMatch> = best.into_values().collect();
        out.sort_unstable_by_key(|m| m.campaign_pos);
        out
    }

    /// True when any negative keyword of the campaign at `campaign_pos`
    /// matches the query (exact: same token set; broad: all its tokens are
    /// in the query).
    pub fn negative_matches(&self, campaign_pos: usize, query: &PreparedQuery) -> bool {
        let Some(negatives) = self.negatives.get(campaign_pos) else {
            return false;
        };
        negatives.iter().any(|neg| match neg.match_type {
            MatchType::Exact => query.all_known() && *neg.tokens == *query.known,
            MatchType::Broad => query.contains_all(&neg.tokens),
        })
    }
}

#[cfg(test)]
pub(crate) mod reference {
    //! Brute-force matcher used only by tests: checks every keyword of every
    //! campaign directly with string sets, sharing no code with the index
    //! except `tokenize` and the relevance formula. If the index and this
    //! disagree, the index is wrong.

    use std::collections::BTreeSet;

    use super::{EXACT_RELEVANCE_BP, broad_relevance, score};
    use crate::model::{Campaign, CampaignId, KeywordId, MatchType, RelevanceBp};
    use crate::normalize::tokenize;

    fn set(text: &str) -> BTreeSet<String> {
        tokenize(text).into_iter().collect()
    }

    fn matches(match_type: MatchType, kw: &BTreeSet<String>, q: &BTreeSet<String>) -> bool {
        !kw.is_empty()
            && match match_type {
                MatchType::Exact => kw == q,
                MatchType::Broad => kw.is_subset(q),
            }
    }

    /// `(campaign, best keyword, relevance)` for every matching campaign not
    /// blocked by a negative, sorted by campaign id.
    pub(crate) fn reference_match(
        campaigns: &[Campaign],
        query: &str,
    ) -> Vec<(CampaignId, KeywordId, RelevanceBp)> {
        let q = set(query);
        let mut out = Vec::new();
        for campaign in campaigns {
            let blocked = campaign
                .negative_keywords
                .iter()
                .any(|n| matches(n.match_type, &set(&n.text), &q));
            if blocked {
                continue;
            }
            let mut best: Option<(u128, i64, i64, KeywordId, RelevanceBp)> = None;
            for kw in &campaign.keywords {
                let kw_set = set(&kw.text);
                if !matches(kw.match_type, &kw_set, &q) {
                    continue;
                }
                let relevance = match kw.match_type {
                    MatchType::Exact => RelevanceBp(EXACT_RELEVANCE_BP),
                    MatchType::Broad => broad_relevance(kw_set.len(), q.len()),
                };
                // Higher score, then higher bid, then lower keyword id.
                let key = (score(kw.max_cpt_bid, relevance), kw.max_cpt_bid.0, -kw.id.0);
                if best.is_none_or(|b| key > (b.0, b.1, b.2)) {
                    best = Some((key.0, key.1, key.2, kw.id, relevance));
                }
            }
            if let Some(b) = best {
                out.push((campaign.id, b.3, b.4));
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::reference::reference_match;
    use super::*;
    use crate::engine::{EngineConfig, Snapshot};
    use crate::model::{AdvertiserId, CampaignStatus, Country, Keyword, NegativeKeyword};

    fn kw(id: i64, text: &str, match_type: MatchType, bid: i64) -> Keyword {
        Keyword {
            id: KeywordId(id),
            text: text.to_owned(),
            match_type,
            max_cpt_bid: Micros(bid),
        }
    }

    fn neg(text: &str, match_type: MatchType) -> NegativeKeyword {
        NegativeKeyword {
            text: text.to_owned(),
            match_type,
        }
    }

    fn campaign(id: i64, keywords: Vec<Keyword>, negatives: Vec<NegativeKeyword>) -> Campaign {
        Campaign {
            id: CampaignId(id),
            advertiser_id: AdvertiserId(1),
            name: format!("c{id}"),
            status: CampaignStatus::Active,
            daily_budget: Micros(1_000_000),
            countries: vec![Country::IN],
            age_buckets: None,
            audience_size: None,
            keywords,
            negative_keywords: negatives,
        }
    }

    /// `(campaign, keyword, relevance)` triples from the real index.
    fn indexed(campaigns: &[Campaign], query: &str) -> Vec<(CampaignId, KeywordId, RelevanceBp)> {
        let snap = Snapshot::build(campaigns.to_vec(), EngineConfig::default()).unwrap();
        let mut out: Vec<_> = snap
            .matches(query)
            .unwrap()
            .into_iter()
            .map(|m| (m.campaign_id, m.keyword_id, m.relevance))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn exact_ignores_word_order_and_case_but_not_extra_words() {
        let c = vec![campaign(
            1,
            vec![kw(10, "Photo Editor", MatchType::Exact, 1_000_000)],
            vec![],
        )];
        let hit = vec![(CampaignId(1), KeywordId(10), RelevanceBp(10_000))];
        assert_eq!(indexed(&c, "photo editor"), hit);
        assert_eq!(indexed(&c, "EDITOR photo!"), hit);
        assert!(indexed(&c, "free photo editor").is_empty());
        assert!(indexed(&c, "photo").is_empty());
    }

    #[test]
    fn broad_needs_every_keyword_token_and_scores_coverage() {
        let c = vec![campaign(
            1,
            vec![kw(10, "photo editor", MatchType::Broad, 1_000_000)],
            vec![],
        )];
        // The worked relevance example: 7_000 × 2 / 3 = 4_666.
        assert_eq!(
            indexed(&c, "free photo editor"),
            vec![(CampaignId(1), KeywordId(10), RelevanceBp(4_666))]
        );
        assert_eq!(
            indexed(&c, "editor photo"),
            vec![(CampaignId(1), KeywordId(10), RelevanceBp(7_000))]
        );
        assert!(indexed(&c, "photo").is_empty());
        assert!(indexed(&c, "video editor").is_empty());
    }

    #[test]
    fn unknown_query_tokens_block_exact_but_count_for_broad() {
        let c = vec![
            campaign(
                1,
                vec![kw(10, "chess", MatchType::Exact, 1_000_000)],
                vec![],
            ),
            campaign(
                2,
                vec![kw(20, "chess", MatchType::Broad, 1_000_000)],
                vec![],
            ),
        ];
        assert_eq!(
            indexed(&c, "chess zzzunknown"),
            vec![(CampaignId(2), KeywordId(20), RelevanceBp(3_500))]
        );
    }

    #[test]
    fn negative_broad_and_exact_exclude_the_campaign() {
        let c = vec![
            campaign(
                1,
                vec![kw(10, "photo editor", MatchType::Broad, 1_000_000)],
                vec![neg("video", MatchType::Broad)],
            ),
            campaign(
                2,
                vec![kw(20, "photo editor", MatchType::Broad, 1_000_000)],
                vec![neg("free photo editor", MatchType::Exact)],
            ),
        ];
        // Broad negative "video" blocks campaign 1 only.
        assert_eq!(
            indexed(&c, "photo video editor"),
            vec![(CampaignId(2), KeywordId(20), RelevanceBp(4_666))]
        );
        // The exact negative blocks campaign 2 only on exactly that token set.
        assert_eq!(
            indexed(&c, "editor free photo"),
            vec![(CampaignId(1), KeywordId(10), RelevanceBp(4_666))]
        );
        assert_eq!(indexed(&c, "free photo editor pro").len(), 2);
    }

    #[test]
    fn empty_keywords_and_negatives_never_match() {
        let c = vec![campaign(
            1,
            vec![
                kw(10, "!!!", MatchType::Broad, 1_000_000),
                kw(11, "chess", MatchType::Broad, 1),
            ],
            vec![neg("---", MatchType::Broad)],
        )];
        assert_eq!(
            indexed(&c, "chess"),
            vec![(CampaignId(1), KeywordId(11), RelevanceBp(7_000))]
        );
    }

    #[test]
    fn one_best_keyword_per_campaign() {
        let c = vec![campaign(
            1,
            vec![
                kw(10, "photo", MatchType::Broad, 1_000_000),
                kw(11, "photo editor", MatchType::Exact, 500_000),
                kw(12, "editor", MatchType::Broad, 1_000_000),
            ],
            vec![],
        )];
        // Scores: 10 → 1e6 × 3_500, 11 → 5e5 × 10_000 (best), 12 same as 10.
        assert_eq!(
            indexed(&c, "photo editor"),
            vec![(CampaignId(1), KeywordId(11), RelevanceBp(10_000))]
        );
        // Tie between 10 and 12 (same score and bid): lower keyword id wins.
        assert_eq!(
            indexed(&c, "photo editor free"),
            vec![(CampaignId(1), KeywordId(10), RelevanceBp(2_333))]
        );
    }

    #[test]
    fn broad_relevance_is_clamped() {
        assert_eq!(broad_relevance(1, 16), RelevanceBp(437));
        assert_eq!(broad_relevance(0, 16), RelevanceBp(MIN_RELEVANCE_BP));
        assert_eq!(broad_relevance(5, 0), RelevanceBp(BROAD_WEIGHT_BP));
    }

    const WORDS: [&str; 8] = [
        "photo", "editor", "free", "chess", "news", "vpn", "pdf", "music",
    ];

    fn words(max: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(WORDS.to_vec()), 1..=max)
            .prop_map(|w| w.join(" "))
    }

    fn match_type() -> impl Strategy<Value = MatchType> {
        prop_oneof![Just(MatchType::Exact), Just(MatchType::Broad)]
    }

    /// Up to 20 campaigns over a tiny vocabulary, so matches, ties and
    /// negatives are all common.
    fn corpus() -> impl Strategy<Value = Vec<Campaign>> {
        let bids = prop::sample::select(vec![100_000_i64, 200_000, 300_000]);
        let keyword = (words(3), match_type(), bids);
        let negative = (words(2), match_type());
        let one = (
            prop::collection::vec(keyword, 1..4),
            prop::collection::vec(negative, 0..2),
        );
        prop::collection::vec(one, 0..20).prop_map(|campaigns| {
            let mut next_kw = 0;
            let mut out = Vec::new();
            for (id, (kws, negs)) in (1..).zip(campaigns) {
                let keywords = kws
                    .into_iter()
                    .map(|(text, mt, bid)| {
                        next_kw += 1;
                        kw(next_kw, &text, mt, bid)
                    })
                    .collect();
                let negatives = negs.into_iter().map(|(t, mt)| neg(&t, mt)).collect();
                out.push(campaign(id, keywords, negatives));
            }
            out
        })
    }

    fn query() -> impl Strategy<Value = String> {
        // Sometimes add a word that no keyword contains.
        (words(4), prop::bool::weighted(0.2)).prop_map(
            |(q, odd)| {
                if odd { format!("{q} zzz") } else { q }
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The index must return exactly what brute force returns.
        #[test]
        fn index_equals_reference(campaigns in corpus(), q in query()) {
            prop_assert_eq!(indexed(&campaigns, &q), reference_match(&campaigns, &q));
        }
    }
}
