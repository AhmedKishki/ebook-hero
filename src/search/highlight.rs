//! Match ranges inside a passage's text, for highlighting, `find` and quote
//! verification. Offsets are UTF-8 byte offsets; `to_char_ranges` converts
//! them to code points for output.

use std::collections::HashSet;

use crate::analysis::{EXACT, STEMMED, tokens};
use crate::search::query::ParsedQuery;

/// Byte ranges of `text` that match `q`: tokens whose stem equals a query
/// word's stem, and every occurrence of each phrase. Sorted, overlaps merged.
pub fn match_ranges(text: &str, q: &ParsedQuery) -> Vec<(usize, usize)> {
    let stems: HashSet<String> = q
        .words
        .iter()
        .flat_map(|word| tokens(STEMMED, word))
        .map(|token| token.text)
        .collect();
    let mut ranges: Vec<(usize, usize)> = tokens(STEMMED, text)
        .into_iter()
        .filter(|token| stems.contains(&token.text))
        .map(|token| (token.start, token.end))
        .collect();
    for phrase in &q.phrases {
        ranges.extend(phrase_ranges(text, phrase));
    }
    merge(ranges)
}

/// Byte ranges of the occurrences of `phrase` in `text`: runs of exact tokens
/// equal to the phrase's, from the first token's start to the last token's end.
pub fn phrase_ranges(text: &str, phrase: &str) -> Vec<(usize, usize)> {
    let wanted = tokens(EXACT, phrase);
    let Some(first) = wanted.first() else {
        return Vec::new();
    };
    // Each token must sit at the phrase's own offset from the first one, so a
    // phrase never spans a token the analyzer dropped unless it contains it too.
    tokens(EXACT, text)
        .windows(wanted.len())
        .filter(|run| {
            run.iter().zip(&wanted).all(|(token, want)| {
                token.text == want.text
                    && token.position - run[0].position == want.position - first.position
            })
        })
        .map(|run| (run[0].start, run[run.len() - 1].end))
        .collect()
}

/// Converts byte ranges of `text` into Unicode code-point `[start, end]` pairs.
pub fn to_char_ranges(text: &str, ranges: &[(usize, usize)]) -> Vec<[usize; 2]> {
    let starts: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    let chars_before = |byte: usize| starts.partition_point(|&start| start < byte);
    ranges
        .iter()
        .map(|&(start, end)| [chars_before(start), chars_before(end)])
        .collect()
}

/// Sorts `ranges` and joins those that overlap or touch.
fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::query::parse_query;

    const P1: &str = "Mutual information between channels.";

    #[test]
    fn match_ranges_cover_words_and_phrases() {
        let q = ParsedQuery {
            words: vec!["channels".into()],
            phrases: vec!["mutual information".into()],
        };
        assert_eq!(match_ranges(P1, &q), [(0, 18), (27, 35)]);
    }

    #[test]
    fn words_match_by_stem() {
        let q = parse_query("channel");
        assert_eq!(match_ranges(P1, &q), [(27, 35)]);
    }

    #[test]
    fn overlapping_matches_merge() {
        let q = parse_query(r#"information "mutual information""#);
        assert_eq!(match_ranges(P1, &q), [(0, 18)]);
    }

    #[test]
    fn no_match_gives_no_ranges() {
        assert!(match_ranges(P1, &parse_query("entropy")).is_empty());
        assert!(match_ranges(P1, &ParsedQuery::default()).is_empty());
    }

    #[test]
    fn phrases_are_exact_not_stemmed() {
        assert_eq!(phrase_ranges(P1, "mutual  INFORMATION"), [(0, 18)]);
        assert!(phrase_ranges(P1, "mutual informations").is_empty());
        assert!(phrase_ranges(P1, "information mutual").is_empty());
        assert!(phrase_ranges(P1, "...").is_empty());
    }

    #[test]
    fn phrase_occurrences_may_overlap() {
        assert_eq!(phrase_ranges("a a a", "a a"), [(0, 3), (2, 5)]);
        let q = ParsedQuery {
            words: vec![],
            phrases: vec!["a a".into()],
        };
        assert_eq!(match_ranges("a a a", &q), [(0, 5)]);
    }

    #[test]
    fn a_phrase_does_not_span_a_dropped_long_token() {
        let long = "x".repeat(45);
        let text = format!("foo {long} bar");
        assert!(phrase_ranges(&text, "foo bar").is_empty());
        assert_eq!(
            phrase_ranges(&text, &format!("foo {long} bar")),
            [(0, text.len())]
        );
    }

    #[test]
    fn char_ranges_count_code_points() {
        assert_eq!(to_char_ranges("é x", &[(3, 4)]), [[2, 3]]);
    }

    #[test]
    fn char_ranges_keep_order_and_survive_odd_offsets() {
        assert_eq!(
            to_char_ranges("日本語 ok", &[(0, 9), (10, 12)]),
            [[0, 3], [4, 6]]
        );
        assert_eq!(to_char_ranges("é", &[(0, 99)]), [[0, 1]]);
        assert!(to_char_ranges("é", &[]).is_empty());
    }
}
