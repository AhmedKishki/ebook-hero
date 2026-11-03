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
/// Two words whose gap is within this many font sizes, either way, touch.
/// pdf_oxide's word breaks hold otherwise: some PDFs give word boxes that
/// overlap their neighbours, so a gap below zero is no sign of a join.
const TOUCH_GAP: f32 = 0.025;
/// Characters that close what comes before them; a word made only of these
/// belongs to the word it touches on its left.
const CLOSING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '’', '”', '"', '\'', '…', '»',
];
/// Characters that open what comes after them; a word made only of these
/// belongs to the word it touches on its right.
const OPENING: &[char] = &['(', '[', '{', '‘', '“', '"', '\'', '«', '¿', '¡'];
/// The hyphens that end a line inside a word, the soft one included.
const HYPHENS: &[char] = &['-', '\u{2010}', '\u{00AD}'];
/// A page whose rebuilt words of letters average more characters than this
/// has lost its word spaces...
const GLUED_WORD_LENGTH: f32 = 9.0;
/// ... judged only on a page with at least this many such words.
const GLUED_MIN_WORDS: usize = 50;

/// What the file gave, before the book is assembled. An `Err` is the text of
/// the pdf_oxide error that stopped that part.
struct Read {
    /// One result per page, in page order. An `Ok` page without lines has no
    /// text.
    pages: Vec<std::result::Result<PageLines, String>>,
    /// How many pages fell back to pdf_oxide's own line text.
    respaced_pages: usize,
    labels: std::result::Result<Vec<PageLabelRange>, String>,
    outline: std::result::Result<Option<Vec<OutlineItem>>, String>,
}

/// Reads a PDF. Fails with `Unreadable` when the file cannot be parsed or
/// needs a password, and with `NoTextLayer` when no page has any text. A part
/// that cannot be read, a page, the page labels or the outline, is left out
/// and named in an `UnreadablePart` warning.
pub fn read_pdf(path: &Path, source: Source) -> Result<Document> {
    let doc = PdfDocument::open(path).map_err(|e| unreadable(path, e))?;
    // An encrypted PDF opens, but every page then reads as empty, which would
    // pass for a scan.
    if doc.is_encrypted() && !doc.is_authenticated() {
        return Err(unreadable(path, "password required"));
    }
    let page_count = doc.page_count().map_err(|e| unreadable(path, e))?;
    let page_total = u32::try_from(page_count).map_err(|e| unreadable(path, e))?;

    let mut respaced_pages = 0;
    let pages = (0..page_count)
        .map(|index| {
            let (page, respaced) = read_page(&doc, index)?;
            respaced_pages += usize::from(respaced);
            Ok(page)
        })
        .collect();
    let read = Read {
        pages,
        respaced_pages,
        labels: PageLabelExtractor::extract(&doc).map_err(|e| e.to_string()),
        outline: doc.get_outline().map_err(|e| e.to_string()),
    };
    let metadata = Metadata {
        title: info_text(&doc, "Title"),
        authors: info_text(&doc, "Author").into_iter().collect(),
        language: None,
        page_count: Some(page_total),
        spine_count: None,
    };
    build_document(path, source, metadata, read)
}

/// Turns what was read into a [`Document`].
///
/// When no page has text the book is an error: `Unreadable` with the first
/// page error if any page failed, otherwise `NoTextLayer`. Otherwise a page
/// that failed is a warning of its own and is not counted among the pages
/// without text.
fn build_document(path: &Path, source: Source, metadata: Metadata, read: Read) -> Result<Document> {
    let mut pages = Vec::new();
    let mut without_text = Vec::new();
    let mut failed = Vec::new();
    for (index, result) in read.pages.into_iter().enumerate() {
        let number = index as u32 + 1;
        match result {
            Ok(page) if page.lines.is_empty() => without_text.push(number),
            Ok(page) => pages.push(page),
            Err(reason) => failed.push((number, reason)),
        }
    }
    if pages.is_empty() {
        return Err(match failed.into_iter().next() {
            Some((_, reason)) => unreadable(path, reason),
            None => Error::NoTextLayer {
                path: path.to_path_buf(),
            },
        });
    }

    let mut warnings = Vec::new();
    if !without_text.is_empty() {
        warnings.push(Warning::PagesWithoutText {
            pages: without_text,
        });
    }
    warnings.extend(
        failed
            .into_iter()
            .map(|(number, reason)| Warning::UnreadablePart {
                part: format!("page {number}"),
                reason,
            }),
    );
    if read.respaced_pages > 0 {
        let n = read.respaced_pages;
        let pages = if n == 1 { "page" } else { "pages" };
        warnings.push(Warning::UnreadablePart {
            part: "word spacing".to_string(),
            reason: format!("geometry looked wrong on {n} {pages}; used the PDF's own word breaks"),
        });
    }
    let labels = read.labels.unwrap_or_else(|reason| {
        warnings.push(Warning::UnreadablePart {
            part: "page labels".to_string(),
            reason,
        });
        Vec::new()
    });
    let items = read.outline.unwrap_or_else(|reason| {
        warnings.push(Warning::UnreadablePart {
            part: "outline".to_string(),
            reason,
        });
        None
    });
    let label_of = |page: u32| page_label(&labels, page);

    let skip = furniture(&pages);
    let laid = paragraphs(&pages, &skip);
    let page_count = metadata.page_count.unwrap_or_default() as usize;
    let entries = outline_entries(items.as_deref().unwrap_or_default(), page_count);
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

    Ok(Document {
        schema_version: SCHEMA_VERSION,
        source,
        metadata,
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

/// The lines of the page at 0-based `index`, in top-origin points, and
/// whether they fell back to pdf_oxide's own line text.
fn read_page(doc: &PdfDocument, index: usize) -> std::result::Result<(PageLines, bool), String> {
    let (x0, y0, x1, y1) = doc.get_page_media_box(index).map_err(|e| e.to_string())?;
    let text_lines = doc.extract_text_lines(index).map_err(|e| e.to_string())?;
    let (lines, respaced) = page_lines(&text_lines, x0, y1);
    let page = PageLines {
        page: index as u32 + 1,
        width: x1 - x0,
        height: y1 - y0,
        lines,
    };
    Ok((page, respaced))
}

/// The lines of a page rebuilt from their words, and `false`. When the
/// rebuilt words of letters are too long on average, the joins misread the
/// page, so its lines are pdf_oxide's own text instead, uncut, and `true`.
fn page_lines(text_lines: &[TextLine], x0: f32, y1: f32) -> (Vec<Line>, bool) {
    let lines: Vec<Line> = text_lines
        .iter()
        .flat_map(|line| split_line(line, x0, y1))
        .collect();
    if !looks_glued(&lines) {
        return (lines, false);
    }
    let lines = text_lines
        .iter()
        .filter_map(|line| whole_line(line, x0, y1))
        .collect();
    (lines, true)
}

/// Whether the lines have at least `GLUED_MIN_WORDS` words of letters and
/// these average more than `GLUED_WORD_LENGTH` characters.
fn looks_glued(lines: &[Line]) -> bool {
    let (words, chars) = lines
        .iter()
        .flat_map(|line| line.text.split_whitespace())
        .filter_map(letters_length)
        .fold((0, 0), |(words, chars), n| (words + 1, chars + n));
    words >= GLUED_MIN_WORDS && chars as f32 > GLUED_WORD_LENGTH * words as f32
}

/// The length in characters of a word made of letters once the punctuation
/// around it is trimmed, or `None` for any other word.
fn letters_length(word: &str) -> Option<usize> {
    let core = word.trim_matches(|c: char| !c.is_alphabetic());
    (!core.is_empty() && core.chars().all(char::is_alphabetic)).then(|| core.chars().count())
}

/// A pdf_oxide line as it stands, with its text, or `None` when it is blank.
fn whole_line(line: &TextLine, x0: f32, y1: f32) -> Option<Line> {
    if line.text.trim().is_empty() {
        return None;
    }
    let sizes: Vec<f32> = line
        .words
        .iter()
        .filter(|word| !word.text.trim().is_empty())
        .map(|word| word_size(word.avg_font_size, word.bbox.height))
        .collect();
    let font_size = if sizes.is_empty() {
        word_size(0.0, line.bbox.height)
    } else {
        sizes.iter().sum::<f32>() / sizes.len() as f32
    };
    let b = &line.bbox;
    Some(Line {
        text: line.text.clone(),
        left: b.x - x0,
        right: b.x + b.width - x0,
        top: y1 - (b.y + b.height),
        bottom: y1 - b.y,
        font_size,
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

/// Whether two neighbouring words are one: they touch, within `TOUCH_GAP`
/// font sizes of the previous word, and their characters say they belong
/// together. Those are digits on both sides of the gap (pdf_oxide cuts
/// `1996` into `199 6`), a word of closing punctuation after a word or one of
/// opening punctuation before it, and a hyphen at a line end.
fn joins(previous: &str, next: &str, gap: f32, previous_size: f32) -> bool {
    if gap.abs() > TOUCH_GAP * previous_size {
        return false;
    }
    let digits = match (previous.chars().next_back(), next.chars().next()) {
        (Some(last), Some(first)) => last.is_numeric() && first.is_numeric(),
        _ => false,
    };
    let only = |word: &str, set: &[char]| word.chars().all(|c| set.contains(&c));
    digits
        || only(next, CLOSING)
        || only(previous, OPENING)
        || (next.chars().count() == 1 && only(next, HYPHENS))
        || previous.ends_with(HYPHENS)
}

/// Rebuilds a pdf_oxide line from its words and cuts it at wide gaps.
///
/// The words are pdf_oxide's, kept apart by a space unless [`joins`] says
/// they are one. The line's own text would put a space between all words,
/// which breaks numbers apart and detaches a line-end hyphen. `x0` is the
/// left edge and `y1` the top edge of the page's media box.
fn split_line(line: &TextLine, x0: f32, y1: f32) -> Vec<Line> {
    let mut runs: Vec<Run> = Vec::new();
    let mut previous_end = 0.0;
    let mut previous_size = 0.0;
    let mut previous_word = "";
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
        if !run.text.is_empty() && !joins(previous_word, &word.text, gap, previous_size) {
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
        previous_word = &word.text;
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
fn outline_entries(items: &[OutlineItem], page_count: usize) -> Vec<Entry> {
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
    use crate::document::Format;

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
    fn digits_and_a_line_end_hyphen_join_when_they_touch() {
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
    fn punctuation_and_a_hyphenated_word_join_when_they_touch() {
        let line = line_of(vec![
            word("see", 100.0, 700.0, 15.0, 10.0),
            word("(", 117.0, 700.0, 3.0, 10.0),
            word("Stones", 120.1, 700.0, 30.0, 10.0),
            word("),", 150.0, 700.0, 5.0, 10.0),
            word("self-", 160.0, 700.0, 20.0, 10.0),
            word("evident", 179.9, 700.0, 30.0, 10.0),
            word("”.", 210.1, 700.0, 5.0, 10.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["see (Stones), self-evident”."]);
    }

    #[test]
    fn overlapping_word_boxes_keep_the_words_apart() {
        // Some PDFs give word boxes that overlap their neighbours by up to a
        // few font sizes; the word breaks still hold.
        let line = line_of(vec![
            word("The", 100.0, 700.0, 18.0, 10.0),
            word("social", 115.0, 700.0, 30.0, 10.0),
            word("world", 120.0, 700.0, 25.0, 10.0),
            word("of", 144.7, 700.0, 10.0, 10.0),
            word("2012", 150.0, 700.0, 20.0, 10.0),
            word("13", 169.0, 700.0, 10.0, 10.0),
            word(",", 178.0, 700.0, 2.0, 10.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["The social world of 2012 13 ,"]);
    }

    #[test]
    fn letters_stay_apart_however_close_they_are() {
        let line = line_of(vec![
            word("one", 100.0, 700.0, 15.0, 10.0),
            word("word", 115.1, 700.0, 20.0, 10.0),
            word("two", 135.0, 700.0, 15.0, 10.0),
        ]);

        let lines = split_line(&line, 0.0, 792.0);

        assert_eq!(texts(&lines), ["one word two"]);
    }

    /// A line whose words carry no spaces between them while its own text,
    /// as pdf_oxide gives it, has them.
    fn glued_line(y: f32) -> TextLine {
        let words: Vec<TextBlock> = (0..10)
            .map(|i| word("joinedwordz", 100.0 + 50.0 * i as f32, y, 50.0, 10.0))
            .collect();
        TextLine {
            bbox: Rect {
                x: 100.0,
                y,
                width: 500.0,
                height: 10.0,
            },
            text: "joined wordz ".repeat(10).trim_end().to_string(),
            words,
        }
    }

    #[test]
    fn a_page_of_overlong_words_falls_back_to_the_pdf_word_breaks() {
        let text_lines: Vec<TextLine> = (0..5)
            .map(|i| glued_line(700.0 - 12.0 * i as f32))
            .collect();

        let (lines, fell_back) = page_lines(&text_lines, 0.0, 792.0);

        assert!(fell_back);
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l.text == text_lines[0].text));
        assert_eq!((lines[0].left, lines[0].right), (100.0, 600.0));
        assert_eq!((lines[0].top, lines[0].bottom), (82.0, 92.0));
        assert_eq!(lines[0].font_size, 10.0);
    }

    #[test]
    fn long_words_on_a_page_with_few_words_are_kept() {
        // 40 words of 11 letters: too few words to judge the page.
        let text_lines: Vec<TextLine> = (0..4)
            .map(|i| glued_line(700.0 - 12.0 * i as f32))
            .collect();

        let (lines, fell_back) = page_lines(&text_lines, 0.0, 792.0);

        assert!(!fell_back);
        assert!(
            lines
                .iter()
                .all(|l| l.text.starts_with("joinedwordz joinedwordz"))
        );
    }

    #[test]
    fn a_page_of_ordinary_words_keeps_the_rebuilt_lines() {
        let words = |y: f32| {
            (0..20)
                .map(|i| word("plain", 100.0 + 30.0 * i as f32, y, 25.0, 10.0))
                .collect::<Vec<_>>()
        };
        let text_lines: Vec<TextLine> = (0..5)
            .map(|i| {
                let mut line = line_of(words(700.0 - 12.0 * i as f32));
                line.text = "something else".to_string();
                line
            })
            .collect();

        let (lines, fell_back) = page_lines(&text_lines, 0.0, 792.0);

        assert!(!fell_back);
        assert!(lines.iter().all(|l| l.text.starts_with("plain plain")));
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
    fn text_page(page: u32) -> PageLines {
        let line = |n: f32, text: &str| Line {
            text: text.to_string(),
            left: 100.0,
            top: 300.0 + 12.0 * n,
            right: 500.0,
            bottom: 310.0 + 12.0 * n,
            font_size: 10.0,
        };
        PageLines {
            page,
            width: 600.0,
            height: 800.0,
            lines: vec![
                line(0.0, "Some text on this page"),
                line(1.0, "runs over two lines."),
            ],
        }
    }

    fn empty_page(page: u32) -> PageLines {
        PageLines {
            lines: Vec::new(),
            ..text_page(page)
        }
    }

    fn read_of(pages: Vec<std::result::Result<PageLines, String>>) -> Read {
        Read {
            pages,
            respaced_pages: 0,
            labels: Ok(Vec::new()),
            outline: Ok(None),
        }
    }

    fn build(read: Read) -> Result<Document> {
        let source = Source {
            sha256: "abc".to_string(),
            file_name: "book.pdf".to_string(),
            format: Format::Pdf,
        };
        let metadata = Metadata {
            title: None,
            authors: Vec::new(),
            language: None,
            page_count: Some(read.pages.len() as u32),
            spine_count: None,
        };
        build_document(Path::new("book.pdf"), source, metadata, read)
    }

    fn unreadable_part(part: &str, reason: &str) -> Warning {
        Warning::UnreadablePart {
            part: part.to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn a_failed_page_is_an_unreadable_part_not_a_page_without_text() {
        let read = read_of(vec![
            Ok(text_page(1)),
            Err("bad xref".to_string()),
            Ok(empty_page(3)),
        ]);

        let doc = build(read).unwrap();

        assert_eq!(
            doc.warnings,
            [
                Warning::PagesWithoutText { pages: vec![3] },
                unreadable_part("page 2", "bad xref"),
            ]
        );
        assert_eq!(doc.blocks.len(), 1);
    }

    #[test]
    fn an_outline_error_is_an_unreadable_part_and_leaves_no_outline() {
        let mut read = read_of(vec![Ok(text_page(1))]);
        read.outline = Err("broken tree".to_string());

        let doc = build(read).unwrap();

        assert_eq!(doc.warnings, [unreadable_part("outline", "broken tree")]);
        assert!(doc.outline.is_empty());
        assert_eq!(doc.blocks.len(), 1);
    }

    #[test]
    fn a_pdf_without_an_outline_has_no_warning() {
        let mut read = read_of(vec![Ok(text_page(1))]);
        assert!(build(read).unwrap().warnings.is_empty());

        read = read_of(vec![Ok(text_page(1))]);
        read.outline = Ok(Some(Vec::new()));
        assert!(build(read).unwrap().warnings.is_empty());
    }

    #[test]
    fn a_page_label_error_is_an_unreadable_part_and_leaves_no_labels() {
        let mut read = read_of(vec![Ok(text_page(1))]);
        read.labels = Err("bad number tree".to_string());

        let doc = build(read).unwrap();

        assert_eq!(
            doc.warnings,
            [unreadable_part("page labels", "bad number tree")]
        );
        assert!(matches!(
            doc.blocks[0].locator,
            Locator::Pdf {
                page_label: None,
                ..
            }
        ));
    }

    #[test]
    fn page_label_ranges_label_the_blocks() {
        let mut read = read_of(vec![Ok(text_page(1))]);
        read.labels = Ok(vec![PageLabelRange::new(0)]);

        let doc = build(read).unwrap();

        assert!(doc.warnings.is_empty());
        assert!(matches!(
            &doc.blocks[0].locator,
            Locator::Pdf { page_label: Some(label), .. } if label == "1"
        ));
    }

    #[test]
    fn warnings_come_in_a_fixed_order() {
        let mut read = read_of(vec![
            Err("first".to_string()),
            Ok(empty_page(2)),
            Ok(text_page(3)),
            Err("second".to_string()),
        ]);
        read.respaced_pages = 1;
        read.labels = Err("labels".to_string());
        read.outline = Err("outline".to_string());

        let doc = build(read).unwrap();

        assert_eq!(
            doc.warnings,
            [
                Warning::PagesWithoutText { pages: vec![2] },
                unreadable_part("page 1", "first"),
                unreadable_part("page 4", "second"),
                unreadable_part(
                    "word spacing",
                    "geometry looked wrong on 1 page; used the PDF's own word breaks"
                ),
                unreadable_part("page labels", "labels"),
                unreadable_part("outline", "outline"),
            ]
        );
    }

    #[test]
    fn pages_that_fell_back_to_the_pdf_word_breaks_are_one_warning() {
        let mut read = read_of(vec![Ok(text_page(1)), Ok(text_page(2))]);
        read.respaced_pages = 2;

        let doc = build(read).unwrap();

        assert_eq!(
            doc.warnings,
            [unreadable_part(
                "word spacing",
                "geometry looked wrong on 2 pages; used the PDF's own word breaks"
            )]
        );
    }

    #[test]
    fn a_book_without_text_is_an_error_named_for_its_cause() {
        let reason = |read| match build(read).unwrap_err() {
            Error::Unreadable { path, reason } => {
                assert_eq!(path, Path::new("book.pdf"));
                Some(reason)
            }
            Error::NoTextLayer { path } => {
                assert_eq!(path, Path::new("book.pdf"));
                None
            }
            other => panic!("unexpected error {other:?}"),
        };

        let all_failed = read_of(vec![Err("one".to_string()), Err("two".to_string())]);
        assert_eq!(reason(all_failed).as_deref(), Some("one"));
        let failed_and_empty = read_of(vec![Ok(empty_page(1)), Err("two".to_string())]);
        assert_eq!(reason(failed_and_empty).as_deref(), Some("two"));
        let all_empty = read_of(vec![Ok(empty_page(1)), Ok(empty_page(2))]);
        assert_eq!(reason(all_empty), None);
        assert_eq!(reason(read_of(Vec::new())), None);
    }

    fn item(title: &str, dest: Option<Destination>, children: Vec<OutlineItem>) -> OutlineItem {
        OutlineItem {
            title: title.to_string(),
            dest,
            children,
        }
    }

    #[test]
    fn bookmarks_flatten_depth_first_with_their_depth_as_level() {
        let page = |index| Some(Destination::PageIndex(index));
        let items = vec![
            item(
                " Part  One ",
                page(0),
                vec![
                    item(
                        "Chapter",
                        page(1),
                        vec![item("Section", page(1), Vec::new())],
                    ),
                    item(
                        "Named",
                        Some(Destination::Named("x".to_string())),
                        vec![item("Under named", page(2), Vec::new())],
                    ),
                    item("Past the end", page(9), Vec::new()),
                ],
            ),
            item("   ", page(0), Vec::new()),
            item("Two", page(2), Vec::new()),
        ];

        let entries = outline_entries(&items, 3);

        let flat: Vec<_> = entries
            .iter()
            .map(|e| (e.title.as_str(), e.level, e.page))
            .collect();
        assert_eq!(
            flat,
            [
                ("Part One", 1, 1),
                ("Chapter", 2, 2),
                ("Section", 3, 2),
                ("Under named", 3, 3),
                ("Two", 1, 3),
            ]
        );
    }
}
