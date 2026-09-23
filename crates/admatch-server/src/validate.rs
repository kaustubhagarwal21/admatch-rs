//! Campaign validation, shared by the seed generator and (from the admin API
//! milestone on) `POST /v1/campaigns`, so both apply exactly the same rules.

use admatch_core::model::Campaign;
use admatch_core::normalize::normalize;
use admatch_core::privacy::{AudienceCounts, audience_size, check_targeting};
use thiserror::Error;

/// Why a campaign was refused. Each variant maps to a stable API error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Error)]
pub enum CampaignRejection {
    /// The daily budget is zero or negative.
    #[error("daily budget must be positive")]
    NonPositiveBudget,
    /// The campaign targets no country, so it could never serve.
    #[error("at least one country is required")]
    NoCountries,
    /// The campaign has no keywords, so it could never match.
    #[error("at least one keyword is required")]
    NoKeywords,
    /// A keyword or negative keyword normalises to nothing or is too long.
    #[error("keyword text is empty or too long")]
    InvalidKeywordText,
    /// A keyword bid is zero or negative.
    #[error("keyword bids must be positive")]
    NonPositiveBid,
    /// `age_buckets` is present but empty. Contextual campaigns omit it.
    #[error("age_buckets must not be empty; omit it for a contextual campaign")]
    EmptyAgeBuckets,
    /// The targeted audience is at or below the privacy threshold. The exact
    /// size is deliberately not reported.
    #[error("audience too small for personalised targeting")]
    AudienceTooSmall,
}

impl CampaignRejection {
    /// The API error code for this rejection.
    pub fn code(self) -> &'static str {
        match self {
            CampaignRejection::NonPositiveBudget => "invalid_budget",
            CampaignRejection::NoCountries => "no_countries",
            CampaignRejection::NoKeywords => "no_keywords",
            CampaignRejection::InvalidKeywordText => "invalid_keyword",
            CampaignRejection::NonPositiveBid => "invalid_bid",
            CampaignRejection::EmptyAgeBuckets => "empty_age_buckets",
            CampaignRejection::AudienceTooSmall => "audience_too_small",
        }
    }
}

/// Checks a campaign and, for an age-targeted one, computes its audience
/// size from `counts`.
///
/// Returns `Ok(Some(size))` for an accepted targeted campaign, `Ok(None)` for
/// an accepted contextual one. The caller stores the size on the campaign.
pub fn validate_campaign(
    campaign: &Campaign,
    counts: &AudienceCounts,
    k_targeting: i64,
) -> Result<Option<i64>, CampaignRejection> {
    if campaign.daily_budget.0 <= 0 {
        return Err(CampaignRejection::NonPositiveBudget);
    }
    if campaign.countries.is_empty() {
        return Err(CampaignRejection::NoCountries);
    }
    if campaign.keywords.is_empty() {
        return Err(CampaignRejection::NoKeywords);
    }
    // Keywords go through the same normaliser as queries, so a keyword that
    // could never match a valid query is refused up front.
    let texts = campaign
        .keywords
        .iter()
        .map(|k| k.text.as_str())
        .chain(campaign.negative_keywords.iter().map(|n| n.text.as_str()));
    for text in texts {
        if normalize(text).is_err() {
            return Err(CampaignRejection::InvalidKeywordText);
        }
    }
    if campaign.keywords.iter().any(|k| k.max_cpt_bid.0 <= 0) {
        return Err(CampaignRejection::NonPositiveBid);
    }

    match &campaign.age_buckets {
        None => Ok(None),
        Some(buckets) if buckets.is_empty() => Err(CampaignRejection::EmptyAgeBuckets),
        Some(buckets) => {
            let size = audience_size(counts, &campaign.countries, buckets);
            check_targeting(size, k_targeting).map_err(|_| CampaignRejection::AudienceTooSmall)?;
            Ok(Some(size))
        }
    }
}

#[cfg(test)]
mod tests {
    use admatch_core::model::{
        AdvertiserId, AgeBucket, CampaignId, CampaignStatus, Country, Keyword, KeywordId,
        MatchType, Micros,
    };

    use super::*;

    fn campaign() -> Campaign {
        Campaign {
            id: CampaignId(1),
            advertiser_id: AdvertiserId(1),
            name: "c".to_owned(),
            status: CampaignStatus::Active,
            daily_budget: Micros(1_000_000),
            countries: vec![Country::IN],
            age_buckets: None,
            audience_size: None,
            keywords: vec![Keyword {
                id: KeywordId(1),
                text: "photo editor".to_owned(),
                match_type: MatchType::Broad,
                max_cpt_bid: Micros(500_000),
            }],
            negative_keywords: vec![],
        }
    }

    fn counts() -> AudienceCounts {
        let mut c = AudienceCounts::new();
        c.insert(Country::IN, AgeBucket::Age18To24, 6_000);
        c.insert(Country::IN, AgeBucket::Age65Plus, 5_000);
        c
    }

    #[test]
    fn contextual_campaign_is_accepted_without_audience() {
        assert_eq!(validate_campaign(&campaign(), &counts(), 5_000), Ok(None));
    }

    #[test]
    fn targeting_above_k_is_accepted_with_its_size() {
        let mut c = campaign();
        c.age_buckets = Some(vec![AgeBucket::Age18To24]);
        assert_eq!(validate_campaign(&c, &counts(), 5_000), Ok(Some(6_000)));
    }

    #[test]
    fn targeting_at_k_is_rejected() {
        let mut c = campaign();
        c.age_buckets = Some(vec![AgeBucket::Age65Plus]);
        assert_eq!(
            validate_campaign(&c, &counts(), 5_000),
            Err(CampaignRejection::AudienceTooSmall)
        );
    }

    #[test]
    fn structural_problems_are_rejected() {
        let mut c = campaign();
        c.daily_budget = Micros(0);
        assert_eq!(
            validate_campaign(&c, &counts(), 5_000),
            Err(CampaignRejection::NonPositiveBudget)
        );

        let mut c = campaign();
        c.keywords[0].text = "!!!".to_owned();
        assert_eq!(
            validate_campaign(&c, &counts(), 5_000),
            Err(CampaignRejection::InvalidKeywordText)
        );

        let mut c = campaign();
        c.age_buckets = Some(vec![]);
        assert_eq!(
            validate_campaign(&c, &counts(), 5_000),
            Err(CampaignRejection::EmptyAgeBuckets)
        );
    }
}
