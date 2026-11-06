//! Text normalization shared by every reader and by quote verification.
//!
//! A reader that marks spans on a block's text calls [`normalize_with_spans`],
//! which normalizes the text and moves the spans onto the result. [`normalize`]
//! takes the same steps without spans, so a block's text equals `normalize` of
//! itself whichever of the two wrote it.

use std::cmp::Reverse;
use std::ops::Range;

use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

use crate::document::{Span, SpanKind};

/// Normalizes text in this order: expand the Latin ligatures U+FB00 to U+FB06
/// to their letters; drop soft hyphens, zero-width spaces, byte-order marks and
/// every control character that is not whitespace; compose to NFC; collapse
/// every whitespace run to one space and trim. Composing after the drops means
/// a combining mark that follows a dropped character is composed with its base,
/// so the output is NFC and a second call returns it unchanged.
pub fn normalize(text: &str) -> String {
    let (expanded, _) = expand(text, &[]);
    let (composed, _) = compose(&expanded, &[]);
    collapse(&composed, &[]).0
}

/// A span over a reader's raw text, before normalization: the bytes it covers
/// and what it marks.
#[derive(Debug)]
pub(crate) struct RawSpan {
    /// The bytes of the raw text it covers, on character boundaries. A point,
    /// such as a note reference, is an empty range.
    pub(crate) range: Range<usize>,
    pub(crate) kind: SpanKind,
}

/// `raw` normalized as [`normalize`] does it, with `spans` moved onto the
/// result and counted in code points.
///
/// `spans` come in the order they open in `raw`, so that of two spans over the
/// same text the outer one comes first. Each step of the normalization records
/// where the offsets of the spans land. An offset inside what a step merges,
/// such as a character and the combining marks composed with it, or a run of
/// whitespace after its first character, lands past the merged result. A span
/// that covered only what the steps removed therefore comes out empty and is
/// dropped, while a `NoteRef` or a `PageBreak` marks its point anyway.
///
/// A `Break` covers a whitespace character of `raw` and marks the one space
/// that the character's whitespace run becomes, so breaks that land on one
/// space become one break whose `lines` add up. A break whose run is trimmed
/// at either end of the text is dropped. The spans come out sorted by start,
/// the longer first.
pub(crate) fn normalize_with_spans(raw: &str, spans: Vec<RawSpan>) -> (String, Vec<Span>) {
    // Every offset a span needs, ascending, so each step moves them all in
    // its one pass over the text.
    let mut offsets: Vec<usize> = spans
        .iter()
        .flat_map(|span| [span.range.start, span.range.end])
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    let (expanded, positions) = expand(raw, &offsets);
    let (composed, positions) = compose(&expanded, &positions);
    let (text, landings) = collapse(&composed, &positions);
    let landing = |offset: usize| landings[offsets.partition_point(|&o| o < offset)];

    let mut moved: Vec<Span> = Vec::with_capacity(spans.len());
    // The last break written, as an index into `moved`.
    let mut last_break: Option<usize> = None;
    for RawSpan { range, kind } in spans {
        let start = landing(range.start);
        match kind {
            SpanKind::Break { lines } => {
                let Some(space) = start.space else {
                    continue;
                };
                if let Some(index) = last_break
                    && moved[index].start == space
                    && let SpanKind::Break { lines: total } = &mut moved[index].kind
                {
                    *total += lines;
                    continue;
                }
                last_break = Some(moved.len());
                moved.push(Span {
                    start: space,
                    end: space + 1,
                    kind,
                });
            }
            SpanKind::NoteRef { .. } | SpanKind::PageBreak { .. } => moved.push(Span {
                start: start.at,
                end: start.at,
                kind,
            }),
            kind => {
                let end = landing(range.end).at;
                if end > start.at {
                    moved.push(Span {
                        start: start.at,
                        end,
                        kind,
                    });
                }
            }
        }
    }
    // A stable sort, so the outer of two spans over the same text stays first.
    moved.sort_by_key(|span| (span.start, Reverse(span.end)));
    (text, moved)
}

/// The first step: ligatures expanded and invisible characters dropped. Each
/// of `offsets`, ascending byte offsets of `text`, moves to the number of
/// characters written before the character at it.
fn expand(text: &str, offsets: &[usize]) -> (String, Vec<usize>) {
    let count = offsets.len();
    let mut expanded = String::with_capacity(text.len());
    let mut written = 0;
    let mut moved = Vec::with_capacity(count);
    let mut offsets = offsets.iter().peekable();
    for (offset, c) in text.char_indices() {
        while offsets.next_if(|&&o| o <= offset).is_some() {
            moved.push(written);
        }
        match ligature_letters(c) {
            Some(letters) => {
                expanded.push_str(letters);
                // The letters are ASCII, one byte each.
                written += letters.len();
            }
            None if is_dropped(c) => {}
            None => {
                expanded.push(c);
                written += 1;
            }
        }
    }
    moved.resize(count, written);
    (expanded, moved)
}

/// The second step: composition to NFC. The text is cut into pieces before
/// each character for which [`starts_piece`] holds, and each piece is
/// composed alone, which gives what one pass over the whole text gives. Each
/// of `positions`, ascending character positions in `expanded`, moves to the
/// start of the composed piece it starts, or past the composed piece it lies
/// inside.
fn compose(expanded: &str, positions: &[usize]) -> (String, Vec<usize>) {
    let count = positions.len();
    let mut composed = String::with_capacity(expanded.len());
    let mut written = 0;
    let mut moved = Vec::with_capacity(count);
    let mut positions = positions.iter().peekable();
    let mut piece = String::new();
    for (index, c) in expanded.chars().enumerate() {
        if index == 0 || starts_piece(c) {
            written += compose_piece(&mut piece, &mut composed);
            while positions.next_if(|&&p| p <= index).is_some() {
                moved.push(written);
            }
        }
        piece.push(c);
    }
    written += compose_piece(&mut piece, &mut composed);
    moved.resize(count, written);
    (composed, moved)
}

/// Appends `piece` in NFC to `composed`, empties it and returns how many
/// characters it appended.
fn compose_piece(piece: &mut String, composed: &mut String) -> usize {
    let mut chars = piece.chars();
    let written = match (chars.next(), chars.next()) {
        // A piece of one character that starts a piece, as most are, is NFC
        // already.
        (Some(c), None) if starts_piece(c) => {
            composed.push(c);
            1
        }
        _ => {
            let before = composed.len();
            composed.extend(piece.nfc());
            composed[before..].chars().count()
        }
    };
    piece.clear();
    written
}

/// Whether NFC composes the text before `c` and the text from `c` on apart:
/// `c` has canonical combining class 0 and passes the NFC quick check, so it
/// is a starter that composes with nothing before it. Its decomposition then
/// starts with such a starter as well, which a test checks for every
/// character, so nothing before `c` is composed with or reordered across
/// anything from `c` on.
fn starts_piece(c: char) -> bool {
    c.is_ascii()
        || (canonical_combining_class(c) == 0
            && is_nfc_quick(std::iter::once(c)) == IsNormalized::Yes)
}

/// Where an offset of the raw text lands in the normalized text.
#[derive(Debug, Clone, Copy)]
struct Landing {
    /// The position, in code points.
    at: usize,
    /// The space that the whitespace run holding the offset became, when the
    /// run lies between two words and is not trimmed.
    space: Option<usize>,
}

/// The last step: every whitespace run collapsed to one space, and the ends
/// trimmed. Each of `positions`, ascending character positions in
/// `composed`, lands on the space its run became when it is at the run's
/// first character, past that space when it is further inside the run, and
/// where the text then is when its run is trimmed.
fn collapse(composed: &str, positions: &[usize]) -> (String, Vec<Landing>) {
    let count = positions.len();
    let mut text = String::with_capacity(composed.len());
    let mut written = 0;
    let mut landed = Vec::with_capacity(count);
    let mut positions = positions.iter().copied().peekable();
    // Where the whitespace run since the last word starts, and for each
    // position in the run whether it is at the run's start. Whether the run
    // becomes a space is known only at the next word.
    let mut run = None;
    let mut in_run = Vec::new();
    for (index, c) in composed.chars().enumerate() {
        if c.is_whitespace() {
            if written == 0 {
                // Before the first word, trimmed.
                while positions.next_if(|&p| p <= index).is_some() {
                    landed.push(Landing { at: 0, space: None });
                }
            } else {
                let start = *run.get_or_insert(index);
                while let Some(p) = positions.next_if(|&p| p <= index) {
                    in_run.push(p == start);
                }
            }
            continue;
        }
        if run.take().is_some() {
            let space = written;
            text.push(' ');
            written += 1;
            landed.extend(in_run.drain(..).map(|at_start| Landing {
                at: if at_start { space } else { space + 1 },
                space: Some(space),
            }));
        }
        while positions.next_if(|&p| p <= index).is_some() {
            landed.push(Landing {
                at: written,
                space: None,
            });
        }
        text.push(c);
        written += 1;
    }
    // The positions in a run after the last word, which is trimmed, and those
    // at the end land at the end.
    landed.resize(
        count,
        Landing {
            at: written,
            space: None,
        },
    );
    (text, landed)
}

fn ligature_letters(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        _ => None,
    }
}

fn is_dropped(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200B}' | '\u{FEFF}') || (c.is_control() && !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use unicode_normalization::char::decompose_canonical;

    use super::*;
    use crate::document::{Block, BlockKind, Locator, span_errors};

    /// A span of `kind` over the first `part` of `raw`.
    fn over(raw: &str, part: &str, kind: SpanKind) -> RawSpan {
        let start = raw.find(part).unwrap();
        RawSpan {
            range: start..start + part.len(),
            kind,
        }
    }

    /// A span of `kind` over the bytes `range` of the raw text.
    fn at(range: Range<usize>, kind: SpanKind) -> RawSpan {
        RawSpan { range, kind }
    }

    fn brk(lines: u32) -> SpanKind {
        SpanKind::Break { lines }
    }

    fn note() -> SpanKind {
        SpanKind::NoteRef {
            marker: "1".to_string(),
            block: None,
        }
    }

    /// `raw` normalised with `spans`, checked against the rules every block
    /// keeps, as `(start, end, kind)` triples.
    fn moved(raw: &str, spans: Vec<RawSpan>) -> (String, Vec<(usize, usize, SpanKind)>) {
        let (text, spans) = normalize_with_spans(raw, spans);
        assert_eq!(text, normalize(raw), "{raw:?}");
        assert_eq!(normalize(&text), text);
        let block = Block {
            id: 0,
            kind: BlockKind::Paragraph,
            text: text.clone(),
            spans: spans.clone(),
            ids: Vec::new(),
            within: Vec::new(),
            section: None,
            locator: Locator::Markdown {
                line: 1,
                line_end: None,
            },
        };
        assert_eq!(span_errors(&block), Vec::<String>::new(), "{raw:?}");
        let triples = spans.into_iter().map(|s| (s.start, s.end, s.kind));
        (text, triples.collect())
    }

    #[test]
    fn spans_move_through_collapsed_and_trimmed_whitespace() {
        let raw = "  a \n\t b  c  ";
        let (text, spans) = moved(raw, vec![over(raw, "b", SpanKind::Emphasis)]);
        assert_eq!(text, "a b c");
        assert_eq!(spans, [(2, 3, SpanKind::Emphasis)]);
    }

    #[test]
    fn spans_move_through_ligatures_dropped_characters_and_composition() {
        let raw = "ﬁx co\u{00AD}op e\u{0301}t";
        let (text, spans) = moved(
            raw,
            vec![
                over(raw, "ﬁx", SpanKind::Emphasis),
                over(raw, "co\u{00AD}op", SpanKind::Strong),
                over(raw, "e\u{0301}t", SpanKind::Code),
            ],
        );
        assert_eq!(text, "fix coop ét");
        assert_eq!(
            spans,
            [
                (0, 3, SpanKind::Emphasis),
                (4, 8, SpanKind::Strong),
                (9, 11, SpanKind::Code)
            ]
        );
    }

    #[test]
    fn a_position_inside_a_composed_character_moves_past_it() {
        let raw = "e\u{0301}x";
        let (text, spans) = moved(
            raw,
            vec![
                over(raw, "e", SpanKind::Emphasis),
                over(raw, "\u{0301}x", SpanKind::Code),
            ],
        );
        assert_eq!(text, "éx");
        assert_eq!(spans, [(0, 1, SpanKind::Emphasis), (1, 2, SpanKind::Code)]);
    }

    #[test]
    fn spans_count_code_points() {
        let raw = "😀 naïve x";
        let (_, spans) = moved(raw, vec![over(raw, "x", SpanKind::Strong)]);
        assert_eq!(spans, [(8, 9, SpanKind::Strong)]);
    }

    #[test]
    fn breaks_mark_the_space_of_their_run_and_a_run_of_them_counts_lines() {
        // one·two··three, where each · is a break.
        let (text, spans) = moved(
            "one two  three",
            vec![at(3..4, brk(1)), at(7..8, brk(1)), at(8..9, brk(1))],
        );
        assert_eq!(text, "one two three");
        assert_eq!(spans, [(3, 4, brk(1)), (7, 8, brk(2))]);

        // A break after a space that the text already had marks that space.
        let (_, spans) = moved("a  b", vec![at(2..3, brk(1))]);
        assert_eq!(spans, [(1, 2, brk(1))]);

        // Trimmed away at either end, a break marks nothing.
        let (text, spans) = moved(" x ", vec![at(0..1, brk(1)), at(2..3, brk(1))]);
        assert_eq!((text.as_str(), spans), ("x", Vec::new()));
    }

    #[test]
    fn points_stay_empty_and_other_spans_left_empty_are_dropped() {
        let raw = "a \u{00AD} b";
        let (text, spans) = moved(
            raw,
            vec![
                at(1..1, note()),
                over(raw, "\u{00AD}", SpanKind::Emphasis),
                // The space after the soft hyphen joins the space before it.
                at(4..5, SpanKind::Code),
            ],
        );
        assert_eq!(text, "a b");
        assert_eq!(spans, [(1, 1, note())]);
    }

    #[test]
    fn spans_are_sorted_by_start_the_longer_first() {
        let raw = "x y z";
        let (_, spans) = moved(
            raw,
            vec![
                over(raw, "y", SpanKind::Emphasis),
                at(0..5, SpanKind::Strong),
                at(0..0, note()),
                over(raw, "x", SpanKind::Code),
            ],
        );
        assert_eq!(
            spans,
            [
                (0, 5, SpanKind::Strong),
                (0, 1, SpanKind::Code),
                (0, 0, note()),
                (2, 3, SpanKind::Emphasis)
            ]
        );
    }

    /// The normalization done as one NFC pass over the whole text, which
    /// `normalize` must equal although it composes piece by piece.
    fn in_one_pass(text: &str) -> String {
        let mut expanded = String::new();
        for c in text.chars() {
            match ligature_letters(c) {
                Some(letters) => expanded.push_str(letters),
                None if is_dropped(c) => {}
                None => expanded.push(c),
            }
        }
        let composed: String = expanded.nfc().collect();
        composed.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn composing_piece_by_piece_equals_one_pass() {
        for text in [
            "\u{0301}a e\u{0301} \u{0301}",
            "\u{1100}\u{1161}\u{11A8} \u{AC00}\u{11A8}",
            "\u{0B47}\u{0B3E} \u{0DD9}\u{0DCF}\u{0DCA}",
            "a\u{0307}\u{0323} a\u{0323}\u{0307} q\u{0307}\u{0323}",
            "\u{2126} \u{0F73} \u{212B}\u{0301} \u{1E9B}\u{0323}",
            "ﬁ\u{0301} e\u{00AD}\u{0301} a\u{2000}b",
        ] {
            assert_eq!(normalize(text), in_one_pass(text), "{text:?}");
        }
    }

    /// NFC leaves the text before a character that [`starts_piece`] and the
    /// text from it on independent: the character's decomposition starts
    /// with a starter that composes with nothing before it, so nothing before
    /// it composes past it or is reordered across it.
    #[test]
    fn every_character_that_starts_a_piece_decomposes_to_such_a_starter() {
        for c in (0..=0x10FFFF).filter_map(char::from_u32) {
            if !starts_piece(c) {
                continue;
            }
            let mut first = None;
            decompose_canonical(c, |d| {
                first.get_or_insert(d);
            });
            let first = first.unwrap();
            assert_eq!(canonical_combining_class(first), 0, "{c:?}");
            assert_ne!(
                is_nfc_quick(std::iter::once(first)),
                IsNormalized::Maybe,
                "{c:?}"
            );
        }
    }

    #[test]
    fn expands_ligatures() {
        assert_eq!(normalize("ﬁnal ﬂow ﬀ ﬃ ﬄ ﬆ"), "final flow ff ffi ffl st");
    }

    #[test]
    fn composes_to_nfc() {
        assert_eq!(normalize("e\u{0301}"), "é");
    }

    #[test]
    fn removes_soft_hyphens_and_invisible_characters() {
        assert_eq!(normalize("co\u{00AD}op\u{200B}er\u{FEFF}ate"), "cooperate");
    }

    #[test]
    fn removes_control_characters() {
        assert_eq!(normalize("a\u{0007}b"), "ab");
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(normalize("  one \n\t two  "), "one two");
    }

    #[test]
    fn preserves_accents_scripts_and_superscripts() {
        assert_eq!(normalize("naïve Ελληνικά x²"), "naïve Ελληνικά x²");
    }

    #[test]
    fn is_idempotent() {
        for x in ["ﬁ\u{0301}", "e\u{00AD}\u{0301}", "ﬁnal  ﬂow\u{000C}x"] {
            assert_eq!(normalize(&normalize(x)), normalize(x));
        }
    }

    #[test]
    fn output_is_nfc() {
        assert_eq!(normalize("ﬁ\u{0301}"), "f\u{00ED}");
        assert_eq!(normalize("e\u{00AD}\u{0301}"), "é");
    }

    #[test]
    fn whitespace_controls_separate_words() {
        assert_eq!(normalize("end\u{000C}start"), "end start");
        assert_eq!(normalize("a\u{000B}b\u{0085}c"), "a b c");
    }
}
