//! Reads a Markdown file into the [`Document`] model.
//!
//! pulldown-cmark parses the file as CommonMark with tables, strikethrough and
//! footnotes, after it has found whether the file starts with front matter,
//! which the reader leaves out. One pass over its events writes the blocks in
//! reading order, and a stack of the containers open at each event gives a
//! block its `within`. Inline markup becomes spans over the block's text, and
//! each heading gets the id GitHub gives it. Once every block is read, the
//! links are resolved.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use unicase::UniCase;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

use super::assign_sections;
use crate::document::{
    Block, BlockKind, Container, Document, LinkTarget, Locator, Metadata, OutlineEntry,
    SCHEMA_VERSION, Source, Span, SpanKind, Warning,
};
use crate::error::{Error, Result};
use crate::normalize::{RawSpan, normalize_with_spans};

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

/// The most table cells a Markdown file may hold, counted before parsing.
/// GFM fills a row shorter than its header with empty cells, and
/// pulldown-cmark builds every cell before it gives the first event, so a few
/// kilobytes of wide tables with short rows would take gigabytes of memory,
/// which [`MAX_MARKDOWN_BYTES`] does not prevent.
pub(crate) const MAX_TABLE_CELLS: usize = 250_000;

/// Reads the Markdown file at `path`, UTF-8 with or without a byte-order mark.
///
/// A file over [`MAX_MARKDOWN_BYTES`], a file whose tables could hold more
/// than [`MAX_TABLE_CELLS`] cells and a file with no text in any block are
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
/// [`Document`]. Text whose tables could hold more than [`MAX_TABLE_CELLS`]
/// cells, and text that leaves no block with text, are an
/// [`Error::Unreadable`] that names the file of `source`. It reads what
/// [`document_to_markdown`](crate::document::markdown::document_to_markdown)
/// writes back into the same blocks, containers and spans.
pub fn markdown_document(text: &str, source: Source) -> Result<Document> {
    let text = with_line_feeds(text);
    check_table_cells(Path::new(&source.file_name), &text)?;
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
        mut warnings,
        ..
    } = walk;
    if blocks.iter().all(|block| block.text.is_empty()) {
        return Err(Error::Unreadable {
            path: PathBuf::from(&source.file_name),
            reason: "no readable text".to_string(),
        });
    }
    let sections = assign_sections(&mut blocks);
    warnings.extend(resolve_links(&mut blocks, &containers));
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

/// Refuses `text` when its tables could hold more than [`MAX_TABLE_CELLS`]
/// cells, before pulldown-cmark builds them.
///
/// A table starts at a delimiter row under a non-blank line and runs to the
/// next blank line, so it counts its delimiter row's columns once for the
/// header and once for every non-blank line after the delimiter row. That
/// also counts a delimiter row in a code block, and lines after a table that
/// a heading ends, so the count errs only toward refusing. Every open table
/// adds at least one cell per line, so besides reading each line once the
/// scan makes at most [`MAX_TABLE_CELLS`] additions before it refuses.
fn check_table_cells(path: &Path, text: &str) -> Result<()> {
    /// A table whose rows are still being counted.
    struct Table {
        /// The line of its delimiter row.
        line: usize,
        columns: usize,
        rows: usize,
    }
    let refuse = |table: &Table| {
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        Error::Unreadable {
            path: path.to_path_buf(),
            reason: format!(
                "the table whose delimiter row is line {} reaches {} row{} × {} column{}, \
                 which takes the file past the {MAX_TABLE_CELLS} table cells a Markdown file \
                 may hold; split the table or the file",
                table.line,
                table.rows,
                plural(table.rows),
                table.columns,
                plural(table.columns)
            ),
        }
    };
    let mut open: Vec<Table> = Vec::new();
    let mut cells = 0;
    let mut after_text = false;
    for (index, line) in text.split('\n').enumerate() {
        // CommonMark's blank line, which ends a table.
        if line.bytes().all(|b| b == b' ' || b == b'\t') {
            open.clear();
            after_text = false;
            continue;
        }
        for table in &mut open {
            table.rows += 1;
            cells += table.columns;
            if cells > MAX_TABLE_CELLS {
                return Err(refuse(table));
            }
        }
        // The line above is the new table's header row.
        if after_text && let Some(columns) = delimiter_columns(line) {
            let table = Table {
                line: index + 1,
                columns,
                rows: 1,
            };
            cells += columns;
            if cells > MAX_TABLE_CELLS {
                return Err(refuse(&table));
            }
            open.push(table);
        }
        after_text = true;
    }
    Ok(())
}

/// How many columns `line` gives a table when it can be a GFM delimiter row,
/// such as `| :-- | --: |`, or `None` when it cannot. pulldown-cmark takes a
/// line of spaces, colons, hyphens and at least one pipe and one hyphen, and
/// each pipe after a leading one closes a column, as does the end of a last
/// column that holds more than spaces. Block-quote markers and indentation in
/// front are skipped, and tabs are let through although pulldown-cmark refuses
/// them, so the test errs toward counting a line.
fn delimiter_columns(line: &str) -> Option<usize> {
    let row = line.trim_start_matches([' ', '\t', '>']);
    let allowed = row
        .bytes()
        .all(|b| matches!(b, b'|' | b'-' | b':' | b' ' | b'\t'));
    if !allowed || !row.contains('|') || !row.contains('-') {
        return None;
    }
    let row = row.strip_prefix('|').unwrap_or(row);
    let last = row.rsplit('|').next().unwrap_or_default();
    let open_column = !last.trim_matches([' ', '\t']).is_empty();
    Some(row.matches('|').count() + usize::from(open_column))
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
    /// The text as the events give it, before normalization.
    raw: String,
    /// The spans over `raw`, in the order they open.
    spans: Vec<RawSpan>,
    /// The spans still open, innermost last, as indexes into `spans`.
    open_spans: Vec<usize>,
    /// For a heading, its text as written, without normalization and with
    /// nothing for a line break, which is the text GitHub takes its id from.
    /// Empty for any other block.
    written: String,
    /// The bytes of the text it covers.
    range: Range<usize>,
}

impl OpenBlock {
    fn new(kind: BlockKind, range: Range<usize>) -> OpenBlock {
        OpenBlock {
            kind,
            raw: String::new(),
            spans: Vec::new(),
            open_spans: Vec::new(),
            written: String::new(),
            range,
        }
    }

    /// Adds the `text` of a text or code event to `written`, if the block is
    /// a heading.
    fn push_written(&mut self, text: &str) {
        if matches!(self.kind, BlockKind::Heading { .. }) {
            self.written.push_str(text);
        }
    }
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
/// containers, and inline events add to the open block's text and spans.
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
    /// The heading ids given so far, with their counts, for [`heading_id`].
    heading_ids: HashMap<String, u32>,
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
            heading_ids: HashMap::new(),
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
            Event::Text(text) => {
                let block = self.inline(range);
                block.raw.push_str(&text);
                block.push_written(&text);
            }
            Event::Code(text) => self
                .push_span(&text, SpanKind::Code, range)
                .push_written(&text),
            Event::SoftBreak => self.inline(range).raw.push(' '),
            Event::HardBreak => {
                self.push_span(" ", SpanKind::Break { lines: 1 }, range);
            }
            // A footnote reference adds no characters; `resolve_links` finds
            // its note once every block is read.
            Event::FootnoteReference(label) => {
                let marker = label.to_string();
                let kind = SpanKind::NoteRef {
                    marker,
                    block: None,
                };
                self.push_span("", kind, range);
            }
            // An inline tag is dropped while the text between tags is kept.
            Event::InlineHtml(_) | Event::Html(_) => {
                self.inline(range);
            }
            Event::Rule => {
                self.finish_block();
                self.push_block(BlockKind::Rule, String::new(), Vec::new(), range);
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
            Tag::Emphasis => self.open_span(SpanKind::Emphasis, range),
            Tag::Strong => self.open_span(SpanKind::Strong, range),
            Tag::Strikethrough => self.open_span(SpanKind::Strikethrough, range),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                // CommonMark makes an email autolink's destination `mailto:`
                // and the address.
                let href = match link_type {
                    LinkType::Email => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                // `resolve_links` decides where it leads once every block is
                // read.
                let to = LinkTarget::Nowhere;
                self.open_span(SpanKind::Link { href, to }, range);
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
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.close_span()
            }
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
        self.block = Some(OpenBlock::new(kind, range));
    }

    /// The open block, for an inline event at `range`. Inline content outside
    /// a paragraph, as in a table cell or an item of a tight list, opens one.
    fn inline(&mut self, range: Range<usize>) -> &mut OpenBlock {
        let block = self
            .block
            .get_or_insert_with(|| OpenBlock::new(BlockKind::Paragraph, range.clone()));
        block.range.end = block.range.end.max(range.end);
        block
    }

    /// Adds `text` to the open block as a span of `kind`, for the inline event
    /// at `range`, and returns the block.
    fn push_span(&mut self, text: &str, kind: SpanKind, range: Range<usize>) -> &mut OpenBlock {
        let block = self.inline(range);
        let start = block.raw.len();
        block.raw.push_str(text);
        let covered = start..block.raw.len();
        block.spans.push(RawSpan {
            range: covered,
            kind,
        });
        block
    }

    /// Opens a span of `kind` where the open block's text ends, for the
    /// inline event at `range`.
    fn open_span(&mut self, kind: SpanKind, range: Range<usize>) {
        let block = self.inline(range);
        let at = block.raw.len();
        block.open_spans.push(block.spans.len());
        block.spans.push(RawSpan {
            range: at..at,
            kind,
        });
    }

    /// Closes the innermost open span where the open block's text ends.
    fn close_span(&mut self) {
        if let Some(block) = &mut self.block
            && let Some(index) = block.open_spans.pop()
        {
            block.spans[index].range.end = block.raw.len();
        }
    }

    /// Writes the open block, if there is one. Every block event closes the
    /// paragraph that inline content opened, since it holds inline content only.
    fn finish_block(&mut self) {
        let Some(OpenBlock {
            kind,
            mut raw,
            spans,
            written,
            range,
            ..
        }) = self.block.take()
        else {
            return;
        };
        let (text, spans) = match kind {
            BlockKind::Code { .. } => {
                if raw.ends_with('\n') {
                    raw.pop();
                }
                (raw, Vec::new())
            }
            _ => normalize_with_spans(&raw, spans),
        };
        // A paragraph that dropped images and tags leave with neither text
        // nor a span is not written. A container that then holds no block
        // gets an empty paragraph as its placeholder when it closes.
        if kind == BlockKind::Paragraph && text.is_empty() && spans.is_empty() {
            return;
        }
        // Each heading takes its id in reading order, so that a repeat is
        // numbered as on GitHub. An empty id, as for `!!!`, names nothing and
        // is not kept, but it still counts as given, as in `gui/markdown.js`.
        let id = matches!(kind, BlockKind::Heading { .. })
            .then(|| heading_id(&written, &mut self.heading_ids))
            .filter(|id| !id.is_empty());
        self.push_block(kind, text, spans, range);
        if let (Some(id), Some(block)) = (id, self.blocks.last_mut()) {
            block.ids.push(id);
        }
    }

    fn push_block(&mut self, kind: BlockKind, text: String, spans: Vec<Span>, range: Range<usize>) {
        for container in &mut self.open {
            container.holds_block = true;
        }
        self.blocks.push(Block {
            id: self.blocks.len() as u32,
            kind,
            text,
            spans,
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
            self.push_block(
                BlockKind::Paragraph,
                String::new(),
                Vec::new(),
                start..start,
            );
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

/// The id GitHub gives a heading whose text as written is `text`, ported
/// from `slugger` in `gui/markdown.js`: `text` in lower case without the
/// characters other than letters, marks, numbers, connector punctuation,
/// spaces and `-`, with each space turned into `-`. `seen` holds the ids given so far, with a count
/// for each, and a repeated id gets `-1`, `-2` and so on.
pub(crate) fn heading_id(text: &str, seen: &mut HashMap<String, u32>) -> String {
    let base: String = text
        .to_lowercase()
        .chars()
        .filter(|&c| c == ' ' || c == '-' || in_heading_id(c))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    let mut slug = base.clone();
    while seen.contains_key(&slug) {
        // The first slug tried is `base`, so `base` is in `seen` here.
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        slug = format!("{base}-{count}");
    }
    seen.insert(slug.clone(), 0);
    slug
}

/// Whether a heading id keeps `c`: a letter, a mark, a number or connector
/// punctuation, `\p{L}\p{M}\p{N}\p{Pc}` in the regular expression of
/// `slugger`.
fn in_heading_id(c: char) -> bool {
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Mark | GeneralCategoryGroup::Number
    ) || c.general_category() == GeneralCategory::ConnectorPunctuation
}

/// Decides where each link and footnote reference of `blocks` leads, once
/// every block is read, and returns a [`Warning::BrokenLink`] for each one
/// into the document that leads nowhere.
///
/// A footnote reference leads to the first block of the first note whose
/// label matches its own as pulldown-cmark matches them, ignoring case.
/// pulldown-cmark makes a reference only when such a note exists, so a
/// broken one cannot come from a file.
fn resolve_links(blocks: &mut [Block], containers: &[Container]) -> Vec<Warning> {
    let mut by_id: HashMap<String, u32> = HashMap::new();
    let mut notes: HashMap<UniCase<&str>, u32> = HashMap::new();
    for block in blocks.iter() {
        for id in &block.ids {
            by_id.entry(id.clone()).or_insert(block.id);
        }
        for &container in &block.within {
            if let Container::Note { label: Some(label) } = &containers[container as usize] {
                notes
                    .entry(UniCase::new(label.as_str()))
                    .or_insert(block.id);
            }
        }
    }
    let mut warnings = Vec::new();
    for block in blocks.iter_mut() {
        for span in &mut block.spans {
            let broken = match &mut span.kind {
                SpanKind::Link { href, to } => {
                    *to = link_target(href, &by_id);
                    (*to == LinkTarget::Nowhere && href.starts_with('#')).then(|| href.clone())
                }
                SpanKind::NoteRef {
                    marker,
                    block: note,
                } => {
                    *note = notes.get(&UniCase::new(marker.as_str())).copied();
                    note.is_none().then(|| format!("[^{marker}]"))
                }
                _ => None,
            };
            if let Some(href) = broken {
                warnings.push(Warning::BrokenLink {
                    href,
                    block: block.id,
                });
            }
        }
    }
    warnings
}

/// Where a link to `href` leads. A `#` fragment leads to the first block
/// whose `ids` hold it, after its percent escapes are decoded as
/// `gui/markdown.js` decodes them, and an empty one to the top of the
/// document, as in HTML. An address with a scheme leads out of the document,
/// and a relative path, without its query or fragment, to the file it names.
/// A fragment that no block holds, an absolute path and an empty `href` lead
/// nowhere.
fn link_target(href: &str, ids: &HashMap<String, u32>) -> LinkTarget {
    if let Some(fragment) = href.strip_prefix('#') {
        if fragment.is_empty() {
            return LinkTarget::Block { block: 0 };
        }
        let id = percent_decoded(fragment).unwrap_or_else(|| fragment.to_string());
        return match ids.get(&id) {
            Some(&block) => LinkTarget::Block { block },
            None => LinkTarget::Nowhere,
        };
    }
    if has_scheme(href) {
        return LinkTarget::Address;
    }
    let path = href.split(['?', '#']).next().unwrap_or_default();
    if path.is_empty() || path.starts_with('/') {
        return LinkTarget::Nowhere;
    }
    LinkTarget::File {
        path: percent_decoded(path).unwrap_or_else(|| path.to_string()),
    }
}

/// Whether `href` starts with a URI scheme and its colon, such as `https:` or
/// `mailto:`.
fn has_scheme(href: &str) -> bool {
    let Some((scheme, _)) = href.split_once(':') else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `text` with its percent escapes decoded, or `None` when an escape is not
/// `%` and two hex digits or the decoded bytes are not UTF-8, which is where
/// JavaScript's `decodeURIComponent` fails too.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let hex = |at: usize| (*bytes.get(at)? as char).to_digit(16);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            decoded.push((hex(at + 1)? * 16 + hex(at + 2)?) as u8);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
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
    use std::time::{Duration, Instant};

    use super::*;
    use crate::document::Container::*;
    // The variants shadow the `Block` struct, which the helpers name as
    // `super::Block`.
    use crate::document::LinkTarget::{Address, Block, File, Nowhere};
    use crate::document::{BlockKind, Format, LinkTarget, SpanKind, Warning, span_errors};
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

    fn spans(b: &super::Block) -> Vec<(usize, usize, SpanKind)> {
        b.spans
            .iter()
            .map(|s| (s.start, s.end, s.kind.clone()))
            .collect()
    }

    fn emphasis() -> SpanKind {
        SpanKind::Emphasis
    }

    fn strong() -> SpanKind {
        SpanKind::Strong
    }

    fn code() -> SpanKind {
        SpanKind::Code
    }

    fn strike() -> SpanKind {
        SpanKind::Strikethrough
    }

    fn brk(lines: u32) -> SpanKind {
        SpanKind::Break { lines }
    }

    fn noteref(marker: &str, block: Option<u32>) -> SpanKind {
        SpanKind::NoteRef {
            marker: marker.to_string(),
            block,
        }
    }

    fn link(href: &str, to: LinkTarget) -> SpanKind {
        SpanKind::Link {
            href: href.to_string(),
            to,
        }
    }

    fn ids(d: &Document) -> Vec<Vec<String>> {
        d.blocks.iter().map(|b| b.ids.clone()).collect()
    }

    fn targets(b: &super::Block) -> Vec<LinkTarget> {
        b.spans
            .iter()
            .filter_map(|s| match &s.kind {
                SpanKind::Link { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn emphasis_strong_code_and_strikethrough_are_spans_over_normalised_text() {
        let b = &doc("Ah,  *that’s*  the **great** `puzzle` ~~x~~.\n").blocks[0];
        assert_eq!(b.text, "Ah, that’s the great puzzle x.");
        assert_eq!(
            spans(b),
            [
                (4, 10, emphasis()),
                (15, 20, strong()),
                (21, 27, code()),
                (28, 29, strike())
            ]
        );
        assert!(span_errors(b).is_empty());
    }

    #[test]
    fn hard_breaks_are_counted_breaks_on_one_space() {
        let b = &doc("one\\\ntwo\\\n\\\nthree\n").blocks[0];
        assert_eq!(b.text, "one two three");
        assert_eq!(spans(b), [(3, 4, brk(1)), (7, 8, brk(2))]);
    }

    // Review Focus 2
    #[test]
    fn spans_count_code_points_after_an_emoji() {
        let b = &doc("😀 *x*\n").blocks[0];
        assert_eq!(spans(b), [(2, 3, emphasis())]);
    }

    #[test]
    fn heading_ids_follow_github_and_repeat_with_numbers() {
        // The cases of 'headings get GitHub ids…' in gui/markdown.test.js.
        assert_eq!(
            ids(&doc("## Outline\n\n## Outline\n## `cache` list!\n")),
            [vec!["outline"], vec!["outline-1"], vec!["cache-list"]]
        );
    }

    #[test]
    fn links_resolve_to_blocks_files_addresses_or_nowhere() {
        let d = doc("# Top\n\n[a](#top) [b](search.md) [c](https://x.org) [d](#gone)\n");
        assert_eq!(
            targets(&d.blocks[1]),
            [
                Block { block: 0 },
                File {
                    path: "search.md".into()
                },
                Address,
                Nowhere
            ]
        );
        assert_eq!(
            d.warnings,
            [Warning::BrokenLink {
                href: "#gone".into(),
                block: 1
            }]
        );
    }

    #[test]
    fn a_footnote_reference_is_an_empty_noteref_to_the_note() {
        let d = doc("Claim[^1] goes.\n\n[^1]: Source.\n");
        assert_eq!(d.blocks[0].text, "Claim goes.");
        assert_eq!(spans(&d.blocks[0]), [(5, 5, noteref("1", Some(1)))]);
    }

    #[test]
    fn images_and_inline_tags_leave_their_text_and_nothing_else() {
        assert_eq!(
            doc("a ![pic](x.png) <kbd>Ctrl</kbd> b\n").blocks[0].text,
            "a Ctrl b"
        );
    }

    #[test]
    fn spans_nest_and_reach_headings_and_cells() {
        let d = doc("## *A* [**b** c](#a-b-c)\n\n| `x` | ~~y~~ |\n|---|---|\n\n*a\\\nb* ***c***\n");
        assert_eq!(texts(&d), ["A b c", "x", "y", "a b c"]);
        assert_eq!(
            spans(&d.blocks[0]),
            [
                (0, 1, emphasis()),
                (2, 5, link("#a-b-c", Block { block: 0 })),
                (2, 3, strong())
            ]
        );
        assert_eq!(spans(&d.blocks[1]), [(0, 1, code())]);
        assert_eq!(spans(&d.blocks[2]), [(0, 1, strike())]);
        assert_eq!(
            spans(&d.blocks[3]),
            [
                (0, 3, emphasis()),
                (1, 2, brk(1)),
                (4, 5, emphasis()),
                (4, 5, strong())
            ]
        );
        for block in &d.blocks {
            assert_eq!(span_errors(block), Vec::<String>::new());
        }
    }

    #[test]
    fn a_span_left_without_text_is_dropped() {
        let d = doc("a [](x.md) *![i](i.png)* ` ` b\n");
        assert_eq!(texts(&d), ["a b"]);
        assert_eq!(spans(&d.blocks[0]), []);
    }

    #[test]
    fn an_empty_paragraph_is_written_only_as_its_container_s_placeholder() {
        let d = doc(
            "![a](i.png)\n\n<b></b>\n\n# ![h](i.png)\n\n> ![q](i.png)\n\n\
             - ![i](i.png)\n\n  Item text.\n\nText.\n",
        );
        // The quote holds no other block, so its empty paragraph stays as its
        // placeholder; the item holds its text, so its empty paragraph goes.
        assert_eq!(
            kinds_and_texts(&d),
            [
                (heading(1), ""),
                (para(), ""),
                (para(), "Item text."),
                (para(), "Text.")
            ]
        );
        assert_eq!(withins(&d), [vec![], vec![0], vec![1, 2], vec![]]);
    }

    #[test]
    fn heading_ids_come_from_the_text_as_written() {
        // GitHub's ids, from the text before normalization: the no-break
        // space and the line breaks of a setext heading give nothing, two
        // spaces give two dashes, and e with U+0301 stays decomposed.
        let d = doc("## Step&nbsp;1\n\n## x  y\n\n## e\u{0301}x\n\nA *b*\nc\n===\n\nA\\\nb\n---\n");
        assert_eq!(texts(&d), ["Step 1", "x y", "éx", "A b c", "A b"]);
        assert_eq!(
            ids(&d),
            [
                vec!["step1"],
                vec!["x--y"],
                vec!["e\u{0301}x"],
                vec!["a-bc"],
                vec!["ab"]
            ]
        );
    }

    #[test]
    fn a_span_keeps_its_bounds_at_characters_that_nfc_replaces() {
        // NFC replaces U+0387 GREEK ANO TELEIA with U+00B7, U+212B ANGSTROM
        // SIGN with U+00C5, and U+095B DEVANAGARI LETTER ZA with U+091C U+093C.
        let d = doc("*λόγος*\u{0387} x *\u{212B}* a *\u{095B}रूर*\n");
        assert_eq!(
            d.blocks[0].text,
            "λόγος\u{00B7} x \u{00C5} a \u{091C}\u{093C}रूर"
        );
        assert_eq!(
            spans(&d.blocks[0]),
            [
                (0, 5, emphasis()),
                (9, 10, emphasis()),
                (13, 18, emphasis())
            ]
        );
    }

    fn slugs(texts: &[&str]) -> Vec<String> {
        let mut seen = HashMap::new();
        texts
            .iter()
            .map(|text| heading_id(text, &mut seen))
            .collect()
    }

    #[test]
    fn heading_id_gives_what_slugger_in_gui_markdown_js_gives() {
        // Each expected list is what `render` in gui/markdown.js gives
        // headings with these texts.
        assert_eq!(
            slugs(&[
                "Using ebook-hero",
                "Where files are kept?",
                "outline",
                "outline",
                "book_info",
                "The read command",
                "six"
            ]),
            [
                "using-ebook-hero",
                "where-files-are-kept",
                "outline",
                "outline-1",
                "book_info",
                "the-read-command",
                "six"
            ]
        );
        assert_eq!(slugs(&["a", "a", "a-1", "a"]), ["a", "a-1", "a-1-1", "a-2"]);
        assert_eq!(slugs(&["a-1", "a", "a"]), ["a-1", "a", "a-2"]);
        assert_eq!(
            slugs(&[
                "Ünïcödé Straße",
                "日本語 見出し",
                "a_b‿c",
                "x  y",
                "!!!",
                "!!!",
                "Ⅻ ½ ٣"
            ]),
            [
                "ünïcödé-straße",
                "日本語-見出し",
                "a_b‿c",
                "x--y",
                "",
                "-1",
                "ⅻ-½-٣"
            ]
        );
    }

    #[test]
    fn an_empty_heading_id_is_not_kept_but_counts() {
        let d = doc("# !!!\n\n# !!!\n\nText.\n");
        assert_eq!(ids(&d), [vec![], vec!["-1"], vec![]]);
    }

    #[test]
    fn fragments_are_decoded_and_paths_lose_their_query_and_fragment() {
        let d = doc(
            "# Café\n\n[a](#caf%C3%A9) [b](#) [c](my%20notes.md#part) [d](x.md?raw=1) \
             [e](/abs.md) [f](mailto:a@b.co) <a@b.co> [g](#bad%zz)\n",
        );
        assert_eq!(d.blocks[1].text, "a b c d e f a@b.co g");
        assert_eq!(
            spans(&d.blocks[1]),
            [
                (0, 1, link("#caf%C3%A9", Block { block: 0 })),
                (2, 3, link("#", Block { block: 0 })),
                (
                    4,
                    5,
                    link(
                        "my%20notes.md#part",
                        File {
                            path: "my notes.md".into()
                        }
                    )
                ),
                (
                    6,
                    7,
                    link(
                        "x.md?raw=1",
                        File {
                            path: "x.md".into()
                        }
                    )
                ),
                (8, 9, link("/abs.md", Nowhere)),
                (10, 11, link("mailto:a@b.co", Address)),
                (12, 18, link("mailto:a@b.co", Address)),
                (19, 20, link("#bad%zz", Nowhere))
            ]
        );
        assert_eq!(
            d.warnings,
            [Warning::BrokenLink {
                href: "#bad%zz".into(),
                block: 1
            }]
        );
    }

    #[test]
    fn a_footnote_reference_finds_its_note_whatever_the_case_of_its_label() {
        let d = doc("A[^Note] b[^note].\n\n[^note]: First.\n\n    Second.\n");
        assert_eq!(texts(&d), ["A b.", "First.", "Second."]);
        assert_eq!(
            spans(&d.blocks[0]),
            [
                (1, 1, noteref("Note", Some(1))),
                (3, 3, noteref("note", Some(1)))
            ]
        );
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }

    #[test]
    fn a_reference_to_a_missing_note_leads_nowhere_with_a_warning() {
        // pulldown-cmark writes a reference only when its note exists, so the
        // note is taken away after reading.
        let mut d = doc("Claim[^1].\n\n[^1]: Source.\n");
        d.blocks.truncate(1);
        let warnings = resolve_links(&mut d.blocks, &d.containers);
        assert_eq!(spans(&d.blocks[0]), [(5, 5, noteref("1", None))]);
        assert_eq!(
            warnings,
            [Warning::BrokenLink {
                href: "[^1]".into(),
                block: 0
            }]
        );
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
        let d = doc("A ![alt *t*](i.png) b <b>bold</b> <!-- x --> c\\\nd *e* [f](g.md) `h  i`\n");
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
    fn every_text_but_code_is_normalized() {
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
        }
        // The heading's id comes from its text as written, as on GitHub: the
        // ligature stays, two spaces give two dashes, and the soft hyphen, a
        // format character, is dropped.
        assert_eq!(ids(&d)[0], ["ﬁne--title"]);
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

    /// A table `columns` wide whose `rows` body rows hold one cell each, which
    /// GFM fills up to the header's width, and a blank line after it.
    fn wide_table(columns: usize, rows: usize) -> String {
        format!(
            "{}|\n{}|\n{}\n",
            "|a".repeat(columns),
            "|-".repeat(columns),
            "x\n".repeat(rows)
        )
    }

    #[test]
    fn tables_that_could_pass_the_cell_cap_are_refused_before_parsing() {
        // 120 KB of this shape gave pulldown-cmark 10.5 million cells and
        // 500 MB, so a refusal that comes after parsing takes seconds and
        // gigabytes, and one that comes before takes neither.
        let md = wide_table(512, 512).repeat(40);
        let started = Instant::now();

        let Err(e) = markdown_document(&md, source()) else {
            panic!("the tables were read");
        };

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(e, Error::Unreadable { .. }), "{e:?}");
        assert_eq!(
            e.to_string(),
            "notes.md: could not be read: the table whose delimiter row is line 2 reaches \
             489 rows × 512 columns, which takes the file past the 250000 table cells a \
             Markdown file may hold; split the table or the file"
        );
    }

    #[test]
    fn the_cell_cap_counts_every_table_of_the_file() {
        // Each table is 101 rows of 100 columns and 103 lines long, so 24
        // tables hold 242,400 cells and the 25th passes the cap at its 77th row.
        let Err(e) = markdown_document(&wide_table(100, 100).repeat(25), source()) else {
            panic!("the tables were read");
        };
        let m = e.to_string();
        assert!(
            m.contains("delimiter row is line 2474 reaches 77 rows × 100 columns"),
            "{m}"
        );
        assert!(m.contains("250000 table cells"), "{m}");
    }

    #[test]
    fn a_table_well_under_the_cell_cap_reads() {
        let md = format!(
            "| a | b | c | d | e |\n|---|---|---|---|---|\n{}",
            "| 1 | 2 | 3 | 4 | 5 |\n".repeat(1000)
        );
        let d = doc(&md);
        assert_eq!(d.blocks.len(), 5 * 1001);
        assert_eq!(d.containers.len(), 1 + 1001 + 5 * 1001);
    }

    #[test]
    fn delimiter_rows_in_a_code_block_are_counted_and_a_normal_file_still_reads() {
        let code = "| a | b |\n|---|---|\n|---|---|\n|---|---|";
        let d = doc(&format!("Text.\n\n```\n{code}\n```\n\nMore.\n"));
        assert_eq!(
            kinds_and_texts(&d),
            [
                (para(), "Text."),
                (BlockKind::Code { language: None }, code),
                (para(), "More.")
            ]
        );
    }

    #[test]
    fn a_delimiter_row_gives_at_least_the_columns_pulldown_cmark_gives_it() {
        for (line, columns) in [
            ("|---|---|", Some(2)),
            ("---|---", Some(2)),
            ("| :-- | --: |", Some(2)),
            ("|-", Some(1)),
            ("-:-|", Some(1)),
            ("|---|   ", Some(1)),
            ("> >   | - | - |", Some(2)),
            // pulldown-cmark refuses tabs; counting them errs toward refusing.
            ("|\t-\t|", Some(1)),
            ("---", None),
            ("| a |", None),
            ("| |", None),
            ("|", None),
            ("", None),
        ] {
            assert_eq!(delimiter_columns(line), columns, "{line:?}");
        }
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
