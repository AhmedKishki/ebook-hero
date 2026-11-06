//! The readers, which turn a PDF or an EPUB into a [`Document`].
//! [`extract`] picks the reader from the file's content.

pub mod epub;
pub mod pdf;
pub mod pdf_layout;
pub mod xhtml;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::document::{Block, BlockKind, Document, Format, Section, Source};
use crate::error::{Error, Result};

/// How far into a file the `%PDF-` marker may start.
const PDF_MARKER_WINDOW: u64 = 1024;
const PDF_MARKER: &[u8] = b"%PDF-";
const EPUB_MIMETYPE: &str = "application/epub+zip";
const EPUB_CONTAINER: &str = "META-INF/container.xml";

/// Tells the format of a file from its content, never from its name.
///
/// A file with `%PDF-` in its first 1024 bytes is a PDF. A ZIP archive whose
/// `mimetype` entry reads `application/epub+zip`, or which holds
/// `META-INF/container.xml`, is an EPUB. Anything else is unsupported.
pub fn detect_format(path: &Path) -> Result<Format> {
    let metadata = fs::metadata(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound {
            path: path.to_path_buf(),
        },
        _ => unreadable(path, &e),
    })?;
    if !metadata.is_file() {
        return Err(Error::NotAFile {
            path: path.to_path_buf(),
        });
    }
    let mut file = File::open(path).map_err(|e| unreadable(path, &e))?;
    let mut head = Vec::new();
    (&mut file)
        .take(PDF_MARKER_WINDOW)
        .read_to_end(&mut head)
        .map_err(|e| unreadable(path, &e))?;
    if head.windows(PDF_MARKER.len()).any(|w| w == PDF_MARKER) {
        return Ok(Format::Pdf);
    }
    if is_epub_archive(file) {
        return Ok(Format::Epub);
    }
    Err(Error::Unsupported {
        path: path.to_path_buf(),
    })
}

fn is_epub_archive(file: File) -> bool {
    let Ok(mut archive) = ZipArchive::new(file) else {
        return false;
    };
    if archive.index_for_name(EPUB_CONTAINER).is_some() {
        return true;
    }
    let Ok(entry) = archive.by_name("mimetype") else {
        return false;
    };
    let mut mimetype = String::new();
    // The expected value is 20 bytes, so a longer entry cannot match.
    let read = entry.take(64).read_to_string(&mut mimetype);
    read.is_ok() && mimetype.trim() == EPUB_MIMETYPE
}

fn unreadable(path: &Path, error: &io::Error) -> Error {
    Error::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

/// Reads a book into the [`Document`] model, whichever of the two formats it is.
pub fn extract(path: &Path, sha256: &str) -> Result<Document> {
    let format = detect_format(path)?;
    let source = Source {
        sha256: sha256.to_string(),
        file_name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        format,
    };
    match source.format {
        Format::Epub => epub::read_epub(path, source),
        Format::Pdf => pdf::read_pdf(path, source),
        // `detect_format` does not return Markdown until its reader exists.
        Format::Markdown => Err(Error::Unsupported {
            path: path.to_path_buf(),
        }),
    }
}

/// Gives every block its section and returns the sections, one per heading
/// in reading order.
///
/// A heading of level `n` closes the open sections of level `n` and deeper
/// and opens its own inside the one left open, so a heading is in the
/// section it opens. Levels that a book skips open nothing: a book whose
/// chapters are all `h2` has one top-level section per chapter, not a growing
/// path. Blocks before the first heading are in no section.
pub(crate) fn assign_sections(blocks: &mut [Block]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut open: Vec<(u8, u32)> = Vec::new();
    for (index, block) in blocks.iter_mut().enumerate() {
        if let BlockKind::Heading { level } = block.kind {
            while open
                .last()
                .is_some_and(|&(open_level, _)| open_level >= level)
            {
                open.pop();
            }
            sections.push(Section {
                heading: index as u32,
                parent: open.last().map(|&(_, section)| section),
            });
            open.push((level, sections.len() as u32 - 1));
        }
        block.section = open.last().map(|&(_, section)| section);
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Locator, Places};

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: 0,
            kind,
            text: text.to_string(),
            spans: Vec::new(),
            ids: Vec::new(),
            within: Vec::new(),
            section: Some(99),
            locator: Locator::Epub { file: 0 },
        }
    }

    /// Each block's text and section path, from the sections `assign_sections` gives.
    fn sections(blocks: Vec<Block>) -> Vec<(String, Vec<String>)> {
        let doc = Document::from_blocks(Format::Epub, blocks);
        let places = Places::new(&doc);
        doc.blocks
            .iter()
            .map(|b| (b.text.clone(), places.section_path(b).to_vec()))
            .collect()
    }

    fn paths(expected: &[(&str, Vec<&str>)]) -> Vec<(String, Vec<String>)> {
        expected
            .iter()
            .map(|(text, path)| {
                (
                    text.to_string(),
                    path.iter().map(|title| title.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_heading_opens_a_section_inside_the_one_left_open() {
        let heading = |level, text| block(BlockKind::Heading { level }, text);
        let mut blocks = vec![
            block(BlockKind::Paragraph, "front"),
            heading(1, "A"),
            heading(2, "A.1"),
            block(BlockKind::Paragraph, "a1"),
            heading(1, "B"),
        ];

        let sections = assign_sections(&mut blocks);

        assert_eq!(
            sections,
            [
                Section {
                    heading: 1,
                    parent: None
                },
                Section {
                    heading: 2,
                    parent: Some(0)
                },
                Section {
                    heading: 4,
                    parent: None
                },
            ]
        );
        let ids: Vec<_> = blocks.iter().map(|b| b.section).collect();
        assert_eq!(ids, [None, Some(0), Some(1), Some(1), Some(2)]);
    }

    #[test]
    fn a_heading_replaces_its_level_and_deeper() {
        let heading = |level, text| block(BlockKind::Heading { level }, text);
        let blocks = vec![
            block(BlockKind::Paragraph, "front"),
            heading(1, "A"),
            block(BlockKind::Paragraph, "a1"),
            heading(2, "A.1"),
            heading(3, "A.1.1"),
            block(BlockKind::ListItem, "item"),
            heading(2, "A.2"),
            block(BlockKind::Paragraph, "a2"),
            heading(1, "B"),
            block(BlockKind::Paragraph, "b1"),
        ];

        assert_eq!(
            sections(blocks),
            paths(&[
                ("front", vec![]),
                ("A", vec!["A"]),
                ("a1", vec!["A"]),
                ("A.1", vec!["A", "A.1"]),
                ("A.1.1", vec!["A", "A.1", "A.1.1"]),
                ("item", vec!["A", "A.1", "A.1.1"]),
                ("A.2", vec!["A", "A.2"]),
                ("a2", vec!["A", "A.2"]),
                ("B", vec!["B"]),
                ("b1", vec!["B"]),
            ])
        );
    }

    #[test]
    fn h2_only_chapters_are_siblings() {
        let blocks = vec![
            block(BlockKind::Heading { level: 2 }, "Ch1"),
            block(BlockKind::Paragraph, "one"),
            block(BlockKind::Heading { level: 2 }, "Ch2"),
            block(BlockKind::Paragraph, "two"),
            block(BlockKind::Heading { level: 2 }, "Ch3"),
        ];

        assert_eq!(
            sections(blocks),
            paths(&[
                ("Ch1", vec!["Ch1"]),
                ("one", vec!["Ch1"]),
                ("Ch2", vec!["Ch2"]),
                ("two", vec!["Ch2"]),
                ("Ch3", vec!["Ch3"]),
            ])
        );
    }

    #[test]
    fn skipped_levels_replace_deeper_entries() {
        let blocks = vec![
            block(BlockKind::Heading { level: 1 }, "A"),
            block(BlockKind::Heading { level: 3 }, "C"),
            block(BlockKind::Heading { level: 3 }, "C2"),
            block(BlockKind::Heading { level: 2 }, "B"),
            block(BlockKind::Heading { level: 4 }, "D"),
            block(BlockKind::Heading { level: 2 }, "B2"),
        ];

        assert_eq!(
            sections(blocks),
            paths(&[
                ("A", vec!["A"]),
                ("C", vec!["A", "C"]),
                ("C2", vec!["A", "C2"]),
                ("B", vec!["A", "B"]),
                ("D", vec!["A", "B", "D"]),
                ("B2", vec!["A", "B2"]),
            ])
        );
    }

    #[test]
    fn a_skipped_level_adds_no_empty_entry() {
        let blocks = vec![
            block(BlockKind::Heading { level: 2 }, "Deep"),
            block(BlockKind::Paragraph, "p"),
            block(BlockKind::Heading { level: 4 }, "Deeper"),
            block(BlockKind::Heading { level: 1 }, "Top"),
        ];

        assert_eq!(
            sections(blocks),
            paths(&[
                ("Deep", vec!["Deep"]),
                ("p", vec!["Deep"]),
                ("Deeper", vec!["Deep", "Deeper"]),
                ("Top", vec!["Top"]),
            ])
        );
    }
}
