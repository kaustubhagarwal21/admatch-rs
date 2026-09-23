//! Auction benchmark: one full `Snapshot::run` (match, eligibility, rank,
//! price, budget charge) with 10, 100 and 1,000 candidates.
//!
//! Run with `cargo bench -p admatch-core --bench auction`. Every campaign
//! bids on the same broad keyword, so every campaign is a candidate. Budgets
//! are effectively unlimited so the winner always pays at the first try.

use std::hint::black_box;

use admatch_core::budget::BudgetStore;
use admatch_core::engine::{AuctionRequest, EngineConfig, Snapshot};
use admatch_core::model::{
    AdvertiserId, Campaign, CampaignId, CampaignStatus, Country, Keyword, KeywordId, MatchType,
    Micros,
};
use chrono::NaiveDate;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn campaigns(n: i64) -> Vec<Campaign> {
    (1..=n)
        .map(|id| Campaign {
            id: CampaignId(id),
            advertiser_id: AdvertiserId(1),
            name: format!("campaign {id}"),
            status: CampaignStatus::Active,
            daily_budget: Micros(i64::MAX),
            countries: vec![Country::IN],
            age_buckets: None,
            audience_size: None,
            keywords: vec![Keyword {
                id: KeywordId(id),
                text: "photo editor".to_owned(),
                match_type: MatchType::Broad,
                // Spread bids so the ranking has real work to do.
                max_cpt_bid: Micros(100_000 + (id * 7_919) % 2_000_000),
            }],
            negative_keywords: vec![],
        })
        .collect()
}

fn bench_auction(c: &mut Criterion) {
    let Some(day) = NaiveDate::from_ymd_opt(2026, 1, 1) else {
        return;
    };
    let request = AuctionRequest {
        query: "free photo editor",
        country: Country::IN,
        personalized: false,
        age_bucket: None,
        debug: false,
    };
    let mut group = c.benchmark_group("auction");
    for n in [10, 100, 1_000] {
        let snapshot = match Snapshot::build(campaigns(n), EngineConfig::default()) {
            Ok(s) => s,
            Err(e) => panic!("benchmark campaigns must build: {e}"),
        };
        let budgets = BudgetStore::in_memory();
        group.bench_with_input(BenchmarkId::new("candidates", n), &snapshot, |b, snap| {
            b.iter(|| black_box(snap.run(black_box(&request), &budgets, day)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_auction);
criterion_main!(benches);
