//! Thin wrapper over the skim fuzzy matcher.

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// Indices of `items` that match `query`, best match first. An empty query keeps everything in order.
pub fn filter<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    if query.is_empty() {
        return items.into_iter().enumerate().map(|(i, _)| i).collect();
    }
    let m = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(i64, usize)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| m.fuzzy_match(s, query).map(|score| (score, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Char positions in `text` matched by `query`, for highlighting.
pub fn positions(query: &str, text: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    SkimMatcherV2::default()
        .ignore_case()
        .fuzzy_indices(text, query)
        .map(|(_, idx)| idx)
        .unwrap_or_default()
}
