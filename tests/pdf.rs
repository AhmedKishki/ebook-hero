use std::fs;
use std::path::PathBuf;

use ebook_hero::Error;
use ebook_hero::document::{Block, BlockKind, Document, Format, Locator, Warning};
use ebook_hero::extract::extract;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read(name: &str) -> Document {
    extract(&fixture(name), "abc").unwrap()
}

fn page_of(block: &Block) -> u32 {
    match &block.locator {
        Locator::Pdf { page, .. } => *page,
        other => panic!("not a PDF locator: {other:?}"),
    }
}

fn label_of(block: &Block) -> Option<&str> {
    match &block.locator {
        Locator::Pdf { page_label, .. } => page_label.as_deref(),
        other => panic!("not a PDF locator: {other:?}"),
    }
}

fn containing<'a>(doc: &'a Document, needle: &str) -> &'a Block {
    doc.blocks
        .iter()
        .find(|b| b.text.contains(needle))
        .unwrap_or_else(|| panic!("no block contains {needle:?}"))
}

#[test]
fn metadata_and_page_count() {
    let doc = read("sample.pdf");

    assert_eq!(doc.source.sha256, "abc");
    assert_eq!(doc.source.file_name, "sample.pdf");
    assert_eq!(doc.source.format, Format::Pdf);
    assert_eq!(doc.metadata.title.as_deref(), Some("Sample Book"));
    assert_eq!(doc.metadata.authors, ["Ada Example"]);
    assert_eq!(doc.metadata.page_count, Some(5));
    assert_eq!(doc.metadata.spine_count, None);
    assert!(doc.warnings.is_empty());
}

#[test]
fn page_labels() {
    let doc = read("sample.pdf");

    let first = &doc.blocks[0];
    assert_eq!(page_of(first), 1);
    assert_eq!(label_of(first), Some("i"));
    let body = doc.blocks.iter().find(|b| page_of(b) == 2).unwrap();
    assert_eq!(label_of(body), Some("1"));
    let last = doc.blocks.iter().find(|b| page_of(b) == 5).unwrap();
    assert_eq!(label_of(last), Some("4"));
}

#[test]
fn outline_from_bookmarks() {
    let doc = read("sample.pdf");

    let entries: Vec<_> = doc
        .outline
        .iter()
        .map(|e| (e.title.as_str(), e.level))
        .collect();
    assert_eq!(
        entries,
        [("Entropy", 1), ("Joint entropy", 2), ("Channels", 1)]
    );
    let ids: Vec<_> = doc.outline.iter().map(|e| e.id).collect();
    assert_eq!(ids, [0, 1, 2]);
    for (entry, level) in doc.outline.iter().zip([1, 2, 1]) {
        let block = &doc.blocks[entry.block_id.unwrap() as usize];
        assert_eq!(block.id, entry.block_id.unwrap());
        assert_eq!(block.text, entry.title);
        assert_eq!(block.kind, BlockKind::Heading { level });
    }
    let headings = doc
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Heading { .. }))
        .count();
    assert_eq!(headings, 3);
}

#[test]
fn block_ids_follow_block_order() {
    let doc = read("sample.pdf");

    for (index, block) in doc.blocks.iter().enumerate() {
        assert_eq!(block.id as usize, index);
    }
}

#[test]
fn sections_follow_headings() {
    let doc = read("sample.pdf");

    let joint = containing(&doc, "Joint entropy is the entropy of two variables");
    assert_eq!(joint.section, ["Entropy", "Joint entropy"]);
    let chain = containing(&doc, "The chain rule ties");
    assert_eq!(chain.section, ["Entropy", "Joint entropy"]);
    let columns = containing(&doc, "Alpha column");
    assert_eq!(columns.section, ["Entropy", "Joint entropy"]);
    let capacity = containing(&doc, "noisy channel coding theorem");
    assert_eq!(capacity.section, ["Channels"]);
    let title_page = &doc.blocks[0];
    assert!(title_page.section.is_empty());
}

#[test]
fn running_headers_are_furniture() {
    let doc = read("sample.pdf");

    assert!(doc.blocks.iter().all(|b| !b.text.contains("SAMPLE BOOK")));
    let headers: Vec<_> = doc
        .furniture
        .iter()
        .filter(|b| b.text == "SAMPLE BOOK")
        .collect();
    assert_eq!(
        headers.iter().map(|b| page_of(b)).collect::<Vec<_>>(),
        [2, 3, 4, 5]
    );
    assert!(headers.iter().all(|b| b.kind == BlockKind::Paragraph));
    let folios: Vec<_> = doc
        .furniture
        .iter()
        .filter(|b| b.text.chars().all(|c| c.is_ascii_digit()))
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(folios, ["1", "2", "3", "4"]);
    assert!(doc.blocks.iter().all(|b| b.text.len() > 2));
}

#[test]
fn hyphenation() {
    let doc = read("sample.pdf");

    assert!(doc.blocks.iter().any(|b| b.text.contains("information")));
    assert!(doc.blocks.iter().all(|b| !b.text.contains("infor-")));
    assert!(doc.blocks.iter().all(|b| !b.text.contains("infor- ")));
    let evident = doc
        .blocks
        .iter()
        .filter(|b| b.text.contains("self-evident"))
        .count();
    assert_eq!(evident, 2);
    // Words that TeX hyphenated at a line end are whole again.
    assert!(doc.blocks.iter().any(|b| b.text.contains("probability.")));
    assert!(doc.blocks.iter().any(|b| b.text.contains("variables is")));
}

#[test]
fn numbers_are_not_split() {
    let doc = read("sample.pdf");

    assert!(
        doc.blocks
            .iter()
            .any(|b| b.text.contains("figure 1996 below"))
    );
    assert!(
        doc.blocks
            .iter()
            .any(|b| b.text.contains("In 1948 the idea"))
    );
}

#[test]
fn two_column_page_is_not_mixed() {
    let doc = read("sample.pdf");

    let alpha = containing(&doc, "Alpha column");
    let omega = containing(&doc, "Omega column");
    assert!(!alpha.text.contains("Omega"));
    assert!(!omega.text.contains("Alpha"));
    assert!(alpha.text.ends_with("no code can do better on average."));
    assert!(omega.text.ends_with("a joint code can collect it."));
    assert!(alpha.id < omega.id);
    assert_eq!(page_of(alpha), 4);
    assert_eq!(page_of(omega), 4);
    assert_eq!(doc.blocks.iter().filter(|b| page_of(b) == 4).count(), 2);
}

#[test]
fn paragraph_locator_has_page_and_bbox() {
    let doc = read("sample.pdf");

    let block = containing(&doc, "Joint entropy is the entropy of two variables");
    let Locator::Pdf {
        page,
        page_end,
        page_label,
        bbox: Some([left, top, right, bottom]),
    } = block.locator.clone()
    else {
        panic!("expected a PDF locator with a box, got {:?}", block.locator);
    };
    assert_eq!(page, 2);
    assert_eq!(page_end, None);
    assert_eq!(page_label.as_deref(), Some("1"));
    // Letter paper, top-left origin: the text block starts 133.8 pt from the
    // left and is 345 pt wide.
    assert!((left - 133.8).abs() < 1.0, "left {left}");
    assert!((right - 477.5).abs() < 1.0, "right {right}");
    assert!(
        0.0 < top && top < bottom && bottom < 792.0,
        "{top} {bottom}"
    );
    let entropy = containing(&doc, "Entropy measures how much surprise");
    let Locator::Pdf {
        bbox: Some([_, entropy_top, _, entropy_bottom]),
        ..
    } = entropy.locator
    else {
        panic!("expected a box");
    };
    assert!(entropy_bottom < top, "paragraphs run top to bottom");
    assert!(entropy_top < entropy_bottom);
}

#[test]
fn image_only_pdf_has_no_text_layer() {
    let error = extract(&fixture("imageonly.pdf"), "abc").unwrap_err();

    assert!(matches!(error, Error::NoTextLayer { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .ends_with("no text layer; OCR is not supported")
    );
}

#[test]
fn mixed_pdf_warns_about_pages_without_text() {
    let doc = read("mixed.pdf");

    assert_eq!(doc.metadata.page_count, Some(2));
    assert_eq!(doc.warnings, [Warning::PagesWithoutText { pages: vec![2] }]);
    assert!(doc.outline.is_empty());
    assert!(doc.blocks.iter().all(|b| b.section.is_empty()));
    assert!(doc.blocks.iter().all(|b| page_of(b) == 1));
    assert!(doc.blocks.iter().all(|b| label_of(b).is_none()));
    assert!(
        doc.blocks
            .iter()
            .any(|b| b.text.starts_with("This first page has a text layer."))
    );
}

#[test]
fn corrupt_pdf_is_unreadable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("broken.pdf");
    fs::write(&path, b"%PDF-1.7\n garbage").unwrap();

    let error = extract(&path, "abc").unwrap_err();

    match error {
        Error::Unreadable { path: reported, .. } => assert_eq!(reported, path),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[test]
fn password_protected_pdf_is_unreadable_not_empty() {
    let error = extract(&fixture("locked.pdf"), "abc").unwrap_err();

    match error {
        Error::Unreadable { reason, .. } => assert_eq!(reason, "password required"),
        other => panic!("expected Unreadable, got {other:?}"),
    }
}
