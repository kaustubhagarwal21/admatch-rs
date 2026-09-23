//! Query normalisation: raw search text in, a list of tokens out.
//!
//! Queries and keywords go through the same function, so `"Photo-Editor!"`
//! and `"photo editor"` produce the same tokens and can match each other.

use thiserror::Error;

/// Longest accepted query, counted in Unicode characters (not bytes) before
/// normalisation. It bounds the work a single request can cause.
pub const MAX_QUERY_CHARS: usize = 200;

/// Most tokens accepted after normalisation. Real search queries are short;
/// the cap bounds index lookups per request.
pub const MAX_QUERY_TOKENS: usize = 16;

/// Why a query was rejected. The server maps each variant to a 422 response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QueryError {
    /// Nothing is left after normalisation (e.g. `""` or `"!!!"`).
    #[error("query is empty after normalisation")]
    Empty,
    /// The raw query is longer than [`MAX_QUERY_CHARS`] characters.
    #[error("query is longer than {MAX_QUERY_CHARS} characters")]
    TooLong,
    /// The query has more than [`MAX_QUERY_TOKENS`] tokens.
    #[error("query has more than {MAX_QUERY_TOKENS} tokens")]
    TooManyTokens,
}

/// Normalises a query into tokens.
///
/// Steps: Unicode-lowercase; turn every character that is not a letter or
/// digit into a space; split on whitespace (which also collapses runs of
/// spaces and trims both ends).
///
/// The length check runs first, on the raw input, so an oversized request is
/// rejected before any allocation proportional to its size.
///
/// ```
/// use admatch_core::normalize::normalize;
/// assert_eq!(normalize("  Photo-Editor, FREE! ").unwrap(), ["photo", "editor", "free"]);
/// ```
pub fn normalize(query: &str) -> Result<Vec<String>, QueryError> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(QueryError::TooLong);
    }

    let tokens = tokenize(query);

    if tokens.is_empty() {
        Err(QueryError::Empty)
    } else if tokens.len() > MAX_QUERY_TOKENS {
        Err(QueryError::TooManyTokens)
    } else {
        Ok(tokens)
    }
}

/// The normalisation rules without the request limits.
///
/// Keywords and negative keywords are split with this, so they follow exactly
/// the same rules as queries (one definition, no drift). The limits are not
/// applied here because they protect the request path, not the index build.
/// Text with no letters or digits gives an empty list.
pub fn tokenize(text: &str) -> Vec<String> {
    // `str::to_lowercase` (rather than per-character lowercasing) applies
    // context-sensitive rules such as the Greek final sigma correctly.
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();

    cleaned.split_whitespace().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_strips_punctuation_and_collapses_spaces() {
        assert_eq!(
            normalize("  Free   PHOTO-editor!! ").unwrap(),
            vec!["free", "photo", "editor"]
        );
    }

    #[test]
    fn keeps_digits_and_non_ascii_letters() {
        assert_eq!(normalize("Café 2048").unwrap(), vec!["café", "2048"]);
    }

    #[test]
    fn rejects_empty_and_punctuation_only() {
        assert_eq!(normalize(""), Err(QueryError::Empty));
        assert_eq!(normalize(" !?- "), Err(QueryError::Empty));
    }

    #[test]
    fn length_limit_counts_characters_not_bytes() {
        // 'é' is two bytes in UTF-8 but one character.
        let at_limit = "é".repeat(MAX_QUERY_CHARS);
        assert!(normalize(&at_limit).is_ok());
        let over = "a".repeat(MAX_QUERY_CHARS + 1);
        assert_eq!(normalize(&over), Err(QueryError::TooLong));
    }

    #[test]
    fn token_limit() {
        let at_limit = vec!["a"; MAX_QUERY_TOKENS].join(" ");
        assert_eq!(normalize(&at_limit).unwrap().len(), MAX_QUERY_TOKENS);
        let over = vec!["a"; MAX_QUERY_TOKENS + 1].join(" ");
        assert_eq!(normalize(&over), Err(QueryError::TooManyTokens));
    }
}
