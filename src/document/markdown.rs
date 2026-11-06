//! A [`Document`] as Markdown, which `ebook-hero extract` prints by default.

use crate::document::{Block, BlockKind, Document, Locator};

/// Writes the blocks as Markdown. A PDF block on a new page is preceded by a
/// `<!-- page N -->` comment; EPUB blocks carry no page comments.
pub fn blocks_to_markdown(blocks: &[Block]) -> String {
    let mut pieces: Vec<String> = Vec::new();
    let mut page = None;
    for block in blocks {
        if let Locator::Pdf { page: this, .. } = block.locator
            && page != Some(this)
        {
            page = Some(this);
            pieces.push(format!("<!-- page {this} -->"));
        }
        pieces.push(block_to_markdown(block));
    }
    if pieces.is_empty() {
        return String::new();
    }
    let mut markdown = pieces.join("\n\n");
    markdown.push('\n');
    markdown
}

/// Writes a `# {title}` line when the title is known, then the blocks.
pub fn document_to_markdown(doc: &Document) -> String {
    let body = blocks_to_markdown(&doc.blocks);
    match (&doc.metadata.title, body.is_empty()) {
        (Some(title), true) => format!("# {title}\n"),
        (Some(title), false) => format!("# {title}\n\n{body}"),
        (None, _) => body,
    }
}

fn block_to_markdown(block: &Block) -> String {
    match &block.kind {
        BlockKind::Heading { level } => {
            format!(
                "{} {}",
                "#".repeat(usize::from(*level).clamp(1, 6)),
                block.text
            )
        }
        BlockKind::Paragraph | BlockKind::Code { .. } | BlockKind::Rule => block.text.clone(),
        BlockKind::ListItem => format!("- {}", block.text),
        BlockKind::Note { label } => {
            let text = match label {
                Some(label) => format!("[{label}] {}", block.text),
                None => block.text.clone(),
            };
            text.lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Format, Metadata, SCHEMA_VERSION, Source};

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

    #[test]
    fn headings_use_hashes_by_level() {
        let md = blocks_to_markdown(&[heading(2, "3.2 Entropy", 1)]);
        assert!(md.contains("## 3.2 Entropy"));
    }

    #[test]
    fn each_pdf_page_gets_a_comment_and_paragraphs_a_blank_line() {
        let md = blocks_to_markdown(&[para("One.", 1), para("Two.", 1), para("Three.", 2)]);
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
        let md = blocks_to_markdown(&[
            epub_block(BlockKind::Paragraph, "One."),
            epub_block(BlockKind::Paragraph, "Two."),
        ]);
        assert_eq!(md, "One.\n\nTwo.\n");
    }

    #[test]
    fn list_items_and_notes_have_their_own_markers() {
        let md = blocks_to_markdown(&[
            epub_block(BlockKind::ListItem, "First"),
            epub_block(
                BlockKind::Note {
                    label: Some("1".to_string()),
                },
                "See chapter 2.",
            ),
            epub_block(BlockKind::Note { label: None }, "Aside."),
        ]);
        assert_eq!(md, "- First\n\n> [1] See chapter 2.\n\n> Aside.\n");
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
        assert_eq!(blocks_to_markdown(&[]), "");
    }
}
