//! Reads a PDF into the [`Document`] model with `pdf_oxide`.
//!
//! The reader turns each page's text lines into top-origin [`Line`]s, hands
//! them to the layout functions, and then places the bookmarks as headings.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;

use pdf_oxide::PdfDocument;
use pdf_oxide::extractors::{PageLabelExtractor, PageLabelRange};
use pdf_oxide::layout::TextLine;
use pdf_oxide::object::Object;
use pdf_oxide::optional_content::decode_pdf_text_string;
use pdf_oxide::outline::{Destination, OutlineItem};

use super::assign_sections;
use super::pdf_layout::{LaidParagraph, Line, PageLines, furniture, paragraphs};
use crate::document::{
    Block, BlockKind, Document, Locator, Metadata, OutlineEntry, SCHEMA_VERSION, Source, Warning,
};
use crate::error::{Error, Result};
use crate::normalize::normalize;

/// A gap between two words above this many font sizes is a column gutter or
/// two labels on one baseline, not a word space.
const SPLIT_GAP: f32 = 2.0;
/// A gap above this many font sizes is a word space. Below it the words join,
/// which mends numbers that pdf_oxide cuts apart (`199 6`) and keeps a
/// line-end hyphen on its word.
const SPACE_GAP: f32 = 0.08;

/// Reads a PDF. Fails with `Unreadable` when the file cannot be parsed or
/// needs a password, and with `NoTextLayer` when no page has any text.
pub fn read_pdf(path: &Path, source: Source) -> Result<Document> {
    let doc = PdfDocument::open(path).map_err(|e| unreadable(path, e))?;
    // An encrypted PDF opens, but every page then reads as empty, which would
    // pass for a scan.
    if doc.is_encrypted() && !doc.is_authenticated() {
        return Err(unreadable(path, "password required"));
    }
    let page_count = doc.page_count().map_err(|e| unreadable(path, e))?;
    let page_total = u32::try_from(page_count).map_err(|e| unreadable(path, e))?;

    let mut pages = Vec::new();
    let mut without_text = Vec::new();
    let mut first_error = None;
    for index in 0..page_count {
        let number = index as u32 + 1;
        match read_page(&doc, index, number) {
            Ok(page) if !page.lines.is_empty() => pages.push(page),
            Ok(_) => without_text.push(number),
            Err(error) => {
                without_text.push(number);
                first_error.get_or_insert(error);
            }
        }
    }
    if pages.is_empty() {
        return Err(match first_error {
            Some(error) => unreadable(path, error),
            None => Error::NoTextLayer {
                path: path.to_path_buf(),
            },
        });
    }

    let labels = PageLabelExtractor::extract(&doc).unwrap_or_default();
    let label_of = |page: u32| page_label(&labels, page);

    let skip = furniture(&pages);
    let laid = paragraphs(&pages, &skip);
    let entries = outline_entries(&doc, page_count);
    let (mut blocks, outline) = place_headings(&laid, &entries, &label_of);
    assign_sections(&mut blocks);

    let mut skipped: Vec<_> = skip.into_iter().collect();
    skipped.sort_unstable();
    let furniture_blocks = skipped
        .into_iter()
        .filter_map(|(page, index)| {
            let at = pages.binary_search_by_key(&page, |p| p.page).ok()?;
            let line = pages[at].lines.get(index)?;
            let text = normalize(&line.text);
            let bbox = [line.left, line.top, line.right, line.bottom];
            (!text.is_empty()).then_some((page, bbox, text))
        })
        .enumerate()
        .map(|(id, (page, bbox, text))| Block {
            id: id as u32,
            kind: BlockKind::Paragraph,
            text,
            orig: None,
            spans: Vec::new(),
            section: Vec::new(),
            locator: pdf_locator(page, None, label_of(page), Some(bbox)),
        })
        .collect();

    let warnings = if without_text.is_empty() {
        Vec::new()
    } else {
        vec![Warning::PagesWithoutText {
            pages: without_text,
        }]
    };
    Ok(Document {
        schema_version: SCHEMA_VERSION,
        source,
        metadata: Metadata {
            title: info_text(&doc, "Title"),
            authors: info_text(&doc, "Author").into_iter().collect(),
            language: None,
            page_count: Some(page_total),
            spine_count: None,
        },
        outline,
        blocks,
        furniture: furniture_blocks,
        warnings,
    })
}

fn unreadable(path: &Path, reason: impl Display) -> Error {
    Error::Unreadable {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

/// The lines of one page, in top-origin points. `number` is 1-based.
fn read_page(
    doc: &PdfDocument,
    index: usize,
    number: u32,
) -> std::result::Result<PageLines, String> {
    let (x0, y0, x1, y1) = doc.get_page_media_box(index).map_err(|e| e.to_string())?;
    let text_lines = doc.extract_text_lines(index).map_err(|e| e.to_string())?;
    let lines = text_lines
        .iter()
        .flat_map(|line| split_line(line, x0, y1))
        .collect();
    Ok(PageLines {
        page: number,
        width: x1 - x0,
        height: y1 - y0,
        lines,
    })
}

/// One run of words that sit together on a line.
struct Run {
    text: String,
    /// `[left, bottom, right, top]` in PDF space, y up.
    bounds: [f32; 4],
    sizes: Vec<f32>,
}

/// The font size of a word, or 0 when the file gives no usable one.
fn word_size(size: f32, height: f32) -> f32 {
    [size, height]
        .into_iter()
        .find(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(0.0)
}

/// Rebuilds a pdf_oxide line from its words and cuts it at wide gaps.
///
/// The line's own text is not used: it puts a space between all words, which
/// breaks numbers apart and detaches a line-end hyphen. `x0` is the left edge
/// and `y1` the top edge of the page's media box.
fn split_line(line: &TextLine, x0: f32, y1: f32) -> Vec<Line> {
    let mut runs: Vec<Run> = Vec::new();
    let mut previous_end = 0.0;
    let mut previous_size = 0.0;
    for word in &line.words {
        if word.text.trim().is_empty() {
            continue;
        }
        let b = &word.bbox;
        let size = word_size(word.avg_font_size, b.height);
        let gap = b.x - previous_end;
        if runs.is_empty() || (previous_size > 0.0 && gap > SPLIT_GAP * previous_size) {
            runs.push(Run {
                text: String::new(),
                bounds: [b.x, b.y, b.x + b.width, b.y + b.height],
                sizes: Vec::new(),
            });
        }
        let Some(run) = runs.last_mut() else { continue };
        if !run.text.is_empty() && gap > SPACE_GAP * previous_size {
            run.text.push(' ');
        }
        run.text.push_str(&word.text);
        run.bounds = [
            run.bounds[0].min(b.x),
            run.bounds[1].min(b.y),
            run.bounds[2].max(b.x + b.width),
            run.bounds[3].max(b.y + b.height),
        ];
        run.sizes.push(size);
        previous_end = b.x + b.width;
        previous_size = size;
    }
    runs.into_iter()
        .map(|run| Line {
            font_size: run.sizes.iter().sum::<f32>() / run.sizes.len() as f32,
            left: run.bounds[0] - x0,
            right: run.bounds[2] - x0,
            top: y1 - run.bounds[3],
            bottom: y1 - run.bounds[1],
            text: run.text,
        })
        .collect()
}

/// The printed label of a 1-based page, or `None` when the PDF has no
/// page-label ranges (the extractor would answer with the physical number).
fn page_label(ranges: &[PageLabelRange], page: u32) -> Option<String> {
    if ranges.is_empty() {
        return None;
    }
    Some(PageLabelExtractor::get_label(ranges, page as usize - 1))
}

fn pdf_locator(
    page: u32,
    page_end: Option<u32>,
    page_label: Option<String>,
    bbox: Option<[f32; 4]>,
) -> Locator {
    Locator::Pdf {
        page,
        page_end,
        page_label,
        bbox,
    }
}

/// A bookmark that leads to a page of the book.
struct Entry {
    title: String,
    level: u8,
    /// 1-based.
    page: u32,
}

/// The bookmarks that lead to a page, depth first. The level is the depth in
/// the bookmark tree, the top level being 1.
fn outline_entries(doc: &PdfDocument, page_count: usize) -> Vec<Entry> {
    let Ok(Some(items)) = doc.get_outline() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut stack: Vec<(&OutlineItem, usize)> = items.iter().rev().map(|i| (i, 1)).collect();
    while let Some((item, depth)) = stack.pop() {
        stack.extend(item.children.iter().rev().map(|child| (child, depth + 1)));
        let Some(Destination::PageIndex(index)) = item.dest else {
            continue;
        };
        let title = normalize(&item.title);
        if title.is_empty() || index >= page_count {
            continue;
        }
        entries.push(Entry {
            title,
            level: u8::try_from(depth).unwrap_or(u8::MAX),
            page: index as u32 + 1,
        });
    }
    entries
}

/// How a bookmark found its heading among the paragraphs.
enum Claim {
    /// The paragraph is the heading.
    Whole,
    /// The paragraph starts with the heading; the heading is its first
    /// `usize` bytes.
    Prefix(usize),
}

/// The paragraph on the bookmark's page that holds its heading: the first one
/// that reads as the title, otherwise the first one that starts with it.
fn find_heading(
    laid: &[LaidParagraph],
    claims: &HashMap<usize, (usize, Claim)>,
    entry: &Entry,
) -> Option<(usize, Claim)> {
    let title = entry.title.to_lowercase();
    let free = || {
        laid.iter()
            .enumerate()
            .filter(|(index, p)| p.page == entry.page && !p.note && !claims.contains_key(index))
    };
    let whole = free().find(|(_, p)| p.text.to_lowercase() == title);
    if let Some((index, _)) = whole {
        return Some((index, Claim::Whole));
    }
    let chars = entry.title.chars().count();
    free().find_map(|(index, p)| {
        let end = p.text.char_indices().nth(chars)?.0;
        let (head, rest) = p.text.split_at(end);
        (head.to_lowercase() == title && rest.starts_with(char::is_whitespace))
            .then_some((index, Claim::Prefix(end)))
    })
}

/// Turns the paragraphs into blocks and marks the bookmarks' headings.
///
/// A bookmark whose heading is not among its page's paragraphs gets a new
/// `Heading` block before the first paragraph that starts on or after its
/// page. The returned outline has the block ids of the final numbering.
fn place_headings(
    laid: &[LaidParagraph],
    entries: &[Entry],
    label_of: &dyn Fn(u32) -> Option<String>,
) -> (Vec<Block>, Vec<OutlineEntry>) {
    let mut claims: HashMap<usize, (usize, Claim)> = HashMap::new();
    let mut inserts: HashMap<usize, Vec<usize>> = HashMap::new();
    for (e, entry) in entries.iter().enumerate() {
        match find_heading(laid, &claims, entry) {
            Some((index, claim)) => {
                claims.insert(index, (e, claim));
            }
            None => {
                let before = laid
                    .iter()
                    .position(|p| p.page >= entry.page)
                    .unwrap_or(laid.len());
                inserts.entry(before).or_default().push(e);
            }
        }
    }

    let mut outline: Vec<OutlineEntry> = entries
        .iter()
        .enumerate()
        .map(|(id, entry)| OutlineEntry {
            id: id as u32,
            title: entry.title.clone(),
            level: entry.level,
            block_id: None,
        })
        .collect();
    let mut blocks: Vec<Block> = Vec::with_capacity(laid.len() + entries.len());
    fn push(blocks: &mut Vec<Block>, kind: BlockKind, text: &str, locator: Locator) {
        blocks.push(Block {
            id: blocks.len() as u32,
            kind,
            text: text.to_string(),
            orig: None,
            spans: Vec::new(),
            section: Vec::new(),
            locator,
        });
    }
    for index in 0..=laid.len() {
        for &e in inserts.get(&index).into_iter().flatten() {
            let entry = &entries[e];
            outline[e].block_id = Some(blocks.len() as u32);
            let locator = pdf_locator(entry.page, None, label_of(entry.page), None);
            push(
                &mut blocks,
                BlockKind::Heading { level: entry.level },
                &entry.title,
                locator,
            );
        }
        let Some(p) = laid.get(index) else { break };
        let locator = pdf_locator(
            p.page,
            (p.page_end != p.page).then_some(p.page_end),
            label_of(p.page),
            Some(p.bbox),
        );
        let kind = if p.note {
            BlockKind::Note { label: None }
        } else {
            BlockKind::Paragraph
        };
        match claims.get(&index) {
            Some(&(e, Claim::Whole)) => {
                outline[e].block_id = Some(blocks.len() as u32);
                let kind = BlockKind::Heading {
                    level: entries[e].level,
                };
                push(&mut blocks, kind, &p.text, locator);
            }
            Some(&(e, Claim::Prefix(end))) => {
                outline[e].block_id = Some(blocks.len() as u32);
                let kind = BlockKind::Heading {
                    level: entries[e].level,
                };
                push(&mut blocks, kind, &p.text[..end], locator.clone());
                push(
                    &mut blocks,
                    BlockKind::Paragraph,
                    p.text[end..].trim_start(),
                    locator,
                );
            }
            None => push(&mut blocks, kind, &p.text, locator),
        }
    }
    (blocks, outline)
}

/// A text entry of the Info dictionary, such as `Title` or `Author`.
fn info_text(doc: &PdfDocument, key: &str) -> Option<String> {
    let resolve = |object: &Object| match object {
        Object::Reference(reference) => doc.load_object(*reference).ok(),
        other => Some(other.clone()),
    };
    let info = resolve(doc.trailer().as_dict()?.get("Info")?)?;
    let value = resolve(info.as_dict()?.get(key)?)?;
    let text = normalize(&decode_pdf_text_string(value.as_string()?));
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use pdf_oxide::geometry::Rect;
    use pdf_oxide::layout::TextBlock;

    use super::*;

    fn word(text: &str, x: f32, y: f32, width: f32, size: f32) -> TextBlock {
        TextBlock {
            chars: Vec::new(),
            bbox: Rect {
                x,
                y,
                width,
                height: size,
            },
            text: text.to_string(),
            avg_font_size: size,
            dominant_font: String::new(),
            is_bold: false,
            is_italic: false,
            mcid: None,
            sequence: 0,
            rotation_degrees: 0.0,
        }
    }

    fn line_of(words: Vec<TextBlock>) -> TextLine {
        TextLine::new(words)
    }

    fn texts(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn words_join_unless_a_gap_is_wider_than_a_space() {
        let line = line_of(vec![
            word("in", 100.0, 700.0, 10.0, 10.0),
            word("199", 113.0, 700.0, 15.0, 10.0),
            word("6", 128.2, 700.0, 5.0, 10.0),
            word("infor", 140.0, 700.0, 25.0, 10.0),
            word("-", 165.0, 700.0, 3.0, 10.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["in 1996 infor-"]);
    }

    #[test]
    fn a_gap_over_two_font_sizes_cuts_the_line() {
        let line = line_of(vec![
            word("left", 100.0, 700.0, 20.0, 10.0),
            word("column", 125.0, 700.0, 30.0, 10.0),
            word("right", 190.0, 700.0, 25.0, 10.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["left column", "right"]);
        assert_eq!((lines[0].left, lines[0].right), (100.0, 155.0));
        assert_eq!((lines[1].left, lines[1].right), (190.0, 215.0));
    }

    #[test]
    fn lines_are_converted_to_a_top_origin_from_the_media_box() {
        let mut raised = word("2", 130.0, 703.0, 5.0, 7.0);
        raised.avg_font_size = 7.0;
        let line = line_of(vec![word("x", 120.0, 700.0, 10.0, 10.0), raised]);

        // A media box that does not start at the origin.
        let lines = split_line(&line, 20.0, 800.0);

        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!((line.left, line.right), (100.0, 115.0));
        // The top is the highest point, the bottom the lowest baseline.
        assert_eq!((line.top, line.bottom), (800.0 - 710.0, 800.0 - 700.0));
        assert_eq!(line.font_size, 8.5);
    }

    #[test]
    fn blank_words_and_missing_font_sizes_do_not_break_a_line() {
        let mut flat = word("flat", 100.0, 700.0, 20.0, 0.0);
        flat.avg_font_size = 0.0;
        let line = line_of(vec![
            word(" ", 90.0, 700.0, 3.0, 10.0),
            flat,
            word("text", 130.0, 700.0, 20.0, 0.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["flat text"]);
        assert!(lines[0].font_size.is_finite());
    }

    fn paragraph(text: &str, page: u32) -> LaidParagraph {
        LaidParagraph {
            text: text.to_string(),
            page,
            page_end: page,
            bbox: [0.0, 0.0, 1.0, 1.0],
            note: false,
        }
    }

    fn entry(title: &str, level: u8, page: u32) -> Entry {
        Entry {
            title: title.to_string(),
            level,
            page,
        }
    }

    fn place(laid: &[LaidParagraph], entries: &[Entry]) -> (Vec<Block>, Vec<OutlineEntry>) {
        place_headings(laid, entries, &|page| Some(format!("p{page}")))
    }

    fn summary(blocks: &[Block]) -> Vec<(String, Option<u8>)> {
        blocks
            .iter()
            .map(|b| {
                let level = match b.kind {
                    BlockKind::Heading { level } => Some(level),
                    _ => None,
                };
                (b.text.clone(), level)
            })
            .collect()
    }

    fn heading(text: &str, level: u8) -> (String, Option<u8>) {
        (text.to_string(), Some(level))
    }

    fn body(text: &str) -> (String, Option<u8>) {
        (text.to_string(), None)
    }

    #[test]
    fn a_paragraph_that_reads_as_the_title_becomes_the_heading() {
        let laid = [
            paragraph("Before.", 1),
            paragraph("Text.", 2),
            paragraph("ENTROPY", 2),
            paragraph("After.", 2),
        ];

        let (blocks, outline) = place(&laid, &[entry("Entropy", 2, 2)]);

        assert_eq!(
            summary(&blocks),
            [
                body("Before."),
                body("Text."),
                heading("ENTROPY", 2),
                body("After.")
            ]
        );
        assert_eq!(outline[0].block_id, Some(2));
        assert_eq!(outline[0].title, "Entropy");
        assert_eq!(outline[0].level, 2);
    }

    #[test]
    fn a_paragraph_that_starts_with_the_title_is_split() {
        let laid = [paragraph("Entropy Entropy measures surprise.", 1)];

        let (blocks, outline) = place(&laid, &[entry("entropy", 1, 1)]);

        assert_eq!(
            summary(&blocks),
            [heading("Entropy", 1), body("Entropy measures surprise.")]
        );
        assert_eq!(outline[0].block_id, Some(0));
        assert_eq!(blocks[0].locator, blocks[1].locator);
        assert_eq!(blocks[1].id, 1);
    }

    #[test]
    fn a_whole_match_wins_over_a_prefix_match_earlier_on_the_page() {
        let laid = [
            paragraph("Entropy is a word. It starts a paragraph.", 1),
            paragraph("Entropy", 1),
        ];

        let (blocks, outline) = place(&laid, &[entry("Entropy", 1, 1)]);

        assert_eq!(
            summary(&blocks),
            [
                body("Entropy is a word. It starts a paragraph."),
                heading("Entropy", 1)
            ]
        );
        assert_eq!(outline[0].block_id, Some(1));
    }

    #[test]
    fn a_title_that_is_only_the_start_of_a_word_is_not_a_prefix_match() {
        let laid = [paragraph("Entropyless text.", 1)];

        let (blocks, _) = place(&laid, &[entry("Entropy", 1, 1)]);

        assert_eq!(
            summary(&blocks),
            [heading("Entropy", 1), body("Entropyless text.")]
        );
    }

    #[test]
    fn a_missing_heading_is_inserted_before_the_first_paragraph_of_its_page() {
        let laid = [
            paragraph("One.", 1),
            paragraph("Two.", 2),
            paragraph("Three.", 2),
        ];

        let (blocks, outline) = place(&laid, &[entry("Chapter", 1, 2)]);

        assert_eq!(
            summary(&blocks),
            [
                body("One."),
                heading("Chapter", 1),
                body("Two."),
                body("Three.")
            ]
        );
        assert_eq!(outline[0].block_id, Some(1));
        assert_eq!(
            blocks[1].locator,
            Locator::Pdf {
                page: 2,
                page_end: None,
                page_label: Some("p2".to_string()),
                bbox: None
            }
        );
        let ids: Vec<_> = blocks.iter().map(|b| b.id).collect();
        assert_eq!(ids, [0, 1, 2, 3]);
    }

    #[test]
    fn a_heading_for_a_page_without_text_goes_before_the_next_text() {
        let laid = [paragraph("One.", 1), paragraph("Four.", 4)];

        let (blocks, outline) = place(&laid, &[entry("Plate", 1, 2), entry("Epilogue", 1, 6)]);

        assert_eq!(
            summary(&blocks),
            [
                body("One."),
                heading("Plate", 1),
                body("Four."),
                heading("Epilogue", 1)
            ]
        );
        assert_eq!(outline[0].block_id, Some(1));
        assert_eq!(outline[1].block_id, Some(3));
    }

    #[test]
    fn notes_are_never_headings_and_a_paragraph_serves_one_bookmark() {
        let mut note = paragraph("Entropy", 1);
        note.note = true;
        let laid = [note, paragraph("Entropy", 1), paragraph("Text.", 1)];

        let (blocks, outline) = place(&laid, &[entry("Entropy", 1, 1), entry("Entropy", 2, 1)]);

        // The first bookmark takes the paragraph; the second finds none left
        // and gets a new heading before the page's first block.
        assert_eq!(
            summary(&blocks),
            [
                heading("Entropy", 2),
                body("Entropy"),
                heading("Entropy", 1),
                body("Text."),
            ]
        );
        assert_eq!(blocks[1].kind, BlockKind::Note { label: None });
        assert_eq!(outline[0].block_id, Some(2));
        assert_eq!(outline[1].block_id, Some(0));
    }
}
