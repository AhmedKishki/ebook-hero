//! The `Document` model: one book as the readers in `extract` write it, and
//! as everything after them reads it without changing it. `extract --format
//! json` prints it, and `docs/document-model.md` describes every field.

pub mod markdown;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The version of the `Document` model. A change to the model adds fields or
/// variants and bumps it, so that cached indexes are built again.
pub const SCHEMA_VERSION: u32 = 2;

/// A book as its reader extracted it: facts about the file, the table of
/// contents, and the text as a flat list of blocks in reading order.
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
    /// The reading text, in reading order. A block's `id` is its index here.
    pub blocks: Vec<Block>,
    /// The lines the PDF reader took out of the reading text, such as running
    /// heads and page numbers. Nothing after the readers reads them, and the
    /// EPUB reader leaves the list empty.
    pub furniture: Vec<Block>,
    /// The parts of the file the reader left out because it could not read
    /// them.
    pub warnings: Vec<Warning>,
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

/// The two formats ebook-hero reads, written `"pdf"` and `"epub"` in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// A PDF file.
    Pdf,
    /// An EPUB 2 or EPUB 3 file.
    Epub,
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

/// One unit of reading text: a heading, a paragraph, a list item or a note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Block {
    /// The block's position in reading order, from 0.
    pub id: u32,
    /// What kind of block it is.
    pub kind: BlockKind,
    /// The text, normalized to one line without markup.
    pub text: String,
    /// Reserved; no reader writes it, and JSON leaves it out when it is
    /// empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig: Option<String>,
    /// Reserved for marked ranges of `text`, such as formulas; no reader
    /// writes any, and JSON leaves the list out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    /// The texts of the headings above the block, outermost first. A
    /// heading's own section ends with its text.
    pub section: Vec<String>,
    /// Where the block is in the file.
    pub locator: Locator,
}

/// The kind of a block, written as an object with a `type` field in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockKind {
    /// A heading. It goes into no passage and reaches the passages under it
    /// as their `section`.
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
}

/// A marked range of a block's text. Reserved: no reader writes spans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// Where the range starts in the block's text.
    pub start: usize,
    /// Where the range ends in the block's text.
    pub end: usize,
    /// What the range is.
    pub kind: SpanKind,
}

/// Reserved for the formula spec; the core writes none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpanKind {
    /// A formula.
    Math {
        /// The formula in LaTeX.
        latex: String,
    },
}

/// Where a block is in its file, in the terms of the file's format. JSON
/// names the variant in a `format` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum Locator {
    /// A place in a PDF.
    Pdf {
        /// The physical page where the block starts, counted from 1.
        page: u32,
        /// The page where the block ends, when that is a later page.
        page_end: Option<u32>,
        /// The printed label of `page`, such as `xii`, when the PDF defines
        /// page labels.
        page_label: Option<String>,
        /// `[left, top, right, bottom]` of the block's lines on `page`, in
        /// points from the top-left corner of the page; `None` for a heading
        /// the reader inserted from a bookmark.
        bbox: Option<[f32; 4]>,
    },
    /// A place in an EPUB.
    Epub {
        /// The content file, relative to the package document's directory.
        href: String,
        /// The last element id at or before the block's start in that file.
        anchor: Option<String>,
        /// The block's position among the blocks of its file, counted from 1.
        para: u32,
        /// The printed page where the block's text starts, when the book
        /// marks its page breaks.
        print_page: Option<String>,
    },
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> Document {
        let pdf = |page| Locator::Pdf {
            page,
            page_end: None,
            page_label: None,
            bbox: None,
        };
        let block = |id, kind, text: &str, page| Block {
            id,
            kind,
            text: text.to_string(),
            orig: None,
            spans: Vec::new(),
            section: vec!["Entropy".to_string()],
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
    fn empty_orig_and_spans_are_omitted() {
        let json = serde_json::to_string(&sample_document().blocks[1]).unwrap();
        assert!(!json.contains("orig") && !json.contains("spans"));
    }

    #[test]
    fn locator_is_tagged_by_format() {
        let json = serde_json::to_string(&sample_document().blocks[1].locator).unwrap();
        assert!(json.contains(r#""format":"pdf""#));
    }

    #[test]
    fn optional_fields_and_every_variant_round_trip() {
        let mut doc = sample_document();
        doc.source.format = Format::Epub;
        doc.blocks[1].orig = Some("Entropy  measures surprise.".to_string());
        doc.blocks[1].spans = vec![Span {
            start: 0,
            end: 7,
            kind: SpanKind::Math {
                latex: "H".to_string(),
            },
        }];
        doc.blocks[1].kind = BlockKind::Note {
            label: Some("1".to_string()),
        };
        doc.blocks[1].locator = Locator::Epub {
            href: "ch1.xhtml".to_string(),
            anchor: Some("s1".to_string()),
            para: 3,
            print_page: Some("45".to_string()),
        };
        doc.blocks[2].locator = Locator::Pdf {
            page: 2,
            page_end: Some(3),
            page_label: Some("ii".to_string()),
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
        assert!(json.contains(r#""format":"epub""#));
        assert!(json.contains(r#""type":"math""#));
        assert!(json.contains(r#""type":"pages_without_text""#));
        assert!(
            json.contains(r#"{"type":"unreadable_part","part":"page labels","reason":"bad tree"}"#)
        );
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }
}
