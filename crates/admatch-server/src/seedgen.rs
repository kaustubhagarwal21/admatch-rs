//! Deterministic synthetic data for the `seed` binary.
//!
//! Everything is drawn from one ChaCha RNG seeded with `--seed`, and every
//! step consumes random numbers in a fixed order, so the same seed always
//! produces byte-identical files. That makes bugs and benchmark inputs
//! reproducible.
//!
//! Floats appear only to *draw* random numbers (the log-normal bid and the
//! Zipf-like popularity). Each bid is converted to integer micros once, at
//! creation; no money arithmetic is ever done in floating point.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use admatch_core::model::{
    AdvertiserId, AgeBucket, Campaign, CampaignId, CampaignStatus, Country, Keyword, KeywordId,
    MatchType, Micros, NegativeKeyword,
};
use rand::seq::{IndexedRandom, SliceRandom};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use crate::seed_file::{AudienceCell, SeedFile};
use crate::validate::validate_campaign;

/// App-store-style vocabulary (about 300 words) that keywords and queries
/// are built from. Earlier words are drawn more often (Zipf-like), so a few
/// words such as "photo" and "free" are very common, as in real searches.
#[rustfmt::skip]
pub const VOCABULARY: &[&str] = &[
    "photo", "editor", "free", "game", "music", "video", "fitness", "workout", "news", "cricket",
    "vpn", "budget", "notes", "scanner", "pdf", "recipe", "puzzle", "chess", "learn", "language",
    "weather", "maps", "camera", "filter", "collage", "player", "radio", "podcast", "streaming",
    "movies", "tv", "shows", "sports", "football", "live", "score", "scores", "yoga", "meditation",
    "sleep", "tracker", "calorie", "diet", "running", "cycling", "steps", "health", "doctor",
    "pharmacy", "bank", "banking", "wallet", "payments", "money", "expense", "invoice",
    "accounting", "tax", "stocks", "trading", "crypto", "bitcoin", "investing", "loan", "credit",
    "card", "shopping", "fashion", "shoes", "grocery", "delivery", "food", "restaurant", "pizza",
    "coffee", "taxi", "ride", "travel", "flight", "flights", "hotel", "booking", "train", "bus",
    "metro", "translator", "dictionary", "english", "spanish", "hindi", "french", "german",
    "japanese", "kids", "baby", "parenting", "school", "homework", "math", "science", "coding",
    "programming", "python", "study", "exam", "quiz", "flashcards", "reading", "books", "ebook",
    "audiobook", "comics", "manga", "novel", "writing", "journal", "diary", "planner", "calendar",
    "todo", "reminder", "habit", "focus", "timer", "alarm", "clock", "calculator", "converter",
    "unit", "currency", "file", "manager", "cleaner", "booster", "battery", "storage", "backup",
    "cloud", "drive", "sync", "email", "mail", "chat", "messenger", "social", "dating", "friends",
    "community", "forum", "voice", "call", "reels", "keyboard", "emoji", "stickers", "wallpaper",
    "themes", "launcher", "widget", "icons", "ringtone", "recorder", "audio", "sound", "equalizer",
    "piano", "guitar", "drums", "karaoke", "dj", "mixer", "beat", "maker", "draw", "drawing",
    "paint", "sketch", "art", "design", "logo", "poster", "flyer", "resume", "cv", "jobs", "job",
    "search", "freelance", "business", "office", "docs", "sheets", "slides", "presentation",
    "meeting", "zoom", "conference", "remote", "team", "project", "tasks", "kanban", "crm",
    "sales", "marketing", "analytics", "ads", "seo", "blog", "website", "builder", "store", "shop",
    "sell", "buy", "auction", "coupons", "deals", "cashback", "rewards", "points", "loyalty",
    "car", "parking", "fuel", "ev", "charging", "navigation", "gps", "compass", "hiking",
    "camping", "fishing", "hunting", "golf", "tennis", "badminton", "basketball", "baseball",
    "hockey", "racing", "bike", "motorcycle", "cars", "simulator", "strategy", "action",
    "adventure", "rpg", "shooter", "arcade", "casual", "word", "trivia", "sudoku", "crossword",
    "solitaire", "cards", "poker", "ludo", "carrom", "board", "tiles", "match", "blast", "candy",
    "farm", "city", "tower", "defense", "war", "zombie", "survival", "craft", "block", "world",
    "pets", "dog", "cat", "horse", "garden", "plants", "home", "interior", "decor", "furniture",
    "real", "estate", "rent", "apartment", "moving", "cleaning", "laundry", "beauty", "makeup",
    "hair", "salon", "skincare", "nails", "tattoo", "selfie", "face", "body", "sticker", "hd",
    "4k", "pro", "lite", "plus", "offline", "private", "secure", "fast", "simple", "easy", "best",
    "top", "new", "daily", "smart", "tools",
];

/// Share of synthetic users per country (percent). Uneven on purpose, so
/// small countries have cells under the 5,000-person threshold.
const COUNTRY_WEIGHTS: [(Country, u32); 10] = [
    (Country::IN, 30),
    (Country::US, 25),
    (Country::GB, 8),
    (Country::DE, 8),
    (Country::JP, 8),
    (Country::CA, 6),
    (Country::AU, 5),
    (Country::SG, 4),
    (Country::AE, 4),
    (Country::NZ, 2),
];

/// Share of synthetic users per age bucket (percent).
const BUCKET_WEIGHTS: [(AgeBucket, u32); 6] = [
    (AgeBucket::Age18To24, 20),
    (AgeBucket::Age25To34, 28),
    (AgeBucket::Age35To44, 22),
    (AgeBucket::Age45To54, 15),
    (AgeBucket::Age55To64, 10),
    (AgeBucket::Age65Plus, 5),
];

/// Total synthetic population.
pub const POPULATION: i64 = 2_000_000;

/// Settings for one generation run.
#[derive(Debug, Clone, Copy)]
pub struct SeedOptions {
    /// RNG seed.
    pub seed: u64,
    /// Campaigns to generate (before validation).
    pub campaigns: usize,
    /// Request lines to generate.
    pub requests: usize,
    /// Privacy threshold used to validate targeted campaigns.
    pub k_targeting: i64,
}

/// One line of `requests.jsonl`: a complete `/v1/match` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestLine {
    /// Search text.
    pub query: String,
    /// Storefront country.
    pub country: Country,
    /// Synthetic pseudonymous user ID.
    pub user_id: i64,
    /// Whether personalised ads are allowed.
    pub personalized: bool,
    /// Age bucket, set only for personalised requests.
    pub age_bucket: Option<AgeBucket>,
    /// Always false; the load generator measures the normal path.
    pub debug: bool,
}

/// Everything one run produces, plus counts to print.
#[derive(Debug, Clone)]
pub struct Generated {
    /// Contents of `seed.json`.
    pub file: SeedFile,
    /// Contents of `requests.jsonl`.
    pub requests: Vec<RequestLine>,
    /// Campaigns rejected by validation, by rejection code.
    pub rejected: BTreeMap<&'static str, usize>,
    /// Requests generated, by kind (exact, broad, negative, no_match).
    pub requests_by_kind: BTreeMap<&'static str, usize>,
}

/// Builds the audience table: `POPULATION × country share × bucket share`.
/// With these weights every cell is an exact integer and the cells add up to
/// exactly `POPULATION`.
pub fn audience_cells() -> Vec<AudienceCell> {
    let country_total: i64 = COUNTRY_WEIGHTS.iter().map(|&(_, w)| i64::from(w)).sum();
    let bucket_total: i64 = BUCKET_WEIGHTS.iter().map(|&(_, w)| i64::from(w)).sum();
    let mut cells = Vec::with_capacity(COUNTRY_WEIGHTS.len() * BUCKET_WEIGHTS.len());
    for &(country, cw) in &COUNTRY_WEIGHTS {
        for &(age_bucket, bw) in &BUCKET_WEIGHTS {
            let users = POPULATION * i64::from(cw) * i64::from(bw) / (country_total * bucket_total);
            cells.push(AudienceCell {
                country,
                age_bucket,
                users,
            });
        }
    }
    cells
}

/// Runs the whole generation.
pub fn generate(opts: SeedOptions) -> Generated {
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);
    let audience_counts = audience_cells();
    let file_for_table = SeedFile {
        seed: opts.seed,
        campaigns: Vec::new(),
        audience_counts: audience_counts.clone(),
    };
    let table = file_for_table.audience_table();

    let mut campaigns = Vec::new();
    let mut rejected: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut next_keyword_id: i64 = 1;
    for _ in 0..opts.campaigns {
        // IDs are assigned only to accepted campaigns, like a database would.
        let next_id = i64::try_from(campaigns.len()).unwrap_or(i64::MAX) + 1;
        let mut campaign = random_campaign(&mut rng, CampaignId(next_id), next_keyword_id);
        match validate_campaign(&campaign, &table, opts.k_targeting) {
            Ok(size) => {
                campaign.audience_size = size;
                next_keyword_id += i64::try_from(campaign.keywords.len()).unwrap_or(0);
                campaigns.push(campaign);
            }
            Err(reason) => *rejected.entry(reason.code()).or_default() += 1,
        }
    }

    let (requests, requests_by_kind) = random_requests(&mut rng, &campaigns, opts.requests);
    Generated {
        file: SeedFile {
            seed: opts.seed,
            campaigns,
            audience_counts,
        },
        requests,
        rejected,
        requests_by_kind,
    }
}

/// Picks an item with probability proportional to its weight.
fn pick_weighted<T: Copy>(rng: &mut ChaCha8Rng, items: &[(T, u32)]) -> T {
    let total: u32 = items.iter().map(|&(_, w)| w).sum();
    let mut roll = rng.random_range(0..total);
    for &(item, weight) in items {
        if roll < weight {
            return item;
        }
        roll -= weight;
    }
    // Unreachable because roll < total; the first item is a safe fallback.
    items[0].0
}

/// A Zipf-like index in `0..n`: index `i` is drawn with probability roughly
/// proportional to `1 / (i + 1)`. Trick: `n^u` for uniform `u` is
/// log-uniform, whose density falls off as `1/x`.
fn zipf_index(rng: &mut ChaCha8Rng, n: usize) -> usize {
    let u: f64 = rng.random();
    let x = (n as f64).powf(u) as usize;
    x.saturating_sub(1).min(n.saturating_sub(1))
}

/// A log-normal bid centred on 1,000,000 micros (1.00), rounded to whole
/// thousands of micros and clamped to [20,000, 20,000,000]. Some bids fall
/// below the default reserve (100,000), which exercises the reserve rule.
fn random_bid(rng: &mut ChaCha8Rng) -> Micros {
    // Box-Muller: two uniforms in, one standard normal out.
    let u1: f64 = 1.0 - rng.random::<f64>(); // (0, 1], so ln(u1) is finite
    let u2: f64 = rng.random();
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    let bid = 1_000_000.0 * (0.6 * z).exp();
    let thousands = (bid / 1_000.0).round().clamp(20.0, 20_000.0) as i64;
    Micros(thousands * 1_000)
}

/// Words for one keyword: 1 to 3 distinct vocabulary words.
fn random_phrase(rng: &mut ChaCha8Rng) -> Vec<&'static str> {
    let len = pick_weighted(rng, &[(1_usize, 40), (2, 45), (3, 15)]);
    let mut words: Vec<&'static str> = Vec::with_capacity(len);
    while words.len() < len {
        let word = VOCABULARY[zipf_index(rng, VOCABULARY.len())];
        if !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// One random campaign. Keyword IDs start at `first_keyword_id`.
fn random_campaign(rng: &mut ChaCha8Rng, id: CampaignId, first_keyword_id: i64) -> Campaign {
    let mut keywords = Vec::new();
    let mut seen_texts = HashSet::new();
    let mut keyword_words: HashSet<&'static str> = HashSet::new();
    let target = rng.random_range(5..=20);
    let mut kid = first_keyword_id;
    while keywords.len() < target {
        let words = random_phrase(rng);
        let text = words.join(" ");
        if !seen_texts.insert(text.clone()) {
            continue;
        }
        keyword_words.extend(words.iter().flat_map(|w| w.split(' ')));
        let match_type = if rng.random_ratio(60, 100) {
            MatchType::Broad
        } else {
            MatchType::Exact
        };
        keywords.push(Keyword {
            id: KeywordId(kid),
            text,
            match_type,
            max_cpt_bid: random_bid(rng),
        });
        kid += 1;
    }

    // Negative keywords never reuse one of the campaign's own keyword words,
    // which would block the campaign's own keywords.
    let mut negative_keywords = Vec::new();
    for _ in 0..rng.random_range(0..=3) {
        let word = VOCABULARY[rng.random_range(0..VOCABULARY.len())];
        if word.split(' ').any(|w| keyword_words.contains(w))
            || negative_keywords
                .iter()
                .any(|n: &NegativeKeyword| n.text == word)
        {
            continue;
        }
        let match_type = if rng.random_ratio(80, 100) {
            MatchType::Broad
        } else {
            MatchType::Exact
        };
        negative_keywords.push(NegativeKeyword {
            text: word.to_owned(),
            match_type,
        });
    }

    let mut countries: BTreeSet<Country> = BTreeSet::new();
    let n_countries = rng.random_range(1..=3);
    while countries.len() < n_countries {
        countries.insert(pick_weighted(rng, &COUNTRY_WEIGHTS));
    }
    let mut countries: Vec<Country> = countries.into_iter().collect();

    // 20% use age targeting. A quarter of those are deliberately narrow (one
    // small country, one older bucket) so the privacy rule has work to do.
    let age_buckets = if rng.random_ratio(20, 100) {
        if rng.random_ratio(25, 100) {
            let small = [Country::NZ, Country::AE, Country::SG, Country::AU];
            countries = vec![*small.choose(rng).unwrap_or(&Country::NZ)];
            let older = [AgeBucket::Age55To64, AgeBucket::Age65Plus];
            Some(vec![*older.choose(rng).unwrap_or(&AgeBucket::Age65Plus)])
        } else {
            let mut buckets: BTreeSet<AgeBucket> = BTreeSet::new();
            let n = rng.random_range(1..=3);
            while buckets.len() < n {
                buckets.insert(pick_weighted(rng, &BUCKET_WEIGHTS));
            }
            Some(buckets.into_iter().collect())
        }
    } else {
        None
    };

    let status = if rng.random_ratio(95, 100) {
        CampaignStatus::Active
    } else {
        CampaignStatus::Paused
    };
    let name_word = VOCABULARY[rng.random_range(0..VOCABULARY.len())];
    Campaign {
        id,
        advertiser_id: AdvertiserId(rng.random_range(1..=2_000)),
        name: format!("{name_word} campaign {}", id.0),
        status,
        // 10 to 500 currency units per day, in whole units.
        daily_budget: Micros(rng.random_range(10_i64..=500) * 1_000_000),
        countries,
        age_buckets,
        audience_size: None,
        keywords,
        negative_keywords,
    }
}

/// Syllables for made-up words that are guaranteed not to match anything.
const NONSENSE: &[&str] = &["zor", "qua", "vex", "plim", "drob", "kest", "yul", "frin"];

/// Generates request lines: 40% exact hits, 30% broad hits, 10% queries
/// that hit a negative keyword and 20% queries that match nothing. The
/// keyword behind each request is picked Zipf-like, so a few keywords are
/// very popular and most are rare.
fn random_requests(
    rng: &mut ChaCha8Rng,
    campaigns: &[Campaign],
    count: usize,
) -> (Vec<RequestLine>, BTreeMap<&'static str, usize>) {
    let mut lines = Vec::with_capacity(count);
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let pool: Vec<(usize, usize)> = campaigns
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| (0..c.keywords.len()).map(move |ki| (ci, ki)))
        .collect();

    for _ in 0..count {
        let mut kind = pick_weighted(
            rng,
            &[
                ("exact", 40),
                ("broad", 30),
                ("negative", 10),
                ("no_match", 20),
            ],
        );
        if pool.is_empty() {
            kind = "no_match";
        }
        let mut country = pick_weighted(rng, &COUNTRY_WEIGHTS);
        let query = if kind == "no_match" {
            let n = rng.random_range(1..=3);
            (0..n)
                .map(|_| {
                    let a = NONSENSE[rng.random_range(0..NONSENSE.len())];
                    let b = NONSENSE[rng.random_range(0..NONSENSE.len())];
                    format!("{a}{b}")
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            let (ci, ki) = pool[zipf_index(rng, pool.len())];
            let campaign = &campaigns[ci];
            // 80% of hits come from a country the campaign serves.
            if rng.random_ratio(80, 100) {
                country = *campaign.countries.choose(rng).unwrap_or(&country);
            }
            let mut words: Vec<String> = campaign.keywords[ki]
                .text
                .split(' ')
                .map(str::to_owned)
                .collect();
            if kind == "broad" {
                for _ in 0..rng.random_range(1..=2) {
                    words.push(VOCABULARY[zipf_index(rng, VOCABULARY.len())].to_owned());
                }
            }
            if kind == "negative" {
                match campaign.negative_keywords.choose(rng) {
                    Some(neg) => words.push(neg.text.clone()),
                    None => kind = "broad",
                }
            }
            // Word order is shuffled: exact match ignores order.
            words.shuffle(rng);
            words.join(" ")
        };

        let personalized = rng.random_ratio(60, 100);
        let age_bucket = personalized.then(|| pick_weighted(rng, &BUCKET_WEIGHTS));
        lines.push(RequestLine {
            query,
            country,
            user_id: rng.random_range(1..=POPULATION),
            personalized,
            age_bucket,
            debug: false,
        });
        *by_kind.entry(kind).or_default() += 1;
    }
    (lines, by_kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(seed: u64) -> SeedOptions {
        SeedOptions {
            seed,
            campaigns: 300,
            requests: 500,
            k_targeting: 5_000,
        }
    }

    #[test]
    fn same_seed_gives_identical_output() {
        let a = generate(opts(7));
        let b = generate(opts(7));
        assert_eq!(a.file, b.file);
        assert_eq!(a.requests, b.requests);
        let c = generate(opts(8));
        assert_ne!(a.file, c.file);
    }

    #[test]
    fn population_adds_up_and_has_small_cells() {
        let cells = audience_cells();
        assert_eq!(cells.len(), 60);
        assert_eq!(cells.iter().map(|c| c.users).sum::<i64>(), POPULATION);
        assert!(cells.iter().any(|c| c.users <= 5_000));
    }

    #[test]
    fn narrow_targeting_is_rejected_and_the_rest_is_valid() {
        let g = generate(opts(42));
        assert!(g.rejected.get("audience_too_small").copied().unwrap_or(0) > 0);
        let total_rejected: usize = g.rejected.values().sum();
        assert_eq!(g.file.campaigns.len() + total_rejected, 300);
        for c in &g.file.campaigns {
            assert!((5..=20).contains(&c.keywords.len()));
            if c.age_buckets.is_some() {
                assert!(c.audience_size.unwrap_or(0) > 5_000);
            }
        }
        assert_eq!(g.requests.len(), 500);
    }

    #[test]
    fn ids_are_unique_and_contiguous() {
        let g = generate(opts(1));
        for (i, c) in g.file.campaigns.iter().enumerate() {
            assert_eq!(c.id.0, i as i64 + 1);
        }
        let mut ids = HashSet::new();
        for k in g.file.campaigns.iter().flat_map(|c| &c.keywords) {
            assert!(ids.insert(k.id));
        }
    }
}
