//! Reading Markdown files through the public library: detection by content,
//! `extract`, and the usage guide as a book.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ebook_hero::Error;
use ebook_hero::document::{
    BlockKind, Container, Format, LinkTarget, Locator, SpanKind, Warning, span_errors,
};
use ebook_hero::extract::{detect_format, extract};
use ebook_hero::normalize::normalize;
use tempfile::TempDir;

const USAGE_GUIDE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/usage.md");

/// The Markdown files in `docs/`, the usage guide among them.
fn docs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    files
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

// Review Focus 4
#[test]
fn a_latin1_text_file_is_unsupported_and_names_utf8() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "cafe.txt", b"caf\xE9\n");

    let error = detect_format(&path).unwrap_err();

    assert!(matches!(error, Error::Unsupported { .. }), "{error:?}");
    let m = error.to_string();
    assert!(m.contains("a PDF, an EPUB or a UTF-8 text file"), "{m}");
    assert!(m.starts_with(&path.display().to_string()), "{m}");
}

#[test]
fn utf8_with_or_without_a_byte_order_mark_is_markdown_and_utf16_is_not() {
    let dir = TempDir::new().unwrap();
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "# T\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }

    for (name, bytes) in [
        ("plain.md", &b"# T\n"[..]),
        ("bom.md", b"\xEF\xBB\xBF# T\n"),
        ("blank.md", b" \n\n"),
        ("crlf.txt", b"# T\r\n"),
    ] {
        assert_eq!(
            detect_format(&write(&dir, name, bytes)).unwrap(),
            Format::Markdown,
            "{name}"
        );
    }
    assert!(matches!(
        detect_format(&write(&dir, "utf16.md", &utf16)),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn extract_reads_a_text_file_as_markdown_whatever_its_name() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "notes.txt", b"# Notes\n\n- one\n- two\n");

    let doc = extract(&path, "abc").unwrap();

    assert_eq!(doc.source.format, Format::Markdown);
    assert_eq!(doc.source.file_name, "notes.txt");
    assert_eq!(doc.source.sha256, "abc");
    let texts: Vec<_> = doc.blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["Notes", "one", "two"]);
    assert_eq!(doc.outline.len(), 1);
}

// Review Focus 3
#[test]
fn an_empty_markdown_file_cannot_be_read_and_names_its_path() {
    let dir = TempDir::new().unwrap();
    for (name, bytes) in [
        ("empty.md", &b""[..]),
        ("blank.md", b"\xEF\xBB\xBF \r\n\t\n"),
    ] {
        let path = write(&dir, name, bytes);

        let error = extract(&path, "abc").unwrap_err();

        assert!(matches!(error, Error::Unreadable { .. }), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!("{}: could not be read: no readable text", path.display())
        );
    }
}

#[test]
fn a_text_file_over_the_cap_reaches_the_reader_and_fails_with_its_size() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big.md");
    // A sparse file of NUL bytes, which are UTF-8.
    let size = 64 * 1024 * 1024 + 1;
    fs::File::create(&path).unwrap().set_len(size).unwrap();

    assert_eq!(detect_format(&path).unwrap(), Format::Markdown);
    let error = extract(&path, "abc").unwrap_err();

    assert!(matches!(error, Error::Unreadable { .. }), "{error:?}");
    let m = error.to_string();
    assert!(m.contains(&format!("{size} bytes")), "{m}");
    assert!(m.contains("64 MiB"), "{m}");
}

/// The rules every Markdown document keeps, checked on the usage guide: text
/// normalized outside code, well-formed spans, lines that rise, and containers
/// that each hold a block and nest in the order they open.
#[test]
fn the_usage_guide_reads_with_its_structure() {
    let doc = extract(Path::new(USAGE_GUIDE), "abc").unwrap();

    assert_eq!(doc.source.format, Format::Markdown);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    assert_eq!(doc.metadata.title, None);
    let headings = doc
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Heading { .. }))
        .count();
    assert_eq!(doc.outline.len(), headings);
    assert_eq!(doc.sections.len(), headings);
    assert!(
        doc.blocks
            .iter()
            .any(|b| matches!(b.kind, BlockKind::Code { .. }))
    );
    assert!(doc.containers.contains(&Container::Table));
    assert!(
        doc.containers
            .iter()
            .any(|c| matches!(c, Container::List { .. }))
    );

    let mut previous_line = 0;
    let mut held = BTreeSet::new();
    for block in &doc.blocks {
        if !matches!(block.kind, BlockKind::Code { .. }) {
            assert_eq!(block.text, normalize(&block.text), "block {}", block.id);
        }
        assert_eq!(span_errors(block), Vec::<String>::new());
        let Locator::Markdown { line, line_end } = block.locator else {
            panic!("block {} has {:?}", block.id, block.locator);
        };
        assert!(line >= previous_line, "block {} at line {line}", block.id);
        assert!(line_end.is_none_or(|end| end > line), "block {}", block.id);
        previous_line = line;
        assert!(
            block.within.windows(2).all(|pair| pair[0] < pair[1]),
            "block {}: {:?}",
            block.id,
            block.within
        );
        held.extend(block.within.iter().copied());
    }
    let every: BTreeSet<u32> = (0..doc.containers.len() as u32).collect();
    assert_eq!(held, every, "every container holds a block");
}

#[test]
fn the_usage_guide_has_no_broken_link() {
    let d = extract(Path::new(USAGE_GUIDE), "abc").unwrap();

    assert!(
        !d.warnings
            .iter()
            .any(|w| matches!(w, Warning::BrokenLink { .. })),
        "{:?}",
        d.warnings
    );
    let into_the_guide = d
        .blocks
        .iter()
        .flat_map(|b| &b.spans)
        .filter(|s| {
            matches!(
                s.kind,
                SpanKind::Link {
                    to: LinkTarget::Block { .. },
                    ..
                }
            )
        })
        .count();
    assert!(into_the_guide > 0);
}

#[test]
fn every_file_link_in_docs_names_a_file_that_exists() {
    let mut links = 0;
    for file in docs() {
        let d = extract(&file, "abc").unwrap();
        for block in &d.blocks {
            for span in &block.spans {
                if let SpanKind::Link {
                    to: LinkTarget::File { path },
                    ..
                } = &span.kind
                {
                    links += 1;
                    assert!(
                        file.parent().unwrap().join(path).exists(),
                        "{}, block {}: {path} does not exist",
                        file.display(),
                        block.id
                    );
                }
            }
        }
    }
    assert!(links > 0);
}

#[test]
fn every_block_of_every_doc_has_well_formed_spans() {
    for file in docs() {
        let d = extract(&file, "abc").unwrap();
        for block in &d.blocks {
            assert_eq!(
                span_errors(block),
                Vec::<String>::new(),
                "{}, block {}",
                file.display(),
                block.id
            );
        }
    }
}
