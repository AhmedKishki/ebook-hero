//! Quote verification: does a quote occur in the book, and where.
//!
//! A quote is `exact` when it is a case-sensitive substring of the text that
//! starts and ends at a word boundary, and `equivalent` when only case,
//! punctuation or spacing differ. Words are never stemmed, so `entropies` does
//! not match `entropy`.

use std::ops::Range;

use schemars::JsonSchema;
use serde::Serialize;

use crate::analysis::{EXACT, tokens};
use crate::index::book::Book;
use crate::normalize::normalize;
use crate::passages::{Passage, PassageLocator};
use crate::search::highlight::to_char_ranges;
use crate::{Error, Result};

/// How a quote relates to the text of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The quote is a case-sensitive substring of the text, from a word start
    /// to a word end.
    Exact,
    /// Only case, punctuation or spacing differ.
    Equivalent,
    /// The quote is not in the text.
    None,
}

/// The passage that shares the longest run of consecutive words with a quote
/// that was not found. Words are compared as lowercase tokens.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Closest {
    pub passage_id: u32,
    pub locator: PassageLocator,
    pub matched_words: usize,
    pub quote_words: usize,
    /// The word of the quote right after the shared run; `None` when the run
    /// reaches the end of the quote.
    pub first_difference: Option<String>,
}

/// Where a quote was found when it is not in the passage that was asked about.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Elsewhere {
    pub passage_id: u32,
    pub locator: PassageLocator,
    pub verdict: Verdict,
}

/// The outcome of checking a quote. `matched` is a `[start, end]` range of
/// Unicode code points in the text of passage `passage_id`.
///
/// With `exact` or `equivalent`, `passage_id`, `locator` and `matched` say where
/// the quote starts. With `none`, `passage_id` and `locator` are those of the
/// passage that was asked about, if any, and `matched` is empty; then either
/// `found_elsewhere` says where the quote is, or `closest` says what comes
/// nearest. `closest` is empty when no passage shares a word with the quote.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct VerifyResult {
    pub verdict: Verdict,
    pub passage_id: Option<u32>,
    pub locator: Option<PassageLocator>,
    pub matched: Option<[usize; 2]>,
    pub closest: Option<Closest>,
    pub found_elsewhere: Option<Elsewhere>,
}

/// Checks `quote` against the passages of `book`; the result says whether it
/// occurs exactly, equivalently or not at all.
///
/// A passage is checked together with the one after it, so a quote may run on
/// into the next passage; a match belongs to the passage it starts in.
///
/// With `passage_id`, that passage is checked first. If the quote is not
/// there, the whole book is searched and a hit is reported as
/// `found_elsewhere`. Without it, the whole book is searched: the first exact
/// match wins, else the first equivalent one. When there is none, `closest`
/// names the passage that shares the longest run of words with the quote.
pub fn verify_quote(book: &Book, quote: &str, passage_id: Option<u32>) -> Result<VerifyResult> {
    verify_in(&book.passages, quote, passage_id)
}

/// A quote ready to be compared: its normalized text, for exact matching, and
/// its words, for equivalence. `words` is never empty.
struct Quote {
    normalized: String,
    words: Vec<String>,
}

impl Quote {
    fn new(quote: &str) -> Result<Quote> {
        let normalized = normalize(quote);
        if normalized.is_empty() {
            return Err(Error::InvalidArgument(
                "quote is blank; give the words to check".to_string(),
            ));
        }
        let words: Vec<String> = tokens(EXACT, &normalized)
            .into_iter()
            .map(|token| token.text)
            .collect();
        if words.is_empty() {
            return Err(Error::InvalidArgument(
                "quote has no words; give letters or digits to check".to_string(),
            ));
        }
        Ok(Quote { normalized, words })
    }
}

/// A quote found in `passages[index]`: `range` is in bytes of its text, ending
/// where the passage ends if the quote runs on.
struct Match {
    index: usize,
    verdict: Verdict,
    range: Range<usize>,
}

/// Looks for a quote in the window of one passage: `(window, own_len, quote)`
/// to a byte range of the window that starts before `own_len`.
type Finder = fn(&str, usize, &Quote) -> Option<Range<usize>>;

/// The ways to match, strictest first.
const FINDERS: [(Verdict, Finder); 2] = [
    (Verdict::Exact, find_exact),
    (Verdict::Equivalent, find_equivalent),
];

fn verify_in(passages: &[Passage], quote: &str, passage_id: Option<u32>) -> Result<VerifyResult> {
    let quote = Quote::new(quote)?;
    let Some(id) = passage_id else {
        return Ok(match scan(passages, &quote) {
            Some(found) => found_result(passages, &found),
            None => VerifyResult {
                closest: closest(passages, &quote),
                ..unverified(None)
            },
        });
    };
    let asked = passages.get(id as usize).ok_or_else(|| {
        Error::InvalidArgument(format!(
            "passage {id} does not exist; the book has {} passages",
            passages.len()
        ))
    })?;
    if let Some(found) = check(passages, id as usize, &quote) {
        return Ok(found_result(passages, &found));
    }
    Ok(match scan(passages, &quote) {
        Some(found) => VerifyResult {
            found_elsewhere: Some(Elsewhere {
                passage_id: passages[found.index].id,
                locator: passages[found.index].locator.clone(),
                verdict: found.verdict,
            }),
            ..unverified(Some(asked))
        },
        None => VerifyResult {
            closest: closest(passages, &quote),
            ..unverified(Some(asked))
        },
    })
}

/// A `none` result about the passage that was asked about, if any, with
/// nothing else filled in.
fn unverified(asked: Option<&Passage>) -> VerifyResult {
    VerifyResult {
        verdict: Verdict::None,
        passage_id: asked.map(|passage| passage.id),
        locator: asked.map(|passage| passage.locator.clone()),
        matched: None,
        closest: None,
        found_elsewhere: None,
    }
}

fn found_result(passages: &[Passage], found: &Match) -> VerifyResult {
    let passage = &passages[found.index];
    let range = to_char_ranges(&passage.text, &[(found.range.start, found.range.end)]);
    VerifyResult {
        verdict: found.verdict,
        passage_id: Some(passage.id),
        locator: Some(passage.locator.clone()),
        matched: range.first().copied(),
        closest: None,
        found_elsewhere: None,
    }
}

/// How the quote matches in the window of `passages[index]`, if it does:
/// exactly, else equivalently.
fn check(passages: &[Passage], index: usize, quote: &Quote) -> Option<Match> {
    FINDERS
        .iter()
        .find_map(|&(verdict, finder)| matched_in(passages, index, quote, verdict, finder))
}

/// The first passage whose window holds the quote exactly, else the first whose
/// window holds it equivalently.
fn scan(passages: &[Passage], quote: &Quote) -> Option<Match> {
    FINDERS.iter().find_map(|&(verdict, finder)| {
        (0..passages.len()).find_map(|index| matched_in(passages, index, quote, verdict, finder))
    })
}

fn matched_in(
    passages: &[Passage],
    index: usize,
    quote: &Quote,
    verdict: Verdict,
    finder: Finder,
) -> Option<Match> {
    let own = &passages[index].text;
    let window = match passages.get(index + 1) {
        Some(next) => format!("{own} {}", next.text),
        None => own.clone(),
    };
    let range = finder(&window, own.len(), quote)?;
    Some(Match {
        index,
        verdict,
        range: range.start..range.end.min(own.len()),
    })
}

/// Finds `quote.normalized` as a case-sensitive substring of `window` with
/// every whitespace run read as one space, starting and ending at a word
/// boundary: the characters next to the match, if any, are not alphanumeric.
/// The first such occurrence is taken, overlapping ones included. A match
/// counts only if it starts before `own_len`, the length of the passage the
/// window belongs to.
fn find_exact(window: &str, own_len: usize, quote: &Quote) -> Option<Range<usize>> {
    let (collapsed, origin) = collapse_whitespace(window);
    let wanted = quote.normalized.as_str();
    let mut from = 0;
    while let Some(offset) = collapsed[from..].find(wanted) {
        let start = from + offset;
        let end = start + wanted.len();
        if origin[start] >= own_len {
            // Every later occurrence starts later still.
            return None;
        }
        if on_word_boundaries(&collapsed, start, end) {
            return Some(origin[start]..origin[end]);
        }
        from = start + collapsed[start..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Whether `text[start..end]` has no alphanumeric character right before or
/// right after it.
fn on_word_boundaries(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

/// Finds the words of the quote as a contiguous run of the words of `window`,
/// from the start of the first to the end of the last. A match counts only if
/// it starts before `own_len`.
fn find_equivalent(window: &str, own_len: usize, quote: &Quote) -> Option<Range<usize>> {
    let found = tokens(EXACT, window);
    let run = found.windows(quote.words.len()).find(|run| {
        run.iter()
            .zip(&quote.words)
            .all(|(token, word)| token.text == *word)
    })?;
    let (start, end) = (run[0].start, run[run.len() - 1].end);
    (start < own_len).then_some(start..end)
}

/// `s` with every whitespace run replaced by one space, and the byte of `s`
/// that each byte of the result comes from (a space comes from the start of its
/// run). The last entry is `s.len()`, so `origin[collapsed.len()]` exists.
fn collapse_whitespace(s: &str) -> (String, Vec<usize>) {
    let mut collapsed = String::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len() + 1);
    let mut in_run = false;
    for (offset, c) in s.char_indices() {
        if c.is_whitespace() {
            if !in_run {
                collapsed.push(' ');
                origin.push(offset);
            }
            in_run = true;
        } else {
            in_run = false;
            collapsed.push(c);
            origin.extend(offset..offset + c.len_utf8());
        }
    }
    origin.push(s.len());
    (collapsed, origin)
}

/// The passage that shares the longest run of words with the quote; the
/// earliest on a tie. `None` when no passage shares a word.
fn closest(passages: &[Passage], quote: &Quote) -> Option<Closest> {
    let mut best: Option<(usize, usize, &Passage)> = None;
    for passage in passages {
        let words: Vec<String> = tokens(EXACT, &passage.text)
            .into_iter()
            .map(|token| token.text)
            .collect();
        let (len, end) = longest_common_run(&quote.words, &words);
        if len > best.map_or(0, |(longest, ..)| longest) {
            best = Some((len, end, passage));
        }
    }
    let (len, end, passage) = best?;
    Some(Closest {
        passage_id: passage.id,
        locator: passage.locator.clone(),
        matched_words: len,
        quote_words: quote.words.len(),
        first_difference: quote.words.get(end).cloned(),
    })
}

/// The longest run of consecutive words that `quote` and `passage` share, as
/// `(length, end)` where `end` is the index in `quote` after the run's last
/// word. Among equal runs, the one that ends earliest in `quote` wins.
fn longest_common_run(quote: &[String], passage: &[String]) -> (usize, usize) {
    let mut best = (0, 0);
    // `above[j]` is the length of the run that ends at the previous quote word
    // and at `passage[j - 1]`.
    let mut above = vec![0; passage.len() + 1];
    let mut here = vec![0; passage.len() + 1];
    for (i, word) in quote.iter().enumerate() {
        for (j, other) in passage.iter().enumerate() {
            here[j + 1] = if word == other { above[j] + 1 } else { 0 };
            if here[j + 1] > best.0 {
                best = (here[j + 1], i + 1);
            }
        }
        std::mem::swap(&mut above, &mut here);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passages(texts: &[&str]) -> Vec<Passage> {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| Passage {
                id: i as u32,
                text: text.to_string(),
                section: Vec::new(),
                blocks: Vec::new(),
                locator: PassageLocator::Epub {
                    href: "ch.xhtml".to_string(),
                    para_start: i as u32,
                    para_end: i as u32,
                    print_page: None,
                },
            })
            .collect()
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    fn quote(text: &str) -> Quote {
        Quote::new(text).unwrap()
    }

    #[test]
    fn whitespace_runs_collapse_and_keep_their_origin() {
        let (collapsed, origin) = collapse_whitespace("a \n\n b");
        assert_eq!(collapsed, "a b");
        assert_eq!(origin, [0, 1, 5, 6]);

        let (collapsed, origin) = collapse_whitespace("é  x");
        assert_eq!(collapsed, "é x");
        // Both bytes of é come from byte 0 and 1; the space run starts at 2.
        assert_eq!(origin, [0, 1, 2, 4, 5]);
    }

    #[test]
    fn leading_and_trailing_whitespace_map_to_the_run_start() {
        let (collapsed, origin) = collapse_whitespace(" ab\n");
        assert_eq!(collapsed, " ab ");
        assert_eq!(origin, [0, 1, 2, 3, 4]);
        let (collapsed, origin) = collapse_whitespace("");
        assert_eq!((collapsed.as_str(), origin), ("", vec![0]));
    }

    #[test]
    fn an_exact_match_maps_back_over_collapsed_whitespace() {
        let window = "one\n\ntwo  three";
        // "two three" is bytes 5 to 15 of the window.
        assert_eq!(find_exact(window, 99, &quote("two three")), Some(5..15));
        assert_eq!(find_exact(window, 99, &quote("one two")), Some(0..8));
        assert_eq!(find_exact(window, 99, &quote("three")), Some(10..15));
    }

    #[test]
    fn an_exact_match_is_case_sensitive() {
        assert_eq!(find_exact("Alpha beta", 99, &quote("alpha")), None);
        assert_eq!(find_exact("Alpha beta", 99, &quote("Alpha")), Some(0..5));
    }

    #[test]
    fn an_exact_match_starts_and_ends_on_word_boundaries() {
        assert_eq!(find_exact("Alpha beta", 99, &quote("lpha b")), None);
        assert_eq!(
            find_exact("entropy measures", 99, &quote("entropy measure")),
            None
        );
        assert_eq!(
            find_exact("entropy measures", 99, &quote("ntropy measures")),
            None
        );
        assert_eq!(
            find_exact("entropy measures", 99, &quote("measures")),
            Some(8..16)
        );
        // Digits are word characters too.
        assert_eq!(find_exact("x2 y", 99, &quote("x")), None);
        assert_eq!(find_exact("2x y", 99, &quote("x")), None);
    }

    #[test]
    fn an_exact_match_may_touch_punctuation_and_the_window_edges() {
        let found = |window: &str, q: &str| find_exact(window, 99, &quote(q));
        assert_eq!(
            found("In short, entropy measures.", "entropy measures"),
            Some(10..26)
        );
        assert_eq!(found("(entropy)", "entropy"), Some(1..8));
        assert_eq!(found("“entropy”", "entropy"), Some(3..10));
        assert_eq!(found("entropy", "entropy"), Some(0..7));
        assert_eq!(found("ends with it; entropy", "entropy"), Some(14..21));
        // A quote that itself ends in punctuation is followed by a space or the edge.
        assert_eq!(found("Yes. No.", "Yes."), Some(0..4));
    }

    #[test]
    fn an_exact_match_tries_every_occurrence() {
        let found = |window: &str, q: &str| find_exact(window, 99, &quote(q));
        assert_eq!(found("xentropy entropy", "entropy"), Some(9..16));
        assert_eq!(found("entropyx entropy", "entropy"), Some(9..16));
        assert_eq!(found("xentropy entropyx", "entropy"), None);
        // The second occurrence overlaps the first, which is rejected.
        assert_eq!(found("xa a a", "a a"), Some(3..6));
    }

    #[test]
    fn a_later_occurrence_must_still_start_inside_the_passage() {
        // The passage is "xentropy" (8 bytes); the second occurrence is in the successor.
        assert_eq!(find_exact("xentropy entropy", 8, &quote("entropy")), None);
        assert_eq!(
            find_exact("xentropy entropy", 10, &quote("entropy")),
            Some(9..16)
        );
    }

    #[test]
    fn an_exact_match_must_start_inside_the_passage() {
        // The passage is the first 5 bytes: "alpha". The rest is the successor.
        let window = "alpha beta gamma";
        assert_eq!(find_exact(window, 5, &quote("alpha beta")), Some(0..10));
        assert_eq!(find_exact(window, 5, &quote("beta gamma")), None);
        assert_eq!(find_exact(window, 7, &quote("beta gamma")), Some(6..16));
        // The first match decides: a later one cannot start earlier.
        assert_eq!(find_exact("ab ab", 3, &quote("ab")), Some(0..2));
        assert_eq!(find_exact("xx ab", 2, &quote("ab")), None);
    }

    #[test]
    fn exact_offsets_stay_on_character_boundaries() {
        let window = "é\n\nü x";
        let found = find_exact(window, 99, &quote("é ü")).unwrap();
        assert_eq!(&window[found], "é\n\nü");
        let window = "日本語\n\n日本 語 x";
        let found = find_exact(window, 99, &quote("日本 語")).unwrap();
        assert_eq!(&window[found], "日本 語");
        // Inside a run of CJK characters there is no word boundary.
        assert_eq!(find_exact(window, 99, &quote("語 日本")), None);
    }

    #[test]
    fn an_equivalent_match_ignores_case_punctuation_and_spacing() {
        let window = "Entropy,  it is\n\nsaid (so).";
        let found = find_equivalent(window, 99, &quote("entropy it is said")).unwrap();
        assert_eq!(&window[found], "Entropy,  it is\n\nsaid");
    }

    #[test]
    fn an_equivalent_match_never_stems() {
        assert_eq!(
            find_equivalent("entropy measures", 99, &quote("entropies measure")),
            None
        );
        assert_eq!(
            find_equivalent("entropy measures", 99, &quote("entropy measure")),
            None
        );
    }

    #[test]
    fn an_equivalent_match_is_contiguous_and_ordered() {
        assert_eq!(find_equivalent("a b c", 99, &quote("a c")), None);
        assert_eq!(find_equivalent("a b c", 99, &quote("b a")), None);
        assert_eq!(find_equivalent("a b c", 99, &quote("a b c d")), None);
    }

    #[test]
    fn an_equivalent_match_must_start_inside_the_passage() {
        let window = "alpha beta gamma";
        assert_eq!(
            find_equivalent(window, 5, &quote("ALPHA, beta")),
            Some(0..10)
        );
        assert_eq!(find_equivalent(window, 5, &quote("Beta gamma")), None);
    }

    #[test]
    fn the_longest_common_run_ends_after_its_last_quote_word() {
        let run = |q: &str, p: &str| longest_common_run(&words(q), &words(p));
        assert_eq!(run("a b c d", "x b c y"), (2, 3));
        assert_eq!(run("a b c d", "a b c d"), (4, 4));
        assert_eq!(run("a b c d", "z c d"), (2, 4));
        assert_eq!(run("a b", "x y"), (0, 0));
        assert_eq!(run("a b", ""), (0, 0));
    }

    #[test]
    fn the_longest_common_run_is_contiguous() {
        let run = |q: &str, p: &str| longest_common_run(&words(q), &words(p));
        assert_eq!(run("a b c d e", "a b x d e"), (2, 2));
        assert_eq!(run("a b c", "c b a"), (1, 1));
    }

    #[test]
    fn equal_runs_end_at_the_earliest_quote_word() {
        let run = |q: &str, p: &str| longest_common_run(&words(q), &words(p));
        assert_eq!(run("a b x c d", "c d x a b"), (2, 2));
        // The same run twice in the passage is still one run.
        assert_eq!(run("a b", "a b a b"), (2, 2));
    }

    #[test]
    fn the_longest_run_wins_over_an_earlier_shorter_one() {
        let run = |q: &str, p: &str| longest_common_run(&words(q), &words(p));
        assert_eq!(run("a b x c d e", "a b y c d e"), (3, 6));
    }

    #[test]
    fn a_quote_needs_a_word() {
        for blank in ["", " \n ", "\u{00AD}"] {
            let Err(Error::InvalidArgument(message)) = Quote::new(blank) else {
                panic!("{blank:?} should be invalid");
            };
            assert!(message.contains("blank"), "{message}");
        }
        for wordless in ["...", "—", "( )"] {
            let Err(Error::InvalidArgument(message)) = Quote::new(wordless) else {
                panic!("{wordless:?} should be invalid");
            };
            assert!(message.contains("no words"), "{message}");
        }
        let q = Quote::new("  The “Quick”\nbrown ﬁx ").unwrap();
        assert_eq!(q.normalized, "The “Quick” brown fix");
        assert_eq!(q.words, ["the", "quick", "brown", "fix"]);
    }

    #[test]
    fn scan_prefers_the_first_exact_match_over_an_earlier_equivalent_one() {
        let book = passages(&["no", "A, B", "x a b", "y"]);
        let found = scan(&book, &quote("a b")).unwrap();
        assert_eq!(
            (found.index, found.verdict, found.range),
            (2, Verdict::Exact, 2..5)
        );
        let found = scan(&book, &quote("A b")).unwrap();
        assert_eq!((found.index, found.verdict), (1, Verdict::Equivalent));
        assert!(scan(&book, &quote("a c")).is_none());
        assert!(scan(&[], &quote("a")).is_none());
    }

    #[test]
    fn scan_clips_a_match_at_the_end_of_its_passage() {
        let book = passages(&["one two", "three four"]);
        let found = scan(&book, &quote("two three")).unwrap();
        assert_eq!((found.index, found.range), (0, 4..7));
        let found = scan(&book, &quote("Two, Three")).unwrap();
        assert_eq!((found.index, found.range), (0, 4..7));
    }

    #[test]
    fn closest_takes_the_longest_run_and_the_earliest_passage() {
        let book = passages(&["a b", "a b c", "a b c x"]);
        let near = closest(&book, &quote("a b c y")).unwrap();
        assert_eq!(near.passage_id, 1);
        assert_eq!((near.matched_words, near.quote_words), (3, 4));
        assert_eq!(near.first_difference.as_deref(), Some("y"));
        assert_eq!(near.locator, book[1].locator);
    }

    #[test]
    fn closest_is_none_without_a_shared_word_or_a_passage() {
        assert!(closest(&passages(&["a b"]), &quote("c d")).is_none());
        assert!(closest(&[], &quote("c d")).is_none());
    }

    #[test]
    fn verify_in_works_on_odd_text_without_panicking() {
        let book = passages(&["日本語 e\u{0301}", "ünï — ﬁx", ""]);
        for quote in ["語", "e\u{0301}", "ünï", "x", "日本語 é ünï"] {
            let result = verify_in(&book, quote, None).unwrap();
            for id in 0..3 {
                verify_in(&book, quote, Some(id)).unwrap();
            }
            assert!(result.matched.is_none() || result.passage_id.is_some());
        }
    }

    #[test]
    fn verdicts_serialize_in_lowercase() {
        let json = |v: Verdict| serde_json::to_string(&v).unwrap();
        assert_eq!(json(Verdict::Exact), r#""exact""#);
        assert_eq!(json(Verdict::Equivalent), r#""equivalent""#);
        assert_eq!(json(Verdict::None), r#""none""#);
    }
}
