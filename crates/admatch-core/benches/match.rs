//! Matching benchmark: index lookup time for a fixed query set at 1k, 10k
//! and 100k keywords.
//!
//! Run with `cargo bench -p admatch-core --bench match`. The corpus comes
//! from a tiny deterministic generator (no RNG crate), so every run measures
//! the same data.

use std::hint::black_box;

use admatch_core::engine::{EngineConfig, Snapshot};
use admatch_core::model::{
    AdvertiserId, Campaign, CampaignId, CampaignStatus, Country, Keyword, KeywordId, MatchType,
    Micros, NegativeKeyword,
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

/// App-store-style words the keywords are drawn from.
const VOCAB: [&str; 40] = [
    "photo", "editor", "free", "fitness", "workout", "cricket", "news", "music", "vpn", "budget",
    "notes", "scanner", "pdf", "recipe", "puzzle", "chess", "learn", "language", "weather", "maps",
    "video", "camera", "game", "offline", "pro", "kids", "timer", "habit", "sleep", "yoga", "bank",
    "wallet", "travel", "hotel", "flight", "taxi", "food", "delivery", "shopping", "radio",
];

/// The fixed query set: exact and broad hits, near misses and a no-match.
const QUERIES: [&str; 8] = [
    "free photo editor",
    "photo editor",
    "chess puzzle offline",
    "cricket news live",
    "vpn free fast",
    "yoga workout timer for beginners",
    "pdf scanner",
    "something nobody bids on",
];

/// Keywords per generated campaign.
const KEYWORDS_PER_CAMPAIGN: usize = 10;

/// xorshift64: a few lines of deterministic pseudo-randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        let n64 = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next() % n64).unwrap_or(0)
    }

    fn word(&mut self) -> &'static str {
        VOCAB[self.below(VOCAB.len())]
    }
}

fn corpus(keywords: usize) -> Vec<Campaign> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut next_kw = 0_i64;
    let mut campaigns = Vec::new();
    for id in 1..=i64::try_from(keywords / KEYWORDS_PER_CAMPAIGN).unwrap_or(0) {
        let mut kws = Vec::new();
        for _ in 0..KEYWORDS_PER_CAMPAIGN {
            next_kw += 1;
            let len = 1 + rng.below(3);
            let text: Vec<&str> = (0..len).map(|_| rng.word()).collect();
            // 60% broad, 40% exact, as in the seed data plan.
            let match_type = if rng.below(5) < 3 {
                MatchType::Broad
            } else {
                MatchType::Exact
            };
            let bid = 100_000 + i64::try_from(rng.below(2_000_000)).unwrap_or(0);
            kws.push(Keyword {
                id: KeywordId(next_kw),
                text: text.join(" "),
                match_type,
                max_cpt_bid: Micros(bid),
            });
        }
        campaigns.push(Campaign {
            id: CampaignId(id),
            advertiser_id: AdvertiserId(1),
            name: format!("campaign {id}"),
            status: CampaignStatus::Active,
            daily_budget: Micros(i64::MAX),
            countries: vec![Country::IN],
            age_buckets: None,
            audience_size: None,
            keywords: kws,
            negative_keywords: vec![NegativeKeyword {
                text: rng.word().to_owned(),
                match_type: MatchType::Broad,
            }],
        });
    }
    campaigns
}

fn bench_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("match");
    for keywords in [1_000, 10_000, 100_000] {
        let snapshot = match Snapshot::build(corpus(keywords), EngineConfig::default()) {
            Ok(s) => s,
            Err(e) => panic!("benchmark corpus must build: {e}"),
        };
        group.bench_with_input(
            BenchmarkId::new("query_set_of_8", keywords),
            &snapshot,
            |b, snap| {
                b.iter(|| {
                    for query in QUERIES {
                        let _ = black_box(snap.matches(black_box(query)));
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_match);
criterion_main!(benches);
