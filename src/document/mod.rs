//! The `Document` model: one book as the readers in `extract` write it, and
//! as everything after them reads it without changing it. `extract --format
//! json` prints it, and `docs/document-model.md` describes every field.

pub mod markdown;
mod places;

pub use places::Places;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

/// The version of the `Document` model. A change to the model bumps it, so
/// that cached indexes are built again.
pub const SCHEMA_VERSION: u32 = 3;

/// A book as its reader extracted it: facts about the file, the table of
/// contents, and the text as a flat list of blocks in reading order. A fact
/// that holds for many blocks is stored once, here, and [`Places`] derives
/// what each block needs from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    /// The [`SCHEMA_VERSION`] the reader wrote the document with.
    pub schema_version: u32,
    /// Which file the document was read from.
    pub source: Source,
    /// The title, authors, language and size the file declares.
    pub metadata: Metadata,
    /// The table of contents, in reading order.
    pub outline: Vec<OutlineEntry>,
    /// The sections the headings open, one per heading block in reading
    /// order. A block's `section` is an index here.
    pub sections: Vec<Section>,
    /// The EPUB's content files in spine order; empty for other formats. A
    /// file the reader skipped with a warning is not among them.
    pub files: Vec<String>,
    /// The printed label of each physical page of a PDF, page 1 first; empty
    /// when it defines none.
    pub page_labels: Vec<Option<String>>,
    /// The lists, items, quotes, tables, rows, cells and notes the blocks sit
    /// in, in the order they open. A container's id is its index here, and a
    /// block's `within` names the containers around it. JSON leaves the list
    /// out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<Container>,
    /// The reading text, in reading order. A block's `id` is its index here.
    #[serde(deserialize_with = "numbered")]
    pub blocks: Vec<Block>,
    /// The lines the PDF reader took out of the reading text, such as running
    /// heads and page numbers, numbered on their own. Nothing after the
    /// readers reads them, and the EPUB reader leaves the list empty.
    #[serde(deserialize_with = "numbered")]
    pub furniture: Vec<Block>,
    /// The parts of the file the reader left out because it could not read
    /// them.
    pub warnings: Vec<Warning>,
}

impl Document {
    /// The printed label of the physical `page`, counted from 1, or `None`
    /// when the PDF defines no labels or has no such page.
    pub fn page_label(&self, page: u32) -> Option<&str> {
        let index = usize::try_from(page.checked_sub(1)?).ok()?;
        self.page_labels.get(index)?.as_deref()
    }
}

/// Reads a list of blocks and gives each its position as its id, which JSON
/// leaves out.
fn numbered<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Block>, D::Error> {
    let mut blocks = Vec::<Block>::deserialize(deserializer)?;
    for (id, block) in blocks.iter_mut().enumerate() {
        block.id = id as u32;
    }
    Ok(blocks)
}

/// The file a document was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    /// The SHA-256 of the file's bytes as 64 lowercase hex digits. It names
    /// the book's directory in the index cache.
    pub sha256: String,
    /// The last component of the file's path.
    pub file_name: String,
    /// The format, decided from the file's content.
    pub format: Format,
}

/// The three formats ebook-hero reads, written `"pdf"`, `"epub"` and
/// `"markdown"` in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// A PDF file.
    Pdf,
    /// An EPUB 2 or EPUB 3 file.
    Epub,
    /// A CommonMark file.
    Markdown,
}

/// What the file says about the book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    /// The title, or `None` when the file gives none.
    pub title: Option<String>,
    /// The authors in the order the file lists them. A PDF gives its author
    /// field as one entry.
    pub authors: Vec<String>,
    /// The language an EPUB declares; always `None` for a PDF.
    pub language: Option<String>,
    /// The number of pages of a PDF; `None` for an EPUB.
    pub page_count: Option<u32>,
    /// The number of entries in an EPUB's spine, its files in reading order;
    /// `None` for a PDF.
    pub spine_count: Option<u32>,
}

/// One entry of the table of contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OutlineEntry {
    /// The entry's position in the outline, from 0. `read` takes it as the
    /// outline entry to read.
    pub id: u32,
    /// The entry's text.
    pub title: String,
    /// Its depth in the table of contents, the top level being 1.
    pub level: u8,
    /// The block where the entry's text starts, or `None` when the reader
    /// could not place it.
    pub block_id: Option<u32>,
}

/// The part of the book a heading opens. It runs until the next heading of
/// its level or nearer the top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Section {
    /// The heading block that opens it.
    pub heading: u32,
    /// The section it lies in, always an earlier one, or `None` at the top.
    pub parent: Option<u32>,
}

/// One unit of reading text: a heading, a paragraph, a list item, a note, a
/// code block or a rule, inside the containers that `within` names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Block {
    /// Its position in `blocks`. JSON leaves it out; deserialising sets it.
    #[serde(skip)]
    pub id: u32,
    /// What kind of block it is.
    pub kind: BlockKind,
    /// The text, normalized to one line without markup. A code block keeps
    /// its lines and indentation, without the final newline.
    pub text: String,
    /// Marked ranges of `text`, such as emphasis, links and page breaks, well
    /// nested and sorted by `start`, longer first ([`span_errors`] checks
    /// that). JSON leaves the list out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    /// Ids of the elements that start in the block, and of empty elements
    /// since the block before. JSON leaves the list out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// The ids of the [`containers`](Document::containers) the block sits in,
    /// outermost first. JSON leaves the list out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub within: Vec<u32>,
    /// The section the block is in, as an index into `sections`; a heading
    /// is in the section it opens. `None`, which JSON leaves out, before the
    /// first heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<u32>,
    /// Where the block is in the file.
    pub locator: Locator,
}

/// The kind of a block, written as an object with a `type` field in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockKind {
    /// A heading. It goes into no passage and opens a section, which reaches
    /// the passages under it as their `section`.
    Heading {
        /// The heading's level, 1 being the top: an EPUB's `h1` to `h6`, or
        /// the depth of a PDF bookmark.
        level: u8,
    },
    /// A paragraph.
    Paragraph,
    /// An item of a list, from an EPUB.
    ListItem,
    /// A footnote or endnote.
    Note {
        /// The id of the note element in an EPUB, when it has one; always
        /// `None` in a PDF.
        label: Option<String>,
    },
    /// A code block. Its `text` keeps its lines and indentation as written,
    /// without the final newline.
    Code {
        /// The language the fence names, when it names one.
        language: Option<String>,
    },
    /// A thematic break, with empty text. It closes the passage being built
    /// but opens no section.
    Rule,
}

/// A marked range of a block's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// Where the range starts in the block's text, in code points.
    pub start: usize,
    /// Where the range ends in the block's text, in code points.
    pub end: usize,
    /// What the range is.
    pub kind: SpanKind,
}

/// What a span marks, written as an object with a `type` field in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpanKind {
    /// A formula. Reserved: no reader writes one.
    Math {
        /// The formula in LaTeX.
        latex: String,
    },
    /// The point where a page of the printed edition starts; `start` equals
    /// `end`.
    PageBreak {
        /// The page's label, such as `45` or `xii`.
        label: String,
    },
    /// Emphasised text.
    Emphasis,
    /// Strongly emphasised text.
    Strong,
    /// Struck-through text.
    Strikethrough,
    /// Inline code.
    Code,
    /// A link around the marked text.
    Link {
        /// The target as the file wrote it.
        href: String,
        /// Where the reader found that it leads.
        to: LinkTarget,
    },
    /// The space in `text` that stands for line breaks.
    Break {
        /// How many line breaks the space stands for; 2 leaves a blank line,
        /// as between two stanzas of a poem.
        lines: u32,
    },
    /// A footnote reference, as a point: `start` equals `end`.
    NoteRef {
        /// What the reference shows, such as `1`.
        marker: String,
        /// The note's first block, when the reader found the note.
        block: Option<u32>,
    },
}

/// Where a link leads, as the reader decided. The window acts on this and
/// never on the book's format. JSON names the variant in a `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkTarget {
    /// A block of this document.
    Block {
        /// The block's id.
        block: u32,
    },
    /// A file beside this one.
    File {
        /// The path relative to this file's directory.
        path: String,
    },
    /// An address with a scheme, such as `https:` or `mailto:`.
    Address,
    /// A target the reader could not find, or one outside what the model
    /// holds.
    Nowhere,
}

/// What a block sits in, stored once in [`Document::containers`]. A block names
/// its containers by id in `within`, outermost first. JSON names the variant
/// in a `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Container {
    /// A list.
    List {
        /// Whether the items are numbered.
        ordered: bool,
        /// The first number of an ordered list, kept because readers cite an
        /// item by its number.
        start: Option<u64>,
    },
    /// An item of a list.
    Item,
    /// A block quote.
    Quote,
    /// A table.
    Table,
    /// A row of a table.
    Row {
        /// Whether it is the table's header row.
        header: bool,
    },
    /// A cell of a row.
    Cell,
    /// A footnote or endnote.
    Note {
        /// The note's id in an EPUB or its label in Markdown; `None` in a PDF.
        label: Option<String>,
    },
}

/// Where a block is in its file, in the terms of the file's format. JSON
/// leaves the variant unnamed, since `source.format` gives it and the fields
/// tell the variants apart.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Locator {
    /// A place in a PDF.
    Pdf {
        /// The physical page where the block starts, counted from 1.
        page: u32,
        /// The page where the block ends, when that is a later page.
        page_end: Option<u32>,
        /// `[left, top, right, bottom]` of the block's lines on `page`, in
        /// points from the top-left corner of the page; `None` for a heading
        /// the reader inserted from a bookmark.
        bbox: Option<[f32; 4]>,
    },
    /// A place in an EPUB.
    Epub {
        /// The content file, as an index into `files`.
        file: u32,
    },
    /// A place in a Markdown file.
    Markdown {
        /// The line where the block starts, counted from 1.
        line: u32,
        /// The line where it ends, when that is a later line.
        line_end: Option<u32>,
    },
}

/// Every field a [`Locator`] may hold, so that one pass over the JSON object
/// reads it and the fields present decide the variant.
#[derive(Deserialize)]
struct LocatorFields {
    page: Option<u32>,
    page_end: Option<u32>,
    bbox: Option<[f32; 4]>,
    file: Option<u32>,
    line: Option<u32>,
    line_end: Option<u32>,
}

impl<'de> Deserialize<'de> for Locator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let LocatorFields {
            page,
            page_end,
            bbox,
            file,
            line,
            line_end,
        } = LocatorFields::deserialize(deserializer)?;
        let pdf_fields = page_end.is_some() || bbox.is_some();
        match (page, file, line) {
            (Some(page), None, None) if line_end.is_none() => Ok(Locator::Pdf {
                page,
                page_end,
                bbox,
            }),
            (None, Some(file), None) if !pdf_fields && line_end.is_none() => {
                Ok(Locator::Epub { file })
            }
            (None, None, Some(line)) if !pdf_fields => Ok(Locator::Markdown { line, line_end }),
            _ => Err(de::Error::custom(
                "a locator holds the fields of one format: `page`, for a PDF, `file`, for an EPUB, \
                 or `line`, for a Markdown file; \
                 extract the book again with this version of ebook-hero",
            )),
        }
    }
}

/// A part of the file that the reader left out while it read the rest. JSON
/// names the variant in a `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Warning {
    /// PDF pages that hold no text.
    PagesWithoutText {
        /// The physical pages, counted from 1.
        pages: Vec<u32>,
    },
    /// An EPUB content file that was skipped.
    UnreadableContent {
        /// The file's href, or its spine `idref` when the manifest lacks it.
        href: String,
        /// Why it could not be read.
        reason: String,
    },
    /// A part of the file that could not be read and is left out, such as
    /// `page 7`, `page labels` or `outline`.
    UnreadablePart {
        /// The part, such as `page 7`.
        part: String,
        /// Why it could not be read.
        reason: String,
    },
    /// A link into the document that leads nowhere.
    BrokenLink {
        /// The target as the file wrote it, such as `#setup`.
        href: String,
        /// The block that holds the link.
        block: u32,
    },
}

/// The byte offset of code point `code_point` in `text`; `text.len()` past the
/// end. A span counts code points and the library slices text by bytes, and
/// this is the one place that converts.
pub fn byte_offset(text: &str, code_point: usize) -> usize {
    text.char_indices()
        .nth(code_point)
        .map_or(text.len(), |(offset, _)| offset)
}

/// Why the block's spans break the rules of a `Document`, if they do: they
/// must be well nested, so that two spans either do not overlap or one holds
/// the other; sorted by `start`, longer first; end within the text's code
/// points; and empty only for a [`NoteRef`](SpanKind::NoteRef) or a
/// [`PageBreak`](SpanKind::PageBreak), which mark a point. A reader's tests
/// call it to check what the reader wrote.
pub fn span_errors(block: &Block) -> Vec<String> {
    let length = block.text.chars().count();
    let mut errors = Vec::new();
    // The spans that hold the current position, innermost last.
    let mut open: Vec<&Span> = Vec::new();
    let mut previous: Option<(usize, &Span)> = None;
    for (index, span) in block.spans.iter().enumerate() {
        let Span { start, end, .. } = *span;
        let range = format!("[{start}, {end})");
        if start > end {
            errors.push(format!("span {index} {range} starts after it ends"));
            continue;
        }
        if end > length {
            errors.push(format!(
                "span {index} {range} ends after the text's {length} code points"
            ));
        }
        let point = matches!(
            span.kind,
            SpanKind::NoteRef { .. } | SpanKind::PageBreak { .. }
        );
        if start == end && !point {
            errors.push(format!(
                "span {index} {range} is empty, and only a note reference or a page break may be"
            ));
        }
        if let Some((before_index, before)) = previous
            && (start < before.start || (start == before.start && end > before.end))
        {
            errors.push(format!(
                "span {index} {range} is out of order after span {before_index} [{}, {}); \
                 spans go by start, the longer first",
                before.start, before.end
            ));
        }
        previous = Some((index, span));
        while open.last().is_some_and(|top| top.end <= start) {
            open.pop();
        }
        if let Some(top) = open.last()
            && top.start < start
            && top.end < end
        {
            errors.push(format!(
                "span {index} {range} overlaps the span [{}, {}) without nesting in it",
                top.start, top.end
            ));
        }
        if start < end {
            open.push(span);
        }
    }
    errors
}

#[cfg(test)]
impl Document {
    /// A document of `format` that holds `blocks` and nothing else, numbered
    /// and given sections from their headings as a reader would.
    pub(crate) fn from_blocks(format: Format, mut blocks: Vec<Block>) -> Document {
        for (id, block) in blocks.iter_mut().enumerate() {
            block.id = id as u32;
        }
        let sections = crate::extract::assign_sections(&mut blocks);
        Document {
            schema_version: SCHEMA_VERSION,
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
            outline: Vec::new(),
            sections,
            files: Vec::new(),
            page_labels: Vec::new(),
            containers: Vec::new(),
            blocks,
            furniture: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> Document {
        let pdf = |page| Locator::Pdf {
            page,
            page_end: None,
            bbox: None,
        };
        let block = |id, kind, text: &str, page| Block {
            id,
            kind,
            text: text.to_string(),
            spans: Vec::new(),
            ids: Vec::new(),
            within: Vec::new(),
            section: Some(0),
            locator: pdf(page),
        };
        Document {
            schema_version: SCHEMA_VERSION,
            source: Source {
                sha256: "0".repeat(64),
                file_name: "sample.pdf".to_string(),
                format: Format::Pdf,
            },
            metadata: Metadata {
                title: Some("Sample".to_string()),
                authors: vec!["A. Author".to_string()],
                language: Some("en".to_string()),
                page_count: Some(2),
                spine_count: None,
            },
            outline: vec![OutlineEntry {
                id: 0,
                title: "Entropy".to_string(),
                level: 1,
                block_id: Some(0),
            }],
            sections: vec![Section {
                heading: 0,
                parent: None,
            }],
            files: Vec::new(),
            page_labels: vec![Some("i".to_string()), None],
            containers: Vec::new(),
            blocks: vec![
                block(0, BlockKind::Heading { level: 1 }, "Entropy", 1),
                block(1, BlockKind::Paragraph, "Entropy measures surprise.", 1),
                block(2, BlockKind::Paragraph, "Channels have a capacity.", 2),
            ],
            furniture: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn document_json_round_trips() {
        let doc = sample_document();
        let json = serde_json::to_string(&doc).unwrap();
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }

    #[test]
    fn json_leaves_out_block_ids_and_locator_tags_and_loading_numbers_the_blocks() {
        let json = serde_json::to_value(sample_document()).unwrap();
        assert!(json["blocks"][1].get("id").is_none());
        assert!(json["blocks"][1]["locator"].get("format").is_none());
        let back: Document = serde_json::from_value(json).unwrap();
        assert_eq!(
            back.blocks.iter().map(|b| b.id).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(back, sample_document());
    }

    #[test]
    fn furniture_is_numbered_on_its_own() {
        let mut doc = sample_document();
        doc.furniture = doc.blocks[1..].to_vec();
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.furniture.iter().map(|b| b.id).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn a_page_label_comes_from_the_table() {
        let doc = sample_document();
        assert_eq!(doc.page_labels, [Some("i".to_string()), None]);
        assert_eq!(doc.page_label(1), Some("i"));
        assert_eq!(doc.page_label(2), None);
        assert_eq!(doc.page_label(9), None);
        assert_eq!(doc.page_label(0), None);
    }

    #[test]
    fn empty_spans_ids_and_within_and_no_section_are_left_out() {
        let mut block = sample_document().blocks[1].clone();
        block.section = None;
        let json = serde_json::to_value(&block).unwrap();
        for field in ["spans", "ids", "within", "section"] {
            assert!(json.get(field).is_none(), "{field} in {json}");
        }
    }

    #[test]
    fn no_containers_are_left_out_of_the_document() {
        let json = serde_json::to_value(sample_document()).unwrap();
        assert!(json.get("containers").is_none(), "{json}");
        let back: Document = serde_json::from_value(json).unwrap();
        assert!(back.containers.is_empty());
    }

    #[test]
    fn a_locator_holds_a_page_a_file_or_a_line() {
        assert_eq!(
            serde_json::from_str::<Locator>(r#"{"file":2}"#).unwrap(),
            Locator::Epub { file: 2 }
        );
        assert_eq!(
            serde_json::from_str::<Locator>(r#"{"page":3,"page_end":null,"bbox":null}"#).unwrap(),
            Locator::Pdf {
                page: 3,
                page_end: None,
                bbox: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Locator>(r#"{"line":4,"line_end":null}"#).unwrap(),
            Locator::Markdown {
                line: 4,
                line_end: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Locator>(r#"{"line":4,"line_end":9}"#).unwrap(),
            Locator::Markdown {
                line: 4,
                line_end: Some(9)
            }
        );
        for json in [
            r#"{"page":1,"file":0}"#,
            r#"{"file":0,"bbox":[1,2,3,4]}"#,
            r#"{"line":1,"page":1}"#,
            r#"{"line":1,"file":0}"#,
            r#"{"line":1,"page_end":2}"#,
            r#"{"line":1,"bbox":[1,2,3,4]}"#,
            r#"{"page":1,"line_end":2}"#,
            r#"{"file":0,"line_end":2}"#,
            r#"{"line_end":2}"#,
            "{}",
        ] {
            let error = serde_json::from_str::<Locator>(json).unwrap_err();
            assert!(
                error.to_string().contains(
                    "`page`, for a PDF, `file`, for an EPUB, or `line`, for a Markdown file"
                ),
                "{json}: {error}"
            );
        }
    }

    #[test]
    fn optional_fields_and_every_variant_round_trip() {
        let mut doc = sample_document();
        doc.source.format = Format::Epub;
        doc.files = vec!["ch1.xhtml".to_string()];
        doc.sections.push(Section {
            heading: 2,
            parent: Some(0),
        });
        doc.blocks[1].spans = vec![
            Span {
                start: 0,
                end: 7,
                kind: SpanKind::Math {
                    latex: "H".to_string(),
                },
            },
            Span {
                start: 0,
                end: 0,
                kind: SpanKind::PageBreak {
                    label: "45".to_string(),
                },
            },
        ];
        doc.blocks[1].ids = vec!["s1".to_string()];
        doc.blocks[1].kind = BlockKind::Note {
            label: Some("1".to_string()),
        };
        doc.blocks[1].locator = Locator::Epub { file: 0 };
        doc.blocks[2].locator = Locator::Pdf {
            page: 2,
            page_end: Some(3),
            bbox: Some([1.0, 2.0, 3.0, 4.0]),
        };
        doc.warnings = vec![
            Warning::PagesWithoutText { pages: vec![4] },
            Warning::UnreadableContent {
                href: "ch9.xhtml".to_string(),
                reason: "bad markup".to_string(),
            },
            Warning::UnreadablePart {
                part: "page labels".to_string(),
                reason: "bad tree".to_string(),
            },
        ];
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains(r#""locator":{"file":0}"#));
        assert!(json.contains(r#""type":"math""#));
        assert!(json.contains(r#""kind":{"type":"page_break","label":"45"}"#));
        assert!(json.contains(r#""ids":["s1"]"#));
        assert!(json.contains(r#""type":"pages_without_text""#));
        assert!(
            json.contains(r#"{"type":"unreadable_part","part":"page labels","reason":"bad tree"}"#)
        );
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }

    fn span(start: usize, end: usize, kind: SpanKind) -> Span {
        Span { start, end, kind }
    }

    fn with_spans(text: &str, spans: Vec<Span>) -> Block {
        Block {
            text: text.to_string(),
            spans,
            ..sample_document().blocks[1].clone()
        }
    }

    /// One block or span of each variant this schema added, in a Markdown
    /// document.
    fn structured_document() -> Document {
        let mut doc = sample_document();
        doc.source.format = Format::Markdown;
        doc.source.file_name = "notes.md".to_string();
        doc.metadata.page_count = None;
        doc.page_labels = Vec::new();
        doc.containers = vec![
            Container::List {
                ordered: true,
                start: Some(3),
            },
            Container::Item,
            Container::Quote,
            Container::Table,
            Container::Row { header: true },
            Container::Cell,
            Container::Note {
                label: Some("n1".to_string()),
            },
            Container::Note { label: None },
        ];
        let line = |line, line_end| Locator::Markdown { line, line_end };
        let marked = with_spans(
            "abcdefghijklmnop",
            vec![
                span(0, 16, SpanKind::Emphasis),
                span(0, 8, SpanKind::Strong),
                span(0, 4, SpanKind::Strikethrough),
                span(1, 3, SpanKind::Code),
                span(
                    4,
                    6,
                    SpanKind::Link {
                        href: "#a".to_string(),
                        to: LinkTarget::Block { block: 0 },
                    },
                ),
                span(
                    6,
                    7,
                    SpanKind::Link {
                        href: "ch2.md".to_string(),
                        to: LinkTarget::File {
                            path: "ch2.md".to_string(),
                        },
                    },
                ),
                span(
                    7,
                    8,
                    SpanKind::Link {
                        href: "https://example.org".to_string(),
                        to: LinkTarget::Address,
                    },
                ),
                span(
                    8,
                    9,
                    SpanKind::Link {
                        href: "#gone".to_string(),
                        to: LinkTarget::Nowhere,
                    },
                ),
                span(9, 10, SpanKind::Break { lines: 2 }),
                span(
                    11,
                    11,
                    SpanKind::NoteRef {
                        marker: "1".to_string(),
                        block: Some(2),
                    },
                ),
                span(
                    12,
                    12,
                    SpanKind::PageBreak {
                        label: "4".to_string(),
                    },
                ),
                span(
                    13,
                    14,
                    SpanKind::Math {
                        latex: "H".to_string(),
                    },
                ),
            ],
        );
        doc.blocks = vec![
            Block {
                locator: line(1, None),
                ..doc.blocks[0].clone()
            },
            Block {
                within: vec![0, 1],
                locator: line(3, None),
                ..marked
            },
            Block {
                id: 2,
                kind: BlockKind::Code {
                    language: Some("rust".to_string()),
                },
                text: "fn main() {\n    run();\n}".to_string(),
                within: vec![2],
                locator: line(5, Some(9)),
                ..doc.blocks[1].clone()
            },
            Block {
                id: 3,
                kind: BlockKind::Code { language: None },
                text: "plain".to_string(),
                locator: line(11, Some(13)),
                ..doc.blocks[1].clone()
            },
            Block {
                id: 4,
                kind: BlockKind::Rule,
                text: String::new(),
                locator: line(15, None),
                ..doc.blocks[1].clone()
            },
        ];
        doc.warnings = vec![Warning::BrokenLink {
            href: "#gone".to_string(),
            block: 1,
        }];
        doc
    }

    #[test]
    fn every_new_variant_round_trips_through_json() {
        let doc = structured_document();
        assert_eq!(span_errors(&doc.blocks[1]), Vec::<String>::new());
        let json = serde_json::to_string(&doc).unwrap();
        for part in [
            r#""format":"markdown""#,
            r#""containers":[{"type":"list","ordered":true,"start":3},{"type":"item"},{"type":"quote"},{"type":"table"},{"type":"row","header":true},{"type":"cell"},{"type":"note","label":"n1"},{"type":"note","label":null}]"#,
            r#""within":[0,1]"#,
            r#""locator":{"line":3,"line_end":null}"#,
            r#""locator":{"line":5,"line_end":9}"#,
            r#""kind":{"type":"code","language":"rust"}"#,
            r#""kind":{"type":"code","language":null}"#,
            r#""kind":{"type":"rule"}"#,
            r#""kind":{"type":"emphasis"}"#,
            r#""kind":{"type":"strong"}"#,
            r#""kind":{"type":"strikethrough"}"#,
            r#""kind":{"type":"code"}"#,
            r##""kind":{"type":"link","href":"#a","to":{"type":"block","block":0}}"##,
            r#""to":{"type":"file","path":"ch2.md"}"#,
            r#""to":{"type":"address"}"#,
            r#""to":{"type":"nowhere"}"#,
            r#""kind":{"type":"break","lines":2}"#,
            r#""kind":{"type":"note_ref","marker":"1","block":2}"#,
            r##"{"type":"broken_link","href":"#gone","block":1}"##,
        ] {
            assert!(json.contains(part), "{part} is not in {json}");
        }
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }

    #[test]
    fn byte_offset_counts_code_points() {
        assert_eq!(byte_offset("café au", 5), 6);
        assert_eq!(byte_offset("😀a", 1), 4);
        assert_eq!(byte_offset("ab", 9), 2);
        assert_eq!(byte_offset("ab", 2), 2);
        assert_eq!(byte_offset("ab", 0), 0);
        assert_eq!(byte_offset("", 0), 0);
    }

    #[test]
    fn span_errors_name_overlaps_order_and_empty_spans() {
        // [0,4) and [2,6) overlap without one containing the other; the empty
        // Emphasis at 6 marks no point; [7,8) comes before the longer [7,10).
        let block = with_spans(
            "abcdefghij",
            vec![
                span(0, 4, SpanKind::Emphasis),
                span(2, 6, SpanKind::Strong),
                span(6, 6, SpanKind::Emphasis),
                span(7, 8, SpanKind::Code),
                span(7, 10, SpanKind::Strong),
            ],
        );
        let errors = span_errors(&block);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors[0].contains("span 1") && errors[0].contains("overlap"));
        assert!(errors[1].contains("span 2") && errors[1].contains("empty"));
        assert!(errors[2].contains("span 4") && errors[2].contains("order"));
    }

    #[test]
    fn spans_that_nest_touch_or_repeat_are_well_formed() {
        let note = SpanKind::NoteRef {
            marker: "1".to_string(),
            block: None,
        };
        let page = SpanKind::PageBreak {
            label: "7".to_string(),
        };
        let block = with_spans(
            "abcdefghij",
            vec![
                span(0, 10, SpanKind::Strong),
                span(0, 10, SpanKind::Emphasis),
                span(0, 5, SpanKind::Code),
                span(0, 0, page.clone()),
                span(2, 2, note.clone()),
                span(5, 8, SpanKind::Strikethrough),
                span(8, 10, SpanKind::Code),
                span(10, 10, note),
                span(10, 10, page),
            ],
        );
        assert_eq!(span_errors(&block), Vec::<String>::new());
        assert_eq!(
            span_errors(&with_spans("abc", Vec::new())),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_span_that_ends_after_the_text_or_before_it_starts_is_an_error() {
        // Code points count: "café" is 4 of them in 5 bytes.
        let inside = with_spans("café", vec![span(0, 4, SpanKind::Code)]);
        assert_eq!(span_errors(&inside), Vec::<String>::new());

        let past = with_spans("café", vec![span(1, 5, SpanKind::Code)]);
        let errors = span_errors(&past);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("span 0") && errors[0].contains("4 code points"));

        let backwards = with_spans("café", vec![span(3, 1, SpanKind::Code)]);
        let errors = span_errors(&backwards);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("starts after it ends"));
    }

    #[test]
    fn a_note_reference_and_a_page_break_may_be_empty_and_nothing_else_may() {
        let kinds = [
            SpanKind::Emphasis,
            SpanKind::Strong,
            SpanKind::Strikethrough,
            SpanKind::Code,
            SpanKind::Link {
                href: "#a".to_string(),
                to: LinkTarget::Nowhere,
            },
            SpanKind::Break { lines: 1 },
            SpanKind::Math {
                latex: "H".to_string(),
            },
        ];
        for kind in kinds {
            let errors = span_errors(&with_spans("abc", vec![span(1, 1, kind.clone())]));
            assert_eq!(errors.len(), 1, "{kind:?}: {errors:?}");
            assert!(errors[0].contains("empty"), "{kind:?}: {errors:?}");
        }
        for kind in [
            SpanKind::NoteRef {
                marker: "1".to_string(),
                block: None,
            },
            SpanKind::PageBreak {
                label: "7".to_string(),
            },
        ] {
            let errors = span_errors(&with_spans("abc", vec![span(1, 1, kind)]));
            assert_eq!(errors, Vec::<String>::new());
        }
    }
}
