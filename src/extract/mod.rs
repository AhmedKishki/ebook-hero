pub mod epub;
pub mod xhtml;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::document::{Block, BlockKind, Document, Format, Source};
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
        Format::Pdf => Err(Error::Unsupported {
            path: path.to_path_buf(),
        }),
    }
}

/// Gives every block the path of headings above it, outermost first.
///
/// A heading of level `n` replaces entry `n` and everything deeper on the
/// stack, so a heading's own section ends with its text.
pub(crate) fn assign_sections(blocks: &mut [Block]) {
    let mut stack: Vec<String> = Vec::new();
    for block in blocks {
        if let BlockKind::Heading { level } = block.kind {
            stack.truncate(usize::from(level.max(1)) - 1);
            stack.push(block.text.clone());
        }
        block.section = stack.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Locator;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: 0,
            kind,
            text: text.to_string(),
            orig: None,
            spans: Vec::new(),
            section: vec!["stale".to_string()],
            locator: Locator::Epub {
                href: "a.xhtml".to_string(),
                anchor: None,
                para: 1,
                print_page: None,
            },
        }
    }

    fn sections(blocks: &[Block]) -> Vec<(&str, Vec<&str>)> {
        blocks
            .iter()
            .map(|b| {
                (
                    b.text.as_str(),
                    b.section.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_heading_replaces_its_level_and_deeper() {
        let heading = |level, text| block(BlockKind::Heading { level }, text);
        let mut blocks = vec![
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

        assign_sections(&mut blocks);

        assert_eq!(
            sections(&blocks),
            [
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
            ]
        );
    }

    #[test]
    fn a_skipped_level_adds_no_empty_entry() {
        let mut blocks = vec![
            block(BlockKind::Heading { level: 2 }, "Deep"),
            block(BlockKind::Paragraph, "p"),
            block(BlockKind::Heading { level: 4 }, "Deeper"),
            block(BlockKind::Heading { level: 1 }, "Top"),
        ];

        assign_sections(&mut blocks);

        assert_eq!(
            sections(&blocks),
            [
                ("Deep", vec!["Deep"]),
                ("p", vec!["Deep"]),
                ("Deeper", vec!["Deep", "Deeper"]),
                ("Top", vec!["Top"]),
            ]
        );
    }
}
