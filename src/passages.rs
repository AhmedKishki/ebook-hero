//! Packs the blocks of a `Document` into passages: the unit that is indexed,
//! embedded, returned by search, read by locator and checked by quote
//! verification.

use std::ops::Range;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::document::{Block, BlockKind, Locator};
use crate::settings::PASSAGE_MAX_WORDS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Passage {
    pub id: u32,
    pub text: String,
    pub section: Vec<String>,
    pub blocks: Vec<BlockRange>,
    pub locator: PassageLocator,
}

/// `passage.text[start..end]` is `block.text[block_offset..block_offset + end - start]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BlockRange {
    pub block_id: u32,
    pub block_offset: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum PassageLocator {
    Pdf {
        page_start: u32,
        page_end: u32,
        label_start: Option<String>,
        label_end: Option<String>,
    },
    Epub {
        href: String,
        para_start: u32,
        para_end: u32,
        print_page: Option<String>,
        /// `None` for text before the book's first heading.
        under_heading: Option<HeadingPlace>,
    },
}

/// A passage's paragraphs counted from the nearest heading above it. The count
/// runs on across files, so the place stays unique when a chapter is split over
/// several files of an EPUB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeadingPlace {
    pub text: String,
    pub para_start: u32,
    pub para_end: u32,
}

/// The text of one block, or a piece of it, that goes into a passage whole.
struct Piece<'a> {
    block: &'a Block,
    offset: usize,
    text: &'a str,
    words: usize,
    /// The block's number among the blocks under its heading, from 1.
    section_para: u32,
}

/// Packs the paragraphs, list items and notes of `blocks`, in order, into
/// passages of at most `PASSAGE_MAX_WORDS` words, joined with a blank line.
/// A passage stays inside one section and, in an EPUB, one file. A block with
/// more words than the limit is split into pieces first. A heading starts a
/// new section even when its path repeats the previous one, so it closes the
/// passage being built; it reaches a passage only through its `section`.
pub fn build_passages(blocks: &[Block]) -> Vec<Passage> {
    let mut passages = Vec::new();
    let mut current: Vec<Piece> = Vec::new();
    let mut words = 0;
    let mut section_para = 0;
    let mut previous_section: Option<&[String]> = None;
    for block in blocks {
        if matches!(block.kind, BlockKind::Heading { .. }) {
            close(&mut passages, &mut current, &mut words);
            section_para = 0;
            continue;
        }
        if previous_section != Some(block.section.as_slice()) {
            section_para = 0;
        }
        previous_section = Some(&block.section);
        section_para += 1;
        for piece in pieces(block, section_para) {
            let fits = current.first().is_some_and(|first| {
                same_part(first.block, block) && words + piece.words <= PASSAGE_MAX_WORDS
            });
            if !fits {
                close(&mut passages, &mut current, &mut words);
            }
            words += piece.words;
            current.push(piece);
        }
    }
    close(&mut passages, &mut current, &mut words);
    passages
}

/// Turns the pieces collected so far, if any, into the next passage.
fn close<'a>(passages: &mut Vec<Passage>, current: &mut Vec<Piece<'a>>, words: &mut usize) {
    if !current.is_empty() {
        passages.push(passage(passages.len() as u32, current));
        current.clear();
    }
    *words = 0;
}

fn word_count(text: &str) -> usize {
    text.unicode_words().count()
}

/// Whether two blocks may share a passage: same section and, in an EPUB, same file.
fn same_part(a: &Block, b: &Block) -> bool {
    a.section == b.section
        && match (&a.locator, &b.locator) {
            (Locator::Pdf { .. }, Locator::Pdf { .. }) => true,
            (Locator::Epub { href: a, .. }, Locator::Epub { href: b, .. }) => a == b,
            _ => false,
        }
}

fn pieces(block: &Block, section_para: u32) -> Vec<Piece<'_>> {
    let words = word_count(&block.text);
    if words <= PASSAGE_MAX_WORDS {
        return vec![Piece {
            block,
            offset: 0,
            text: &block.text,
            words,
            section_para,
        }];
    }
    split_ranges(&block.text)
        .into_iter()
        .map(|range| {
            let text = &block.text[range.clone()];
            Piece {
                block,
                offset: range.start,
                text,
                words: word_count(text),
                section_para,
            }
        })
        .collect()
}

/// Splits `text` into trimmed byte ranges of at most `PASSAGE_MAX_WORDS`
/// words each. A range holds whole sentences, except that a sentence with more
/// words than the limit is cut at words.
fn split_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut piece = 0..0;
    for (offset, sentence) in text.split_sentence_bound_indices() {
        let end = offset + sentence.len();
        if word_count(&text[piece.start..end]) <= PASSAGE_MAX_WORDS {
            piece.end = end;
            continue;
        }
        ranges.push(piece);
        piece = offset..end;
        if word_count(sentence) > PASSAGE_MAX_WORDS {
            let mut cuts = split_at_words(sentence)
                .into_iter()
                .map(|cut| offset + cut.start..offset + cut.end);
            // The last cut stays open so the sentences after it can join it.
            let last = cuts.next_back().expect("a split has at least one cut");
            ranges.extend(cuts);
            piece = last;
        }
    }
    ranges.push(piece);
    ranges
        .into_iter()
        .filter_map(|range| trimmed(text, range))
        .collect()
}

/// Cuts one sentence into consecutive ranges of at most `PASSAGE_MAX_WORDS`
/// words. A cut falls after the whitespace that precedes the first word of the
/// next range, so punctuation and hyphenated parts stay with their word; text
/// without whitespace is cut at the word itself.
fn split_at_words(sentence: &str) -> Vec<Range<usize>> {
    let starts: Vec<usize> = sentence.unicode_word_indices().map(|(i, _)| i).collect();
    let mut ranges = Vec::new();
    let mut from = 0;
    loop {
        let first = starts.partition_point(|&start| start < from);
        let Some(&next_word) = starts.get(first + PASSAGE_MAX_WORDS) else {
            break;
        };
        let cut = sentence[from..next_word]
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(next_word, |(i, c)| from + i + c.len_utf8());
        ranges.push(from..cut);
        from = cut;
    }
    ranges.push(from..sentence.len());
    ranges
}

/// `range` without leading and trailing whitespace, or `None` if nothing is left.
fn trimmed(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let slice = &text[range.clone()];
    let kept = slice.trim();
    if kept.is_empty() {
        return None;
    }
    let start = range.start + slice.len() - slice.trim_start().len();
    Some(start..start + kept.len())
}

fn passage(id: u32, pieces: &[Piece]) -> Passage {
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(pieces.len());
    for (i, piece) in pieces.iter().enumerate() {
        if i > 0 {
            text.push_str("\n\n");
        }
        let start = text.len();
        text.push_str(piece.text);
        ranges.push(BlockRange {
            block_id: piece.block.id,
            block_offset: piece.offset,
            start,
            end: text.len(),
        });
    }
    Passage {
        id,
        text,
        section: pieces[0].block.section.clone(),
        blocks: ranges,
        locator: locator(pieces),
    }
}

/// The locator of a passage made of `pieces`, which share one format.
fn locator(pieces: &[Piece]) -> PassageLocator {
    match &pieces[0].block.locator {
        Locator::Pdf {
            page, page_label, ..
        } => {
            // A block's label belongs to the page it starts on, so a block that
            // runs on to a later page supplies the end page but no end label.
            let mut page_end = *page;
            let mut label_end = None;
            for piece in pieces {
                if let Locator::Pdf {
                    page,
                    page_end: block_end,
                    page_label,
                    ..
                } = &piece.block.locator
                {
                    let block_end = block_end.unwrap_or(*page);
                    let label = if block_end == *page {
                        page_label.clone()
                    } else {
                        None
                    };
                    if block_end > page_end || (block_end == page_end && label_end.is_none()) {
                        page_end = block_end;
                        label_end = label;
                    }
                }
            }
            PassageLocator::Pdf {
                page_start: *page,
                page_end,
                label_start: page_label.clone(),
                label_end,
            }
        }
        Locator::Epub {
            href,
            para,
            print_page,
            ..
        } => PassageLocator::Epub {
            href: href.clone(),
            para_start: *para,
            para_end: pieces
                .iter()
                .rev()
                .find_map(|piece| match &piece.block.locator {
                    Locator::Epub { para, .. } => Some(*para),
                    Locator::Pdf { .. } => None,
                })
                .unwrap_or(*para),
            print_page: print_page.clone(),
            under_heading: pieces[0].block.section.last().map(|text| HeadingPlace {
                text: text.clone(),
                para_start: pieces[0].section_para,
                para_end: pieces[pieces.len() - 1].section_para,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{BlockKind, Locator};
    use crate::settings::PASSAGE_MAX_WORDS;
    use unicode_segmentation::UnicodeSegmentation;

    fn words(n: usize) -> String {
        vec!["word"; n].join(" ")
    }

    fn word_count(text: &str) -> usize {
        text.unicode_words().count()
    }

    fn section(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn pdf_block(id: u32, text: &str, sect: &[&str], page: u32) -> Block {
        Block {
            id,
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            orig: None,
            spans: Vec::new(),
            section: section(sect),
            locator: Locator::Pdf {
                page,
                page_end: None,
                page_label: None,
                bbox: None,
            },
        }
    }

    fn epub_block(id: u32, text: &str, sect: &[&str], href: &str, para: u32) -> Block {
        Block {
            locator: Locator::Epub {
                href: href.to_string(),
                anchor: None,
                para,
                print_page: None,
            },
            ..pdf_block(id, text, sect, 1)
        }
    }

    fn heading(id: u32, text: &str, sect: &[&str]) -> Block {
        Block {
            kind: BlockKind::Heading { level: 1 },
            ..pdf_block(id, text, sect, 1)
        }
    }

    fn block_counts(passages: &[Passage]) -> Vec<usize> {
        passages.iter().map(|p| p.blocks.len()).collect()
    }

    /// Asserts every range of every passage reads the same bytes from the passage and from its block.
    fn assert_ranges_map_back(passages: &[Passage], blocks: &[Block]) {
        for p in passages {
            for r in &p.blocks {
                let block = blocks.iter().find(|b| b.id == r.block_id).unwrap();
                assert_eq!(
                    &p.text[r.start..r.end],
                    &block.text[r.block_offset..r.block_offset + r.end - r.start]
                );
            }
        }
    }

    #[test]
    fn packs_whole_blocks_up_to_the_limit() {
        let blocks: Vec<Block> = (0..5)
            .map(|i| pdf_block(i, &words(100), &["A"], 1))
            .collect();
        let passages = build_passages(&blocks);
        assert_eq!(block_counts(&passages), [2, 2, 1]);
        assert_eq!(
            passages[0].text,
            format!("{}\n\n{}", words(100), words(100))
        );
        assert_eq!(
            passages[0].blocks,
            [
                BlockRange {
                    block_id: 0,
                    block_offset: 0,
                    start: 0,
                    end: words(100).len(),
                },
                BlockRange {
                    block_id: 1,
                    block_offset: 0,
                    start: words(100).len() + 2,
                    end: 2 * words(100).len() + 2,
                },
            ]
        );
    }

    #[test]
    fn never_crosses_a_section() {
        let blocks = [
            pdf_block(0, &words(10), &["A"], 1),
            pdf_block(1, &words(10), &["A"], 1),
            pdf_block(2, &words(10), &["B"], 1),
            pdf_block(3, &words(10), &["A", "B"], 1),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(block_counts(&passages), [2, 1, 1]);
        assert_eq!(passages[0].section, section(&["A"]));
        assert_eq!(passages[1].section, section(&["B"]));
        assert_eq!(passages[2].section, section(&["A", "B"]));
    }

    #[test]
    fn never_crosses_an_epub_file() {
        let blocks = [
            epub_block(0, &words(10), &["A"], "one.xhtml", 0),
            epub_block(1, &words(10), &["A"], "one.xhtml", 1),
            epub_block(2, &words(10), &["A"], "two.xhtml", 0),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(block_counts(&passages), [2, 1]);
    }

    #[test]
    fn headings_are_not_passages() {
        let blocks = [
            heading(0, "Chapter One", &["Chapter One"]),
            pdf_block(1, "The first paragraph.", &["Chapter One"], 1),
            pdf_block(2, "The second paragraph.", &["Chapter One"], 1),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 1);
        assert!(!passages[0].text.contains("Chapter One"));
        assert_eq!(passages[0].section, section(&["Chapter One"]));
        assert_eq!(
            passages[0]
                .blocks
                .iter()
                .map(|r| r.block_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn heading_breaks_passage_even_with_identical_section_path() {
        let blocks = [
            pdf_block(0, "The first chapter.", &["Introduction"], 1),
            heading(1, "Introduction", &["Introduction"]),
            pdf_block(2, "The second chapter.", &["Introduction"], 2),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(block_counts(&passages), [1, 1]);
        assert_eq!(passages[0].text, "The first chapter.");
        assert_eq!(passages[1].text, "The second chapter.");
    }

    #[test]
    fn list_items_and_notes_are_packed() {
        let blocks = [
            Block {
                kind: BlockKind::ListItem,
                ..pdf_block(0, "A list item.", &["A"], 1)
            },
            Block {
                kind: BlockKind::Note {
                    label: Some("1".to_string()),
                },
                ..pdf_block(1, "A note.", &["A"], 1)
            },
        ];
        assert_eq!(block_counts(&build_passages(&blocks)), [2]);
    }

    fn sentence(i: usize) -> String {
        format!("Sentence number {i} has exactly ten words in it now.")
    }

    #[test]
    fn oversized_block_splits_at_sentences() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        assert_eq!(word_count(&text), 500);
        let blocks = [pdf_block(7, &text, &["A"], 1)];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 3);
        let mut previous_offset = None;
        for p in &passages {
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
            assert!(p.text.ends_with('.'));
            assert!(p.text.starts_with("Sentence"));
            assert_eq!(p.blocks.len(), 1);
            let r = &p.blocks[0];
            assert_eq!(r.block_id, 7);
            assert!(previous_offset.is_none_or(|previous| r.block_offset > previous));
            previous_offset = Some(r.block_offset);
        }
        assert_eq!(passages[0].blocks[0].block_offset, 0);
        assert_eq!(
            passages
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            text
        );
        assert_ranges_map_back(&passages, &blocks);
    }

    #[test]
    fn oversized_sentence_splits_at_words() {
        let text = format!(
            "Start {} end.",
            (1..299)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert_eq!(word_count(&text), 300);
        let blocks = [pdf_block(0, &text, &["A"], 1)];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 2);
        for p in &passages {
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
        }
        assert_eq!(
            passages
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            text
        );
        assert_ranges_map_back(&passages, &blocks);
    }

    #[test]
    fn a_word_cut_keeps_a_hyphenated_token_whole() {
        let text = format!("{} state-of-the-art tail words follow.", words(219));
        let blocks = [pdf_block(0, &text, &["A"], 1)];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 2);
        assert_eq!(passages[0].text, words(219));
        assert_eq!(passages[1].text, "state-of-the-art tail words follow.");
        assert_ranges_map_back(&passages, &blocks);
    }

    #[test]
    fn oversized_text_without_spaces_splits_at_words() {
        let text = "字".repeat(500);
        let blocks = [pdf_block(0, &text, &["A"], 1)];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 3);
        for p in &passages {
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
        }
        assert_eq!(
            passages.iter().map(|p| p.text.as_str()).collect::<String>(),
            text
        );
        assert_ranges_map_back(&passages, &blocks);
    }

    #[test]
    fn split_pieces_pack_with_the_following_blocks() {
        let big = format!("{} {}", words(150), words(100));
        let blocks = [
            pdf_block(0, &big, &["A"], 1),
            pdf_block(1, &words(100), &["A"], 1),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(block_counts(&passages), [1, 2]);
        assert_eq!(passages[1].blocks[0].block_id, 0);
        assert_eq!(passages[1].blocks[1].block_id, 1);
        assert_ranges_map_back(&passages, &blocks);
    }

    #[test]
    fn ranges_map_back_to_blocks() {
        let long = (0..30)
            .map(|i| format!("Phrase {i} über naïve café — {}.", words(12)))
            .collect::<Vec<_>>()
            .join(" ");
        let blocks = [
            pdf_block(0, "Ünïcode ‘quotes’ and dashes — fine.", &["A"], 1),
            pdf_block(1, &long, &["A"], 1),
            pdf_block(2, &words(150), &["A"], 2),
            pdf_block(3, "Çà et là.", &["A"], 2),
            pdf_block(4, &words(90), &["B"], 3),
        ];
        let passages = build_passages(&blocks);
        assert!(passages.len() > 3);
        assert_ranges_map_back(&passages, &blocks);
        for p in &passages {
            assert_eq!(p.text, p.text.trim());
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
            assert!(p.blocks.windows(2).all(|w| w[0].end + 2 == w[1].start));
            assert_eq!(p.blocks.last().unwrap().end, p.text.len());
        }
    }

    fn pdf_locator(page: u32, page_end: Option<u32>, label: Option<&str>) -> Locator {
        Locator::Pdf {
            page,
            page_end,
            page_label: label.map(str::to_string),
            bbox: None,
        }
    }

    #[test]
    fn pdf_locator_spans_pages_and_labels() {
        let blocks = [
            Block {
                locator: pdf_locator(3, None, Some("xii")),
                ..pdf_block(0, &words(10), &["A"], 3)
            },
            Block {
                locator: pdf_locator(4, None, Some("xiii")),
                ..pdf_block(1, &words(10), &["A"], 4)
            },
        ];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 1);
        assert_eq!(
            passages[0].locator,
            PassageLocator::Pdf {
                page_start: 3,
                page_end: 4,
                label_start: Some("xii".to_string()),
                label_end: Some("xiii".to_string()),
            }
        );
    }

    #[test]
    fn a_block_that_spans_pages_extends_the_passage_without_an_end_label() {
        let blocks = [Block {
            locator: pdf_locator(3, Some(4), Some("xii")),
            ..pdf_block(0, &words(10), &["A"], 3)
        }];
        assert_eq!(
            build_passages(&blocks)[0].locator,
            PassageLocator::Pdf {
                page_start: 3,
                page_end: 4,
                label_start: Some("xii".to_string()),
                label_end: None,
            }
        );
    }

    #[test]
    fn a_later_block_on_the_end_page_supplies_the_end_label() {
        let blocks = [
            Block {
                locator: pdf_locator(3, Some(4), Some("xii")),
                ..pdf_block(0, &words(10), &["A"], 3)
            },
            Block {
                locator: pdf_locator(4, None, Some("xiii")),
                ..pdf_block(1, &words(10), &["A"], 4)
            },
        ];
        assert_eq!(
            build_passages(&blocks)[0].locator,
            PassageLocator::Pdf {
                page_start: 3,
                page_end: 4,
                label_start: Some("xii".to_string()),
                label_end: Some("xiii".to_string()),
            }
        );
    }

    #[test]
    fn epub_locator_spans_paragraphs() {
        let epub = |id, para, print_page: Option<&str>| Block {
            locator: Locator::Epub {
                href: "ch1.xhtml".to_string(),
                anchor: Some("a".to_string()),
                para,
                print_page: print_page.map(str::to_string),
            },
            ..pdf_block(id, &words(10), &["A"], 1)
        };
        let blocks = [
            epub(0, 4, Some("12")),
            epub(1, 5, Some("13")),
            epub(2, 6, None),
        ];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 1);
        assert_eq!(
            passages[0].locator,
            PassageLocator::Epub {
                href: "ch1.xhtml".to_string(),
                para_start: 4,
                para_end: 6,
                print_page: Some("12".to_string()),
                under_heading: Some(HeadingPlace {
                    text: "A".to_string(),
                    para_start: 1,
                    para_end: 3,
                }),
            }
        );
    }

    fn heading_place(passage: &Passage) -> Option<(String, u32, u32)> {
        match &passage.locator {
            PassageLocator::Epub {
                under_heading: Some(place),
                ..
            } => Some((place.text.clone(), place.para_start, place.para_end)),
            _ => None,
        }
    }

    #[test]
    fn epub_paragraphs_are_counted_from_their_heading_across_files() {
        let blocks = [
            heading(0, "Book", &["Book"]),
            heading(1, "One", &["Book", "One"]),
            epub_block(2, &words(150), &["Book", "One"], "a.xhtml", 3),
            epub_block(3, &words(150), &["Book", "One"], "a.xhtml", 4),
            epub_block(4, &words(10), &["Book", "One"], "b.xhtml", 1),
            heading(5, "Two", &["Book", "Two"]),
            epub_block(6, &words(10), &["Book", "Two"], "b.xhtml", 3),
        ];
        let passages = build_passages(&blocks);
        let places: Vec<_> = passages.iter().map(heading_place).collect();
        assert_eq!(
            places,
            [
                Some(("One".to_string(), 1, 1)),
                Some(("One".to_string(), 2, 2)),
                Some(("One".to_string(), 3, 3)),
                Some(("Two".to_string(), 1, 1)),
            ]
        );
    }

    #[test]
    fn a_repeated_heading_starts_the_count_again() {
        let blocks = [
            heading(0, "Notes", &["Notes"]),
            epub_block(1, &words(10), &["Notes"], "a.xhtml", 2),
            epub_block(2, &words(10), &["Notes"], "a.xhtml", 3),
            heading(3, "Notes", &["Notes"]),
            epub_block(4, &words(10), &["Notes"], "a.xhtml", 5),
        ];
        let places: Vec<_> = build_passages(&blocks).iter().map(heading_place).collect();
        assert_eq!(
            places,
            [
                Some(("Notes".to_string(), 1, 2)),
                Some(("Notes".to_string(), 1, 1)),
            ]
        );
    }

    #[test]
    fn epub_text_before_the_first_heading_has_no_heading_place() {
        let blocks = [
            epub_block(0, &words(10), &[], "cover.xhtml", 1),
            heading(1, "One", &["One"]),
            epub_block(2, &words(10), &["One"], "a.xhtml", 2),
        ];
        let places: Vec<_> = build_passages(&blocks).iter().map(heading_place).collect();
        assert_eq!(places, [None, Some(("One".to_string(), 1, 1))]);
    }

    #[test]
    fn pieces_of_a_split_epub_block_share_its_paragraph_number() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        let blocks = [
            heading(0, "One", &["One"]),
            epub_block(1, &words(10), &["One"], "a.xhtml", 2),
            epub_block(2, &text, &["One"], "a.xhtml", 3),
        ];
        let places: Vec<_> = build_passages(&blocks).iter().map(heading_place).collect();
        assert!(places.len() >= 3, "{places:?}");
        assert_eq!(places[0].as_ref().map(|place| place.1), Some(1));
        for place in &places[1..] {
            assert_eq!(place, &Some(("One".to_string(), 2, 2)));
        }
    }

    #[test]
    fn pieces_of_a_split_block_keep_its_locator() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        let blocks = [Block {
            locator: pdf_locator(9, None, Some("ix")),
            ..pdf_block(0, &text, &["A"], 9)
        }];
        let passages = build_passages(&blocks);
        assert_eq!(passages.len(), 3);
        for p in passages {
            assert_eq!(
                p.locator,
                PassageLocator::Pdf {
                    page_start: 9,
                    page_end: 9,
                    label_start: Some("ix".to_string()),
                    label_end: Some("ix".to_string()),
                }
            );
        }
    }

    #[test]
    fn ids_follow_reading_order() {
        let mut blocks: Vec<Block> = (0..6)
            .map(|i| pdf_block(i, &words(100), &["A"], 1))
            .collect();
        blocks.push(pdf_block(
            6,
            &(0..50).map(sentence).collect::<Vec<_>>().join(" "),
            &["B"],
            2,
        ));
        let passages = build_passages(&blocks);
        assert_eq!(
            passages.iter().map(|p| p.id).collect::<Vec<_>>(),
            (0..passages.len() as u32).collect::<Vec<_>>()
        );
        let block_ids: Vec<u32> = passages
            .iter()
            .flat_map(|p| p.blocks.iter().map(|r| r.block_id))
            .collect();
        assert!(block_ids.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(block_ids.first(), Some(&0));
        assert_eq!(block_ids.last(), Some(&6));
    }

    #[test]
    fn pdf_and_epub_blocks_never_share_a_passage() {
        let blocks = [
            pdf_block(0, &words(10), &["A"], 1),
            epub_block(1, &words(10), &["A"], "one.xhtml", 0),
        ];
        assert_eq!(block_counts(&build_passages(&blocks)), [1, 1]);
    }

    #[test]
    fn no_blocks_make_no_passages() {
        assert!(build_passages(&[]).is_empty());
    }
}
