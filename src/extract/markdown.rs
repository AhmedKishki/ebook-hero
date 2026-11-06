//! Reads a Markdown file into the [`Document`] model.
//!
//! pulldown-cmark parses the file as CommonMark with tables, strikethrough and
//! footnotes, after it has found whether the file starts with front matter,
//! which the reader leaves out. One pass over its events writes the blocks in
//! reading order, and a stack of the containers open at each event gives a
//! block its `within`.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::assign_sections;
use crate::document::{
    Block, BlockKind, Container, Document, Locator, Metadata, OutlineEntry, SCHEMA_VERSION, Source,
    Warning,
};
use crate::error::{Error, Result};
use crate::normalize::normalize;

const MIB: u64 = 1024 * 1024;

/// The extensions the file is parsed with. Every other one stays off, and its
/// syntax reads as CommonMark has it, so a task list's `[x]` stays text.
const EXTENSIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_FOOTNOTES);

/// The most a Markdown file may hold, in bytes. A larger file is refused, so
/// that one file cannot fill memory. 64 MiB of text is about ten million
/// words, more than any book.
pub(crate) const MAX_MARKDOWN_BYTES: u64 = 64 * MIB;

/// Reads the Markdown file at `path`, UTF-8 with or without a byte-order mark.
///
/// A file over [`MAX_MARKDOWN_BYTES`] and a file with no text in any block are
/// [`Error::Unreadable`]. An HTML block and front matter are left out, each
/// with a [`Warning::UnreadablePart`] that names its line.
pub fn read_markdown(path: &Path, source: Source) -> Result<Document> {
    let unreadable = |reason: String| Error::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    let file = File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let size = file
        .metadata()
        .map_err(|e| unreadable(e.to_string()))?
        .len();
    check_size(path, size)?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_MARKDOWN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| unreadable(e.to_string()))?;
    // The file may have grown since its size was taken.
    check_size(path, bytes.len() as u64)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| unreadable("the file is not UTF-8 text; save it as UTF-8".to_string()))?;
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    markdown_document(text, source).map_err(|error| match error {
        Error::Unreadable { reason, .. } => unreadable(reason),
        error => error,
    })
}

/// Refuses a file of `size` bytes when that is more than [`MAX_MARKDOWN_BYTES`].
fn check_size(path: &Path, size: u64) -> Result<()> {
    if size <= MAX_MARKDOWN_BYTES {
        return Ok(());
    }
    Err(Error::Unreadable {
        path: path.to_path_buf(),
        reason: format!(
            "the file holds {size} bytes, more than the {} MiB a Markdown file may hold; \
             split it into smaller files",
            MAX_MARKDOWN_BYTES / MIB
        ),
    })
}

/// Reads the Markdown `text`, which holds no byte-order mark, into a
/// [`Document`]. Text that leaves no block with text is an
/// [`Error::Unreadable`] that names the file of `source`.
pub(crate) fn markdown_document(text: &str, source: Source) -> Result<Document> {
    let text = with_line_feeds(text);
    let mut walk = Walk::new(Lines::new(&text));
    let body = match front_matter_end(&text) {
        Some(end) => {
            walk.warnings.push(Warning::UnreadablePart {
                part: "front matter at line 1".to_string(),
                reason: "the reader does not read front matter, so its text is left out"
                    .to_string(),
            });
            end
        }
        None => 0,
    };
    for (event, range) in Parser::new_ext(&text[body..], EXTENSIONS).into_offset_iter() {
        walk.event(event, range.start + body..range.end + body);
    }
    let Walk {
        mut blocks,
        containers,
        warnings,
        ..
    } = walk;
    if blocks.iter().all(|block| block.text.is_empty()) {
        return Err(Error::Unreadable {
            path: PathBuf::from(&source.file_name),
            reason: "no readable text".to_string(),
        });
    }
    let sections = assign_sections(&mut blocks);
    Ok(Document {
        schema_version: SCHEMA_VERSION,
        source,
        // CommonMark defines no title, authors or language, and a Markdown
        // file has neither pages nor a spine.
        metadata: Metadata {
            title: None,
            authors: Vec::new(),
            language: None,
            page_count: None,
            spine_count: None,
        },
        outline: outline(&blocks),
        sections,
        files: Vec::new(),
        page_labels: Vec::new(),
        containers,
        blocks,
        furniture: Vec::new(),
        warnings,
    })
}

/// Where the front matter that starts `text` ends, if it starts with one.
///
/// pulldown-cmark takes every `---` line with text on the next line, up to a
/// later `---` or `...` line, for a YAML metadata block, wherever it stands.
/// Between two thematic breaks that would leave the document's own text out,
/// so pulldown-cmark is asked only about the start of the text, and the rest
/// is parsed without the option.
fn front_matter_end(text: &str) -> Option<usize> {
    if !text.starts_with("---") {
        return None;
    }
    let options = Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    match Parser::new_ext(text, options).into_offset_iter().next()? {
        (Event::Start(Tag::MetadataBlock(_)), range) if range.start == 0 => Some(range.end),
        _ => None,
    }
}

/// `text` with every line ending written as a line feed. CommonMark also ends
/// a line with a carriage return, alone or before a line feed, but
/// pulldown-cmark does not end a fence's opening line at a lone carriage
/// return. Each ending stays one character, so every line keeps its number.
fn with_line_feeds(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// The byte offset where each line of a text starts, built once, so that the
/// line of an offset is found by binary search.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Lines {
        let ends = text.match_indices('\n').map(|(offset, _)| offset + 1);
        Lines(std::iter::once(0).chain(ends).collect())
    }

    /// The line, counted from 1, that holds byte `offset`.
    fn line(&self, offset: usize) -> u32 {
        self.0.partition_point(|&start| start <= offset) as u32
    }

    /// Where the bytes `range` are: the line of their first byte, and the line
    /// of their last byte when that is a later line.
    fn locator(&self, range: &Range<usize>) -> Locator {
        let line = self.line(range.start);
        let last = self.line(range.end.saturating_sub(1).max(range.start));
        Locator::Markdown {
            line,
            line_end: (last > line).then_some(last),
        }
    }
}

/// A container that the current event lies in.
struct OpenContainer {
    /// Its index in `containers`.
    id: u32,
    /// The bytes of the text it covers.
    range: Range<usize>,
    /// Whether a block was written inside it.
    holds_block: bool,
}

/// The block that inline events add their text to.
struct OpenBlock {
    kind: BlockKind,
    /// The text as the events give it, before `normalize`.
    raw: String,
    /// The bytes of the text it covers.
    range: Range<usize>,
}

/// An HTML block, which the reader leaves out.
struct HtmlBlock {
    /// Where it starts in the text.
    start: usize,
    /// Its content, which tells a block of comments from one that may hold
    /// text.
    content: String,
}

/// One pass over the parser's events. Block events open and close blocks and
/// containers, and inline events add to the open block's text.
struct Walk {
    lines: Lines,
    containers: Vec<Container>,
    blocks: Vec<Block>,
    warnings: Vec<Warning>,
    /// The containers around the current event, outermost first.
    open: Vec<OpenContainer>,
    block: Option<OpenBlock>,
    /// How many images the current event lies in. An image's alt text is left
    /// out, as the EPUB reader leaves out images.
    image_depth: usize,
    html_block: Option<HtmlBlock>,
}

impl Walk {
    fn new(lines: Lines) -> Walk {
        Walk {
            lines,
            containers: Vec::new(),
            blocks: Vec::new(),
            warnings: Vec::new(),
            open: Vec::new(),
            block: None,
            image_depth: 0,
            html_block: None,
        }
    }

    fn event(&mut self, event: Event, range: Range<usize>) {
        if let Some(html_block) = &mut self.html_block {
            match event {
                Event::Html(content) | Event::Text(content) => {
                    html_block.content.push_str(&content)
                }
                Event::End(TagEnd::HtmlBlock) => self.leave_out_html_block(),
                _ => {}
            }
            return;
        }
        if self.image_depth > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => self.image_depth += 1,
                Event::End(TagEnd::Image) => self.image_depth -= 1,
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Code(text) => self.inline(range).push_str(&text),
            Event::SoftBreak | Event::HardBreak => self.inline(range).push(' '),
            // A footnote reference adds no characters, and an inline tag is
            // dropped while the text between tags is kept.
            Event::FootnoteReference(_) | Event::InlineHtml(_) | Event::Html(_) => {
                self.inline(range);
            }
            Event::Rule => {
                self.finish_block();
                self.push_block(BlockKind::Rule, String::new(), range);
            }
            // Math and task lists are options the reader leaves off.
            Event::InlineMath(_) | Event::DisplayMath(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag, range: Range<usize>) {
        match tag {
            Tag::Paragraph => self.open_block(BlockKind::Paragraph, range),
            Tag::Heading { level, .. } => {
                self.open_block(BlockKind::Heading { level: level as u8 }, range)
            }
            Tag::CodeBlock(kind) => self.open_block(
                BlockKind::Code {
                    language: language(&kind),
                },
                range,
            ),
            Tag::BlockQuote(_) => self.open_container(Container::Quote, range),
            Tag::List(start) => self.open_container(
                Container::List {
                    ordered: start.is_some(),
                    start,
                },
                range,
            ),
            Tag::Item => self.open_container(Container::Item, range),
            Tag::FootnoteDefinition(label) => self.open_container(
                Container::Note {
                    label: Some(label.to_string()),
                },
                range,
            ),
            Tag::Table(_) => self.open_container(Container::Table, range),
            Tag::TableHead => self.open_container(Container::Row { header: true }, range),
            Tag::TableRow => self.open_container(Container::Row { header: false }, range),
            Tag::TableCell => self.open_container(Container::Cell, range),
            Tag::HtmlBlock => {
                self.finish_block();
                self.html_block = Some(HtmlBlock {
                    start: range.start,
                    content: String::new(),
                });
            }
            Tag::Image { .. } => self.image_depth = 1,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } => {
                self.inline(range);
            }
            // Options the reader leaves off.
            Tag::MetadataBlock(_)
            | Tag::Superscript
            | Tag::Subscript
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => self.finish_block(),
            TagEnd::BlockQuote(_)
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell => self.close_container(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {}
            // `event` meets these while it leaves an image or an HTML block
            // out; the rest are options the reader leaves off.
            TagEnd::Image
            | TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_)
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition => {}
        }
    }

    fn open_block(&mut self, kind: BlockKind, range: Range<usize>) {
        self.finish_block();
        self.block = Some(OpenBlock {
            kind,
            raw: String::new(),
            range,
        });
    }

    /// The text of the open block, for an inline event at `range`. Inline
    /// content outside a paragraph, as in a table cell or an item of a tight
    /// list, opens one.
    fn inline(&mut self, range: Range<usize>) -> &mut String {
        let block = self.block.get_or_insert_with(|| OpenBlock {
            kind: BlockKind::Paragraph,
            raw: String::new(),
            range: range.clone(),
        });
        block.range.end = block.range.end.max(range.end);
        &mut block.raw
    }

    /// Writes the open block, if there is one. Every block event closes the
    /// paragraph that inline content opened, since it holds inline content only.
    fn finish_block(&mut self) {
        let Some(OpenBlock {
            kind,
            mut raw,
            range,
        }) = self.block.take()
        else {
            return;
        };
        let text = match kind {
            BlockKind::Code { .. } => {
                if raw.ends_with('\n') {
                    raw.pop();
                }
                raw
            }
            _ => normalize(&raw),
        };
        self.push_block(kind, text, range);
    }

    fn push_block(&mut self, kind: BlockKind, text: String, range: Range<usize>) {
        for container in &mut self.open {
            container.holds_block = true;
        }
        self.blocks.push(Block {
            id: self.blocks.len() as u32,
            kind,
            text,
            spans: Vec::new(),
            ids: Vec::new(),
            within: self.open.iter().map(|container| container.id).collect(),
            section: None,
            locator: self.lines.locator(&range),
        });
    }

    fn open_container(&mut self, container: Container, range: Range<usize>) {
        self.finish_block();
        self.open.push(OpenContainer {
            id: self.containers.len() as u32,
            range,
            holds_block: false,
        });
        self.containers.push(container);
    }

    /// Closes the innermost container. A container exists only through the
    /// blocks inside it, so one that holds none, such as an empty table cell,
    /// gets an empty paragraph where it starts.
    fn close_container(&mut self) {
        self.finish_block();
        let Some(container) = self.open.last() else {
            return;
        };
        if !container.holds_block {
            // pulldown-cmark gives a cell it adds to fill a short row an empty
            // range after the row, at the start of the next line, so such a
            // cell takes its row's start.
            let mut start = container.range.start;
            if container.range.is_empty()
                && let Some(row) = self.open.iter().rev().nth(1)
            {
                start = row.range.start;
            }
            self.push_block(BlockKind::Paragraph, String::new(), start..start);
        }
        self.open.pop();
    }

    /// Ends the HTML block being left out, with a warning unless it holds only
    /// comments, which are not part of the text.
    fn leave_out_html_block(&mut self) {
        let Some(HtmlBlock { start, content }) = self.html_block.take() else {
            return;
        };
        if only_comments(&content) {
            return;
        }
        self.warnings.push(Warning::UnreadablePart {
            part: format!("HTML block at line {}", self.lines.line(start)),
            reason: "the reader does not read HTML, so any text in it is left out".to_string(),
        });
    }
}

/// One outline entry per heading with text, at the heading's level.
fn outline(blocks: &[Block]) -> Vec<OutlineEntry> {
    blocks
        .iter()
        .filter_map(|block| match block.kind {
            BlockKind::Heading { level } if !block.text.is_empty() => Some((level, block)),
            _ => None,
        })
        .enumerate()
        .map(|(id, (level, block))| OutlineEntry {
            id: id as u32,
            title: block.text.clone(),
            level,
            block_id: Some(block.id),
        })
        .collect()
}

/// The language a fence names: the first word of its info string, as
/// CommonMark has it.
fn language(kind: &CodeBlockKind) -> Option<String> {
    match kind {
        CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_string),
        CodeBlockKind::Indented => None,
    }
}

/// Whether `html` holds nothing but comments and whitespace. A comment that
/// is not closed runs to the end, as it does in CommonMark.
fn only_comments(mut html: &str) -> bool {
    loop {
        html = html.trim_start();
        if html.is_empty() {
            return true;
        }
        if !html.starts_with("<!--") {
            return false;
        }
        // The close is looked for from the first `-`, since `<!-->` and
        // `<!--->` are comments too.
        match html[2..].find("-->") {
            Some(close) => html = &html[2 + close + 3..],
            None => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Container::*;
    use crate::document::{BlockKind, Format, Warning, span_errors};
    use crate::normalize::normalize;

    fn source() -> Source {
        Source {
            sha256: "0".repeat(64),
            file_name: "notes.md".to_string(),
            format: Format::Markdown,
        }
    }

    fn doc(md: &str) -> Document {
        markdown_document(md, source()).unwrap()
    }

    /// The document `read_markdown` reads from a file holding `bytes`.
    fn read_bytes(bytes: &[u8]) -> Document {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, bytes).unwrap();
        read_markdown(&path, source()).unwrap()
    }

    fn heading(level: u8) -> BlockKind {
        BlockKind::Heading { level }
    }

    fn para() -> BlockKind {
        BlockKind::Paragraph
    }

    fn kinds_and_texts(d: &Document) -> Vec<(BlockKind, &str)> {
        d.blocks
            .iter()
            .map(|b| (b.kind.clone(), b.text.as_str()))
            .collect()
    }

    fn texts(d: &Document) -> Vec<&str> {
        d.blocks.iter().map(|b| b.text.as_str()).collect()
    }

    fn lines(d: &Document) -> Vec<(u32, Option<u32>)> {
        d.blocks
            .iter()
            .map(|b| match b.locator {
                Locator::Markdown { line, line_end } => (line, line_end),
                ref other => panic!("not a Markdown locator: {other:?}"),
            })
            .collect()
    }

    fn withins(d: &Document) -> Vec<Vec<u32>> {
        d.blocks.iter().map(|b| b.within.clone()).collect()
    }

    fn parts(warnings: &[Warning]) -> Vec<&str> {
        warnings
            .iter()
            .map(|w| match w {
                Warning::UnreadablePart { part, .. } => part.as_str(),
                other => panic!("not an unreadable part: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn headings_paragraphs_and_lines() {
        let d = doc("# Title\n\nOne\ntwo.\n\n## Sub\n\nThree.\n");
        assert_eq!(
            kinds_and_texts(&d),
            [
                (heading(1), "Title"),
                (para(), "One two."),
                (heading(2), "Sub"),
                (para(), "Three.")
            ]
        );
        assert_eq!(lines(&d), [(1, None), (3, Some(4)), (6, None), (8, None)]);
        assert_eq!(d.outline.len(), 2);
        assert_eq!(d.metadata.title, None);
    }

    #[test]
    fn the_outline_and_sections_come_from_the_headings() {
        let d = doc("Front.\n\n# A\n\nText.\n\n## B\n\nMore.\n");
        assert_eq!(
            d.outline,
            [
                OutlineEntry {
                    id: 0,
                    title: "A".to_string(),
                    level: 1,
                    block_id: Some(1),
                },
                OutlineEntry {
                    id: 1,
                    title: "B".to_string(),
                    level: 2,
                    block_id: Some(3),
                },
            ]
        );
        let sections: Vec<_> = d.blocks.iter().map(|b| b.section).collect();
        assert_eq!(sections, [None, Some(0), Some(0), Some(1), Some(1)]);
        assert_eq!(
            d.metadata,
            Metadata {
                title: None,
                authors: Vec::new(),
                language: None,
                page_count: None,
                spine_count: None,
            }
        );
        assert_eq!(d.source, source());
        assert!(d.files.is_empty() && d.page_labels.is_empty() && d.furniture.is_empty());
    }

    #[test]
    fn a_table_is_containers_and_an_empty_cell_holds_an_empty_paragraph() {
        let d = doc("| Register | Address |\n|---|---|\n| STATUS | 0x1A |\n| CONTROL | |\n");
        assert_eq!(
            d.containers,
            [
                Table,
                Row { header: true },
                Cell,
                Cell,
                Row { header: false },
                Cell,
                Cell,
                Row { header: false },
                Cell,
                Cell
            ]
        );
        assert_eq!(
            withins(&d),
            [
                vec![0, 1, 2],
                vec![0, 1, 3],
                vec![0, 4, 5],
                vec![0, 4, 6],
                vec![0, 7, 8],
                vec![0, 7, 9]
            ]
        );
        assert_eq!(d.blocks[5].text, "");
        assert_eq!(
            texts(&d),
            ["Register", "Address", "STATUS", "0x1A", "CONTROL", ""]
        );
        assert!(d.blocks.iter().all(|b| b.kind == para()));
        assert_eq!(
            lines(&d),
            [
                (1, None),
                (1, None),
                (3, None),
                (3, None),
                (4, None),
                (4, None)
            ]
        );
    }

    #[test]
    fn a_cell_added_to_fill_a_short_row_is_on_the_row_s_line() {
        let d = doc("| a | b |\n|---|---|\n| x |\n\nNext.\n");
        assert_eq!(texts(&d), ["a", "b", "x", "", "Next."]);
        assert_eq!(withins(&d)[3], [0, 4, 6]);
        assert_eq!(
            lines(&d),
            [(1, None), (1, None), (3, None), (3, None), (5, None)]
        );
    }

    #[test]
    fn nested_lists_quotes_and_numbering() {
        let d = doc("> 3. a\n>    - b\n>\n>    c\n");
        // quote 0, list 1 (ordered, start 3), item 2 holds "a" and "c"; list 3
        // (unordered), item 4 holds "b"
        assert_eq!(
            d.containers,
            [
                Quote,
                List {
                    ordered: true,
                    start: Some(3)
                },
                Item,
                List {
                    ordered: false,
                    start: None
                },
                Item
            ]
        );
        assert_eq!(texts(&d), ["a", "b", "c"]);
        assert_eq!(
            withins(&d),
            [vec![0, 1, 2], vec![0, 1, 2, 3, 4], vec![0, 1, 2]]
        );
        assert!(d.blocks.iter().all(|b| b.kind == para()));
        assert_eq!(lines(&d), [(1, None), (2, None), (4, None)]);
    }

    #[test]
    fn the_items_of_a_tight_list_are_paragraphs_and_two_lists_differ_by_id() {
        let d = doc("- one\n- two\n  - deep\n\n1. first\n");
        assert_eq!(
            d.containers,
            [
                List {
                    ordered: false,
                    start: None
                },
                Item,
                Item,
                List {
                    ordered: false,
                    start: None
                },
                Item,
                List {
                    ordered: true,
                    start: Some(1)
                },
                Item
            ]
        );
        assert_eq!(texts(&d), ["one", "two", "deep", "first"]);
        assert_eq!(
            withins(&d),
            [vec![0, 1], vec![0, 2], vec![0, 2, 3, 4], vec![5, 6]]
        );
        assert!(d.blocks.iter().all(|b| b.kind == para()));
        assert_eq!(lines(&d), [(1, None), (2, None), (3, None), (5, None)]);
    }

    #[test]
    fn an_empty_item_quote_or_note_holds_an_empty_paragraph() {
        let d = doc("Text.\n\n- \n- x\n\n>\n\n[^e]:\n");
        assert_eq!(texts(&d), ["Text.", "", "x", "", ""]);
        assert_eq!(
            d.containers,
            [
                List {
                    ordered: false,
                    start: None
                },
                Item,
                Item,
                Quote,
                Note {
                    label: Some("e".to_string())
                }
            ]
        );
        assert_eq!(
            withins(&d),
            [vec![], vec![0, 1], vec![0, 2], vec![3], vec![4]]
        );
        assert_eq!(
            lines(&d),
            [(1, None), (3, None), (4, None), (6, None), (8, None)]
        );
    }

    #[test]
    fn a_code_block_keeps_its_lines_and_language() {
        let d = doc("```rust\nfn  a() {\n    b\n}\n```\n");
        assert_eq!(
            d.blocks[0].kind,
            BlockKind::Code {
                language: Some("rust".into())
            }
        );
        assert_eq!(d.blocks[0].text, "fn  a() {\n    b\n}");
        assert_eq!(lines(&d), [(1, Some(5))]);
    }

    #[test]
    fn a_fence_names_its_language_by_the_first_word_and_indented_code_names_none() {
        let d = doc(
            "~~~ python  extra\nx = 1\n~~~\n\n```\nplain\n```\n\nText:\n\n    indented\n      more\n",
        );
        let kinds: Vec<_> = d.blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Code {
                    language: Some("python".into())
                },
                BlockKind::Code { language: None },
                para(),
                BlockKind::Code { language: None },
            ]
        );
        assert_eq!(texts(&d), ["x = 1", "plain", "Text:", "indented\n  more"]);
        assert_eq!(lines(&d)[3], (11, Some(12)));
    }

    #[test]
    fn a_rule_and_a_footnote_definition() {
        let d = doc("Text.[^n]\n\n---\n\n[^n]: Note text.\n");
        assert_eq!(
            kinds_and_texts(&d),
            [
                (para(), "Text."),
                (BlockKind::Rule, ""),
                (para(), "Note text.")
            ]
        );
        assert_eq!(
            d.containers,
            [Note {
                label: Some("n".to_string())
            }]
        );
        assert_eq!(withins(&d), [vec![], vec![], vec![0]]);
        assert_eq!(lines(&d), [(1, None), (3, None), (5, None)]);
    }

    #[test]
    fn a_note_keeps_the_kinds_of_its_blocks() {
        let d = doc("[^long]: First.\n\n    - item\n\n    ```\n    code\n    ```\n");
        assert_eq!(
            kinds_and_texts(&d),
            [
                (para(), "First."),
                (para(), "item"),
                (BlockKind::Code { language: None }, "code")
            ]
        );
        assert_eq!(withins(&d), [vec![0], vec![0, 1, 2], vec![0]]);
    }

    #[test]
    fn front_matter_and_html_blocks_are_left_out_with_warnings_and_comments_silently() {
        let d = doc("---\ntitle: x\n---\n\n<div>\nhi\n</div>\n\n<!-- c -->\n\nText.\n");
        assert_eq!(texts(&d), ["Text."]);
        assert_eq!(
            parts(&d.warnings),
            ["front matter at line 1", "HTML block at line 5"]
        );
    }

    #[test]
    fn only_the_start_of_the_file_is_front_matter() {
        // pulldown-cmark would read each `---` with text under it, up to the
        // next `---`, as a metadata block.
        let d = doc("A\n\n---\nB\n\nC\n\n---\n\nD\n\n---\nE\n---\n\n...\n");
        assert_eq!(
            kinds_and_texts(&d),
            [
                (para(), "A"),
                (BlockKind::Rule, ""),
                (para(), "B"),
                (para(), "C"),
                (BlockKind::Rule, ""),
                (para(), "D"),
                (BlockKind::Rule, ""),
                (heading(2), "E"),
                (para(), "...")
            ]
        );
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);

        let d = doc("---\ntitle: x\n...\nText.\n");
        assert_eq!(texts(&d), ["Text."]);
        assert_eq!(parts(&d.warnings), ["front matter at line 1"]);
        let d = doc("\n---\ntitle: x\n---\n");
        assert_eq!(
            kinds_and_texts(&d),
            [(BlockKind::Rule, ""), (heading(2), "title: x")]
        );
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }

    #[test]
    fn an_html_block_with_more_than_comments_is_reported() {
        let d = doc("<!-- a --> <div>x</div>\n\n<!-- b\nc -->\n\n<!-->\n\nText.\n");
        assert_eq!(texts(&d), ["Text."]);
        assert_eq!(parts(&d.warnings), ["HTML block at line 1"]);
    }

    #[test]
    fn inline_html_and_images_are_dropped_and_the_text_between_kept() {
        let d = doc("A ![alt *t*](i.png) b <b>bold</b> <!-- x --> c\\\nd *e* [f](#g) `h  i`\n");
        assert_eq!(texts(&d), ["A b bold c d e f h i"]);
        assert!(d.warnings.is_empty());
    }

    #[test]
    fn a_tight_item_that_holds_only_an_image_holds_an_empty_paragraph() {
        let d = doc("- ![alt](i.png)\n- x\n");
        assert_eq!(texts(&d), ["", "x"]);
        assert_eq!(withins(&d), [vec![0, 1], vec![0, 2]]);
    }

    #[test]
    fn every_text_but_code_is_normalized_and_no_spans_are_written() {
        let d = doc(
            "# ﬁne  Ti\u{00AD}tle\n\nCafe\u{0301}   au\tlait.\n\n| x\u{200B}y |\n|---|\n\n```\n\u{00AD}  keep\n```\n",
        );
        assert_eq!(
            texts(&d),
            ["fine Title", "Café au lait.", "xy", "\u{00AD}  keep"]
        );
        for block in &d.blocks {
            if !matches!(block.kind, BlockKind::Code { .. }) {
                assert_eq!(block.text, normalize(&block.text));
            }
            assert_eq!(span_errors(block), Vec::<String>::new());
            assert!(block.spans.is_empty() && block.ids.is_empty());
        }
    }

    #[test]
    fn block_ids_are_their_positions() {
        let d = doc("# A\n\nb\n\n- c\n");
        let ids: Vec<_> = d.blocks.iter().map(|b| b.id).collect();
        assert_eq!(ids, [0, 1, 2]);
    }

    #[test]
    fn a_setext_heading_spans_its_two_lines() {
        let d = doc("Title\n=====\n\nText.\n");
        assert_eq!(
            kinds_and_texts(&d),
            [(heading(1), "Title"), (para(), "Text.")]
        );
        assert_eq!(lines(&d), [(1, Some(2)), (4, None)]);
    }

    // Review Focus 1
    #[test]
    fn crlf_and_bom_give_the_same_document_as_lf() {
        assert_eq!(
            read_bytes(b"\xEF\xBB\xBF# T\r\n\r\nA\r\nb.\r\n"),
            read_bytes(b"# T\n\nA\nb.\n")
        );
    }

    #[test]
    fn crlf_and_lone_cr_count_lines_as_lf_does_in_code_too() {
        let lf = read_bytes(b"# T\n\nA\nb.\n\n```\nx\n  y\n```\n");
        assert_eq!(lines(&lf), [(1, None), (3, Some(4)), (6, Some(9))]);
        assert_eq!(texts(&lf), ["T", "A b.", "x\n  y"]);
        for bytes in [
            &b"# T\r\n\r\nA\r\nb.\r\n\r\n```\r\nx\r\n  y\r\n```\r\n"[..],
            b"# T\r\rA\rb.\r\r```\rx\r  y\r```\r",
        ] {
            assert_eq!(read_bytes(bytes), lf);
        }
    }

    // Review Focus 3
    #[test]
    fn an_empty_markdown_file_has_no_readable_text() {
        let e = markdown_document(" \n\n", source()).unwrap_err();
        assert!(
            matches!(e, Error::Unreadable { .. }) && e.to_string().contains("no readable text")
        );
    }

    #[test]
    fn a_file_of_rules_comments_and_front_matter_has_no_readable_text() {
        for md in [
            "",
            "---\n\n***\n",
            "<!-- c -->\n",
            "---\ntitle: x\n---\n",
            "\u{FEFF}",
        ] {
            let e = markdown_document(md, source()).unwrap_err();
            assert!(
                matches!(e, Error::Unreadable { .. }) && e.to_string().contains("no readable text"),
                "{md:?}: {e}"
            );
        }
    }

    #[test]
    fn reading_an_empty_file_names_its_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.md");
        std::fs::write(&path, b"\xEF\xBB\xBF \r\n").unwrap();
        let e = read_markdown(&path, source()).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("{}: could not be read: no readable text", path.display())
        );
    }

    #[test]
    fn a_file_over_the_cap_fails_with_its_size_and_the_cap() {
        let path = Path::new("big.md");
        assert!(check_size(path, MAX_MARKDOWN_BYTES).is_ok());
        let e = check_size(path, MAX_MARKDOWN_BYTES + 1).unwrap_err();
        assert!(matches!(e, Error::Unreadable { .. }), "{e:?}");
        let message = e.to_string();
        assert!(message.starts_with("big.md: "), "{message}");
        assert!(message.contains("67108865 bytes"), "{message}");
        assert!(message.contains("64 MiB"), "{message}");
    }
}
