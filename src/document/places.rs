//! Where each block is in the book, derived from what the `Document` stores
//! once. The readers store a section, a file and a page label once, where the
//! fact holds, and [`Places::new`] works out in one pass over the blocks what
//! each block needs, so that every later lookup takes constant time.

use super::{Block, BlockKind, Document, Locator, SpanKind};

/// The places of a document's blocks: the tables built when a book is loaded.
/// They hold copies of the texts they return, so that they can live beside
/// the document they were built from.
#[derive(Debug, Clone, PartialEq)]
pub struct Places {
    /// For each section, the texts of its headings, outermost first.
    paths: Vec<Vec<String>>,
    /// For each block, in the order of `Document::blocks`.
    blocks: Vec<BlockPlace>,
    /// The labels of the page breaks, in reading order.
    pages: Vec<String>,
}

/// What [`Places`] knows about one block.
#[derive(Debug, Clone, PartialEq)]
struct BlockPlace {
    para: u32,
    section_para: u32,
    /// The index in `Places::pages` of the printed page in effect where the
    /// block starts.
    print_page: Option<u32>,
}

impl Places {
    /// Builds the tables for `document`, which must be as a reader wrote it:
    /// each section's heading is one of its blocks and its parent an earlier
    /// section.
    pub fn new(document: &Document) -> Places {
        let mut paths: Vec<Vec<String>> = Vec::with_capacity(document.sections.len());
        for section in &document.sections {
            let mut path = section
                .parent
                .map_or_else(Vec::new, |parent| paths[parent as usize].clone());
            path.push(document.blocks[section.heading as usize].text.clone());
            paths.push(path);
        }

        let mut blocks = Vec::with_capacity(document.blocks.len());
        let mut pages = Vec::new();
        let mut file = None;
        let mut para = 0;
        let mut section = None;
        let mut section_para = 0;
        let mut print_page = None;
        for block in &document.blocks {
            para = match block.locator {
                Locator::Epub { file: this } if file == Some(this) => para + 1,
                Locator::Epub { file: this } => {
                    file = Some(this);
                    1
                }
                Locator::Pdf { .. } | Locator::Markdown { .. } => 0,
            };
            if block.section != section {
                section = block.section;
                section_para = 0;
            }
            let heading = matches!(block.kind, BlockKind::Heading { .. });
            if !heading {
                section_para += 1;
            }
            // A break at the block's start counts for the block, and one
            // inside it from the next block on.
            let mut inside = None;
            for span in &block.spans {
                if let SpanKind::PageBreak { label } = &span.kind {
                    let index = pages.len() as u32;
                    pages.push(label.clone());
                    match span.start {
                        0 => print_page = Some(index),
                        _ => inside = Some(index),
                    }
                }
            }
            blocks.push(BlockPlace {
                para,
                section_para: if heading { 0 } else { section_para },
                print_page,
            });
            print_page = inside.or(print_page);
        }
        Places {
            paths,
            blocks,
            pages,
        }
    }

    /// The heading texts above the block, outermost first; a heading's own
    /// path ends with its text.
    pub fn section_path(&self, block: &Block) -> &[String] {
        block
            .section
            .map_or(&[], |section| &self.paths[section as usize])
    }

    /// The block's position among the blocks of its EPUB file, from 1; 0 in a
    /// PDF or a Markdown file.
    pub fn para(&self, block: u32) -> u32 {
        self.blocks[block as usize].para
    }

    /// The block's number among the non-heading blocks of its section, from
    /// 1, the count an EPUB passage's place under its heading gives; 0 for a
    /// heading.
    pub fn section_para(&self, block: u32) -> u32 {
        self.blocks[block as usize].section_para
    }

    /// The label of the last [`PageBreak`](SpanKind::PageBreak) at or before
    /// the block's start, in this block or an earlier one.
    pub fn print_page(&self, block: u32) -> Option<&str> {
        let index = self.blocks[block as usize].print_page?;
        Some(&self.pages[index as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Block, BlockKind, Document, Format, Locator, Span, SpanKind};

    fn block(kind: BlockKind, text: &str, file: u32) -> Block {
        Block {
            id: 0,
            kind,
            text: text.to_string(),
            spans: Vec::new(),
            ids: Vec::new(),
            within: Vec::new(),
            section: None,
            locator: Locator::Epub { file },
        }
    }

    fn heading(level: u8, text: &str) -> Block {
        block(BlockKind::Heading { level }, text, 0)
    }

    fn para(file: u32) -> Block {
        block(BlockKind::Paragraph, "Text.", file)
    }

    fn with_break(label: &str, start: usize, block: Block) -> Block {
        Block {
            spans: vec![Span {
                start,
                end: start,
                kind: SpanKind::PageBreak {
                    label: label.to_string(),
                },
            }],
            ..block
        }
    }

    fn places(blocks: Vec<Block>) -> (Document, Places) {
        let doc = Document::from_blocks(Format::Epub, blocks);
        let places = Places::new(&doc);
        (doc, places)
    }

    #[test]
    fn a_section_path_follows_the_parents() {
        let (doc, places) = places(vec![
            heading(1, "One"),
            para(0),
            heading(2, "Sub"),
            para(0),
            heading(1, "Two"),
            para(0),
        ]);
        assert_eq!(places.section_path(&doc.blocks[3]), ["One", "Sub"]);
        assert_eq!(places.section_path(&doc.blocks[5]), ["Two"]);
        assert_eq!(places.section_path(&doc.blocks[2]), ["One", "Sub"]);
    }

    #[test]
    fn a_block_before_the_first_heading_has_no_section_path() {
        let (doc, places) = places(vec![para(0), heading(1, "One")]);
        assert!(places.section_path(&doc.blocks[0]).is_empty());
    }

    #[test]
    fn para_counts_within_each_epub_file() {
        let (_, places) = places(vec![para(0), para(0), para(0), para(1), para(1)]);
        assert_eq!(
            (0..5).map(|b| places.para(b)).collect::<Vec<_>>(),
            [1, 2, 3, 1, 2]
        );
    }

    #[test]
    fn para_is_zero_in_a_pdf_and_in_a_markdown_file() {
        let pdf = Block {
            locator: Locator::Pdf {
                page: 1,
                page_end: None,
                bbox: None,
            },
            ..para(0)
        };
        let (_, in_pdf) = places(vec![pdf]);
        assert_eq!(in_pdf.para(0), 0);

        let markdown = |line| Block {
            locator: Locator::Markdown {
                line,
                line_end: None,
            },
            ..para(0)
        };
        let (_, in_markdown) = places(vec![markdown(1), markdown(3)]);
        assert_eq!([0, 1].map(|b| in_markdown.para(b)), [0, 0]);
    }

    #[test]
    fn section_para_counts_markdown_blocks_under_their_heading() {
        let markdown = |kind, text, line| Block {
            locator: Locator::Markdown {
                line,
                line_end: None,
            },
            ..block(kind, text, 0)
        };
        let (_, places) = places(vec![
            markdown(BlockKind::Heading { level: 1 }, "One", 1),
            markdown(BlockKind::Paragraph, "a", 3),
            markdown(BlockKind::Rule, "", 5),
            markdown(BlockKind::Paragraph, "b", 7),
        ]);
        assert_eq!(
            (0..4).map(|b| places.section_para(b)).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn section_para_counts_the_blocks_under_each_heading() {
        let (_, places) = places(vec![
            para(0),
            heading(1, "One"),
            para(0),
            para(1),
            heading(1, "One"),
            para(1),
        ]);
        assert_eq!(
            (0..6).map(|b| places.section_para(b)).collect::<Vec<_>>(),
            [1, 0, 1, 2, 0, 1]
        );
    }

    #[test]
    fn a_print_page_carries_on_until_the_next_break_even_into_the_next_file() {
        let (_, places) = places(vec![
            para(0),
            with_break("45", 0, para(0)),
            para(1),
            with_break("46", 0, para(1)),
            para(2),
        ]);
        assert_eq!(
            [0, 1, 2, 3, 4].map(|b| places.print_page(b)),
            [None, Some("45"), Some("45"), Some("46"), Some("46")]
        );
    }

    #[test]
    fn a_break_inside_a_block_counts_from_the_next_block() {
        let (_, places) = places(vec![with_break("7", 3, para(0)), para(0)]);
        assert_eq!([0, 1].map(|b| places.print_page(b)), [None, Some("7")]);
    }
}
