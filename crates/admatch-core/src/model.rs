//! Shared data model: IDs, money, targeting enums and campaigns.
//!
//! Every ID and every amount of money gets its own *newtype* (a one-field
//! tuple struct). At runtime `CampaignId(42)` is just an `i64`, so it costs
//! nothing, but the compiler refuses to pass a `KeywordId` where a
//! `CampaignId` is expected, or to add a bid to a relevance score. In Java
//! terms it is like wrapping a `long` in a tiny value class, without the
//! boxing cost.
//!
//! `#[serde(transparent)]` makes each newtype serialise as its bare inner
//! value, so JSON stays `"campaign_id": 42` rather than
//! `"campaign_id": {"0": 42}`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies a campaign (the `campaigns.id` column).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CampaignId(pub i64);

/// Identifies a keyword row (the `keywords.id` column).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeywordId(pub i64);

/// Identifies the advertiser that owns a campaign.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdvertiserId(pub i64);

/// Money in micros: one millionth of a currency unit (1.50 = 1_500_000).
///
/// Money is always an integer. Floats cannot represent most decimal
/// fractions exactly (0.1 + 0.2 != 0.3), and rounding errors in prices or
/// budgets add up. `i64` micros covers about 9.2 trillion units, far beyond
/// any budget here.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Micros(pub i64);

/// Relevance in basis points: 10_000 means a perfect (exact) match.
///
/// Fixed-point integers keep the auction score (`bid * relevance`) exact and
/// deterministic across machines, which floats would not guarantee.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelevanceBp(pub u32);

/// A query or keyword token interned to a small integer by the index.
///
/// Comparing and hashing a `u32` is much cheaper than comparing strings on
/// the hot path.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenId(pub u32);

/// How a keyword is compared with a query (see the matching rules in the
/// engine).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchType {
    /// The query has exactly the keyword's tokens, in any order.
    Exact,
    /// Every keyword token appears somewhere in the query.
    Broad,
}

/// Whether a campaign may serve.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CampaignStatus {
    /// Serving normally. The default, matching the database column default.
    #[default]
    Active,
    /// Kept in storage but never enters an auction.
    Paused,
}

/// Returned when a string is not one of the supported country codes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown country code {0:?}")]
pub struct UnknownCountry(pub String);

/// Returned when a string is not one of the supported age buckets.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown age bucket {0:?}")]
pub struct UnknownAgeBucket(pub String);

/// The storefront countries this service supports (ISO 3166-1 alpha-2).
///
/// A closed enum instead of a free-form string means an unsupported country
/// is rejected once, when a request is parsed, and every later `match` on a
/// country is checked for completeness by the compiler.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Country {
    /// India.
    IN,
    /// United States.
    US,
    /// United Kingdom.
    GB,
    /// Australia.
    AU,
    /// Singapore.
    SG,
    /// United Arab Emirates.
    AE,
    /// Canada.
    CA,
    /// Germany.
    DE,
    /// Japan.
    JP,
    /// New Zealand.
    NZ,
}

impl Country {
    /// Every supported country, in declaration order.
    pub const ALL: [Country; 10] = [
        Country::IN,
        Country::US,
        Country::GB,
        Country::AU,
        Country::SG,
        Country::AE,
        Country::CA,
        Country::DE,
        Country::JP,
        Country::NZ,
    ];

    /// The two-letter code, exactly as it appears in JSON and the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Country::IN => "IN",
            Country::US => "US",
            Country::GB => "GB",
            Country::AU => "AU",
            Country::SG => "SG",
            Country::AE => "AE",
            Country::CA => "CA",
            Country::DE => "DE",
            Country::JP => "JP",
            Country::NZ => "NZ",
        }
    }
}

impl fmt::Display for Country {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Country {
    type Err = UnknownCountry;

    /// Parses an exact, upper-case code such as `"IN"`. It is strict on
    /// purpose, to agree with the JSON form.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Country::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownCountry(s.to_owned()))
    }
}

/// Age ranges used for personalised targeting and reporting.
///
/// Rust identifiers cannot start with a digit or contain `-`, so each variant
/// has a readable name and a `serde(rename)` giving its wire form.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AgeBucket {
    /// Ages 18 to 24.
    #[serde(rename = "18-24")]
    Age18To24,
    /// Ages 25 to 34.
    #[serde(rename = "25-34")]
    Age25To34,
    /// Ages 35 to 44.
    #[serde(rename = "35-44")]
    Age35To44,
    /// Ages 45 to 54.
    #[serde(rename = "45-54")]
    Age45To54,
    /// Ages 55 to 64.
    #[serde(rename = "55-64")]
    Age55To64,
    /// Ages 65 and over.
    #[serde(rename = "65+")]
    Age65Plus,
}

impl AgeBucket {
    /// Every age bucket, youngest first.
    pub const ALL: [AgeBucket; 6] = [
        AgeBucket::Age18To24,
        AgeBucket::Age25To34,
        AgeBucket::Age35To44,
        AgeBucket::Age45To54,
        AgeBucket::Age55To64,
        AgeBucket::Age65Plus,
    ];

    /// The label, exactly as it appears in JSON and the database.
    pub fn as_str(self) -> &'static str {
        match self {
            AgeBucket::Age18To24 => "18-24",
            AgeBucket::Age25To34 => "25-34",
            AgeBucket::Age35To44 => "35-44",
            AgeBucket::Age45To54 => "45-54",
            AgeBucket::Age55To64 => "55-64",
            AgeBucket::Age65Plus => "65+",
        }
    }
}

impl fmt::Display for AgeBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgeBucket {
    type Err = UnknownAgeBucket;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgeBucket::ALL
            .into_iter()
            .find(|b| b.as_str() == s)
            .ok_or_else(|| UnknownAgeBucket(s.to_owned()))
    }
}

/// A keyword a campaign bids on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyword {
    /// Database ID, reported back when this keyword wins.
    pub id: KeywordId,
    /// The keyword text, e.g. `"photo editor"`.
    pub text: String,
    /// Exact or broad match.
    pub match_type: MatchType,
    /// The most the advertiser will pay for one tap on this keyword. The
    /// JSON name carries the unit so API clients cannot mistake it for whole
    /// currency units.
    #[serde(rename = "max_cpt_bid_micros")]
    pub max_cpt_bid: Micros,
}

/// A keyword that stops a campaign from serving on queries that match it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegativeKeyword {
    /// The negative keyword text, e.g. `"video"`.
    pub text: String,
    /// Exact or broad match, with the same rules as ordinary keywords.
    pub match_type: MatchType,
}

/// An advertiser's campaign: budget, targeting and keywords.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    /// Database ID.
    pub id: CampaignId,
    /// Owner of the campaign.
    pub advertiser_id: AdvertiserId,
    /// Human-readable name.
    pub name: String,
    /// Paused campaigns never serve. Defaults to active when omitted.
    #[serde(default)]
    pub status: CampaignStatus,
    /// The most this campaign may spend per UTC day.
    #[serde(rename = "daily_budget_micros")]
    pub daily_budget: Micros,
    /// Countries the campaign serves in.
    pub countries: Vec<Country>,
    /// `None` means contextual only (no age targeting). `Some` means the
    /// campaign targets these age buckets and only serves personalised
    /// requests from them.
    #[serde(default)]
    pub age_buckets: Option<Vec<AgeBucket>>,
    /// Number of people matching the targeting, set whenever `age_buckets` is
    /// set. Used by the privacy threshold check.
    #[serde(default)]
    pub audience_size: Option<i64>,
    /// Keywords the campaign bids on.
    pub keywords: Vec<Keyword>,
    /// Keywords that exclude the campaign from a query.
    #[serde(default)]
    pub negative_keywords: Vec<NegativeKeyword>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_from_str_display_and_json_agree() {
        for country in Country::ALL {
            let json = serde_json::to_string(&country).unwrap();
            assert_eq!(json, format!("\"{country}\""));
            assert_eq!(country.as_str().parse::<Country>(), Ok(country));
        }
        assert!("in".parse::<Country>().is_err());
    }

    #[test]
    fn age_bucket_from_str_display_and_json_agree() {
        for bucket in AgeBucket::ALL {
            let json = serde_json::to_string(&bucket).unwrap();
            assert_eq!(json, format!("\"{bucket}\""));
            assert_eq!(bucket.as_str().parse::<AgeBucket>(), Ok(bucket));
        }
        assert!("18-25".parse::<AgeBucket>().is_err());
    }

    #[test]
    fn campaign_uses_api_field_names() {
        let json = r#"{
            "id": 1,
            "advertiser_id": 7,
            "name": "Photo Pro India",
            "daily_budget_micros": 50000000,
            "countries": ["IN"],
            "age_buckets": ["18-24", "25-34"],
            "keywords": [
                {"id": 10, "text": "photo editor", "match_type": "broad", "max_cpt_bid_micros": 2000000}
            ],
            "negative_keywords": [{"text": "video", "match_type": "broad"}]
        }"#;
        let campaign: Campaign = serde_json::from_str(json).unwrap();
        assert_eq!(campaign.status, CampaignStatus::Active);
        assert_eq!(campaign.daily_budget, Micros(50_000_000));
        assert_eq!(campaign.keywords[0].max_cpt_bid, Micros(2_000_000));
        assert_eq!(
            campaign.age_buckets,
            Some(vec![AgeBucket::Age18To24, AgeBucket::Age25To34])
        );
        assert_eq!(campaign.audience_size, None);
    }
}
