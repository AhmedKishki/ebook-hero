pub mod epub;
pub mod pdf;
pub mod pdf_layout;
pub mod xhtml;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::document::{Apparatus, Block, BlockKind, Document, Format, Source};
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
    }
}

/// Gives every block the path of headings above it, outermost first, and the
/// apparatus that path names; see [`assign_apparatus`].
///
/// A heading of level `n` replaces the entries of level `n` and deeper on the
/// stack, so a heading's own section ends with its text. Levels that a book
/// skips add no entries: a book whose chapters are all `h2` has one entry per
/// section, not a growing list.
pub(crate) fn assign_sections(blocks: &mut [Block]) {
    let mut stack: Vec<(u8, &str)> = Vec::new();
    for block in blocks.iter_mut() {
        if let BlockKind::Heading { level } = block.kind {
            while stack.last().is_some_and(|(open, _)| *open >= level) {
                stack.pop();
            }
            stack.push((level, block.text.as_str()));
        }
        block.section = stack.iter().map(|(_, text)| text.to_string()).collect();
    }
    assign_apparatus(blocks);
}

/// The headings that open apparatus, as [`apparatus_named`] compares them.
const APPARATUS_HEADINGS: &[(&str, Apparatus)] = &[
    ("contents", Apparatus::Contents),
    ("table of contents", Apparatus::Contents),
    ("figures", Apparatus::Lists),
    ("tables", Apparatus::Lists),
    ("illustrations", Apparatus::Lists),
    ("abbreviations", Apparatus::Lists),
    ("figures and tables", Apparatus::Lists),
    ("list of figures", Apparatus::Lists),
    ("list of tables", Apparatus::Lists),
    ("list of illustrations", Apparatus::Lists),
    ("list of abbreviations", Apparatus::Lists),
    ("list of figures and tables", Apparatus::Lists),
    ("list of maps", Apparatus::Lists),
    ("list of plates", Apparatus::Lists),
    ("notes", Apparatus::Notes),
    ("endnotes", Apparatus::Notes),
    ("footnotes", Apparatus::Notes),
    ("bibliography", Apparatus::Bibliography),
    ("select bibliography", Apparatus::Bibliography),
    ("selected bibliography", Apparatus::Bibliography),
    ("references", Apparatus::Bibliography),
    ("works cited", Apparatus::Bibliography),
    ("further reading", Apparatus::Bibliography),
    ("index", Apparatus::Index),
    ("general index", Apparatus::Index),
    ("subject index", Apparatus::Index),
    ("name index", Apparatus::Index),
    ("author index", Apparatus::Index),
    ("index of names", Apparatus::Index),
    ("index of subjects", Apparatus::Index),
];

/// Gives every block without an apparatus the one its section names: that of
/// the innermost heading of its `section` that is one of
/// `APPARATUS_HEADINGS`, so the notes under "Notes" in a chapter, and the
/// whole of a bibliography with subheadings, are apparatus. A block keeps
/// the apparatus its reader gave it.
fn assign_apparatus(blocks: &mut [Block]) {
    for block in blocks.iter_mut().filter(|b| b.apparatus.is_none()) {
        block.apparatus = block
            .section
            .iter()
            .rev()
            .find_map(|heading| apparatus_named(heading));
    }
}

/// The apparatus that `heading` opens: the heading lowercased, with what is
/// not a letter trimmed from both ends and each run of whitespace read as one
/// space, must be one of `APPARATUS_HEADINGS`. "Notes on method" opens none.
fn apparatus_named(heading: &str) -> Option<Apparatus> {
    let trimmed = heading.trim_matches(|c: char| !c.is_alphabetic());
    let name = trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    APPARATUS_HEADINGS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|&(_, apparatus)| apparatus)
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
            apparatus: None,
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
    fn h2_only_chapters_are_siblings() {
        let mut blocks = vec![
            block(BlockKind::Heading { level: 2 }, "Ch1"),
            block(BlockKind::Paragraph, "one"),
            block(BlockKind::Heading { level: 2 }, "Ch2"),
            block(BlockKind::Paragraph, "two"),
            block(BlockKind::Heading { level: 2 }, "Ch3"),
        ];

        assign_sections(&mut blocks);

        assert_eq!(
            sections(&blocks),
            [
                ("Ch1", vec!["Ch1"]),
                ("one", vec!["Ch1"]),
                ("Ch2", vec!["Ch2"]),
                ("two", vec!["Ch2"]),
                ("Ch3", vec!["Ch3"]),
            ]
        );
    }

    #[test]
    fn skipped_levels_replace_deeper_entries() {
        let mut blocks = vec![
            block(BlockKind::Heading { level: 1 }, "A"),
            block(BlockKind::Heading { level: 3 }, "C"),
            block(BlockKind::Heading { level: 3 }, "C2"),
            block(BlockKind::Heading { level: 2 }, "B"),
            block(BlockKind::Heading { level: 4 }, "D"),
            block(BlockKind::Heading { level: 2 }, "B2"),
        ];

        assign_sections(&mut blocks);

        assert_eq!(
            sections(&blocks),
            [
                ("A", vec!["A"]),
                ("C", vec!["A", "C"]),
                ("C2", vec!["A", "C2"]),
                ("B", vec!["A", "B"]),
                ("D", vec!["A", "B", "D"]),
                ("B2", vec!["A", "B2"]),
            ]
        );
    }

    fn apparatus(blocks: &[Block]) -> Vec<(&str, Option<Apparatus>)> {
        blocks
            .iter()
            .map(|b| (b.text.as_str(), b.apparatus))
            .collect()
    }

    #[test]
    fn apparatus_comes_from_the_innermost_heading_that_names_one() {
        let heading = |level, text| block(BlockKind::Heading { level }, text);
        let paragraph = |text| block(BlockKind::Paragraph, text);
        let mut blocks = vec![
            heading(1, "CONTENTS"),
            paragraph("1 Beginnings"),
            heading(1, "List of  Figures"),
            paragraph("2.1 A figure"),
            heading(1, "1 Beginnings"),
            paragraph("Text."),
            heading(2, "Notes"),
            paragraph("1 A source."),
            heading(1, "2 Method"),
            heading(2, "Notes on method"),
            paragraph("Method text."),
            heading(1, "Bibliography"),
            heading(2, "Primary sources"),
            paragraph("Bhaskar, Roy. 1975."),
            heading(1, " Index. "),
            paragraph("absence, 12"),
        ];

        assign_sections(&mut blocks);

        use Apparatus::*;
        assert_eq!(
            apparatus(&blocks),
            [
                ("CONTENTS", Some(Contents)),
                ("1 Beginnings", Some(Contents)),
                ("List of  Figures", Some(Lists)),
                ("2.1 A figure", Some(Lists)),
                ("1 Beginnings", None),
                ("Text.", None),
                ("Notes", Some(Notes)),
                ("1 A source.", Some(Notes)),
                ("2 Method", None),
                ("Notes on method", None),
                ("Method text.", None),
                ("Bibliography", Some(Bibliography)),
                ("Primary sources", Some(Bibliography)),
                ("Bhaskar, Roy. 1975.", Some(Bibliography)),
                (" Index. ", Some(Index)),
                ("absence, 12", Some(Index)),
            ]
        );
    }

    #[test]
    fn apparatus_the_markup_gave_is_kept() {
        let mut blocks = vec![
            block(BlockKind::Heading { level: 1 }, "Notes"),
            Block {
                apparatus: Some(Apparatus::Bibliography),
                ..block(BlockKind::Paragraph, "Bhaskar, Roy. 1975.")
            },
        ];

        assign_sections(&mut blocks);

        assert_eq!(blocks[1].apparatus, Some(Apparatus::Bibliography));
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
