pub mod markdown;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    pub schema_version: u32,
    pub source: Source,
    pub metadata: Metadata,
    pub outline: Vec<OutlineEntry>,
    pub blocks: Vec<Block>,
    pub furniture: Vec<Block>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub sha256: String,
    pub file_name: String,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Epub,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub page_count: Option<u32>,
    pub spine_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OutlineEntry {
    pub id: u32,
    pub title: String,
    pub level: u8,
    pub block_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Block {
    pub id: u32,
    pub kind: BlockKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    pub section: Vec<String>,
    pub locator: Locator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockKind {
    Heading { level: u8 },
    Paragraph,
    ListItem,
    Note { label: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: SpanKind,
}

/// Reserved for the formula spec; the core writes none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpanKind {
    Math { latex: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum Locator {
    Pdf {
        page: u32,
        page_end: Option<u32>,
        page_label: Option<String>,
        bbox: Option<[f32; 4]>,
    },
    Epub {
        href: String,
        anchor: Option<String>,
        para: u32,
        print_page: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Warning {
    PagesWithoutText { pages: Vec<u32> },
    UnreadableContent { href: String, reason: String },
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
        ];
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains(r#""format":"epub""#));
        assert!(json.contains(r#""type":"math""#));
        assert!(json.contains(r#""type":"pages_without_text""#));
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
    }
}
