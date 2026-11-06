//! Packs the blocks of a `Document` into passages: the unit that is indexed,
//! embedded, returned by search, read by locator and checked by quote
//! verification.

use std::ops::Range;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::document::{Block, BlockKind, Document, Locator, Places};
use crate::settings::PASSAGE_MAX_WORDS;

/// A run of whole paragraphs, list items and notes of at most
/// [`PASSAGE_MAX_WORDS`] words, inside one section and, in an EPUB, one file.
/// Search returns passages, `read` prints them and quote verification checks
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Passage {
    /// The passage's position in reading order, from 0. Every tool takes and
    /// returns passages by this id.
    pub id: u32,
    /// The text of its blocks, or pieces of them, joined by a blank line.
    pub text: String,
    /// The texts of the headings above its first block, outermost first.
    pub section: Vec<String>,
    /// Where each piece of `text` comes from, in order.
    pub blocks: Vec<BlockRange>,
    /// Where the passage is in the book.
    pub locator: PassageLocator,
}

/// A piece of a passage's text and the block it was taken from, in UTF-8
/// bytes: `passage.text[start..end]` is `block.text[block_offset..block_offset + end - start]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BlockRange {
    /// The block's id in [`Document::blocks`](crate::document::Document::blocks).
    pub block_id: u32,
    /// Where the piece starts in the block's text.
    pub block_offset: usize,
    /// Where the piece starts in the passage's text.
    pub start: usize,
    /// Where the piece ends in the passage's text.
    pub end: usize,
}

/// Where a passage is, as a person would cite it. JSON names the variant in a
/// `format` field; [`locator_text`](crate::read::locator_text) prints it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum PassageLocator {
    /// A place in a PDF: `p. 12–13 (xii–xiii)`.
    Pdf {
        /// The physical page the passage starts on, counted from 1.
        page_start: u32,
        /// The physical page it ends on.
        page_end: u32,
        /// The printed label of `page_start`, when the PDF defines labels.
        label_start: Option<String>,
        /// The printed label of `page_end`, when the PDF defines labels.
        label_end: Option<String>,
    },
    /// A place in an EPUB: `CHAPTER I ¶3–5, print p. 45`.
    Epub {
        /// The content file the passage is in.
        href: String,
        /// The position of its first block among the blocks of that file,
        /// counted from 1.
        para_start: u32,
        /// The position of its last block among the blocks of that file.
        para_end: u32,
        /// The printed page where it starts, when the book marks its page
        /// breaks.
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
    /// The heading's text.
    pub text: String,
    /// The number of the passage's first block among the blocks under the
    /// heading, counted from 1.
    pub para_start: u32,
    /// The number of its last block among the blocks under the heading.
    pub para_end: u32,
}

/// The text of one block, or a piece of it, that goes into a passage whole.
struct Piece<'a> {
    block: &'a Block,
    offset: usize,
    text: &'a str,
    words: usize,
}

/// Packs the paragraphs, list items and notes of the document's blocks, in
/// order, into passages of at most `PASSAGE_MAX_WORDS` words, joined with a
/// blank line. A passage stays inside one section and, in an EPUB, one file.
/// A block with more words than the limit is split into pieces first. A
/// heading opens a new section even when its path repeats the previous one,
/// so it closes the passage being built; it reaches a passage only through
/// its `section`. `places` must be the document's.
pub fn build_passages(document: &Document, places: &Places) -> Vec<Passage> {
    let mut passages = Vec::new();
    let mut current: Vec<Piece> = Vec::new();
    let mut words = 0;
    for block in &document.blocks {
        if matches!(block.kind, BlockKind::Heading { .. }) {
            close(&mut passages, &mut current, &mut words, document, places);
            continue;
        }
        for piece in pieces(block) {
            let fits = current.first().is_some_and(|first| {
                same_part(first.block, block) && words + piece.words <= PASSAGE_MAX_WORDS
            });
            if !fits {
                close(&mut passages, &mut current, &mut words, document, places);
            }
            words += piece.words;
            current.push(piece);
        }
    }
    close(&mut passages, &mut current, &mut words, document, places);
    passages
}

/// Turns the pieces collected so far, if any, into the next passage.
fn close(
    passages: &mut Vec<Passage>,
    current: &mut Vec<Piece>,
    words: &mut usize,
    document: &Document,
    places: &Places,
) {
    if !current.is_empty() {
        passages.push(passage(passages.len() as u32, current, document, places));
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
            (Locator::Epub { file: a }, Locator::Epub { file: b }) => a == b,
            _ => false,
        }
}

fn pieces(block: &Block) -> Vec<Piece<'_>> {
    let words = word_count(&block.text);
    if words <= PASSAGE_MAX_WORDS {
        return vec![Piece {
            block,
            offset: 0,
            text: &block.text,
            words,
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

fn passage(id: u32, pieces: &[Piece], document: &Document, places: &Places) -> Passage {
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
        section: places.section_path(pieces[0].block).to_vec(),
        blocks: ranges,
        locator: locator(pieces, document, places),
    }
}

/// The locator of a passage made of `pieces`, which share one format.
fn locator(pieces: &[Piece], document: &Document, places: &Places) -> PassageLocator {
    let first = pieces[0].block;
    let last = pieces[pieces.len() - 1].block;
    let label = |page| document.page_label(page).map(str::to_string);
    match &first.locator {
        Locator::Pdf { page, .. } => {
            // A block's label belongs to the page it starts on, so a block that
            // runs on to a later page supplies the end page but no end label.
            let mut page_end = *page;
            let mut label_end = None;
            for piece in pieces {
                if let Locator::Pdf {
                    page,
                    page_end: block_end,
                    ..
                } = &piece.block.locator
                {
                    let block_end = block_end.unwrap_or(*page);
                    let block_label = if block_end == *page {
                        label(*page)
                    } else {
                        None
                    };
                    if block_end > page_end || (block_end == page_end && label_end.is_none()) {
                        page_end = block_end;
                        label_end = block_label;
                    }
                }
            }
            PassageLocator::Pdf {
                page_start: *page,
                page_end,
                label_start: label(*page),
                label_end,
            }
        }
        Locator::Epub { file } => PassageLocator::Epub {
            href: document.files[*file as usize].clone(),
            para_start: places.para(first.id),
            para_end: places.para(last.id),
            print_page: places.print_page(first.id).map(str::to_string),
            under_heading: places.section_path(first).last().map(|text| HeadingPlace {
                text: text.clone(),
                para_start: places.section_para(first.id),
                para_end: places.section_para(last.id),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{BlockKind, Format, Locator, Span, SpanKind};
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

    fn pdf_block(text: &str, page: u32) -> Block {
        Block {
            id: 0,
            kind: BlockKind::Paragraph,
            text: text.to_string(),
            spans: Vec::new(),
            ids: Vec::new(),
            section: None,
            locator: Locator::Pdf {
                page,
                page_end: None,
                bbox: None,
            },
        }
    }

    /// A paragraph in the EPUB file `ch{file + 1}.xhtml`.
    fn epub_block(text: &str, file: u32) -> Block {
        Block {
            locator: Locator::Epub { file },
            ..pdf_block(text, 1)
        }
    }

    fn heading(level: u8, text: &str) -> Block {
        Block {
            kind: BlockKind::Heading { level },
            ..pdf_block(text, 1)
        }
    }

    fn epub_heading(level: u8, text: &str, file: u32) -> Block {
        Block {
            locator: Locator::Epub { file },
            ..heading(level, text)
        }
    }

    /// `block` with a printed page that starts where it starts.
    fn on_print_page(label: &str, block: Block) -> Block {
        Block {
            spans: vec![Span {
                start: 0,
                end: 0,
                kind: SpanKind::PageBreak {
                    label: label.to_string(),
                },
            }],
            ..block
        }
    }

    /// A document of `blocks`, numbered and given sections from their
    /// headings, whose EPUB files are `ch1.xhtml` to `ch3.xhtml`.
    fn document(blocks: Vec<Block>) -> Document {
        let mut doc = Document::from_blocks(Format::Pdf, blocks);
        doc.files = ["ch1.xhtml", "ch2.xhtml", "ch3.xhtml"]
            .map(String::from)
            .to_vec();
        doc
    }

    /// `doc` with the printed labels of some of its pages.
    fn labelled(mut doc: Document, labels: &[(u32, &str)]) -> Document {
        let pages = labels.iter().map(|&(page, _)| page).max().unwrap_or(0);
        doc.page_labels = vec![None; pages as usize];
        for &(page, label) in labels {
            doc.page_labels[page as usize - 1] = Some(label.to_string());
        }
        doc
    }

    fn passages_of(doc: &Document) -> Vec<Passage> {
        build_passages(doc, &Places::new(doc))
    }

    fn build(blocks: Vec<Block>) -> Vec<Passage> {
        passages_of(&document(blocks))
    }

    fn block_counts(passages: &[Passage]) -> Vec<usize> {
        passages.iter().map(|p| p.blocks.len()).collect()
    }

    /// Asserts every range of every passage reads the same bytes from the passage and from its block.
    fn assert_ranges_map_back(passages: &[Passage], blocks: &[Block]) {
        for p in passages {
            for r in &p.blocks {
                let block = &blocks[r.block_id as usize];
                assert_eq!(
                    &p.text[r.start..r.end],
                    &block.text[r.block_offset..r.block_offset + r.end - r.start]
                );
            }
        }
    }

    #[test]
    fn packs_whole_blocks_up_to_the_limit() {
        let passages = build((0..5).map(|_| pdf_block(&words(100), 1)).collect());
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
        let passages = build(vec![
            heading(1, "A"),
            pdf_block(&words(10), 1),
            pdf_block(&words(10), 1),
            heading(1, "B"),
            pdf_block(&words(10), 1),
            heading(1, "A"),
            heading(2, "B"),
            pdf_block(&words(10), 1),
        ]);
        assert_eq!(block_counts(&passages), [2, 1, 1]);
        assert_eq!(passages[0].section, section(&["A"]));
        assert_eq!(passages[1].section, section(&["B"]));
        assert_eq!(passages[2].section, section(&["A", "B"]));
    }

    #[test]
    fn never_crosses_an_epub_file() {
        let passages = build(vec![
            epub_block(&words(10), 0),
            epub_block(&words(10), 0),
            epub_block(&words(10), 1),
        ]);
        assert_eq!(block_counts(&passages), [2, 1]);
    }

    #[test]
    fn headings_are_not_passages() {
        let passages = build(vec![
            heading(1, "Chapter One"),
            pdf_block("The first paragraph.", 1),
            pdf_block("The second paragraph.", 1),
        ]);
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
        let passages = build(vec![
            heading(1, "Introduction"),
            pdf_block("The first chapter.", 1),
            heading(1, "Introduction"),
            pdf_block("The second chapter.", 2),
        ]);
        assert_eq!(block_counts(&passages), [1, 1]);
        assert_eq!(passages[0].section, passages[1].section);
        assert_eq!(passages[0].text, "The first chapter.");
        assert_eq!(passages[1].text, "The second chapter.");
    }

    #[test]
    fn list_items_and_notes_are_packed() {
        let blocks = vec![
            Block {
                kind: BlockKind::ListItem,
                ..pdf_block("A list item.", 1)
            },
            Block {
                kind: BlockKind::Note {
                    label: Some("1".to_string()),
                },
                ..pdf_block("A note.", 1)
            },
        ];
        assert_eq!(block_counts(&build(blocks)), [2]);
    }

    fn sentence(i: usize) -> String {
        format!("Sentence number {i} has exactly ten words in it now.")
    }

    #[test]
    fn oversized_block_splits_at_sentences() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        assert_eq!(word_count(&text), 500);
        let doc = document(vec![heading(1, "A"), pdf_block(&text, 1)]);
        let passages = passages_of(&doc);
        assert_eq!(passages.len(), 3);
        let mut previous_offset = None;
        for p in &passages {
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
            assert!(p.text.ends_with('.'));
            assert!(p.text.starts_with("Sentence"));
            assert_eq!(p.blocks.len(), 1);
            let r = &p.blocks[0];
            assert_eq!(r.block_id, 1);
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
        assert_ranges_map_back(&passages, &doc.blocks);
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
        let doc = document(vec![pdf_block(&text, 1)]);
        let passages = passages_of(&doc);
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
        assert_ranges_map_back(&passages, &doc.blocks);
    }

    #[test]
    fn a_word_cut_keeps_a_hyphenated_token_whole() {
        let text = format!("{} state-of-the-art tail words follow.", words(219));
        let doc = document(vec![pdf_block(&text, 1)]);
        let passages = passages_of(&doc);
        assert_eq!(passages.len(), 2);
        assert_eq!(passages[0].text, words(219));
        assert_eq!(passages[1].text, "state-of-the-art tail words follow.");
        assert_ranges_map_back(&passages, &doc.blocks);
    }

    #[test]
    fn oversized_text_without_spaces_splits_at_words() {
        let text = "字".repeat(500);
        let doc = document(vec![pdf_block(&text, 1)]);
        let passages = passages_of(&doc);
        assert_eq!(passages.len(), 3);
        for p in &passages {
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
        }
        assert_eq!(
            passages.iter().map(|p| p.text.as_str()).collect::<String>(),
            text
        );
        assert_ranges_map_back(&passages, &doc.blocks);
    }

    #[test]
    fn split_pieces_pack_with_the_following_blocks() {
        let big = format!("{} {}", words(150), words(100));
        let doc = document(vec![pdf_block(&big, 1), pdf_block(&words(100), 1)]);
        let passages = passages_of(&doc);
        assert_eq!(block_counts(&passages), [1, 2]);
        assert_eq!(passages[1].blocks[0].block_id, 0);
        assert_eq!(passages[1].blocks[1].block_id, 1);
        assert_ranges_map_back(&passages, &doc.blocks);
    }

    #[test]
    fn ranges_map_back_to_blocks() {
        let long = (0..30)
            .map(|i| format!("Phrase {i} über naïve café — {}.", words(12)))
            .collect::<Vec<_>>()
            .join(" ");
        let doc = document(vec![
            heading(1, "A"),
            pdf_block("Ünïcode ‘quotes’ and dashes — fine.", 1),
            pdf_block(&long, 1),
            pdf_block(&words(150), 2),
            pdf_block("Çà et là.", 2),
            heading(1, "B"),
            pdf_block(&words(90), 3),
        ]);
        let passages = passages_of(&doc);
        assert!(passages.len() > 3);
        assert_ranges_map_back(&passages, &doc.blocks);
        for p in &passages {
            assert_eq!(p.text, p.text.trim());
            assert!(word_count(&p.text) <= PASSAGE_MAX_WORDS);
            assert!(p.blocks.windows(2).all(|w| w[0].end + 2 == w[1].start));
            assert_eq!(p.blocks.last().unwrap().end, p.text.len());
        }
    }

    fn spanning(page: u32, page_end: u32, text: &str) -> Block {
        Block {
            locator: Locator::Pdf {
                page,
                page_end: Some(page_end),
                bbox: None,
            },
            ..pdf_block(text, page)
        }
    }

    #[test]
    fn pdf_locator_spans_pages_and_labels() {
        let doc = labelled(
            document(vec![pdf_block(&words(10), 3), pdf_block(&words(10), 4)]),
            &[(3, "xii"), (4, "xiii")],
        );
        let passages = passages_of(&doc);
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
        let doc = labelled(
            document(vec![spanning(3, 4, &words(10))]),
            &[(3, "xii"), (4, "xiii")],
        );
        assert_eq!(
            passages_of(&doc)[0].locator,
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
        let doc = labelled(
            document(vec![spanning(3, 4, &words(10)), pdf_block(&words(10), 4)]),
            &[(3, "xii"), (4, "xiii")],
        );
        assert_eq!(
            passages_of(&doc)[0].locator,
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
        let passages = build(vec![
            epub_heading(1, "A", 0),
            on_print_page("12", epub_block(&words(10), 0)),
            on_print_page("13", epub_block(&words(10), 0)),
            epub_block(&words(10), 0),
        ]);
        assert_eq!(passages.len(), 1);
        assert_eq!(
            passages[0].locator,
            PassageLocator::Epub {
                href: "ch1.xhtml".to_string(),
                para_start: 2,
                para_end: 4,
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
        let passages = build(vec![
            epub_heading(1, "Book", 0),
            epub_heading(2, "One", 0),
            epub_block(&words(150), 0),
            epub_block(&words(150), 0),
            epub_block(&words(10), 1),
            epub_heading(2, "Two", 1),
            epub_block(&words(10), 1),
        ]);
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
        let passages = build(vec![
            epub_heading(1, "Notes", 0),
            epub_block(&words(10), 0),
            epub_block(&words(10), 0),
            epub_heading(1, "Notes", 0),
            epub_block(&words(10), 0),
        ]);
        let places: Vec<_> = passages.iter().map(heading_place).collect();
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
        let passages = build(vec![
            epub_block(&words(10), 0),
            epub_heading(1, "One", 1),
            epub_block(&words(10), 1),
        ]);
        let places: Vec<_> = passages.iter().map(heading_place).collect();
        assert_eq!(places, [None, Some(("One".to_string(), 1, 1))]);
    }

    #[test]
    fn pieces_of_a_split_epub_block_share_its_paragraph_number() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        let passages = build(vec![
            epub_heading(1, "One", 0),
            epub_block(&words(10), 0),
            epub_block(&text, 0),
        ]);
        let places: Vec<_> = passages.iter().map(heading_place).collect();
        assert!(places.len() >= 3, "{places:?}");
        assert_eq!(places[0].as_ref().map(|place| place.1), Some(1));
        for place in &places[1..] {
            assert_eq!(place, &Some(("One".to_string(), 2, 2)));
        }
    }

    #[test]
    fn pieces_of_a_split_block_keep_its_locator() {
        let text = (0..50).map(sentence).collect::<Vec<_>>().join(" ");
        let doc = labelled(document(vec![pdf_block(&text, 9)]), &[(9, "ix")]);
        let passages = passages_of(&doc);
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
        let mut blocks: Vec<Block> = (0..6).map(|_| pdf_block(&words(100), 1)).collect();
        blocks.push(heading(1, "B"));
        blocks.push(pdf_block(
            &(0..50).map(sentence).collect::<Vec<_>>().join(" "),
            2,
        ));
        let passages = build(blocks);
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
        assert_eq!(block_ids.last(), Some(&7));
    }

    #[test]
    fn pdf_and_epub_blocks_never_share_a_passage() {
        let passages = build(vec![pdf_block(&words(10), 1), epub_block(&words(10), 0)]);
        assert_eq!(block_counts(&passages), [1, 1]);
    }

    #[test]
    fn no_blocks_make_no_passages() {
        assert!(build(Vec::new()).is_empty());
    }
}
