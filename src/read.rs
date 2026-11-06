//! Reading a book: passages by id, PDF page or outline entry, rendered as
//! Markdown with locators; the outline listing; and the book summary.

use std::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::Serialize;

use crate::document::{Document, Format, OutlineEntry, Warning};
use crate::index::book::Book;
use crate::passages::{Passage, PassageLocator};
use crate::settings::{READ_MAX_CHARS, READ_NEIGHBOURS_MAX};
use crate::{Error, Result};

/// What to read: one passage, the passages on one PDF page, or the passages
/// of one outline entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAt {
    /// The passage with this id.
    Passage(u32),
    /// The passages whose pages include this physical page of a PDF,
    /// counted from 1. An EPUB has no pages, so reading one by page is an
    /// error.
    Page(u32),
    /// The passages of the outline entry with this id, up to the next entry
    /// at the same level or above.
    Outline(u32),
}

/// `before` and `after` count passages added on each side of the range `at`
/// names, at most `READ_NEIGHBOURS_MAX` each. `cursor` is the id of the
/// passage to start at, from the `next_cursor` of an earlier call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadParams {
    /// The passages to read.
    pub at: ReadAt,
    /// How many passages to add before them, at most [`READ_NEIGHBOURS_MAX`].
    pub before: u32,
    /// How many passages to add after them, at most [`READ_NEIGHBOURS_MAX`].
    pub after: u32,
    /// The passage to start at, `None` for the first page of the range.
    pub cursor: Option<u32>,
}

/// A passage that was read.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PassageRef {
    /// The passage's id.
    pub passage_id: u32,
    /// The texts of the headings above it, outermost first.
    pub section: Vec<String>,
    /// Where it is in the book.
    pub locator: PassageLocator,
    /// The ids of the blocks it covers, in order, each once. A block that
    /// the passage holds only a piece of is listed too.
    pub blocks: Vec<u32>,
}

/// One page of a read. `next_cursor` is the id of the first passage of the
/// range that did not fit, if any.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReadResult {
    /// The passages as Markdown. Each passage is a `<!-- passage N ·
    /// locator -->` comment followed by its text. The first passage, and
    /// each one whose section differs from the one before it, is headed by
    /// its section path as a `##` heading, unless it has none.
    pub markdown: String,
    /// The passages on this page, in order.
    pub passages: Vec<PassageRef>,
    /// The passage to continue at, given as `cursor` with the same `at`,
    /// `before` and `after`; `None` when the range is done.
    pub next_cursor: Option<u32>,
}

/// An outline entry and where its text starts: the first passage at or after
/// the entry's heading, or `None` when the entry points nowhere or past the
/// last passage.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct OutlineItem {
    /// The entry's id, which [`ReadAt::Outline`] takes.
    pub id: u32,
    /// The entry's text.
    pub title: String,
    /// Its depth in the table of contents, the top level being 1.
    pub level: u8,
    /// The first passage of its text.
    pub passage_id: Option<u32>,
    /// That passage's place in the book.
    pub locator: Option<PassageLocator>,
}

/// The table of contents of a book.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct OutlineResult {
    /// The entries in reading order; empty when the book has no outline.
    pub entries: Vec<OutlineItem>,
}

/// What the book is and how far its index is built. `lexical_ready` is always
/// true; `semantic_ready` is false until stage 2 (the embeddings) exists.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct BookInfo {
    /// The title, if the file gives one.
    pub title: Option<String>,
    /// The authors, in the order the file lists them.
    pub authors: Vec<String>,
    /// The language an EPUB declares; always `None` for a PDF.
    pub language: Option<String>,
    /// PDF or EPUB.
    pub format: Format,
    /// The number of pages of a PDF.
    pub page_count: Option<u32>,
    /// The number of entries in an EPUB's spine, its files in reading
    /// order, which the command line prints as `Chapters`.
    pub spine_count: Option<u32>,
    /// The number of passages, whose ids run from 0 to one less than this.
    pub passage_count: usize,
    /// Whether lexical search can run: always true, since the summary is made
    /// from the index.
    pub lexical_ready: bool,
    /// Whether the passages are embedded, so that semantic and hybrid search
    /// start without embedding the book first.
    pub semantic_ready: bool,
    /// The parts of the file the reader could not read and left out.
    pub warnings: Vec<Warning>,
}

/// Checks the arguments that need no book.
pub(crate) fn validate(p: &ReadParams) -> Result<()> {
    for (name, count) in [("before", p.before), ("after", p.after)] {
        if count > READ_NEIGHBOURS_MAX {
            return Err(Error::InvalidArgument(format!(
                "{name} is {count}; it must be at most {READ_NEIGHBOURS_MAX}"
            )));
        }
    }
    Ok(())
}

/// Reads the passages `p` names, as one page of Markdown.
///
/// The range is the passage, the passages on the page, or the passages of the
/// outline entry, widened by `before` and `after` and clamped to the book. The
/// page starts at `p.cursor` (the range start by default) and takes passages
/// while the Markdown stays within `READ_MAX_CHARS` characters, but always at
/// least one. `next_cursor` is the first passage left over, if any.
pub fn read(book: &Book, p: &ReadParams) -> Result<ReadResult> {
    read_passages(&book.document, &book.passages, p)
}

/// The outline entries in order, each with the first passage of its text.
pub fn outline(book: &Book) -> OutlineResult {
    outline_of(&book.document, &book.passages)
}

/// A summary of the book. Stage 1 of the index exists whenever a [`Book`]
/// does, so only stage 2 is looked up.
pub fn info(book: &Book) -> BookInfo {
    let metadata = &book.document.metadata;
    BookInfo {
        title: metadata.title.clone(),
        authors: metadata.authors.clone(),
        language: metadata.language.clone(),
        format: book.document.source.format.clone(),
        page_count: metadata.page_count,
        spine_count: metadata.spine_count,
        passage_count: book.passages.len(),
        lexical_ready: true,
        semantic_ready: book.vectors_ready(),
        warnings: book.document.warnings.clone(),
    }
}

fn read_passages(document: &Document, passages: &[Passage], p: &ReadParams) -> Result<ReadResult> {
    validate(p)?;
    let range = resolve(document, passages, p)?;
    let (first, last) = (*range.start(), *range.end());
    let start = match p.cursor {
        None => first,
        Some(cursor) if range.contains(&(cursor as usize)) => cursor as usize,
        Some(cursor) => {
            return Err(Error::InvalidArgument(format!(
                "cursor {cursor} is not inside the passages {first}–{last} being read; \
                 use the next_cursor of the previous call"
            )));
        }
    };
    let mut markdown = String::new();
    // Characters in `markdown`, not counting the newline that ends it.
    let mut chars = 0;
    let mut refs: Vec<PassageRef> = Vec::new();
    let mut next = start;
    while next <= last {
        let passage = &passages[next];
        let heading = !passage.section.is_empty()
            && refs
                .last()
                .is_none_or(|prev| prev.section != passage.section);
        let piece = render(passage, heading);
        let separator = if refs.is_empty() {
            0
        } else {
            BLOCK_BREAK.len()
        };
        let piece_chars = piece.chars().count();
        if !refs.is_empty() && chars + separator + piece_chars + 1 > READ_MAX_CHARS {
            break;
        }
        if !refs.is_empty() {
            markdown.push_str(BLOCK_BREAK);
        }
        markdown.push_str(&piece);
        chars += separator + piece_chars;
        refs.push(PassageRef {
            passage_id: passage.id,
            section: passage.section.clone(),
            locator: passage.locator.clone(),
            blocks: block_ids(passage),
        });
        next += 1;
    }
    markdown.push('\n');
    Ok(ReadResult {
        markdown,
        passages: refs,
        next_cursor: (next <= last).then(|| passages[next].id),
    })
}

/// The ids of the blocks `passage` takes text from, in order, each once. The
/// pieces of one block are next to each other, so dropping repeats is enough.
fn block_ids(passage: &Passage) -> Vec<u32> {
    let mut ids: Vec<u32> = passage.blocks.iter().map(|range| range.block_id).collect();
    ids.dedup();
    ids
}

/// Markdown blocks are separated by a blank line.
const BLOCK_BREAK: &str = "\n\n";

/// A passage as Markdown: its section as a heading when `heading` is set, a
/// comment with its id and locator, and its text.
fn render(passage: &Passage, heading: bool) -> String {
    let mut blocks = Vec::with_capacity(3);
    if heading {
        blocks.push(format!("## {}", passage.section.join(" › ")));
    }
    blocks.push(format!(
        "<!-- passage {} · {} -->",
        passage.id,
        locator_text(&passage.locator)
    ));
    blocks.push(passage.text.clone());
    blocks.join(BLOCK_BREAK)
}

/// The indexes in `passages` of the first and last passage `p.at` names,
/// widened by `p.before` and `p.after`.
fn resolve(
    document: &Document,
    passages: &[Passage],
    p: &ReadParams,
) -> Result<RangeInclusive<usize>> {
    let range = match p.at {
        ReadAt::Passage(id) => {
            let index = id as usize;
            if index >= passages.len() {
                return Err(Error::InvalidArgument(format!(
                    "passage {id} does not exist; the book has {} passages",
                    passages.len()
                )));
            }
            index..=index
        }
        ReadAt::Page(page) => page_range(document, passages, page)?,
        ReadAt::Outline(id) => entry_range(document, passages, id)?,
    };
    let first = range.start().saturating_sub(p.before as usize);
    let last = range
        .end()
        .saturating_add(p.after as usize)
        .min(passages.len() - 1);
    Ok(first..=last)
}

/// The passages whose pages hold `page`, from the first to the last.
fn page_range(
    document: &Document,
    passages: &[Passage],
    page: u32,
) -> Result<RangeInclusive<usize>> {
    if document.source.format != Format::Pdf {
        return Err(Error::InvalidArgument(
            "page addressing applies to PDFs; use passage_id or outline_id".to_string(),
        ));
    }
    let holds = |passage: &Passage| {
        matches!(
            passage.locator,
            PassageLocator::Pdf { page_start, page_end, .. } if page_start <= page && page <= page_end
        )
    };
    match (
        passages.iter().position(holds),
        passages.iter().rposition(holds),
    ) {
        (Some(first), Some(last)) => Ok(first..=last),
        _ => Err(Error::InvalidArgument(format!("page {page} has no text"))),
    }
}

/// The passages of outline entry `id`: from its first passage to the passage
/// before the first passage of the next entry whose level is the same or
/// higher, or to the last passage when there is none. Entries that point at no
/// passage cannot bound a range and are passed over.
fn entry_range(
    document: &Document,
    passages: &[Passage],
    id: u32,
) -> Result<RangeInclusive<usize>> {
    let outline = &document.outline;
    if outline.is_empty() {
        let addresses = match document.source.format {
            Format::Pdf => "passage_id or page",
            Format::Epub => "passage_id",
        };
        return Err(Error::InvalidArgument(format!(
            "this book has no outline; use {addresses}"
        )));
    }
    let Some(position) = outline.iter().position(|entry| entry.id == id) else {
        return Err(Error::InvalidArgument(format!(
            "outline entry {id} does not exist; the book has {} entries",
            outline.len()
        )));
    };
    let entry = &outline[position];
    let no_text = || Error::InvalidArgument(format!("outline entry {id} has no text after it"));
    let first = first_passage(passages, entry).ok_or_else(no_text)?;
    let end = outline[position + 1..]
        .iter()
        .filter(|next| next.level <= entry.level)
        .find_map(|next| first_passage(passages, next))
        .unwrap_or(passages.len());
    if end <= first {
        // The next entry starts where this one does: there is no text between.
        return Err(no_text());
    }
    Ok(first..=end - 1)
}

/// The index of the first passage at or after the block `entry` points at: the
/// first passage that holds the block, or when headings or other blocks that
/// no passage holds are in the way, the first one after it. `None` when the
/// entry points at no block, or past the last passage.
fn first_passage(passages: &[Passage], entry: &OutlineEntry) -> Option<usize> {
    let block_id = entry.block_id?;
    // A passage holds consecutive blocks, so the first passage that ends at or
    // after the block either holds it or starts after it.
    let index = passages.partition_point(|passage| last_block(passage) < block_id);
    (index < passages.len()).then_some(index)
}

fn last_block(passage: &Passage) -> u32 {
    passage.blocks.last().map_or(0, |range| range.block_id)
}

fn outline_of(document: &Document, passages: &[Passage]) -> OutlineResult {
    let entries = document
        .outline
        .iter()
        .map(|entry| {
            let passage = first_passage(passages, entry).map(|index| &passages[index]);
            OutlineItem {
                id: entry.id,
                title: entry.title.clone(),
                level: entry.level,
                passage_id: passage.map(|passage| passage.id),
                locator: passage.map(|passage| passage.locator.clone()),
            }
        })
        .collect();
    OutlineResult { entries }
}

/// Where a passage is, as a person would cite it: `p. 12–13 (xii–xiii)` for a
/// PDF and `CHAPTER I ¶3–5, print p. 45` for an EPUB, whose paragraphs are
/// counted from the nearest heading. EPUB text before the first heading is
/// placed by its file instead: `cover.xhtml ¶1`.
pub fn locator_text(locator: &PassageLocator) -> String {
    match locator {
        PassageLocator::Pdf {
            page_start,
            page_end,
            label_start,
            label_end,
        } => {
            let mut text = range_text("p. ", *page_start, *page_end);
            let labels = match (label_start, label_end) {
                (Some(start), Some(end)) if page_start != page_end && start != end => {
                    Some(format!("{start}–{end}"))
                }
                (Some(label), _) | (None, Some(label)) => Some(label.clone()),
                (None, None) => None,
            };
            if let Some(labels) = labels {
                text.push_str(&format!(" ({labels})"));
            }
            text
        }
        PassageLocator::Epub {
            href,
            para_start,
            para_end,
            print_page,
            under_heading,
        } => {
            let mut text = match under_heading {
                Some(place) => format!(
                    "{} {}",
                    place.text,
                    range_text("¶", place.para_start, place.para_end)
                ),
                None => format!("{href} {}", range_text("¶", *para_start, *para_end)),
            };
            if let Some(page) = print_page {
                text.push_str(&format!(", print p. {page}"));
            }
            text
        }
    }
}

/// `{prefix}{start}`, or `{prefix}{start}–{end}` when they differ.
fn range_text(prefix: &str, start: u32, end: u32) -> String {
    if start == end {
        format!("{prefix}{start}")
    } else {
        format!("{prefix}{start}–{end}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::document::{Metadata, Source};
    use crate::passages::BlockRange;
    use crate::passages::HeadingPlace;
    use crate::settings::{READ_MAX_CHARS, READ_NEIGHBOURS_MAX};

    fn pdf(
        pages: (u32, u32),
        start_label: Option<&str>,
        end_label: Option<&str>,
    ) -> PassageLocator {
        PassageLocator::Pdf {
            page_start: pages.0,
            page_end: pages.1,
            label_start: start_label.map(str::to_string),
            label_end: end_label.map(str::to_string),
        }
    }

    fn epub(paras: (u32, u32), print_page: Option<&str>) -> PassageLocator {
        PassageLocator::Epub {
            href: "ch1.xhtml".to_string(),
            para_start: paras.0,
            para_end: paras.1,
            print_page: print_page.map(str::to_string),
            under_heading: None,
        }
    }

    fn epub_under(heading_paras: (u32, u32), print_page: Option<&str>) -> PassageLocator {
        PassageLocator::Epub {
            href: "ch1.xhtml".to_string(),
            para_start: 7,
            para_end: 9,
            print_page: print_page.map(str::to_string),
            under_heading: Some(HeadingPlace {
                text: "CHAPTER I. Down the Rabbit-Hole".to_string(),
                para_start: heading_paras.0,
                para_end: heading_paras.1,
            }),
        }
    }

    #[test]
    fn an_epub_locator_names_the_heading_and_paragraphs_under_it() {
        let cases = [
            (
                epub_under((1, 1), None),
                "CHAPTER I. Down the Rabbit-Hole ¶1",
            ),
            (
                epub_under((1, 3), None),
                "CHAPTER I. Down the Rabbit-Hole ¶1–3",
            ),
            (
                epub_under((1, 3), Some("45")),
                "CHAPTER I. Down the Rabbit-Hole ¶1–3, print p. 45",
            ),
        ];
        for (locator, text) in cases {
            assert_eq!(locator_text(&locator), text, "{locator:?}");
        }
    }

    #[test]
    fn a_pdf_locator_names_pages_and_labels() {
        let cases = [
            (pdf((12, 12), None, None), "p. 12"),
            (pdf((12, 13), None, None), "p. 12–13"),
            (pdf((12, 12), Some("xii"), None), "p. 12 (xii)"),
            (pdf((12, 12), None, Some("xii")), "p. 12 (xii)"),
            (pdf((12, 12), Some("xii"), Some("xii")), "p. 12 (xii)"),
            (
                pdf((12, 13), Some("xii"), Some("xiii")),
                "p. 12–13 (xii–xiii)",
            ),
            (pdf((12, 13), Some("xii"), None), "p. 12–13 (xii)"),
            (pdf((12, 13), None, Some("xiii")), "p. 12–13 (xiii)"),
            (pdf((12, 13), Some("7"), Some("7")), "p. 12–13 (7)"),
        ];
        for (locator, text) in cases {
            assert_eq!(locator_text(&locator), text, "{locator:?}");
        }
    }

    #[test]
    fn an_epub_locator_before_any_heading_names_the_file_paragraphs_and_print_page() {
        let cases = [
            (epub((3, 3), None), "ch1.xhtml ¶3"),
            (epub((3, 5), None), "ch1.xhtml ¶3–5"),
            (epub((3, 3), Some("45")), "ch1.xhtml ¶3, print p. 45"),
            (epub((3, 5), Some("45")), "ch1.xhtml ¶3–5, print p. 45"),
        ];
        for (locator, text) in cases {
            assert_eq!(locator_text(&locator), text, "{locator:?}");
        }
    }

    fn passage(id: u32, text: &str, section: &[&str], blocks: &[u32]) -> Passage {
        Passage {
            id,
            text: text.to_string(),
            section: section.iter().map(|s| s.to_string()).collect(),
            blocks: blocks
                .iter()
                .map(|&block_id| BlockRange {
                    block_id,
                    block_offset: 0,
                    start: 0,
                    end: text.len(),
                })
                .collect(),
            locator: pdf((id + 1, id + 1), None, None),
        }
    }

    fn entry(id: u32, level: u8, block_id: Option<u32>) -> OutlineEntry {
        OutlineEntry {
            id,
            title: format!("Entry {id}"),
            level,
            block_id,
        }
    }

    fn document(format: Format, outline: Vec<OutlineEntry>) -> Document {
        Document {
            schema_version: crate::document::SCHEMA_VERSION,
            source: Source {
                sha256: "0".repeat(64),
                file_name: "book".to_string(),
                format,
            },
            metadata: Metadata {
                title: None,
                authors: Vec::new(),
                language: None,
                page_count: None,
                spine_count: None,
            },
            outline,
            sections: Vec::new(),
            files: Vec::new(),
            page_labels: Vec::new(),
            blocks: Vec::new(),
            furniture: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn params(at: ReadAt) -> ReadParams {
        ReadParams {
            at,
            before: 0,
            after: 0,
            cursor: None,
        }
    }

    fn ids(result: &ReadResult) -> Vec<u32> {
        result.passages.iter().map(|p| p.passage_id).collect()
    }

    fn invalid<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(Error::InvalidArgument(message)) => message,
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// Block 0 is a heading, 1 and 2 share passage 0, 3 is a heading, 4 is
    /// passage 1, 5 is a long block split across passages 2 and 3, 6 is a
    /// heading with nothing after it that a passage holds, and 7 is passage 4.
    fn book() -> (Document, Vec<Passage>) {
        let passages = vec![
            passage(0, "one\n\ntwo", &["A"], &[1, 2]),
            passage(1, "three", &["A", "B"], &[4]),
            passage(2, "four a", &["C"], &[5]),
            passage(3, "four b", &["C"], &[5]),
            passage(4, "five", &["D"], &[7]),
        ];
        let outline = vec![
            entry(0, 1, Some(0)), // heading before passage 0
            entry(1, 2, Some(3)), // heading before passage 1
            entry(2, 1, Some(2)), // a block inside passage 0
            entry(3, 1, Some(5)), // a block split over passages 2 and 3
            entry(4, 1, Some(6)), // heading before passage 4
            entry(5, 1, None),    // points nowhere
            entry(6, 1, Some(9)), // past the last block
        ];
        (document(Format::Pdf, outline), passages)
    }

    #[test]
    fn an_entry_starts_at_the_first_passage_at_or_after_its_block() {
        let (document, passages) = book();

        let starts: Vec<_> = document
            .outline
            .iter()
            .map(|e| first_passage(&passages, e))
            .collect();

        assert_eq!(
            starts,
            [Some(0), Some(1), Some(0), Some(2), Some(4), None, None]
        );
    }

    #[test]
    fn the_outline_lists_the_first_passage_of_each_entry() {
        let (document, passages) = book();

        let entries = outline_of(&document, &passages).entries;

        let listed: Vec<_> = entries.iter().map(|e| e.passage_id).collect();
        assert_eq!(
            listed,
            [Some(0), Some(1), Some(0), Some(2), Some(4), None, None]
        );
        assert_eq!(entries[3].locator, Some(passages[2].locator.clone()));
        assert_eq!(entries[5].locator, None);
        assert_eq!(
            (entries[3].id, entries[3].title.as_str(), entries[3].level),
            (3, "Entry 3", 1)
        );
    }

    #[test]
    fn an_entry_runs_to_the_passage_before_the_next_entry_of_its_level_or_above() {
        let (document, passages) = book();
        let range = |id| resolve(&document, &passages, &params(ReadAt::Outline(id)));

        // Entry 1 is level 2; entry 2 (level 1) starts at passage 0, which is
        // before it, so the range is empty.
        assert!(range(0).is_err());
        // Entry 3 runs up to entry 4, which starts at passage 4.
        assert_eq!(range(3).unwrap(), 2..=3);
        // Entry 4 has only unlocatable entries after it, so it runs to the end.
        assert_eq!(range(4).unwrap(), 4..=4);
    }

    #[test]
    fn deeper_entries_are_part_of_their_parent() {
        let passages = vec![
            passage(0, "a", &["P"], &[1]),
            passage(1, "b", &["P", "Q"], &[3]),
            passage(2, "c", &["P", "Q", "R"], &[5]),
            passage(3, "d", &["S"], &[7]),
        ];
        let outline = vec![
            entry(0, 1, Some(0)),
            entry(1, 2, Some(2)),
            entry(2, 3, Some(4)),
            entry(3, 1, Some(6)),
        ];
        let document = document(Format::Epub, outline);
        let range = |id| resolve(&document, &passages, &params(ReadAt::Outline(id))).unwrap();

        assert_eq!(range(0), 0..=2);
        assert_eq!(range(1), 1..=2);
        assert_eq!(range(2), 2..=2);
        assert_eq!(range(3), 3..=3);
    }

    #[test]
    fn an_unlocatable_boundary_is_skipped() {
        let passages = vec![
            passage(0, "a", &["P"], &[1]),
            passage(1, "b", &["Q"], &[3]),
            passage(2, "c", &["R"], &[5]),
        ];
        let outline = vec![
            entry(0, 1, Some(0)),
            entry(1, 1, None),
            entry(2, 1, Some(4)),
        ];
        let document = document(Format::Epub, outline);

        let range = resolve(&document, &passages, &params(ReadAt::Outline(0))).unwrap();

        assert_eq!(range, 0..=1);
    }

    #[test]
    fn an_entry_without_text_is_invalid() {
        let (document, passages) = book();

        for id in [5, 6] {
            assert_eq!(
                invalid(resolve(&document, &passages, &params(ReadAt::Outline(id)))),
                format!("outline entry {id} has no text after it")
            );
        }
        // Two entries of one level that start at the same passage: the first
        // has no text of its own.
        let passages = vec![passage(0, "a", &["P"], &[2])];
        let adjacent = self::document(
            Format::Epub,
            vec![entry(0, 1, Some(0)), entry(1, 1, Some(1))],
        );
        assert_eq!(
            invalid(resolve(&adjacent, &passages, &params(ReadAt::Outline(0)))),
            "outline entry 0 has no text after it"
        );
        assert_eq!(
            resolve(&adjacent, &passages, &params(ReadAt::Outline(1))).unwrap(),
            0..=0
        );
    }

    #[test]
    fn an_unknown_entry_names_the_count() {
        let (document, passages) = book();

        assert_eq!(
            invalid(resolve(&document, &passages, &params(ReadAt::Outline(7)))),
            "outline entry 7 does not exist; the book has 7 entries"
        );
        assert_eq!(
            invalid(resolve(
                &document,
                &passages,
                &params(ReadAt::Outline(u32::MAX))
            )),
            format!(
                "outline entry {} does not exist; the book has 7 entries",
                u32::MAX
            )
        );
    }

    #[test]
    fn a_book_with_no_passages_has_nothing_to_read() {
        let document = document(Format::Pdf, vec![entry(0, 1, Some(0))]);

        for at in [ReadAt::Passage(0), ReadAt::Page(1), ReadAt::Outline(0)] {
            let message = invalid(read_passages(&document, &[], &params(at)));
            assert!(
                message.contains("does not exist")
                    || message.contains("has no text")
                    || message.contains("no text after"),
                "{message}"
            );
        }
        assert!(outline_of(&document, &[]).entries[0].passage_id.is_none());
    }

    #[test]
    fn neighbours_widen_a_range_up_to_the_book() {
        let (document, passages) = book();
        let at = |at, before, after| {
            resolve(
                &document,
                &passages,
                &ReadParams {
                    at,
                    before,
                    after,
                    cursor: None,
                },
            )
            .unwrap()
        };

        assert_eq!(at(ReadAt::Passage(2), 1, 1), 1..=3);
        assert_eq!(at(ReadAt::Passage(0), 10, 0), 0..=0);
        assert_eq!(at(ReadAt::Passage(4), 0, 10), 4..=4);
        assert_eq!(at(ReadAt::Passage(2), 10, 10), 0..=4);
        assert_eq!(at(ReadAt::Outline(3), 1, 1), 1..=4);
    }

    #[test]
    fn a_read_passage_names_its_blocks_once_each_in_order() {
        // Passage 1 holds two pieces of block 2 and the start of block 3;
        // passage 2 holds the rest of block 3 and block 4.
        let passages = vec![
            passage(0, "a", &[], &[0, 1]),
            passage(1, "b c d", &[], &[2, 2, 3]),
            passage(2, "e", &[], &[3, 4]),
        ];
        let document = document(Format::Pdf, Vec::new());
        let read_blocks = |params: &ReadParams| -> Vec<Vec<u32>> {
            let result = read_passages(&document, &passages, params).unwrap();
            result.passages.into_iter().map(|p| p.blocks).collect()
        };

        assert_eq!(read_blocks(&params(ReadAt::Passage(1))), [[2, 3]]);
        let neighbours = ReadParams {
            before: 1,
            after: 1,
            ..params(ReadAt::Passage(1))
        };
        assert_eq!(
            read_blocks(&neighbours),
            [vec![0, 1], vec![2, 3], vec![3, 4]]
        );
    }

    #[test]
    fn a_page_is_the_passages_whose_pages_hold_it() {
        let mut passages = vec![
            passage(0, "a", &[], &[0]),
            passage(1, "b", &[], &[1]),
            passage(2, "c", &[], &[2]),
            passage(3, "d", &[], &[3]),
        ];
        passages[0].locator = pdf((1, 1), None, None);
        passages[1].locator = pdf((2, 3), None, None);
        passages[2].locator = pdf((3, 3), None, None);
        passages[3].locator = pdf((5, 5), None, None);
        let document = document(Format::Pdf, Vec::new());
        let page = |n| resolve(&document, &passages, &params(ReadAt::Page(n)));

        assert_eq!(page(1).unwrap(), 0..=0);
        assert_eq!(page(2).unwrap(), 1..=1);
        assert_eq!(page(3).unwrap(), 1..=2);
        assert_eq!(page(5).unwrap(), 3..=3);
        for n in [0, 4, 6] {
            assert_eq!(invalid(page(n)), format!("page {n} has no text"));
        }
    }

    #[test]
    fn page_addressing_is_for_pdfs() {
        let passages = vec![passage(0, "a", &[], &[0])];
        let document = document(Format::Epub, Vec::new());

        assert_eq!(
            invalid(resolve(&document, &passages, &params(ReadAt::Page(1)))),
            "page addressing applies to PDFs; use passage_id or outline_id"
        );
    }

    #[test]
    fn a_book_without_an_outline_says_so_and_names_the_addresses_it_has() {
        let passages = vec![passage(0, "a", &[], &[0])];
        let epub = document(Format::Epub, Vec::new());
        let pdf = document(Format::Pdf, Vec::new());

        assert_eq!(
            invalid(resolve(&epub, &passages, &params(ReadAt::Outline(0)))),
            "this book has no outline; use passage_id"
        );
        assert_eq!(
            invalid(resolve(&pdf, &passages, &params(ReadAt::Outline(0)))),
            "this book has no outline; use passage_id or page"
        );
    }

    #[test]
    fn the_markdown_names_a_section_when_it_changes() {
        let (document, passages) = book();

        let result = read_passages(
            &document,
            &passages,
            &ReadParams {
                at: ReadAt::Passage(0),
                before: 0,
                after: 3,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(
            result.markdown,
            "## A\n\n<!-- passage 0 · p. 1 -->\n\none\n\ntwo\n\n\
             ## A › B\n\n<!-- passage 1 · p. 2 -->\n\nthree\n\n\
             ## C\n\n<!-- passage 2 · p. 3 -->\n\nfour a\n\n\
             <!-- passage 3 · p. 4 -->\n\nfour b\n"
        );
        assert_eq!(ids(&result), [0, 1, 2, 3]);
        assert_eq!(result.next_cursor, None);
        assert_eq!(result.passages[1].section, ["A", "B"]);
        assert_eq!(result.passages[1].locator, passages[1].locator);
    }

    #[test]
    fn a_page_that_continues_names_its_section_again() {
        let (document, passages) = book();

        let result = read_passages(
            &document,
            &passages,
            &ReadParams {
                at: ReadAt::Passage(2),
                before: 0,
                after: 1,
                cursor: Some(3),
            },
        )
        .unwrap();

        assert_eq!(ids(&result), [3]);
        assert!(result.markdown.starts_with("## C\n\n<!-- passage 3 ·"));
    }

    #[test]
    fn a_passage_without_a_section_has_no_heading() {
        let passages = vec![passage(0, "a", &[], &[0]), passage(1, "b", &[], &[1])];
        let document = document(Format::Pdf, Vec::new());

        let result = read_passages(
            &document,
            &passages,
            &ReadParams {
                at: ReadAt::Passage(0),
                before: 0,
                after: 1,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(
            result.markdown,
            "<!-- passage 0 · p. 1 -->\n\na\n\n<!-- passage 1 · p. 2 -->\n\nb\n"
        );
    }

    /// `count` passages of `chars` characters each, in one section.
    fn long_book(count: u32, chars: usize, filler: char) -> (Document, Vec<Passage>) {
        let text: String = std::iter::repeat_n(filler, chars).collect();
        let passages = (0..count)
            .map(|id| passage(id, &text, &["S"], &[id + 1]))
            .collect();
        (document(Format::Pdf, Vec::new()), passages)
    }

    fn read_all(document: &Document, passages: &[Passage], at: ReadAt) -> Vec<ReadResult> {
        let mut pages = Vec::new();
        let mut cursor = None;
        loop {
            let result = read_passages(
                document,
                passages,
                &ReadParams {
                    at,
                    before: 0,
                    after: 10,
                    cursor,
                },
            )
            .unwrap();
            cursor = result.next_cursor;
            pages.push(result);
            if cursor.is_none() {
                return pages;
            }
        }
    }

    #[test]
    fn a_page_stops_before_the_passage_that_would_not_fit() {
        let (document, passages) = long_book(10, 3000, 'x');

        let pages = read_all(&document, &passages, ReadAt::Passage(0));

        // Each passage adds 3,000 characters and a comment line of 25; the
        // section line is on the first passage of a page: two passages fit.
        assert_eq!(pages.len(), 5);
        for page in &pages {
            assert_eq!(page.passages.len(), 2);
            let chars = page.markdown.chars().count();
            assert!(chars <= READ_MAX_CHARS, "{chars}");
            assert!(page.markdown.ends_with("x\n"));
        }
        assert_eq!(pages[0].next_cursor, Some(2));
        assert_eq!(pages[4].next_cursor, None);
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // 'é' takes two bytes: 3,500 of them are 7,000 bytes, and two passages
        // are 14,000 bytes but fewer than 8,000 characters.
        let (document, passages) = long_book(2, 3500, 'é');

        let pages = read_all(&document, &passages, ReadAt::Passage(0));

        assert_eq!(pages.len(), 1);
        assert!(pages[0].markdown.len() > READ_MAX_CHARS);
        assert!(pages[0].markdown.chars().count() <= READ_MAX_CHARS);
    }

    #[test]
    fn a_passage_longer_than_the_limit_is_still_returned() {
        let (document, passages) = long_book(3, READ_MAX_CHARS + 500, 'x');

        let pages = read_all(&document, &passages, ReadAt::Passage(0));

        assert_eq!(pages.len(), 3);
        for page in &pages {
            assert_eq!(page.passages.len(), 1);
        }
        assert_eq!(
            pages.iter().map(|p| p.next_cursor).collect::<Vec<_>>(),
            [Some(1), Some(2), None]
        );
    }

    #[test]
    fn the_cursor_must_be_a_passage_of_the_range() {
        let (document, passages) = long_book(6, 10, 'x');
        let read = |cursor| {
            read_passages(
                &document,
                &passages,
                &ReadParams {
                    at: ReadAt::Passage(3),
                    before: 1,
                    after: 1,
                    cursor: Some(cursor),
                },
            )
        };

        assert_eq!(ids(&read(2).unwrap()), [2, 3, 4]);
        assert_eq!(ids(&read(4).unwrap()), [4]);
        for cursor in [0, 1, 5, 6, u32::MAX] {
            let message = invalid(read(cursor));
            assert!(
                message.starts_with(&format!("cursor {cursor} is not inside")),
                "{message}"
            );
            assert!(message.contains("2–4"), "{message}");
        }
    }

    #[test]
    fn neighbours_above_the_limit_are_invalid() {
        let over = READ_NEIGHBOURS_MAX + 1;
        for (before, after, name) in [(over, 0, "before"), (0, over, "after")] {
            let message = invalid(validate(&ReadParams {
                at: ReadAt::Passage(0),
                before,
                after,
                cursor: None,
            }));
            assert_eq!(
                message,
                format!("{name} is {over}; it must be at most {READ_NEIGHBOURS_MAX}")
            );
        }
        assert!(
            validate(&ReadParams {
                at: ReadAt::Passage(0),
                before: READ_NEIGHBOURS_MAX,
                after: READ_NEIGHBOURS_MAX,
                cursor: None,
            })
            .is_ok()
        );
    }
}
