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

/// The part of the book a heading opens. It runs until the next heading of
/// its level or nearer the top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Section {
    /// The heading block that opens it.
    pub heading: u32,
    /// The section it lies in, always an earlier one, or `None` at the top.
    pub parent: Option<u32>,
}

/// One unit of reading text: a heading, a paragraph, a list item or a note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Block {
    /// Its position in `blocks`. JSON leaves it out; deserialising sets it.
    #[serde(skip)]
    pub id: u32,
    /// What kind of block it is.
    pub kind: BlockKind,
    /// The text, normalized to one line without markup.
    pub text: String,
    /// Marked ranges of `text`, such as page breaks. JSON leaves the list out
    /// when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    /// Ids of the elements that start in the block, and of empty elements
    /// since the block before. JSON leaves the list out when it is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
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
}

/// Every field a [`Locator`] may hold, so that one pass over the JSON object
/// reads it and the fields present decide the variant.
#[derive(Deserialize)]
struct LocatorFields {
    page: Option<u32>,
    page_end: Option<u32>,
    bbox: Option<[f32; 4]>,
    file: Option<u32>,
}

impl<'de> Deserialize<'de> for Locator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let LocatorFields {
            page,
            page_end,
            bbox,
            file,
        } = LocatorFields::deserialize(deserializer)?;
        match (page, file) {
            (Some(page), None) => Ok(Locator::Pdf {
                page,
                page_end,
                bbox,
            }),
            (None, Some(file)) if page_end.is_none() && bbox.is_none() => {
                Ok(Locator::Epub { file })
            }
            _ => Err(de::Error::custom(
                "a locator holds either `page`, for a PDF, or `file`, for an EPUB; \
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
    fn empty_spans_and_ids_and_no_section_are_left_out() {
        let mut block = sample_document().blocks[1].clone();
        block.section = None;
        let json = serde_json::to_value(&block).unwrap();
        for field in ["spans", "ids", "section"] {
            assert!(json.get(field).is_none(), "{field} in {json}");
        }
    }

    #[test]
    fn a_locator_holds_a_page_or_a_file() {
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
        for json in [
            r#"{"page":1,"file":0}"#,
            r#"{"file":0,"bbox":[1,2,3,4]}"#,
            "{}",
        ] {
            let error = serde_json::from_str::<Locator>(json).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("either `page`, for a PDF, or `file`, for an EPUB"),
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
}
