mod common;

use std::path::PathBuf;

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::document::Format;
use ebook_hero::passages::{HeadingPlace, PassageLocator};
use ebook_hero::read::{ReadAt, ReadParams, ReadResult};
use ebook_hero::session::Session;
use ebook_hero::settings::{READ_MAX_CHARS, READ_NEIGHBOURS_MAX};
use tempfile::TempDir;

/// A book on disk and a session with a private cache for it.
struct Fixture {
    _root: TempDir,
    _books: TempDir,
    session: Session,
    path: String,
}

impl Fixture {
    fn epub(builder: EpubBuilder) -> Fixture {
        let books = TempDir::new().unwrap();
        let path = books.path().join("book.epub");
        builder.write(&path);
        Fixture::at(books, path)
    }

    fn sample_pdf() -> Fixture {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf");
        Fixture::at(TempDir::new().unwrap(), path)
    }

    /// An EPUB with one chapter per entry of `paragraphs`, each under its own
    /// heading, so that entry `i` is passage `i`.
    fn chapters(paragraphs: &[&str]) -> Fixture {
        let mut builder = EpubBuilder::new(EpubVersion::V3).title("Reading Book");
        for (i, text) in paragraphs.iter().enumerate() {
            let href = format!("ch{i}.xhtml");
            builder = builder
                .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
                .toc(&format!("Part {i}"), &href, 1);
        }
        Fixture::epub(builder)
    }

    /// Passages 0 to 2 are in `ch1.xhtml`, passage 3 is in `ch2.xhtml`.
    ///
    /// ```text
    /// 0 One           (level 1)  passage 0
    /// 1   One A       (level 2)  passage 1
    /// 2   One B       (level 2)  passage 2
    /// 3 Two           (level 1)  passage 3
    /// ```
    fn nested() -> Fixture {
        Fixture::epub(
            EpubBuilder::new(EpubVersion::V3)
                .title("Nested Book")
                .author("Ada Example")
                .chapter(
                    "ch1.xhtml",
                    r#"<h1>One</h1><p>Alpha opens the first part.</p>
<h2 id="a">One A</h2><p>Bravo sits in the first section.</p>
<h2 id="b">One B</h2><p>Charlie sits in the second section.</p>"#,
                )
                .chapter(
                    "ch2.xhtml",
                    "<h1>Two</h1><p>Delta starts the second part.</p>",
                )
                .toc("One", "ch1.xhtml", 1)
                .toc("One A", "ch1.xhtml#a", 2)
                .toc("One B", "ch1.xhtml#b", 2)
                .toc("Two", "ch2.xhtml", 1),
        )
    }

    fn at(books: TempDir, path: PathBuf) -> Fixture {
        let root = private_tempdir();
        Fixture {
            session: Session::with_cache_root(root.path().to_path_buf()),
            path: path.to_str().unwrap().to_string(),
            _root: root,
            _books: books,
        }
    }

    fn read(&self, at: ReadAt, before: u32, after: u32) -> Result<ReadResult, Error> {
        self.session.read(
            &self.path,
            ReadParams {
                at,
                before,
                after,
                cursor: None,
            },
        )
    }
}

fn ids(result: &ReadResult) -> Vec<u32> {
    result.passages.iter().map(|p| p.passage_id).collect()
}

fn invalid(result: Result<ReadResult, Error>) -> String {
    match result {
        Err(Error::InvalidArgument(message)) => message,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn reads_a_passage_with_neighbours() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two.", "Three.", "Four."]);

    let result = fixture.read(ReadAt::Passage(2), 1, 1).unwrap();

    assert_eq!(ids(&result), [1, 2, 3]);
    assert_eq!(result.next_cursor, None);
    assert!(
        result.markdown.contains("<!-- passage 2 ·"),
        "{}",
        result.markdown
    );
    assert!(result.markdown.contains("<!-- passage 1 ·"));
    assert!(result.markdown.contains("<!-- passage 3 ·"));
    assert!(!result.markdown.contains("<!-- passage 0 ·"));
    assert!(!result.markdown.contains("<!-- passage 4 ·"));
    assert_eq!(result.passages[1].section, ["Part 2"]);
    assert_eq!(
        result.passages[1].locator,
        PassageLocator::Epub {
            href: "ch2.xhtml".to_string(),
            para_start: 2,
            para_end: 2,
            print_page: None,
            under_heading: Some(HeadingPlace {
                text: "Part 2".to_string(),
                para_start: 1,
                para_end: 1,
            }),
        }
    );
    assert_eq!(
        result.markdown,
        "## Part 1\n\n<!-- passage 1 · Part 1 ¶1 -->\n\nOne.\n\n\
         ## Part 2\n\n<!-- passage 2 · Part 2 ¶1 -->\n\nTwo.\n\n\
         ## Part 3\n\n<!-- passage 3 · Part 3 ¶1 -->\n\nThree.\n"
    );
}

#[test]
fn neighbours_are_clamped_to_the_book() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two."]);

    assert_eq!(ids(&fixture.read(ReadAt::Passage(0), 5, 0).unwrap()), [0]);
    assert_eq!(
        ids(&fixture.read(ReadAt::Passage(0), 0, 10).unwrap()),
        [0, 1, 2]
    );
    assert_eq!(
        ids(&fixture.read(ReadAt::Passage(2), 10, 10).unwrap()),
        [0, 1, 2]
    );
}

#[test]
fn an_unknown_passage_is_invalid() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two."]);

    assert_eq!(
        invalid(fixture.read(ReadAt::Passage(3), 0, 0)),
        "passage 3 does not exist; the book has 3 passages"
    );
    assert_eq!(
        invalid(fixture.read(ReadAt::Passage(u32::MAX), 10, 10)),
        format!(
            "passage {} does not exist; the book has 3 passages",
            u32::MAX
        )
    );
}

#[test]
fn reads_a_whole_outline_entry() {
    let fixture = Fixture::nested();

    // A level 1 entry runs to the next level 1 entry, over its subsections.
    let one = fixture.read(ReadAt::Outline(0), 0, 0).unwrap();
    assert_eq!(ids(&one), [0, 1, 2]);
    assert!(one.markdown.contains("Alpha opens the first part."));
    assert!(one.markdown.contains("Charlie sits in the second section."));
    assert!(!one.markdown.contains("Delta"));
    assert!(one.markdown.contains("## One\n"));
    assert!(one.markdown.contains("## One › One A\n"));
    assert!(one.markdown.contains("## One › One B\n"));

    // A subsection stops at the next entry of its level or above.
    assert_eq!(ids(&fixture.read(ReadAt::Outline(1), 0, 0).unwrap()), [1]);
    assert_eq!(ids(&fixture.read(ReadAt::Outline(2), 0, 0).unwrap()), [2]);
    // The last entry runs to the end of the book.
    assert_eq!(ids(&fixture.read(ReadAt::Outline(3), 0, 0).unwrap()), [3]);

    // Neighbours widen the entry's range.
    assert_eq!(
        ids(&fixture.read(ReadAt::Outline(1), 1, 1).unwrap()),
        [0, 1, 2]
    );
    assert_eq!(
        ids(&fixture.read(ReadAt::Outline(0), 0, 1).unwrap()),
        [0, 1, 2, 3]
    );
}

#[test]
fn an_unknown_outline_entry_is_invalid() {
    let fixture = Fixture::nested();

    assert_eq!(
        invalid(fixture.read(ReadAt::Outline(4), 0, 0)),
        "outline entry 4 does not exist; the book has 4 entries"
    );
}

#[test]
fn reads_a_pdf_page() {
    let fixture = Fixture::sample_pdf();

    // Page 1 is the title page; page 2 is the first body page.
    let result = fixture.read(ReadAt::Page(2), 0, 0).unwrap();

    assert!(!result.passages.is_empty());
    for passage in &result.passages {
        match &passage.locator {
            PassageLocator::Pdf {
                page_start,
                page_end,
                ..
            } => assert!(*page_start <= 2 && 2 <= *page_end, "{passage:?}"),
            other => panic!("not a PDF locator: {other:?}"),
        }
    }
    assert!(
        result.markdown.contains("<!-- passage "),
        "{}",
        result.markdown
    );
    assert!(
        result.markdown.contains(" · p. 2 (1) -->"),
        "{}",
        result.markdown
    );
    assert_eq!(result.next_cursor, None);

    // All five pages hold text; the pages outside the book do not.
    for page in 1..=5 {
        assert!(
            fixture.read(ReadAt::Page(page), 0, 0).is_ok(),
            "page {page}"
        );
    }
    for page in [0, 6, u32::MAX] {
        assert_eq!(
            invalid(fixture.read(ReadAt::Page(page), 0, 0)),
            format!("page {page} has no text")
        );
    }
}

#[test]
fn reads_a_pdf_outline_entry_and_neighbours_of_a_page() {
    let fixture = Fixture::sample_pdf();

    let joint = fixture.read(ReadAt::Outline(1), 0, 0).unwrap();
    assert!(!joint.passages.is_empty());
    assert!(
        joint
            .passages
            .iter()
            .all(|p| p.section == ["Entropy", "Joint entropy"]),
        "{:?}",
        joint.passages
    );
    // "Entropy" holds "Joint entropy"; "Channels" follows both.
    let entropy = fixture.read(ReadAt::Outline(0), 0, 0).unwrap();
    assert!(ids(&entropy).len() > ids(&joint).len());
    assert!(entropy.passages.iter().all(|p| p.section[0] == "Entropy"));
    let channels = fixture.read(ReadAt::Outline(2), 0, 0).unwrap();
    assert!(channels.passages.iter().all(|p| p.section == ["Channels"]));
    assert_eq!(
        entropy.passages.last().unwrap().passage_id + 1,
        channels.passages[0].passage_id
    );

    let page = fixture.read(ReadAt::Page(3), 0, 0).unwrap();
    let widened = fixture.read(ReadAt::Page(3), 1, 1).unwrap();
    assert_eq!(widened.passages.len(), page.passages.len() + 2);
}

#[test]
fn page_on_epub_is_invalid() {
    let fixture = Fixture::chapters(&["Zero.", "One."]);

    assert_eq!(
        invalid(fixture.read(ReadAt::Page(1), 0, 0)),
        "page addressing applies to PDFs; use passage_id or outline_id"
    );
}

#[test]
fn paginates_long_ranges() {
    // 167 words of 9 characters make a passage of about 1,500 characters, and
    // two of them do not fit one passage.
    let mut body = String::from("<h1>Long</h1>");
    for i in 0..60 {
        let words: Vec<String> = (0..167).map(|j| format!("w{i:03}x{j:03}")).collect();
        body.push_str(&format!("<p>{}</p>", words.join(" ")));
    }
    let fixture = Fixture::epub(
        EpubBuilder::new(EpubVersion::V3)
            .chapter("long.xhtml", &body)
            .toc("Long", "long.xhtml", 1),
    );
    assert_eq!(
        fixture.session.info(&fixture.path).unwrap().passage_count,
        60
    );

    let mut seen = Vec::new();
    let mut pages = 0;
    let mut cursor = None;
    loop {
        let result = fixture
            .session
            .read(
                &fixture.path,
                ReadParams {
                    at: ReadAt::Outline(0),
                    before: 0,
                    after: 0,
                    cursor,
                },
            )
            .unwrap();
        pages += 1;
        let chars = result.markdown.chars().count();
        assert!(
            chars <= READ_MAX_CHARS,
            "page {pages} has {chars} characters"
        );
        assert!(!result.passages.is_empty());
        // The section is named once per page, and a page that continues a
        // range names it again.
        assert_eq!(result.markdown.matches("## Long\n").count(), 1);
        seen.extend(ids(&result));
        match result.next_cursor {
            Some(next) => {
                assert_eq!(next, seen.last().unwrap() + 1);
                cursor = Some(next);
            }
            None => break,
        }
    }

    assert_eq!(seen, (0..60).collect::<Vec<_>>());
    assert!(pages > 1);
    // Each page is filled: it stops only when the next passage would not fit.
    assert!(pages <= 14, "{pages} pages for about 92,000 characters");
}

#[test]
fn a_cursor_must_be_inside_the_range() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two.", "Three.", "Four."]);
    let read = |at, before, after, cursor| {
        fixture.session.read(
            &fixture.path,
            ReadParams {
                at,
                before,
                after,
                cursor: Some(cursor),
            },
        )
    };

    // The range is 1 to 3; the cursor starts the page inside it.
    let result = read(ReadAt::Passage(2), 1, 1, 2).unwrap();
    assert_eq!(ids(&result), [2, 3]);
    assert!(result.markdown.starts_with("## Part 2\n"));
    for cursor in [0, 4, 99] {
        let message = invalid(read(ReadAt::Passage(2), 1, 1, cursor));
        assert!(
            message.contains(&format!("cursor {cursor} ")) && message.contains("1–3"),
            "{message}"
        );
    }
}

#[test]
fn neighbours_over_ten_are_invalid() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two."]);

    for (before, after) in [(READ_NEIGHBOURS_MAX + 1, 0), (0, READ_NEIGHBOURS_MAX + 1)] {
        let message = invalid(fixture.read(ReadAt::Passage(1), before, after));
        assert!(
            message.contains(&READ_NEIGHBOURS_MAX.to_string()),
            "{message}"
        );
    }
    assert!(
        fixture
            .read(ReadAt::Passage(1), READ_NEIGHBOURS_MAX, READ_NEIGHBOURS_MAX)
            .is_ok()
    );
    // The check comes before the book is opened.
    let root = private_tempdir();
    let missing = Session::with_cache_root(root.path().to_path_buf())
        .read(
            "/nonexistent/book.epub",
            ReadParams {
                at: ReadAt::Passage(0),
                before: 11,
                after: 0,
                cursor: None,
            },
        )
        .unwrap_err();
    assert!(matches!(missing, Error::InvalidArgument(_)), "{missing}");
}

#[test]
fn outline_and_info() {
    let fixture = Fixture::nested();

    let outline = fixture.session.outline(&fixture.path).unwrap();
    let entries: Vec<_> = outline
        .entries
        .iter()
        .map(|e| (e.id, e.title.as_str(), e.level, e.passage_id))
        .collect();
    assert_eq!(
        entries,
        [
            (0, "One", 1, Some(0)),
            (1, "One A", 2, Some(1)),
            (2, "One B", 2, Some(2)),
            (3, "Two", 1, Some(3)),
        ]
    );
    assert_eq!(
        outline.entries[3].locator,
        Some(PassageLocator::Epub {
            href: "ch2.xhtml".to_string(),
            para_start: 2,
            para_end: 2,
            print_page: None,
            under_heading: Some(HeadingPlace {
                text: "Two".to_string(),
                para_start: 1,
                para_end: 1,
            }),
        })
    );

    let info = fixture.session.info(&fixture.path).unwrap();
    assert_eq!(info.title.as_deref(), Some("Nested Book"));
    assert_eq!(info.authors, ["Ada Example"]);
    assert_eq!(info.language.as_deref(), Some("en"));
    assert_eq!(info.format, Format::Epub);
    assert_eq!(info.page_count, None);
    assert_eq!(info.spine_count, Some(2));
    assert!(info.passage_count > 0);
    assert_eq!(info.passage_count, 4);
    assert!(info.lexical_ready);
    assert!(!info.semantic_ready);
    assert!(info.warnings.is_empty());
}

#[test]
fn pdf_outline_and_info() {
    let fixture = Fixture::sample_pdf();

    let info = fixture.session.info(&fixture.path).unwrap();
    assert_eq!(info.format, Format::Pdf);
    assert_eq!(info.title.as_deref(), Some("Sample Book"));
    assert_eq!(info.page_count, Some(5));
    assert_eq!(info.spine_count, None);

    let outline = fixture.session.outline(&fixture.path).unwrap();
    let titles: Vec<_> = outline.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["Entropy", "Joint entropy", "Channels"]);
    for entry in &outline.entries {
        let id = entry.passage_id.expect("every sample entry has text");
        match entry.locator.as_ref().unwrap() {
            PassageLocator::Pdf { page_start, .. } => assert!(*page_start >= 2),
            other => panic!("not a PDF locator: {other:?}"),
        }
        // The first passage of the entry is where reading it starts.
        let read = fixture.read(ReadAt::Outline(entry.id), 0, 0).unwrap();
        assert_eq!(read.passages[0].passage_id, id);
    }
}

#[test]
fn book_without_outline() {
    let fixture = Fixture::epub(
        EpubBuilder::new(EpubVersion::V3)
            .chapter("ch1.xhtml", "<h1>Plain</h1><p>No table of contents.</p>"),
    );

    assert!(
        fixture
            .session
            .outline(&fixture.path)
            .unwrap()
            .entries
            .is_empty()
    );
    let message = invalid(fixture.read(ReadAt::Outline(0), 0, 0));
    assert!(message.contains("no outline"), "{message}");
    assert_eq!(message, "this book has no outline; use passage_id");
    // The text is still readable by passage.
    assert!(fixture.read(ReadAt::Passage(0), 0, 0).is_ok());
}

#[test]
#[ignore = "needs the embedding model"]
fn index_builds_both_stages() {
    use std::cell::Cell;

    let books = TempDir::new().unwrap();
    let path = books.path().join("book.epub");
    EpubBuilder::new(EpubVersion::V3)
        .chapter("ch1.xhtml", "<h1>One</h1><p>Entropy measures surprise.</p>")
        .toc("One", "ch1.xhtml", 1)
        .write(&path);
    let fixture = Fixture::at(books, path);
    assert!(!fixture.session.info(&fixture.path).unwrap().semantic_ready);

    let last = Cell::new((0, 0));
    let info = fixture
        .session
        .index(&fixture.path, &|done, total| last.set((done, total)))
        .unwrap();

    assert!(info.lexical_ready && info.semantic_ready);
    assert_eq!(info.passage_count, 1);
    assert_eq!(last.get(), (1, 1));
    assert!(fixture.session.info(&fixture.path).unwrap().semantic_ready);
}
