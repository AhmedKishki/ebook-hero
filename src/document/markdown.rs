//! A [`Document`] as Markdown, which `ebook-hero extract` prints by default.
//!
//! The writer writes each block inside the containers that hold it, marks its
//! spans and escapes its text, so that the Markdown reader reads the output
//! back into the same blocks, containers and spans. It escapes a character
//! only where it could start syntax, so that prose stays readable.
//! `docs/document-model.md` lists what it writes and where a round trip
//! cannot hold.

use std::collections::HashMap;

use unicase::UniCase;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::document::{
    Block, BlockKind, Container, Document, Locator, Span, SpanKind, byte_offset,
};

/// Writes the document as Markdown: a `# {title}` line when the title is
/// known, then the blocks, with a blank line between one and the next. A PDF
/// block on a new page is preceded by a `<!-- page N -->` comment, which the
/// Markdown reader drops.
pub fn document_to_markdown(doc: &Document) -> String {
    let writer = Writer::new(doc);
    let mut lines = Vec::new();
    if let Some(title) = &doc.metadata.title {
        // A line break in the title would end the heading.
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let content = writer.inline(&title, &[], Context::Heading).concat();
        lines.push(atx_heading(1, &content));
    }
    let body = writer.run(&doc.blocks, 0);
    if !lines.is_empty() && !body.is_empty() {
        lines.push(String::new());
    }
    lines.extend(body);
    if lines.is_empty() {
        return String::new();
    }
    let mut markdown = lines.join("\n");
    markdown.push('\n');
    markdown
}

/// Writes one document. It knows the label each note is written with.
struct Writer<'a> {
    doc: &'a Document,
    /// The label of each `Note` container, by container id: its own label,
    /// or else the marker of the first reference to it, or else its number
    /// among the notes.
    note_labels: HashMap<u32, String>,
}

impl<'a> Writer<'a> {
    fn new(doc: &'a Document) -> Writer<'a> {
        let mut markers: HashMap<u32, &str> = HashMap::new();
        for span in doc.blocks.iter().flat_map(|block| &block.spans) {
            if let SpanKind::NoteRef {
                marker,
                block: Some(target),
            } = &span.kind
                && let Some(note) = note_of(doc, *target)
            {
                markers.entry(note).or_insert(marker);
            }
        }
        let notes =
            doc.containers
                .iter()
                .enumerate()
                .filter_map(|(id, container)| match container {
                    Container::Note { label } => Some((id as u32, label)),
                    _ => None,
                });
        let mut note_labels = HashMap::new();
        for (number, (id, label)) in notes.enumerate() {
            let label = label
                .clone()
                .or_else(|| markers.get(&id).map(|marker| marker.to_string()))
                .unwrap_or_else(|| (number + 1).to_string());
            note_labels.insert(id, label);
        }
        Writer { doc, note_labels }
    }

    /// The label a reference with `marker` to the note whose first block is
    /// `target` is written with. That is the marker when it matches the
    /// note's label as the reader matches them, ignoring case, so that the
    /// marker reads back as it was; otherwise it is the note's label, so that
    /// the reference leads to the note.
    fn reference_label<'m>(&'m self, marker: &'m str, target: Option<u32>) -> &'m str {
        let label = target
            .and_then(|block| note_of(self.doc, block))
            .and_then(|note| self.note_labels.get(&note));
        match label {
            Some(label) if UniCase::new(label.as_str()) != UniCase::new(marker) => label,
            _ => marker,
        }
    }

    /// Writes `blocks`, which share the containers `within[..depth]`, as
    /// lines. A block in no further container is one element, and so is each
    /// further container with the blocks in it; a blank line separates
    /// elements. At the top level a PDF block on a new page is preceded by
    /// `<!-- page N -->`.
    fn run(&self, blocks: &[Block], depth: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut page = None;
        let mut list = None;
        for group in groups(blocks, depth) {
            if depth == 0
                && let Locator::Pdf { page: this, .. } = group[0].locator
                && page != Some(this)
            {
                page = Some(this);
                push_element(&mut lines, vec![format!("<!-- page {this} -->")]);
            }
            let (element, marker) = self.element(group, depth, list);
            list = marker;
            push_element(&mut lines, element);
        }
        lines
    }

    /// Writes one element: a block, or the container at `depth` with the
    /// blocks in it. A list also returns its marker, which a list right after
    /// it must differ from.
    fn element(
        &self,
        group: &[Block],
        depth: usize,
        previous: Option<Marker>,
    ) -> (Vec<String>, Option<Marker>) {
        let Some(&id) = group[0].within.get(depth) else {
            return (self.block(&group[0]), None);
        };
        let inner = depth + 1;
        let lines = match self.doc.containers.get(id as usize) {
            Some(&Container::List { ordered, start }) => {
                let marker = Marker::after(previous, ordered, start);
                return (self.list(group, depth, marker), Some(marker));
            }
            // An item outside a list, which no reader writes.
            Some(Container::Item) => item_lines("-", self.run(group, inner)),
            Some(Container::Quote) => self
                .run(group, inner)
                .into_iter()
                .map(|line| match line.is_empty() {
                    true => ">".to_string(),
                    false => format!("> {line}"),
                })
                .collect(),
            Some(Container::Note { .. }) => {
                note_lines(&self.note_labels[&id], self.run(group, inner))
            }
            Some(Container::Table) => self.table(group, depth),
            // A row or a cell outside a table, which no reader writes.
            Some(Container::Row { .. } | Container::Cell) | None => self.run(group, inner),
        };
        (lines, None)
    }

    /// Writes a list whose items sit at `depth + 1`. A blank line separates
    /// the items only when one of them holds more than one block, because
    /// Markdown needs one there.
    fn list(&self, group: &[Block], depth: usize, marker: Marker) -> Vec<String> {
        let items: Vec<&[Block]> = groups(group, depth + 1).collect();
        let tight = items.iter().all(|item| item.len() == 1);
        let mut lines = Vec::new();
        for (index, item) in items.into_iter().enumerate() {
            if index > 0 && !tight {
                lines.push(String::new());
            }
            let in_item = matches!(self.container(&item[0], depth + 1), Some(Container::Item));
            let content = self.run(item, if in_item { depth + 2 } else { depth + 1 });
            lines.extend(item_lines(&marker.text(index), content));
        }
        lines
    }

    /// Writes a table as a GFM pipe table, whose rows sit at `depth + 1` and
    /// cells at `depth + 2`. A pipe table writes each row on one line, so the
    /// blocks of a cell are written on one line, separated by a space. GFM
    /// requires a header line, so a table whose first row is no header row
    /// gets an empty one.
    fn table(&self, group: &[Block], depth: usize) -> Vec<String> {
        let header = matches!(
            self.container(&group[0], depth + 1),
            Some(Container::Row { header: true })
        );
        let mut rows: Vec<Vec<String>> = groups(group, depth + 1)
            .map(|row| groups(row, depth + 2).map(|cell| self.cell(cell)).collect())
            .collect();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        if !header {
            rows.insert(0, Vec::new());
        }
        let mut lines = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            lines.push(row_line(row, columns));
            if index == 0 {
                lines.push(row_line(&vec!["---".to_string(); columns], columns));
            }
        }
        lines
    }

    /// The content of a table cell, on one line: the inline text of its
    /// blocks separated by a space, a code block as a code span, and a rule
    /// as nothing.
    fn cell(&self, blocks: &[Block]) -> String {
        let parts: Vec<String> = blocks
            .iter()
            .map(|block| match &block.kind {
                BlockKind::Code { .. } if block.text.trim().is_empty() => String::new(),
                BlockKind::Code { .. } => code_span(&block.text.replace('\n', " "), true),
                BlockKind::Rule => String::new(),
                _ => self
                    .inline(&block.text, &block.spans, Context::Cell)
                    .concat(),
            })
            .filter(|part| !part.is_empty())
            .collect();
        parts.join(" ")
    }

    /// The container at `depth` of the block's `within`, if there is one.
    fn container(&self, block: &Block, depth: usize) -> Option<&Container> {
        let id = *block.within.get(depth)?;
        self.doc.containers.get(id as usize)
    }

    fn block(&self, block: &Block) -> Vec<String> {
        match &block.kind {
            BlockKind::Heading { level } => self.heading(block, *level),
            // The PDF and EPUB readers write `Note` and `ListItem` blocks
            // without containers around them.
            BlockKind::Paragraph | BlockKind::Note { .. } => {
                self.inline(&block.text, &block.spans, Context::Paragraph)
            }
            BlockKind::ListItem => item_lines(
                "-",
                self.inline(&block.text, &block.spans, Context::Paragraph),
            ),
            BlockKind::Code { language } => code_block(&block.text, language.as_deref()),
            BlockKind::Rule => vec!["---".to_string()],
        }
    }

    /// A heading as `#` marks and its text on one line. Only a setext
    /// heading, of level 1 or 2, can hold a line break, so a heading of
    /// those levels that holds one is written as its lines underlined by
    /// `===` or `---`, and a deeper heading writes its breaks as spaces.
    fn heading(&self, block: &Block, level: u8) -> Vec<String> {
        let level = level.clamp(1, 6);
        let breaks = block
            .spans
            .iter()
            .any(|span| matches!(span.kind, SpanKind::Break { .. }));
        if breaks && level <= 2 {
            let mut lines = self.inline(&block.text, &block.spans, Context::Paragraph);
            lines.push(if level == 1 { "===" } else { "---" }.to_string());
            return lines;
        }
        let content = self.inline(&block.text, &block.spans, Context::Heading);
        vec![atx_heading(level, &content.concat())]
    }

    /// `text` with `spans` marked and its characters escaped, as lines. Only
    /// a `Break` in a paragraph starts a new line.
    fn inline(&self, text: &str, spans: &[Span], context: Context) -> Vec<String> {
        let mut spans: Vec<(&Span, Place)> = spans
            .iter()
            .map(|span| (span, place(text, spans, span)))
            .collect();
        // A point reads back the same on either side of a delimiter at its
        // place, so it goes before the spans that open there unless it goes
        // after them. It always goes before a break there, which ends the
        // line at the point.
        spans.sort_by_key(|(span, place)| {
            let order = match (place, &span.kind) {
                (_, SpanKind::Break { .. }) => 3,
                (Place::AfterOpeners, _) => 2,
                _ if span.start == span.end => 0,
                _ => 1,
            };
            (span.start, order)
        });
        let mut out = Inline::new(context);
        let mut open: Vec<Open> = Vec::new();
        // The bytes of `text` written so far.
        let mut at = 0;
        for (span, place) in spans {
            let start = byte_offset(text, span.start);
            let end = byte_offset(text, span.end);
            // Inside a code span, or the space a break stands for.
            if start < at {
                continue;
            }
            let closes = |top: &mut Open| {
                top.end < start || (top.end == start && place != Place::BeforeClosers)
            };
            while let Some(top) = open.pop_if(closes) {
                out.text_to(text, &mut at, top.end);
                out.close(top);
            }
            out.text_to(text, &mut at, start);
            // What opens here stands after the closers, as punctuation.
            out.space_after_closers = false;
            // What the span closes with, if it stays open until its end.
            let close = match &span.kind {
                SpanKind::NoteRef { marker, block } => {
                    out.note(self.reference_label(marker, *block));
                    out.space_after_closers = place == Place::BeforeClosers;
                    None
                }
                SpanKind::Code => {
                    out.code(&text[start..end]);
                    at = end;
                    None
                }
                SpanKind::Break { lines } if context == Context::Paragraph => {
                    out.hard_break(*lines);
                    at = end;
                    None
                }
                SpanKind::Emphasis | SpanKind::Strong => {
                    let around = open.iter().filter_map(|open| open.close.mark()).collect();
                    let strong = span.kind == SpanKind::Strong;
                    Some(Close::Mark(out.open_mark(strong, around)))
                }
                SpanKind::Strikethrough => {
                    out.tildes();
                    Some(Close::Tildes)
                }
                SpanKind::Link { href, .. } => {
                    out.open_link();
                    let destination = destination(href, context == Context::Cell);
                    Some(Close::Link(format!("]({destination})")))
                }
                // A break in a heading or a cell stays the space it covers. A
                // page break has no Markdown, and a formula's text is written
                // as text.
                SpanKind::Break { .. } | SpanKind::PageBreak { .. } | SpanKind::Math { .. } => None,
            };
            if let Some(close) = close {
                open.push(Open { end, close });
            }
        }
        while let Some(top) = open.pop() {
            out.text_to(text, &mut at, top.end);
            out.close(top);
        }
        out.text_to(text, &mut at, text.len());
        out.finish()
    }
}

/// Where a span is written among the delimiters at its start: a span with
/// text, and most points, as they come; a point inside the spans that open or
/// close where it stands, when they need it there.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    AsTheyCome,
    AfterOpeners,
    BeforeClosers,
}

/// Where the span `span` of `text`, among `spans`, is written. A delimiter
/// of emphasis, strong or strikethrough opens only before something other
/// than whitespace and closes only after it, so a point goes after those that
/// open where it stands before a space, and before those that close where it
/// stands after one. Not where a link or a code span opens or closes there,
/// since a note reference in a link's text undoes the link, and a code span
/// holds no point at all. Anywhere else it goes outside them, where `[^1]*a*`
/// after a letter keeps the emphasis that `*[^1]a*` loses.
fn place(text: &str, spans: &[Span], span: &Span) -> Place {
    let at = span.start;
    if span.end > at {
        return Place::AsTheyCome;
    }
    let flanked = |other: &Span| {
        other.start < other.end
            && matches!(
                other.kind,
                SpanKind::Emphasis | SpanKind::Strong | SpanKind::Strikethrough
            )
    };
    let bracketed = |other: &Span| matches!(other.kind, SpanKind::Link { .. } | SpanKind::Code);
    // Whether a span that `edge` picks stands there, and none in brackets.
    let inside = |edge: fn(&Span) -> usize| {
        let here: Vec<&Span> = spans.iter().filter(|other| edge(other) == at).collect();
        here.iter().any(|other| flanked(other)) && !here.iter().any(|other| bracketed(other))
    };
    let space = |position: usize| text.chars().nth(position).is_some_and(char::is_whitespace);
    if space(at) && inside(|other| other.start) {
        Place::AfterOpeners
    } else if at.checked_sub(1).is_some_and(space) && inside(|other| other.end) {
        Place::BeforeClosers
    } else {
        Place::AsTheyCome
    }
}

/// The innermost note that holds `block`.
fn note_of(doc: &Document, block: u32) -> Option<u32> {
    let block = doc.blocks.get(block as usize)?;
    block.within.iter().rev().copied().find(|&id| {
        matches!(
            doc.containers.get(id as usize),
            Some(Container::Note { .. })
        )
    })
}

/// `blocks` cut into elements at `depth`: each run of blocks in the same
/// container there, and each block in none.
fn groups(blocks: &[Block], depth: usize) -> impl Iterator<Item = &[Block]> {
    blocks.chunk_by(
        move |a, b| match (a.within.get(depth), b.within.get(depth)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    )
}

/// Adds `element` to `lines`, after a blank line when it is not the first.
fn push_element(lines: &mut Vec<String>, element: Vec<String>) {
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(element);
}

/// How a list marks its items: `-` or `*` for a bullet list, and the number
/// followed by `.` or `)` for an ordered one. Markdown reads two lists of the
/// same kind side by side as one list, unless their markers differ.
#[derive(Clone, Copy)]
struct Marker {
    ordered: bool,
    start: u64,
    character: char,
}

impl Marker {
    /// The marker of a list that comes right after the list marked
    /// `previous`, if one does.
    fn after(previous: Option<Marker>, ordered: bool, start: Option<u64>) -> Marker {
        let (first, second) = if ordered { ('.', ')') } else { ('-', '*') };
        let character = match previous {
            Some(previous) if previous.ordered == ordered && previous.character == first => second,
            _ => first,
        };
        Marker {
            ordered,
            start: start.unwrap_or(1),
            character,
        }
    }

    /// The marker of the item at `index`, counted from 0.
    fn text(&self, index: usize) -> String {
        match self.ordered {
            true => format!(
                "{}{}",
                self.start.saturating_add(index as u64),
                self.character
            ),
            false => self.character.to_string(),
        }
    }
}

/// The lines of a list item: `marker` before its first line, and its other
/// lines indented to where the first line's content starts.
fn item_lines(marker: &str, content: Vec<String>) -> Vec<String> {
    let indent = " ".repeat(marker.len() + 1);
    content
        .into_iter()
        .enumerate()
        .map(|(index, line)| match (index, line.as_str()) {
            (0, "") => marker.to_string(),
            // `- ---` reads as one rule across the line, not as an item that
            // holds a rule, and no list marker is made of underscores.
            (0, "---") => format!("{marker} ___"),
            (0, _) => format!("{marker} {line}"),
            (_, "") => line,
            _ => format!("{indent}{line}"),
        })
        .collect()
}

/// The lines of a footnote definition: `[^label]:` before its first line,
/// and its other lines indented by four spaces.
fn note_lines(label: &str, content: Vec<String>) -> Vec<String> {
    content
        .into_iter()
        .enumerate()
        .map(|(index, line)| match (index, line.is_empty()) {
            (0, true) => format!("[^{label}]:"),
            (0, false) => format!("[^{label}]: {line}"),
            (_, true) => line,
            (_, false) => format!("    {line}"),
        })
        .collect()
}

/// A table row of `columns` cells, the missing ones empty.
fn row_line(cells: &[String], columns: usize) -> String {
    let mut line = String::from("|");
    for column in 0..columns {
        line.push(' ');
        line.push_str(cells.get(column).map_or("", String::as_str));
        line.push_str(" |");
    }
    line
}

/// `level` `#` marks and `content`. A run of `#` that ends the content, after
/// a space or alone, would close the heading, so its first `#` is escaped.
fn atx_heading(level: u8, content: &str) -> String {
    let marks = "#".repeat(usize::from(level));
    if content.is_empty() {
        return marks;
    }
    let before = content.trim_end_matches('#');
    if before.len() < content.len() && (before.is_empty() || before.ends_with(' ')) {
        return format!("{marks} {before}\\{}", &content[before.len()..]);
    }
    format!("{marks} {content}")
}

/// A fenced code block. Its fence is longer than the longest run of the
/// fence's character in the code, so that no line of the code closes it. A
/// backtick fence's info string cannot hold a backtick, so a language that
/// holds one gets a fence of tildes.
fn code_block(text: &str, language: Option<&str>) -> Vec<String> {
    let language = language.unwrap_or_default();
    let character = if language.contains('`') { '~' } else { '`' };
    let fence = character
        .to_string()
        .repeat((longest_run(text, character) + 1).max(3));
    let mut lines = vec![format!("{fence}{}", escape_info(language))];
    lines.extend(text.split('\n').map(str::to_string));
    lines.push(fence);
    lines
}

/// `language` as an info string, in which CommonMark reads backslash escapes
/// and entities.
fn escape_info(language: &str) -> String {
    let mut info = String::with_capacity(language.len());
    for (at, c) in language.char_indices() {
        let rest = &language[at + c.len_utf8()..];
        if (c == '\\' && escapes_next(rest)) || (c == '&' && starts_entity(rest)) {
            info.push('\\');
        }
        info.push(c);
    }
    info
}

/// `content` as a code span: in one more backtick than its longest run of
/// them, with a space inside each end when a backtick at an end would join
/// the fence, or when the content starts and ends with a space, one of which
/// CommonMark strips from each end. In a table cell `|` is escaped, even
/// there.
fn code_span(content: &str, cell: bool) -> String {
    let fence = "`".repeat(longest_run(content, '`') + 1);
    let pad = content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ') && content.ends_with(' ') && content.contains(|c| c != ' '));
    let space = if pad { " " } else { "" };
    let content = match cell {
        true => content.replace('|', "\\|"),
        false => content.to_string(),
    };
    format!("{fence}{space}{content}{space}{fence}")
}

/// `href` as a link destination. One that holds a space or a control
/// character goes in angle brackets. Any other goes bare, with its
/// parentheses escaped unless they pair up.
fn destination(href: &str, cell: bool) -> String {
    let pointy = href.contains(|c: char| c == ' ' || c.is_ascii_control());
    let paired = parentheses_pair_up(href);
    let mut destination = String::with_capacity(href.len() + 2);
    if pointy {
        destination.push('<');
    }
    for (at, c) in href.char_indices() {
        let rest = &href[at + c.len_utf8()..];
        let escape = match c {
            '\\' => escapes_next(rest),
            '&' => starts_entity(rest),
            '<' => pointy || at == 0,
            '>' => pointy,
            '(' | ')' => !pointy && !paired,
            '|' => cell,
            _ => false,
        };
        if escape {
            destination.push('\\');
        }
        destination.push(c);
    }
    if pointy {
        destination.push('>');
    }
    destination
}

/// Whether every `(` of `text` is closed by a later `)` and every `)` closes
/// one.
fn parentheses_pair_up(text: &str) -> bool {
    let mut depth = 0_usize;
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => match depth.checked_sub(1) {
                Some(less) => depth = less,
                None => return false,
            },
            _ => {}
        }
    }
    depth == 0
}

/// The length of the longest run of `character` in `text`.
fn longest_run(text: &str, character: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == character { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// Whether a backslash written before `rest` escapes something: it escapes
/// ASCII punctuation, and at the end of a piece of text whatever is written
/// next may be punctuation.
fn escapes_next(rest: &str) -> bool {
    rest.chars().next().is_none_or(|c| c.is_ascii_punctuation())
}

/// Whether `rest`, the text after a `&`, starts like an entity or a numeric
/// character reference, which CommonMark reads as the character it names.
fn starts_entity(rest: &str) -> bool {
    let Some((name, _)) = rest.split_once(';') else {
        return false;
    };
    match name.strip_prefix('#') {
        Some(number) => match number.strip_prefix(['x', 'X']) {
            Some(hex) => !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()),
        },
        None => !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()),
    }
}

/// Where inline content is written, which decides how a line break and a `|`
/// are written and whether a line's start is escaped.
#[derive(Clone, Copy, PartialEq)]
enum Context {
    /// A paragraph, or a heading over several lines: a `Break` ends the line,
    /// and what would start a block at the start of a line is escaped.
    Paragraph,
    /// A heading on one line: a `Break` stays the space it covers.
    Heading,
    /// A table cell: a `Break` stays the space it covers, and `|` is escaped.
    Cell,
}

/// A span whose start is written, with what closes it.
struct Open {
    /// Where it ends in the text, in bytes.
    end: usize,
    close: Close,
}

/// What closes a span.
enum Close {
    /// The emphasis or strong span with this index in [`Inline::marks`].
    Mark(usize),
    Tildes,
    /// A link's `](destination)`.
    Link(String),
}

impl Close {
    fn mark(&self) -> Option<usize> {
        match self {
            Close::Mark(mark) => Some(*mark),
            _ => None,
        }
    }
}

/// A piece of a line of inline content: text as it is written, or a
/// delimiter of emphasis or strong, whose character is chosen once the whole
/// content is written and what stands beside each delimiter is known.
enum Piece {
    Text(String),
    Mark { mark: usize, opens: bool },
}

/// An emphasis or strong span of the content being written.
struct Mark {
    strong: bool,
    /// The emphasis and strong spans around it, as indexes into
    /// [`Inline::marks`].
    around: Vec<usize>,
    /// `*` or `_`, once chosen.
    character: Option<char>,
}

impl Mark {
    /// The length of its delimiter runs.
    fn length(&self) -> usize {
        if self.strong { 2 } else { 1 }
    }
}

/// What stands beside a delimiter, in the classes that decide whether
/// pulldown-cmark lets it open or close: whitespace, which the start and the
/// end of a line count as, punctuation, which is Unicode's punctuation and
/// symbols, anything else, or another delimiter of emphasis or strong.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Space,
    Punctuation,
    Other,
    Mark(usize),
}

impl Side {
    fn of(c: Option<char>) -> Side {
        match c {
            None => Side::Space,
            Some(c) if c.is_whitespace() => Side::Space,
            Some(c) if c.is_ascii_punctuation() => Side::Punctuation,
            Some(c)
                if matches!(
                    c.general_category_group(),
                    GeneralCategoryGroup::Punctuation | GeneralCategoryGroup::Symbol
                ) =>
            {
                Side::Punctuation
            }
            Some(_) => Side::Other,
        }
    }

    /// The class of a delimiter, which is punctuation.
    fn class(self) -> Side {
        match self {
            Side::Mark(_) => Side::Punctuation,
            side => side,
        }
    }
}

/// Whether a run of `character` between `before` and `after` can open
/// emphasis, as pulldown-cmark decides: `*` before anything but whitespace
/// and punctuation, and either character before anything but whitespace when
/// it follows whitespace or punctuation.
fn can_open(character: char, before: Side, after: Side) -> bool {
    match (before.class(), after.class()) {
        (_, Side::Space) => false,
        (_, Side::Other) if character == '*' => true,
        (before, _) => before != Side::Other,
    }
}

/// Whether a run of `character` between `before` and `after` can close
/// emphasis, the mirror of [`can_open`].
fn can_close(character: char, before: Side, after: Side) -> bool {
    match (before.class(), after.class()) {
        (Side::Space, _) => false,
        (Side::Other, _) if character == '*' => true,
        (_, after) => after != Side::Other,
    }
}

/// Inline content as it is written, line by line.
struct Inline {
    context: Context,
    lines: Vec<Vec<Piece>>,
    line: Vec<Piece>,
    /// The emphasis and strong spans, in the order they open.
    marks: Vec<Mark>,
    /// How many links the text being written is in. A `]` there would end
    /// the link's text.
    links: usize,
    /// Whether the last thing written is a note reference, which a `(` right
    /// after it turns into a link and a `[` undoes.
    after_note: bool,
    /// Whether the last thing written is text that ends in `!`, which a `[`
    /// right after it turns into an image.
    after_bang: bool,
    /// The backtick or tilde of the code span or strikethrough delimiter
    /// written last, whose run another one right after it would join.
    after_run: Option<char>,
    /// Whether the text written next follows closers that a note reference
    /// before them separates from the space their spans end with. The text
    /// then gets a space before it, which normalization joins to that space,
    /// so that the closers stand before whitespace, where they can close.
    space_after_closers: bool,
}

impl Inline {
    fn new(context: Context) -> Inline {
        Inline {
            context,
            lines: Vec::new(),
            line: Vec::new(),
            marks: Vec::new(),
            links: 0,
            after_note: false,
            after_bang: false,
            after_run: None,
            space_after_closers: false,
        }
    }

    /// Adds `text`, as it is to be written, to the line.
    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.line.last_mut() {
            Some(Piece::Text(last)) => last.push_str(text),
            _ => self.line.push(Piece::Text(text.to_string())),
        }
    }

    /// Forgets what was written last, before something else is written.
    fn forget_last(&mut self) {
        self.after_note = false;
        self.after_bang = false;
        self.after_run = None;
    }

    /// Writes the text from `at` to `to`, if any, and moves `at` there.
    fn text_to(&mut self, text: &str, at: &mut usize, to: usize) {
        if to > *at {
            self.text(&text[*at..to]);
            *at = to;
        }
    }

    /// Writes `text`, escaping each character that could start syntax where
    /// it stands. What lies outside `text` is not known here, so a character
    /// at either end is escaped whenever something written next to it could
    /// make it syntax.
    fn text(&mut self, text: &str) {
        let mut written = String::with_capacity(text.len() + 8);
        if std::mem::take(&mut self.space_after_closers) {
            written.push(' ');
        }
        if self.after_note {
            if text.starts_with('(') {
                written.push('\\');
            } else if text.starts_with('[') {
                // `[^1]\[` loses the reference, and an empty comment, which
                // the reader drops, keeps it.
                written.push_str("<!---->");
            }
        }
        for (at, c) in text.char_indices() {
            let rest = &text[at + c.len_utf8()..];
            if self.escapes(c, &text[..at], rest) {
                written.push('\\');
            }
            written.push(c);
        }
        self.push(&written);
        self.forget_last();
        self.after_bang = text.ends_with('!');
    }

    /// Whether `c`, between `before` and `rest`, is escaped.
    fn escapes(&self, c: char, before: &str, rest: &str) -> bool {
        // The characters around the run of `c` that holds this one.
        let around = || {
            (
                before.trim_end_matches(c).chars().next_back(),
                rest.trim_start_matches(c).chars().next(),
            )
        };
        match c {
            '\\' => escapes_next(rest),
            '`' | '[' => true,
            ']' => self.links > 0,
            // A run of `*` or `~` with a space on each side can neither open
            // nor close.
            '*' | '~' => around() != (Some(' '), Some(' ')),
            // Nor can a run of `_` between two spaces, or between two letters
            // or digits, as in `snake_case`.
            '_' => {
                !matches!(around(), (Some(' '), Some(' ')))
                    && !matches!(
                        around(),
                        (Some(b), Some(a)) if b.is_alphanumeric() && a.is_alphanumeric()
                    )
            }
            // An HTML tag or an autolink starts with `<` and a character other
            // than a space.
            '<' => !rest.starts_with(' '),
            '&' => starts_entity(rest),
            '|' => self.context == Context::Cell,
            _ => false,
        }
    }

    /// Writes the opening delimiter of an emphasis or strong span inside the
    /// spans `around`, and returns its index in `marks`.
    fn open_mark(&mut self, strong: bool, around: Vec<usize>) -> usize {
        let mark = self.marks.len();
        self.marks.push(Mark {
            strong,
            around,
            character: None,
        });
        self.line.push(Piece::Mark { mark, opens: true });
        self.forget_last();
        mark
    }

    /// Writes `~~`, which a run of tildes right before it would join.
    fn tildes(&mut self) {
        if self.after_run == Some('~') {
            self.push("<!---->");
        }
        self.push("~~");
        self.forget_last();
        self.after_run = Some('~');
    }

    /// Escapes a `!` that ends the line, before a `[` is written after it.
    fn escape_bang(&mut self) {
        if self.after_bang
            && let Some(Piece::Text(last)) = self.line.last_mut()
        {
            last.insert(last.len() - 1, '\\');
        }
    }

    fn open_link(&mut self) {
        if self.after_note {
            self.push("<!---->");
        }
        self.escape_bang();
        self.push("[");
        self.links += 1;
        self.forget_last();
    }

    fn close(&mut self, open: Open) {
        match open.close {
            Close::Mark(mark) => {
                self.line.push(Piece::Mark { mark, opens: false });
                self.forget_last();
            }
            Close::Tildes => self.tildes(),
            Close::Link(close) => {
                self.links -= 1;
                self.push(&close);
                self.forget_last();
            }
        }
    }

    fn note(&mut self, label: &str) {
        self.escape_bang();
        let label = match self.context {
            Context::Cell => label.replace('|', "\\|"),
            _ => label.to_string(),
        };
        self.push(&format!("[^{label}]"));
        self.forget_last();
        self.after_note = true;
    }

    fn code(&mut self, content: &str) {
        if self.after_run == Some('`') {
            self.push("<!---->");
        }
        self.push(&code_span(content, self.context == Context::Cell));
        self.forget_last();
        self.after_run = Some('`');
    }

    /// Ends the line with `\` for each of `lines` line breaks.
    fn hard_break(&mut self, lines: u32) {
        for _ in 0..lines.max(1) {
            self.push("\\");
            self.lines.push(std::mem::take(&mut self.line));
        }
        self.forget_last();
    }

    fn finish(mut self) -> Vec<String> {
        self.lines.push(std::mem::take(&mut self.line));
        self.choose_characters();
        let mut lines: Vec<String> = self
            .lines
            .iter()
            .map(|pieces| {
                pieces
                    .iter()
                    .map(|piece| match piece {
                        Piece::Text(text) => text.clone(),
                        Piece::Mark { mark, .. } => {
                            let mark = &self.marks[*mark];
                            let character = mark.character.unwrap_or('*');
                            character.to_string().repeat(mark.length())
                        }
                    })
                    .collect()
            })
            .collect();
        if self.context == Context::Paragraph {
            for line in &mut lines {
                if let Some(at) = line_start_escape(line) {
                    line.insert(at, '\\');
                }
            }
        }
        lines
    }

    /// Chooses `*` or `_` for each emphasis and strong span, so that
    /// pulldown-cmark pairs every delimiter as written. The spans are taken
    /// in the order they open, the spans around one before it, and a span
    /// that fits neither character sends the search back to try the other
    /// character for an earlier one. A search that runs past
    /// [`CHOICE_BUDGET`] tries gives way to taking, span by span, the first
    /// character that fits, or `*`.
    fn choose_characters(&mut self) {
        // Where each span's delimiters stand, as line and piece.
        let mut places = vec![[(0, 0); 2]; self.marks.len()];
        for (line, pieces) in self.lines.iter().enumerate() {
            for (index, piece) in pieces.iter().enumerate() {
                if let Piece::Mark { mark, opens } = piece {
                    places[*mark][usize::from(!opens)] = (line, index);
                }
            }
        }
        let mut budget = CHOICE_BUDGET;
        if self.choose_from(0, &places, &mut budget) {
            return;
        }
        for mark in &mut self.marks {
            mark.character = None;
        }
        for mark in 0..self.marks.len() {
            let character = ['*', '_']
                .into_iter()
                .find(|&character| self.fits(&places, mark, character))
                .unwrap_or('*');
            self.marks[mark].character = Some(character);
        }
    }

    /// Chooses characters for the spans from `mark` on, given those before
    /// it, and says whether every one of them fits.
    fn choose_from(
        &mut self,
        mark: usize,
        places: &[[(usize, usize); 2]],
        budget: &mut usize,
    ) -> bool {
        if mark == self.marks.len() {
            // Every span has its character now, so each is checked again with
            // its runs complete.
            return (0..self.marks.len()).all(|mark| {
                *budget = budget.saturating_sub(1);
                let character = self.marks[mark].character.unwrap_or('*');
                self.fits(places, mark, character)
            });
        }
        for character in ['*', '_'] {
            let Some(left) = budget.checked_sub(1) else {
                return false;
            };
            *budget = left;
            if self.fits(places, mark, character) {
                self.marks[mark].character = Some(character);
                if self.choose_from(mark + 1, places, budget) {
                    return true;
                }
            }
        }
        self.marks[mark].character = None;
        false
    }

    fn fits(&self, places: &[[(usize, usize); 2]], mark: usize, character: char) -> bool {
        Delimiters {
            lines: &self.lines,
            marks: &self.marks,
            places,
            mark,
            character,
        }
        .fit()
    }
}

/// How many characters the search of [`Inline::choose_characters`] tries
/// for one block before it takes the first that fits, span by span. A
/// block's spans almost always fit at the first try, and the cap keeps a
/// block of many spans that cannot all fit from taking time that doubles
/// with each span.
const CHOICE_BUDGET: usize = 10_000;

/// The delimiters of the inline content being written, with `character`
/// tried for the span `mark` and the characters chosen for the spans before
/// it. It answers whether pulldown-cmark pairs the delimiters of `mark` as
/// written.
struct Delimiters<'a> {
    lines: &'a [Vec<Piece>],
    marks: &'a [Mark],
    places: &'a [[(usize, usize); 2]],
    mark: usize,
    character: char,
}

/// A run of delimiters of one character, which pulldown-cmark reads as one.
struct Run {
    /// The spans whose delimiters it holds, in the order they stand.
    members: Vec<usize>,
    /// How many characters it holds.
    length: usize,
    can_open: bool,
    can_close: bool,
    /// Whether a delimiter of a span that has no character yet stands beside
    /// it, which may join it and change what it can do.
    open_ended: bool,
}

impl Run {
    /// Whether it can both open and close, which subjects it to the rule of
    /// three.
    fn both(&self) -> bool {
        self.can_open && self.can_close
    }
}

impl Delimiters<'_> {
    fn character(&self, mark: usize) -> Option<char> {
        match mark == self.mark {
            true => Some(self.character),
            false => self.marks[mark].character,
        }
    }

    /// The run that holds the opening or the closing delimiter of `mark`, or
    /// `None` when an opener and a closer of one character stand side by
    /// side, which neither pair as written.
    fn run(&self, mark: usize, opens: bool) -> Option<Run> {
        let character = self.character(mark)?;
        let (line, index) = self.places[mark][usize::from(!opens)];
        let pieces = &self.lines[line];
        // Whether the piece at `index` holds a delimiter of `character`, and
        // whether that delimiter opens.
        let delimiter = |index: usize| match pieces.get(index) {
            Some(Piece::Mark { mark, opens }) if self.character(*mark) == Some(character) => {
                Some((*mark, *opens))
            }
            _ => None,
        };
        let (mut first, mut last) = (index, index);
        while let Some(before) = first.checked_sub(1)
            && let Some((_, side)) = delimiter(before)
        {
            if side != opens {
                return None;
            }
            first = before;
        }
        while let Some((_, side)) = delimiter(last + 1) {
            if side != opens {
                return None;
            }
            last += 1;
        }
        let members: Vec<usize> = (first..=last)
            .filter_map(|index| delimiter(index).map(|(mark, _)| mark))
            .collect();
        let before = first
            .checked_sub(1)
            .map_or(Side::Space, |i| side(&pieces[i], false));
        let after = pieces
            .get(last + 1)
            .map_or(Side::Space, |piece| side(piece, true));
        let undecided = |side: Side| match side {
            Side::Mark(mark) => self.character(mark).is_none(),
            _ => false,
        };
        Some(Run {
            length: members.iter().map(|&mark| self.marks[mark].length()).sum(),
            members,
            can_open: can_open(character, before, after),
            can_close: can_close(character, before, after),
            open_ended: undecided(before) || undecided(after),
        })
    }

    /// Whether the delimiters of `mark` pair as written with the character
    /// tried for it.
    ///
    /// Its opener must be able to open and its closer to close. A run that
    /// holds the delimiters of several spans splits from the inside out, two
    /// characters for strong and then one for emphasis, so over the same text
    /// only a strong span can join the span around it. An opener that could
    /// also close must not close a span around it. And under the rule of
    /// three, which counts the length of whole runs, each run must still pair
    /// with the runs it closes or opens for.
    fn fit(&self) -> bool {
        let mark = self.mark;
        let (Some(opener), Some(closer)) = (self.run(mark, true), self.run(mark, false)) else {
            return false;
        };
        // A run that may still grow is checked once every span has its
        // character.
        if opener.open_ended || closer.open_ended {
            return true;
        }
        if !opener.can_open || !closer.can_close {
            return false;
        }
        let inside_another =
            opener.members.first() != Some(&mark) && closer.members.last() != Some(&mark);
        if inside_another && !self.marks[mark].strong {
            return false;
        }
        if opener.can_close && self.closes_around(&opener) {
            return false;
        }
        let others_pair = opener
            .members
            .iter()
            .filter(|&&other| other != mark)
            .filter_map(|&other| self.run(other, false))
            .all(|their| their.open_ended || pairs(&opener, &their))
            && closer
                .members
                .iter()
                .filter(|&&other| other != mark)
                .filter_map(|&other| self.run(other, true))
                .all(|their| their.open_ended || pairs(&their, &closer));
        pairs(&opener, &closer) && others_pair
    }

    /// Whether the run `opener`, which holds the opener of `mark` and could
    /// also close, would close the opener of a span around the spans in it.
    fn closes_around(&self, opener: &Run) -> bool {
        self.marks[self.mark]
            .around
            .iter()
            .filter(|outer| !opener.members.contains(outer))
            .any(|&outer| {
                self.character(outer) == Some(self.character)
                    && self
                        .run(outer, true)
                        .is_some_and(|around| !around.open_ended && pairs(&around, opener))
            })
    }
}

/// Whether pulldown-cmark lets the run `closer` close the run `opener` of
/// the same character: always when neither can both open and close, and
/// otherwise only when their lengths do not add up to a multiple of three or
/// the closer's length is a multiple of three.
fn pairs(opener: &Run, closer: &Run) -> bool {
    (!closer.both() && !opener.both())
        || !(closer.length + opener.length).is_multiple_of(3)
        || closer.length.is_multiple_of(3)
}

/// What stands at the near end of `piece`: its last character when it comes
/// before a delimiter, and its first when it comes after one.
fn side(piece: &Piece, after: bool) -> Side {
    match piece {
        Piece::Mark { mark, .. } => Side::Mark(*mark),
        Piece::Text(text) if after => Side::of(text.chars().next()),
        Piece::Text(text) => Side::of(text.chars().next_back()),
    }
}

/// Where a `\` goes in a paragraph's line that would otherwise start a
/// block, or turn the line above into a heading or a table's header row: an
/// ATX heading's `#` marks, a quote's `>`, a list marker, a line of `-` or
/// `=`, a delimiter row, and note references followed by `:`, which would
/// define the note.
fn line_start_escape(line: &str) -> Option<usize> {
    let first = line.chars().next()?;
    let rest = &line[first.len_utf8()..];
    let ends_marker = |rest: &str| rest.is_empty() || rest.starts_with(' ');
    let only = |allowed: &[char]| line.chars().all(|c| allowed.contains(&c));
    match first {
        '#' => {
            let marks = line.len() - line.trim_start_matches('#').len();
            (marks <= 6 && ends_marker(&line[marks..])).then_some(0)
        }
        '>' => Some(0),
        '-' | '+' if ends_marker(rest) => Some(0),
        '-' if only(&['-', ' ']) => Some(0),
        '=' if only(&['=', ' ']) => Some(0),
        '|' | '-' | ':'
            if only(&['|', '-', ':', ' ']) && line.contains('|') && line.contains('-') =>
        {
            Some(0)
        }
        '0'..='9' => {
            let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let after = &line[digits..];
            (digits <= 9 && after.starts_with(['.', ')']) && ends_marker(&after[1..]))
                .then_some(digits)
        }
        '[' => {
            let mut rest = line;
            while let Some(reference) = rest.strip_prefix("[^") {
                rest = &reference[reference.find(']')? + 1..];
            }
            (rest.len() < line.len() && rest.starts_with(':')).then(|| line.len() - rest.len())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Container, Format, Metadata, SCHEMA_VERSION, Source};
    use crate::extract::markdown::markdown_document;

    fn block(kind: BlockKind, text: &str, page: u32) -> Block {
        Block {
            id: 0,
            kind,
            text: text.to_string(),
            spans: Vec::new(),
            ids: Vec::new(),
            within: Vec::new(),
            section: None,
            locator: Locator::Pdf {
                page,
                page_end: None,
                bbox: None,
            },
        }
    }

    fn heading(level: u8, text: &str, page: u32) -> Block {
        block(BlockKind::Heading { level }, text, page)
    }

    fn para(text: &str, page: u32) -> Block {
        block(BlockKind::Paragraph, text, page)
    }

    /// The Markdown of a document without a title that holds `blocks`.
    fn markdown(blocks: Vec<Block>) -> String {
        document_to_markdown(&document(None, Format::Pdf, blocks))
    }

    #[test]
    fn headings_use_hashes_by_level() {
        let md = markdown(vec![heading(2, "3.2 Entropy", 1)]);
        assert!(md.contains("## 3.2 Entropy"));
    }

    #[test]
    fn each_pdf_page_gets_a_comment_and_paragraphs_a_blank_line() {
        let md = markdown(vec![para("One.", 1), para("Two.", 1), para("Three.", 2)]);
        assert_eq!(
            md,
            "<!-- page 1 -->\n\nOne.\n\nTwo.\n\n<!-- page 2 -->\n\nThree.\n"
        );
    }

    fn epub_block(kind: BlockKind, text: &str) -> Block {
        Block {
            locator: Locator::Epub { file: 0 },
            ..block(kind, text, 1)
        }
    }

    fn document(title: Option<&str>, format: Format, blocks: Vec<Block>) -> Document {
        Document {
            schema_version: SCHEMA_VERSION,
            source: Source {
                sha256: "0".repeat(64),
                file_name: "book".to_string(),
                format,
            },
            metadata: Metadata {
                title: title.map(str::to_string),
                authors: Vec::new(),
                language: None,
                page_count: None,
                spine_count: None,
            },
            outline: Vec::new(),
            sections: Vec::new(),
            files: Vec::new(),
            page_labels: Vec::new(),
            containers: Vec::new(),
            blocks,
            furniture: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn epub_blocks_get_no_page_comments() {
        let md = markdown(vec![
            epub_block(BlockKind::Paragraph, "One."),
            epub_block(BlockKind::Paragraph, "Two."),
        ]);
        assert_eq!(md, "One.\n\nTwo.\n");
    }

    #[test]
    fn a_list_item_block_gets_a_marker_and_a_note_block_is_a_paragraph() {
        let md = markdown(vec![
            epub_block(BlockKind::ListItem, "First"),
            epub_block(
                BlockKind::Note {
                    label: Some("1".to_string()),
                },
                "See chapter 2.",
            ),
            epub_block(BlockKind::Note { label: None }, "Aside."),
        ]);
        assert_eq!(md, "- First\n\nSee chapter 2.\n\nAside.\n");
    }

    #[test]
    fn a_known_title_leads_the_document() {
        let doc = document(Some("Test Book"), Format::Pdf, vec![para("One.", 1)]);
        assert_eq!(
            document_to_markdown(&doc),
            "# Test Book\n\n<!-- page 1 -->\n\nOne.\n"
        );
    }

    #[test]
    fn an_unknown_title_writes_only_the_blocks() {
        let doc = document(
            None,
            Format::Epub,
            vec![epub_block(BlockKind::Paragraph, "One.")],
        );
        assert_eq!(document_to_markdown(&doc), "One.\n");
    }

    #[test]
    fn no_blocks_write_nothing() {
        assert_eq!(markdown(Vec::new()), "");
    }

    #[test]
    fn a_title_is_escaped_and_kept_on_one_line() {
        let doc = document(Some("# C#\nnotes"), Format::Epub, Vec::new());
        assert_eq!(document_to_markdown(&doc), "# # C# notes\n");
        let doc = document(Some("1. *x* #"), Format::Epub, Vec::new());
        assert_eq!(document_to_markdown(&doc), "# 1. \\*x\\* \\#\n");
    }

    fn source() -> Source {
        Source {
            sha256: "0".repeat(64),
            file_name: "notes.md".to_string(),
            format: Format::Markdown,
        }
    }

    fn markdown_block(kind: BlockKind, text: &str, within: Vec<u32>) -> Block {
        Block {
            within,
            locator: Locator::Markdown {
                line: 1,
                line_end: None,
            },
            ..block(kind, text, 1)
        }
    }

    fn doc_with_paragraph(text: &str) -> Document {
        Document::from_blocks(
            Format::Markdown,
            vec![markdown_block(BlockKind::Paragraph, text, Vec::new())],
        )
    }

    #[test]
    fn text_that_looks_like_markdown_is_escaped_and_reads_back_the_same() {
        for text in [
            "1. Introduction",
            "* * *",
            "# not a heading",
            "a *b* c",
            "[x](y)",
            "a | b",
            "&amp;",
            "<b>",
            "`tick`",
        ] {
            let d = doc_with_paragraph(text);
            assert_eq!(
                markdown_document(&document_to_markdown(&d), source())
                    .unwrap()
                    .blocks[0]
                    .text,
                text
            );
        }
    }

    #[test]
    fn a_table_without_a_header_gets_an_empty_header_line_and_a_cell_of_two_blocks_one_line() {
        let mut d = Document::from_blocks(
            Format::Markdown,
            vec![
                markdown_block(BlockKind::Paragraph, "A", vec![0, 1, 2]),
                markdown_block(BlockKind::Paragraph, "B", vec![0, 1, 2]),
                markdown_block(BlockKind::Paragraph, "C", vec![0, 1, 3]),
            ],
        );
        d.containers = vec![
            Container::Table,
            Container::Row { header: false },
            Container::Cell,
            Container::Cell,
        ];
        assert_eq!(
            document_to_markdown(&d),
            "|  |  |\n| --- | --- |\n| A B | C |\n"
        );
    }

    #[test]
    fn fences_outgrow_the_backticks_in_the_code() {
        let d = Document::from_blocks(
            Format::Markdown,
            vec![markdown_block(
                BlockKind::Code { language: None },
                "a\n```\nb",
                Vec::new(),
            )],
        );
        assert_eq!(document_to_markdown(&d), "````\na\n```\nb\n````\n");
    }

    /// `md` read, then written again.
    fn rewritten(md: &str) -> String {
        document_to_markdown(&markdown_document(md, source()).unwrap())
    }

    #[test]
    fn prose_keeps_the_characters_that_start_no_syntax_where_they_stand() {
        for text in [
            "snake_case, 2 * 3 < 4 and 5 ~ 6",
            "Hi! (a) --option b",
            "C# costs 50% of 1.5 x.y",
            "a+b - c = d",
        ] {
            assert_eq!(
                document_to_markdown(&doc_with_paragraph(text)),
                format!("{text}\n")
            );
        }
    }

    #[test]
    fn containers_nest_by_their_prefixes_and_a_list_beside_a_list_takes_other_markers() {
        let md = "> 1. a\n>    - b\n>\n>    c\n\n- x\n- y\n\n* z\n\n3) n\n\n\
                  [^n]: Note.\n\n    More.\n\nText[^n].\n";
        assert_eq!(
            rewritten(md),
            "> 1. a\n>\n>    - b\n>\n>    c\n\n- x\n- y\n\n* z\n\n3. n\n\n\
             [^n]: Note.\n\n    More.\n\nText[^n].\n"
        );
    }

    #[test]
    fn a_break_ends_its_line_and_a_heading_that_holds_one_is_underlined() {
        let md = "a\\\n\\\nb\n\nH\\\n2\n---\n\n- *x\\\n  y*\n";
        assert_eq!(rewritten(md), md);
    }

    #[test]
    fn a_rule_that_starts_an_item_is_written_with_underscores() {
        assert_eq!(rewritten("T\n\n* ---\n* x\n"), "T\n\n- ___\n- x\n");
    }

    #[test]
    fn a_table_escapes_its_pipes_and_fills_its_rows() {
        assert_eq!(
            rewritten("| a \\| b | `c\\|d` |\n|:--|--:|\n| [e](f\\|g) |\n"),
            "| a \\| b | `c\\|d` |\n| --- | --- |\n| [e](f\\|g) |  |\n"
        );
    }

    #[test]
    fn a_note_reference_keeps_its_note_before_a_parenthesis_a_bracket_or_a_colon() {
        let md = "a[^1]\\(b) c[^1]<!---->\\[d] e[^1]<!---->[f](g)\\\n[^1]\\: h\n\n[^1]: N.\n";
        assert_eq!(rewritten(md), md);
    }

    #[test]
    fn link_destinations_and_code_spans_read_back_as_written() {
        let md = "[a](<b c>) [d](e\\(f) [g](h(i)) [j]() ``a`b`` `` `c` `` x` `y\n";
        assert_eq!(rewritten(md), md);
    }

    /// Whether `md` read, written and read again keeps its blocks and
    /// containers. When it does not, the Markdown written and both readings
    /// of the blocks are printed, so that a failing test shows them.
    fn reads_back_the_same(md: &str) -> bool {
        let d = markdown_document(md, source()).unwrap();
        let back = markdown_document(&document_to_markdown(&d), source()).unwrap();
        let kept = |d: &Document| -> Vec<_> {
            d.blocks
                .iter()
                .map(|b| {
                    (
                        b.kind.clone(),
                        b.text.clone(),
                        b.within.clone(),
                        b.spans.clone(),
                    )
                })
                .collect()
        };
        let same = kept(&d) == kept(&back) && d.containers == back.containers;
        if !same {
            eprintln!(
                "{:?}\n{:?}\n{:?}",
                document_to_markdown(&d),
                kept(&d),
                kept(&back)
            );
        }
        same
    }

    #[test]
    fn a_note_reference_beside_a_break_or_a_code_span_is_kept() {
        // The code span starts with a space, and the strong span with the
        // space the break stands for, each where a note reference stands.
        for md in ["a[^1]` b`\n\n[^1]: N.\n", "x**[^1]\\\nc**\n\n[^1]: N.\n"] {
            assert!(reads_back_the_same(md), "{md:?}");
        }
    }

    #[test]
    fn a_note_reference_goes_inside_emphasis_that_starts_or_ends_with_a_space() {
        // `__[^1] (d)__` and `_a [^1]_`: the delimiters stand beside the
        // references, since none opens before a space or closes after one.
        for md in [
            "e. __(d)_[^1] [x](y.md)_\n\n[^1]: N.\n",
            "_a [^1]_ b\n\n[^1]: N.\n",
        ] {
            assert!(reads_back_the_same(md), "{md:?}");
        }
    }

    #[test]
    fn emphasis_and_strong_nested_over_punctuation_take_the_delimiters_that_pair() {
        for md in [
            "a ***x*** b",
            "a ***“q”*** b",
            "a **_“q”_** b",
            "a __***😀***__ b",
            "a ***---* -->* !* b",
            "***😀 (d)* `x_y`*[^1] “q”[^1]*\n\n[^1]: N.\n",
            r"~~\\\\ *-|- _\&copy;_*~~",
        ] {
            assert!(reads_back_the_same(md), "{md:?}");
        }
    }
}
