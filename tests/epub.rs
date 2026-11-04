mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{EpubBuilder, EpubVersion};
use ebook_hero::Error;
use ebook_hero::document::{Apparatus, BlockKind, Document, Format, Locator, Warning};
use ebook_hero::extract::{detect_format, extract};
use tempfile::TempDir;

fn book_path(dir: &TempDir) -> PathBuf {
    dir.path().join("book.epub")
}

fn two_chapter_book(version: EpubVersion) -> EpubBuilder {
    EpubBuilder::new(version)
        .title("Test Book")
        .author("A. Author")
        .chapter(
            "ch1.xhtml",
            r#"<h1 id="c1">Chapter One</h1><p>Alpha text.</p><h2 id="s">Sub</h2><p>Beta text.</p>"#,
        )
        .chapter("ch2.xhtml", "<h1>Chapter Two</h1><p>Gamma.</p>")
        .toc("Chapter One", "ch1.xhtml#c1", 1)
        .toc("Sub", "ch1.xhtml#s", 2)
        .toc("Chapter Two", "ch2.xhtml", 1)
}

fn read(builder: &EpubBuilder) -> Document {
    let dir = TempDir::new().unwrap();
    let path = book_path(&dir);
    builder.write(&path);
    extract(&path, "abc").unwrap()
}

fn texts(doc: &Document) -> Vec<&str> {
    doc.blocks.iter().map(|b| b.text.as_str()).collect()
}

#[test]
fn epub3_metadata_outline_sections_locators() {
    let dir = TempDir::new().unwrap();
    let path = book_path(&dir);
    two_chapter_book(EpubVersion::V3).write(&path);

    let doc = extract(&path, "abc").unwrap();

    assert_eq!(doc.source.sha256, "abc");
    assert_eq!(doc.source.file_name, "book.epub");
    assert_eq!(doc.source.format, Format::Epub);
    assert_eq!(doc.metadata.title.as_deref(), Some("Test Book"));
    assert_eq!(doc.metadata.authors, ["A. Author"]);
    assert_eq!(doc.metadata.language.as_deref(), Some("en"));
    assert_eq!(doc.metadata.page_count, None);
    assert_eq!(doc.metadata.spine_count, Some(2));
    let titles: Vec<_> = doc
        .outline
        .iter()
        .map(|e| (e.title.as_str(), e.level))
        .collect();
    assert_eq!(titles, [("Chapter One", 1), ("Sub", 2), ("Chapter Two", 1)]);
    let outline_ids: Vec<_> = doc.outline.iter().map(|e| e.id).collect();
    assert_eq!(outline_ids, [0, 1, 2]);
    let beta = doc.blocks.iter().find(|b| b.text == "Beta text.").unwrap();
    assert_eq!(beta.section, ["Chapter One", "Sub"]);
    assert_eq!(
        beta.locator,
        Locator::Epub {
            href: "ch1.xhtml".into(),
            anchor: Some("s".into()),
            para: 4,
            print_page: None
        }
    );
    assert_eq!(
        doc.blocks[doc.outline[1].block_id.unwrap() as usize].text,
        "Sub"
    );
    assert_eq!(
        doc.blocks[doc.outline[0].block_id.unwrap() as usize].text,
        "Chapter One"
    );
    // A target without a fragment is the first block of its file.
    assert_eq!(
        doc.blocks[doc.outline[2].block_id.unwrap() as usize].text,
        "Chapter Two"
    );
    for (i, block) in doc.blocks.iter().enumerate() {
        assert_eq!(block.id as usize, i);
    }
    let gamma = doc.blocks.iter().find(|b| b.text == "Gamma.").unwrap();
    assert_eq!(gamma.section, ["Chapter Two"]);
    assert_eq!(
        gamma.locator,
        Locator::Epub {
            href: "ch2.xhtml".into(),
            anchor: None,
            para: 2,
            print_page: None
        }
    );
    let sub = doc.blocks.iter().find(|b| b.text == "Sub").unwrap();
    assert_eq!(sub.kind, BlockKind::Heading { level: 2 });
    assert_eq!(sub.section, ["Chapter One", "Sub"]);
    assert!(doc.warnings.is_empty());
    assert!(doc.furniture.is_empty());
}

#[test]
fn epub2_ncx_gives_the_same_outline() {
    let v2 = read(&two_chapter_book(EpubVersion::V2));
    let v3 = read(&two_chapter_book(EpubVersion::V3));

    let titles: Vec<_> = v2
        .outline
        .iter()
        .map(|e| (e.title.as_str(), e.level))
        .collect();
    assert_eq!(titles, [("Chapter One", 1), ("Sub", 2), ("Chapter Two", 1)]);
    assert_eq!(v2.outline, v3.outline);
    assert_eq!(v2.blocks, v3.blocks);
    assert_eq!(v2.metadata, v3.metadata);
    assert_eq!(v2.metadata.language.as_deref(), Some("en"));
}

#[test]
fn nonlinear_items_are_read_in_spine_position() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<p>First.</p>")
            .nonlinear_chapter("notes.xhtml", "<p>Aside.</p>")
            .chapter("ch2.xhtml", "<p>Last.</p>"),
    );

    assert_eq!(texts(&doc), ["First.", "Aside.", "Last."]);
    assert_eq!(doc.metadata.spine_count, Some(3));
    let hrefs: Vec<_> = doc
        .blocks
        .iter()
        .map(|b| match &b.locator {
            Locator::Epub { href, .. } => href.as_str(),
            other => panic!("not an EPUB locator: {other:?}"),
        })
        .collect();
    assert_eq!(hrefs, ["ch1.xhtml", "notes.xhtml", "ch2.xhtml"]);
}

#[test]
fn print_page_carries_into_the_next_file() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter(
                "ch1.xhtml",
                r#"<p>A</p><span epub:type="pagebreak" title="45"/><p>B</p>"#,
            )
            .chapter("ch2.xhtml", "<p>C</p>"),
    );

    let page = |text: &str| match &doc.blocks.iter().find(|b| b.text == text).unwrap().locator {
        Locator::Epub { print_page, .. } => print_page.clone(),
        other => panic!("not an EPUB locator: {other:?}"),
    };
    assert_eq!(page("A"), None);
    assert_eq!(page("B"), Some("45".to_string()));
    assert_eq!(page("C"), Some("45".to_string()));
}

#[test]
fn obfuscated_fonts_do_not_block_reading() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<p>Readable.</p>")
            .obfuscated_font(),
    );

    assert_eq!(texts(&doc), ["Readable."]);
    assert!(doc.warnings.is_empty());
}

#[test]
fn missing_content_document_warns() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<p>Before.</p>")
            .missing_chapter("ch9.xhtml")
            .chapter("ch2.xhtml", "<p>After.</p>"),
    );

    assert_eq!(texts(&doc), ["Before.", "After."]);
    assert_eq!(doc.blocks[1].id, 1);
    assert_eq!(doc.metadata.spine_count, Some(3));
    match doc.warnings.as_slice() {
        [Warning::UnreadableContent { href, reason }] => {
            assert_eq!(href, "ch9.xhtml");
            assert!(!reason.is_empty());
        }
        other => panic!("unexpected warnings: {other:?}"),
    }
}

#[test]
fn epub_without_text_is_unreadable() {
    let dir = TempDir::new().unwrap();
    let path = book_path(&dir);
    EpubBuilder::new(EpubVersion::V3)
        .chapter("ch1.xhtml", r#"<img src="a.png" alt=""/>"#)
        .write(&path);

    let error = extract(&path, "abc").unwrap_err();

    assert!(matches!(error, Error::Unreadable { .. }), "{error:?}");
    assert!(error.to_string().contains("no readable text"), "{error}");
}

#[test]
fn epub_without_toc_has_empty_outline_but_sections() {
    for version in [EpubVersion::V2, EpubVersion::V3] {
        let doc = read(&EpubBuilder::new(version).chapter(
            "ch1.xhtml",
            "<h1>Intro</h1><p>Body.</p><h2>Detail</h2><p>More.</p><h1>Next</h1><p>End.</p>",
        ));

        assert!(doc.outline.is_empty(), "{version:?}");
        let sections: Vec<_> = doc
            .blocks
            .iter()
            .map(|b| (b.text.as_str(), b.section.clone()))
            .collect();
        assert_eq!(
            sections,
            [
                ("Intro", vec!["Intro".to_string()]),
                ("Body.", vec!["Intro".to_string()]),
                ("Detail", vec!["Intro".to_string(), "Detail".to_string()]),
                ("More.", vec!["Intro".to_string(), "Detail".to_string()]),
                ("Next", vec!["Next".to_string()]),
                ("End.", vec!["Next".to_string()]),
            ],
            "{version:?}"
        );
    }
}

#[test]
fn toc_fragment_that_matches_no_block_falls_back_to_the_files_first_block() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<h1>One</h1><p>Text.</p>")
            .chapter(
                "ch2.xhtml",
                r#"<h1>Two</h1><p id="mid">Middle.</p><span id="tail"/>"#,
            )
            .toc("Middle", "ch2.xhtml#mid", 1)
            .toc("Gone", "ch2.xhtml#nothing", 1)
            .toc("Trailing", "ch2.xhtml#tail", 1)
            .toc("Bare", "ch2.xhtml", 1)
            .toc("Elsewhere", "other.xhtml", 1),
    );

    let targets: Vec<_> = doc.outline.iter().map(|e| e.block_id).collect();
    assert_eq!(targets, [Some(3), Some(2), Some(2), Some(2), None]);
}

#[test]
fn percent_encoded_hrefs_and_fragments_are_decoded() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter(
                "my chapter.xhtml",
                r#"<h1 id="x">One</h1><h2 id="ب">Two</h2>"#,
            )
            .toc("One", "my%20chapter.xhtml#x", 1)
            .toc("Two", "my%20chapter.xhtml#%D8%A8", 2),
    );

    let targets: Vec<_> = doc.outline.iter().map(|e| e.block_id).collect();
    assert_eq!(targets, [Some(0), Some(1)]);
    match &doc.blocks[1].locator {
        Locator::Epub { href, anchor, .. } => {
            assert_eq!(href, "my chapter.xhtml");
            assert_eq!(anchor.as_deref(), Some("ب"));
        }
        other => panic!("not an EPUB locator: {other:?}"),
    }
}

#[test]
fn a_paragraph_that_a_marker_links_to_and_that_links_back_is_a_note() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V2)
            .chapter(
                "Text/ch1.xhtml",
                r##"<h1>One<a id="r4" href="notes.xhtml#n4">4</a></h1><p>Claim<a id="r1" href="notes.xhtml#n1"><sup>1</sup></a> and more<a id="r2" href="../Text/notes%20b.xhtml#n2">2</a>, then <a id="r5" href="notes.xhtml#n5">the fifth note</a>.</p><p>A <a href="#r1">link to the marker</a>.</p>"##,
            )
            .chapter(
                "Text/notes.xhtml",
                r##"<h1>Endmatter</h1><p id="n1"><a href="ch1.xhtml#r1">1</a> Source one.</p><p id="n3">Links to a marker that points elsewhere <a href="ch1.xhtml#r1">1</a>.</p><p id="n4"><a href="ch1.xhtml#r4">4</a> Marked in a heading.</p><p id="n5"><a href="ch1.xhtml#r5">5</a> Marked by long text.</p>"##,
            )
            .chapter(
                "Text/notes b.xhtml",
                r##"<div id="n2"><p><a href="/OEBPS/Text/ch1.xhtml#r2">2</a> Source two.</p></div>"##,
            ),
    );

    let kinds: Vec<_> = doc
        .blocks
        .iter()
        .map(|b| (b.kind.clone(), b.text.as_str()))
        .collect();
    let note = |label: &str| BlockKind::Note {
        label: Some(label.to_string()),
    };
    assert_eq!(
        kinds,
        [
            (BlockKind::Heading { level: 1 }, "One4"),
            (
                BlockKind::Paragraph,
                "Claim1 and more2, then the fifth note."
            ),
            (BlockKind::Paragraph, "A link to the marker."),
            (BlockKind::Heading { level: 1 }, "Endmatter"),
            (note("n1"), "1 Source one."),
            (
                BlockKind::Paragraph,
                "Links to a marker that points elsewhere 1."
            ),
            (BlockKind::Paragraph, "4 Marked in a heading."),
            (BlockKind::Paragraph, "5 Marked by long text."),
            (note("n2"), "2 Source two."),
        ]
    );
}

#[test]
fn a_list_of_figures_that_links_to_its_captions_and_back_makes_no_notes() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V2)
            .chapter(
                "figures.xhtml",
                r##"<h1>Figures</h1><p><a id="lf1" href="ch1.xhtml#f1">1.1</a> The model</p>"##,
            )
            .chapter(
                "ch1.xhtml",
                r##"<h1>One</h1><p id="f1"><a href="figures.xhtml#lf1">FIGURE 1.1</a> The model</p>"##,
            ),
    );

    let caption = doc.blocks.iter().find(|b| b.text == "FIGURE 1.1 The model");
    assert_eq!(caption.unwrap().kind, BlockKind::Paragraph);
}

#[test]
fn headings_and_markup_set_apart_notes_bibliography_and_index() {
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter(
                "ch1.xhtml",
                "<h1>One</h1><p>Text.</p><h2>Notes</h2><p>1 A source.</p>",
            )
            .chapter(
                "bib.xhtml",
                "<h1>BIBLIOGRAPHY</h1><p>Bhaskar, Roy. 1975.</p>",
            )
            .chapter(
                "ix.xhtml",
                r#"<section epub:type="index"><h1>Words</h1><p>absence, 12</p></section>"#,
            ),
    );

    let apparatus: Vec<_> = doc
        .blocks
        .iter()
        .map(|b| (b.text.as_str(), b.apparatus))
        .collect();
    assert_eq!(
        apparatus,
        [
            ("One", None),
            ("Text.", None),
            ("Notes", Some(Apparatus::Notes)),
            ("1 A source.", Some(Apparatus::Notes)),
            ("BIBLIOGRAPHY", Some(Apparatus::Bibliography)),
            ("Bhaskar, Roy. 1975.", Some(Apparatus::Bibliography)),
            ("Words", Some(Apparatus::Index)),
            ("absence, 12", Some(Apparatus::Index)),
        ]
    );
}

#[test]
fn oversized_content_document_warns() {
    // One byte of body past the 32 MiB cap is enough, since the wrapper adds more.
    let big = format!("<p>{}</p>", "a".repeat(32 * 1024 * 1024));
    let doc = read(
        &EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<p>Before.</p>")
            .chapter("big.xhtml", &big)
            .chapter("ch2.xhtml", "<p>After.</p>"),
    );

    assert_eq!(
        doc.warnings,
        [Warning::UnreadableContent {
            href: "big.xhtml".to_string(),
            reason: "content document exceeds 32 MiB".to_string(),
        }]
    );
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(texts(&doc), ["Before.", "After."]);
}

#[test]
fn corrupt_epub_is_unreadable() {
    const CONTAINER: &[u8] = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
    let dir = TempDir::new().unwrap();
    let garbage_opf = dir.path().join("garbage.epub");
    write_zip(
        &garbage_opf,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", b"\x00\xFF this is <<< not a package"),
        ],
    );
    let missing_opf = dir.path().join("missing-opf.epub");
    write_zip(
        &missing_opf,
        &[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
        ],
    );

    for path in [garbage_opf, missing_opf] {
        let error = extract(&path, "abc").unwrap_err();

        assert!(matches!(error, Error::Unreadable { .. }), "{error:?}");
        assert!(
            error.to_string().starts_with(&path.display().to_string()),
            "{error}"
        );
    }
}

#[test]
fn detects_formats() {
    let dir = TempDir::new().unwrap();
    let write = |name: &str, bytes: &[u8]| -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    };
    let detect = |path: &Path| detect_format(path);

    let pdf = write("a.pdf", b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n");
    assert_eq!(detect(&pdf).unwrap(), Format::Pdf);
    // The name does not matter, and the marker may follow a short preamble.
    let mut padded = vec![b' '; 1000];
    padded.extend_from_slice(b"%PDF-1.4");
    assert_eq!(detect(&write("noext", &padded)).unwrap(), Format::Pdf);
    let mut late = vec![b' '; 1024];
    late.extend_from_slice(b"%PDF-1.4");
    assert!(matches!(
        detect(&write("late.pdf", &late)),
        Err(Error::Unsupported { .. })
    ));

    let epub = dir.path().join("b.dat");
    two_chapter_book(EpubVersion::V3).write(&epub);
    assert_eq!(detect(&epub).unwrap(), Format::Epub);
    let epub2 = dir.path().join("c.bin");
    two_chapter_book(EpubVersion::V2).write(&epub2);
    assert_eq!(detect(&epub2).unwrap(), Format::Epub);

    let text = write("note.txt", b"hello");
    assert!(matches!(detect(&text), Err(Error::Unsupported { .. })));
    let empty = write("empty.epub", b"");
    assert!(matches!(detect(&empty), Err(Error::Unsupported { .. })));
    let zip_only = dir.path().join("other.zip");
    write_zip(&zip_only, &[("mimetype", b"text/plain"), ("a.txt", b"x")]);
    assert!(matches!(detect(&zip_only), Err(Error::Unsupported { .. })));

    // Either the mimetype entry or the container file is enough.
    let by_mimetype = dir.path().join("mime.zip");
    write_zip(&by_mimetype, &[("mimetype", b"application/epub+zip")]);
    assert_eq!(detect(&by_mimetype).unwrap(), Format::Epub);
    let by_container = dir.path().join("container.zip");
    write_zip(
        &by_container,
        &[("META-INF/container.xml", b"<container/>")],
    );
    assert_eq!(detect(&by_container).unwrap(), Format::Epub);

    assert!(matches!(
        detect(dir.path()),
        Err(Error::NotAFile { path }) if path == dir.path()
    ));
    let missing = dir.path().join("missing.epub");
    assert!(matches!(
        detect(&missing),
        Err(Error::NotFound { path }) if path == missing
    ));
}

#[test]
fn extract_reports_the_error_of_a_file_that_is_not_a_book() {
    let dir = TempDir::new().unwrap();
    let text = dir.path().join("note.txt");
    fs::write(&text, b"hello").unwrap();

    assert!(matches!(
        extract(&text, "abc"),
        Err(Error::Unsupported { .. })
    ));
    assert!(matches!(
        extract(&dir.path().join("none.epub"), "abc"),
        Err(Error::NotFound { .. })
    ));
}

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}
