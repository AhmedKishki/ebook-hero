//! Reading Markdown files through the public library: detection by content,
//! `extract`, and the usage guide as a book.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ebook_hero::Error;
use ebook_hero::document::markdown::document_to_markdown;
use ebook_hero::document::{
    Block, BlockKind, Container, Document, Format, LinkTarget, Locator, Metadata, SCHEMA_VERSION,
    Source, Span, SpanKind, Warning, span_errors,
};
use ebook_hero::extract::markdown::markdown_document;
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

/// The first way `b` differs from `a` in what writing and reading back keeps:
/// the blocks in order with their kind, text, `within` and spans, and the
/// containers. Locators are left out, since lines move.
fn difference(a: &Document, b: &Document) -> Option<String> {
    let kept = |block: &Block| {
        (
            block.kind.clone(),
            block.text.clone(),
            block.within.clone(),
            block.spans.clone(),
        )
    };
    for (x, y) in a.blocks.iter().zip(&b.blocks) {
        if kept(x) != kept(y) {
            return Some(format!(
                "block {}:\n  {:?}\nread back as\n  {:?}",
                x.id,
                kept(x),
                kept(y)
            ));
        }
    }
    if a.blocks.len() != b.blocks.len() {
        return Some(format!(
            "{} blocks read back as {}",
            a.blocks.len(),
            b.blocks.len()
        ));
    }
    (a.containers != b.containers).then(|| {
        format!(
            "containers {:?}\nread back as\n  {:?}",
            a.containers, b.containers
        )
    })
}

/// Whether `a` and `b` hold the same blocks and containers, as [`difference`]
/// compares them. A difference is printed, so a failing test shows it.
fn same_structure(a: &Document, b: &Document) -> bool {
    match difference(a, b) {
        Some(difference) => {
            eprintln!("{difference}");
            false
        }
        None => true,
    }
}

#[test]
fn every_doc_reads_back_the_same() {
    for path in docs() {
        let d = extract(&path, "abc").unwrap();
        let markdown = document_to_markdown(&d);
        let back = markdown_document(&markdown, d.source.clone()).unwrap();
        assert!(same_structure(&d, &back), "{path:?}");
    }
}

#[test]
fn generated_documents_read_back_the_same() {
    for seed in 0..500 {
        let d = Generator::new(seed).document();
        for block in &d.blocks {
            assert_eq!(span_errors(block), Vec::<String>::new(), "seed {seed}");
            if !matches!(block.kind, BlockKind::Code { .. }) {
                assert_eq!(block.text, normalize(&block.text), "seed {seed}");
            }
        }
        let markdown = document_to_markdown(&d);
        let back = markdown_document(&markdown, d.source.clone())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}\n{markdown}"));
        assert!(same_structure(&d, &back), "seed {seed}:\n{markdown}");
    }
}

#[test]
fn generated_markdown_reads_back_the_same() {
    for seed in 0..500 {
        let source = generated_markdown(seed);
        let d = markdown_document(&source, generated_source()).unwrap();
        let markdown = document_to_markdown(&d);
        let back = markdown_document(&markdown, d.source.clone())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}\n{markdown}"));
        assert!(
            same_structure(&d, &back),
            "seed {seed}:\n{source}\nwritten as\n{markdown}"
        );
    }
}

fn generated_source() -> Source {
    Source {
        sha256: "0".repeat(64),
        file_name: "generated.md".to_string(),
        format: Format::Markdown,
    }
}

/// Words for [`generated_markdown`]: letters, and punctuation and symbols,
/// which decide whether a run of `*` or `_` beside them can open or close.
const MARKDOWN_WORDS: &[&str] = &[
    "a", "bc", "é", "x_y", "(d)", "“q”", "!!", "😀", "1.", "...", "\"", "e.", "$", "#", "-",
];

/// The pairs of delimiters around generated inline Markdown.
const PAIRS: &[(&str, &str)] = &[
    ("*", "*"),
    ("_", "_"),
    ("**", "**"),
    ("__", "__"),
    ("~~", "~~"),
    ("[", "](x.md)"),
];

/// Markdown of one to three paragraphs and a note, whose words stand inside
/// pairs of delimiters nested up to three deep, often sharing an edge, as in
/// `***a** b*`, with line breaks, code spans and note references among them.
/// What the reader makes of it holds emphasis and strong nested deeper, and
/// with their delimiters joined in more ways, than [`Generator`] makes, and
/// only in shapes that some Markdown gives.
fn generated_markdown(seed: u64) -> String {
    let mut rng = Rng(seed);
    let mut paragraphs: Vec<String> = (0..rng.between(1, 3))
        .map(|_| inline_markdown(&mut rng, 0))
        .collect();
    paragraphs.push("[^1]: Note.".to_string());
    paragraphs.join("\n\n") + "\n"
}

/// One to three words, code spans or delimited runs of inline Markdown at
/// nesting `depth`, separated by spaces or line breaks.
fn inline_markdown(rng: &mut Rng, depth: usize) -> String {
    let mut markdown = String::new();
    for index in 0..rng.between(1, 3) {
        if index > 0 {
            markdown.push_str(if rng.chance(10) { "\\\n" } else { " " });
        }
        if depth < 3 && rng.chance(45) {
            let (open, close) = rng.pick(PAIRS);
            markdown.push_str(open);
            markdown.push_str(&inline_markdown(rng, depth + 1));
            markdown.push_str(close);
        } else if rng.chance(10) {
            markdown.push_str(&format!("`{}`", rng.pick(MARKDOWN_WORDS)));
        } else {
            markdown.push_str(rng.pick(MARKDOWN_WORDS));
        }
        if rng.chance(8) {
            markdown.push_str("[^1]");
        }
    }
    markdown
}

/// Words that hold Markdown's special characters, alone and inside other
/// text, among plain words. Each is NFC and holds no whitespace, so text made
/// of them is normalized.
const WORDS: &[&str] = &[
    "plain",
    "words",
    "é",
    "日本",
    "😀",
    "snake_case",
    "_lead",
    "trail_",
    "x*y",
    "2*3",
    "*",
    "**",
    "_",
    "__",
    "~",
    "~~",
    "~~~",
    "`",
    "``",
    "```",
    "`tick`",
    "#",
    "##",
    "#######",
    "C#",
    "-",
    "--",
    "---",
    "+",
    ">",
    ">>",
    "=",
    "===",
    "1.",
    "2)",
    "123456789.",
    "0.",
    "[x]",
    "[x](y)",
    "![a](b)",
    "[^1]",
    "[",
    "]",
    "(a)",
    "(",
    ")",
    ":",
    "\\",
    "\\*",
    "\\\\",
    "a\\",
    "!",
    "!!",
    "<",
    "<b>",
    "</div>",
    "<!--",
    "-->",
    "<http://x.y>",
    "<a@b.co>",
    "a<b",
    "&amp;",
    "&#42;",
    "&copy;",
    "&",
    "&foo;",
    "|",
    "a|b",
    "-|-",
    "|---|",
    ":--",
    "$x$",
    "^",
    "{",
    "}",
    "50%",
    "~/",
    "'",
    "\"",
    "“q”",
    "...",
];

/// Lines of code blocks: indentation, blank lines, tabs, fences and lines that
/// would be syntax outside code.
const CODE_LINES: &[&str] = &[
    "fn main() {",
    "    run();",
    "}",
    "",
    "  ",
    "\tindented",
    "```",
    "````",
    "~~~",
    "<b>",
    "> q",
    "- item",
    "# h",
    "a  b",
    "\\",
];

/// The languages code blocks name, among them ones with a backtick, an entity
/// and a backslash escape.
const LANGUAGES: &[Option<&str>] = &[
    None,
    None,
    Some("rust"),
    Some("c++"),
    Some("x`y"),
    Some("a&amp;b"),
    Some("\\*"),
];

/// Link targets, each with where the reader finds that it leads. An anchor's
/// target is set once its heading has a block.
const LINKS: &[&str] = &[
    "https://example.org/a_(b)",
    "mailto:x@y.z",
    "notes.md",
    "my%20notes.md#part",
    "dir/a b.md",
    "x.md?raw=1",
    "a(b.md",
    "a|b.md",
    "&amp;.md",
    "c:\\a\\*b",
    "",
    "#",
    "/abs.md",
    "#missing",
];

/// The labels notes may have, distinct whatever their case.
const NOTE_LABELS: &[&str] = &["1", "n", "Note", "a-b", "x_y"];

/// splitmix64, so that every run generates the same documents.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from `low` to `high`, both included.
    fn between(&mut self, low: usize, high: usize) -> usize {
        low + (self.next() % (high - low + 1) as u64) as usize
    }

    /// True `percent` times in a hundred.
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.between(0, items.len() - 1)]
    }
}

/// Where a block's inline content may hold a line break and a note
/// reference.
#[derive(Clone, Copy)]
struct Allowed {
    breaks: bool,
    notes: bool,
}

/// The spans around the words that further spans are generated over.
#[derive(Clone, Copy)]
struct Inside {
    /// Whether they are in a link, which holds no further link.
    link: bool,
    /// Whether they are in a strikethrough, which holds no further one.
    strikethrough: bool,
    /// How many emphasis and strong spans are around them.
    emphasis: usize,
}

/// A span whose target is known only once every block exists.
enum Pending {
    /// A note reference to the note with this label.
    Note(&'static str),
    /// A link to the anchor heading with this text.
    Anchor(String),
}

/// Builds a `Document` such as the Markdown reader writes: nested lists and
/// quotes, tables, code, rules, headings, footnotes, and paragraphs whose
/// words hold Markdown's special characters under emphasis, strong,
/// strikethrough, code spans, links, line breaks and note references.
struct Generator {
    rng: Rng,
    containers: Vec<Container>,
    blocks: Vec<Block>,
    /// The labels of the notes the document defines.
    notes: Vec<&'static str>,
    /// The texts of the headings that links lead to.
    anchors: Vec<String>,
    /// Spans to complete, as block, span and what they lead to.
    pending: Vec<(usize, usize, Pending)>,
}

impl Generator {
    fn new(seed: u64) -> Generator {
        Generator {
            rng: Rng(seed),
            containers: Vec::new(),
            blocks: Vec::new(),
            notes: Vec::new(),
            anchors: Vec::new(),
            pending: Vec::new(),
        }
    }

    fn document(mut self) -> Document {
        let mut labels = NOTE_LABELS.to_vec();
        for _ in 0..self.rng.between(0, 2) {
            let label = labels.remove(self.rng.between(0, labels.len() - 1));
            self.notes.push(label);
        }
        self.anchors = (0..self.rng.between(0, 2))
            .map(|n| format!("anchor-{n}"))
            .collect();
        // A paragraph with text first, so the document can be read.
        let allowed = Allowed {
            breaks: true,
            notes: true,
        };
        self.paragraph(&[], allowed, 1);
        let mut notes = self.notes.clone();
        let mut anchors = self.anchors.clone();
        for _ in 0..self.rng.between(1, 6) {
            if !notes.is_empty() && self.rng.chance(20) {
                let label = notes.remove(0);
                self.note(label);
            } else if !anchors.is_empty() && self.rng.chance(20) {
                let text = anchors.remove(0);
                self.push(BlockKind::Heading { level: 2 }, text, Vec::new(), &[]);
            } else {
                self.element(&[], 0);
            }
        }
        for label in notes {
            self.note(label);
        }
        for text in anchors {
            self.push(BlockKind::Heading { level: 2 }, text, Vec::new(), &[]);
        }
        self.complete();
        let mut doc = Document {
            schema_version: SCHEMA_VERSION,
            source: Source {
                sha256: "0".repeat(64),
                file_name: "generated.md".to_string(),
                format: Format::Markdown,
            },
            metadata: Metadata {
                title: None,
                authors: Vec::new(),
                language: None,
                page_count: None,
                spine_count: None,
            },
            outline: Vec::new(),
            sections: Vec::new(),
            files: Vec::new(),
            page_labels: Vec::new(),
            containers: self.containers,
            blocks: self.blocks,
            furniture: Vec::new(),
            warnings: Vec::new(),
        };
        for (id, block) in doc.blocks.iter_mut().enumerate() {
            block.id = id as u32;
        }
        doc
    }

    /// Sets the targets of note references and anchor links.
    fn complete(&mut self) {
        for (block, span, pending) in std::mem::take(&mut self.pending) {
            let target = match pending {
                Pending::Note(label) => {
                    let note = self.containers.iter().position(
                        |c| matches!(c, Container::Note { label: Some(l) } if l == label),
                    );
                    self.blocks
                        .iter()
                        .position(|b| note.is_some_and(|n| b.within.contains(&(n as u32))))
                }
                Pending::Anchor(text) => self.blocks.iter().position(|b| b.text == text),
            };
            let target = target.expect("every note and anchor is written") as u32;
            match &mut self.blocks[block].spans[span].kind {
                SpanKind::NoteRef { block, .. } => *block = Some(target),
                SpanKind::Link { to, .. } => *to = LinkTarget::Block { block: target },
                _ => unreachable!(),
            }
        }
    }

    fn push(&mut self, kind: BlockKind, text: String, spans: Vec<Span>, within: &[u32]) -> usize {
        self.blocks.push(Block {
            id: self.blocks.len() as u32,
            kind,
            text,
            spans,
            ids: Vec::new(),
            within: within.to_vec(),
            section: None,
            locator: Locator::Markdown {
                line: 1,
                line_end: None,
            },
        });
        self.blocks.len() - 1
    }

    /// Opens `container` inside `within` and returns the path into it.
    fn open(&mut self, container: Container, within: &[u32]) -> Vec<u32> {
        let mut path = within.to_vec();
        path.push(self.containers.len() as u32);
        self.containers.push(container);
        path
    }

    /// One block, or one container with blocks in it, at nesting `depth`.
    fn element(&mut self, within: &[u32], depth: usize) {
        let allowed = Allowed {
            breaks: true,
            notes: true,
        };
        let nested = depth < 3;
        match self.rng.between(0, 99) {
            0..=34 => self.paragraph(within, allowed, 1),
            35..=44 => self.heading(within),
            45..=54 => self.code(within),
            55..=59 => {
                self.push(BlockKind::Rule, String::new(), Vec::new(), within);
            }
            60..=74 if nested => self.list(within, depth),
            75..=86 if nested => self.quote(within, depth),
            87..=99 if nested => self.table(within),
            _ => self.paragraph(within, allowed, 1),
        }
    }

    /// One to `max` elements inside `within`, or, now and then, the empty
    /// paragraph a reader writes into a container that holds nothing else.
    fn contents(&mut self, within: &[u32], depth: usize, max: usize) {
        if self.rng.chance(6) {
            self.push(BlockKind::Paragraph, String::new(), Vec::new(), within);
            return;
        }
        for _ in 0..self.rng.between(1, max) {
            self.element(within, depth + 1);
        }
    }

    fn paragraph(&mut self, within: &[u32], allowed: Allowed, min_words: usize) {
        let (text, spans) = self.inline(allowed, min_words);
        self.push_inline(BlockKind::Paragraph, text, spans, within);
    }

    /// Pushes a block whose spans may wait for their targets.
    fn push_inline(
        &mut self,
        kind: BlockKind,
        text: String,
        spans: Vec<(Span, Option<Pending>)>,
        within: &[u32],
    ) {
        let block = self.blocks.len();
        let mut kept = Vec::new();
        for (index, (span, pending)) in spans.into_iter().enumerate() {
            if let Some(pending) = pending {
                self.pending.push((block, index, pending));
            }
            kept.push(span);
        }
        self.push(kind, text, kept, within);
    }

    fn heading(&mut self, within: &[u32]) {
        let level = self.rng.between(1, 6) as u8;
        if self.rng.chance(3) {
            self.push(
                BlockKind::Heading { level },
                String::new(),
                Vec::new(),
                within,
            );
            return;
        }
        // Only a setext heading, of level 1 or 2, holds a line break.
        let allowed = Allowed {
            breaks: level <= 2,
            notes: true,
        };
        let (text, spans) = self.inline(allowed, 1);
        self.push_inline(BlockKind::Heading { level }, text, spans, within);
    }

    fn code(&mut self, within: &[u32]) {
        let language = self.rng.pick(LANGUAGES).map(str::to_string);
        let lines: Vec<&str> = (0..self.rng.between(1, 4))
            .map(|_| self.rng.pick(CODE_LINES))
            .collect();
        self.push(
            BlockKind::Code { language },
            lines.join("\n"),
            Vec::new(),
            within,
        );
    }

    fn list(&mut self, within: &[u32], depth: usize) {
        let ordered = self.rng.chance(50);
        let start = ordered.then(|| self.rng.pick(&[0, 1, 1, 1, 3, 9, 10, 99, 123_456_789]));
        let list = self.open(Container::List { ordered, start }, within);
        for _ in 0..self.rng.between(1, 3) {
            let item = self.open(Container::Item, &list);
            self.contents(&item, depth + 1, 2);
        }
    }

    fn quote(&mut self, within: &[u32], depth: usize) {
        let quote = self.open(Container::Quote, within);
        self.contents(&quote, depth, 3);
    }

    fn note(&mut self, label: &'static str) {
        let note = self.open(
            Container::Note {
                label: Some(label.to_string()),
            },
            &[],
        );
        self.contents(&note, 0, 2);
    }

    fn table(&mut self, within: &[u32]) {
        let table = self.open(Container::Table, within);
        let columns = self.rng.between(1, 3);
        let cell = Allowed {
            breaks: false,
            notes: true,
        };
        for row in 0..=self.rng.between(0, 2) {
            let row = self.open(Container::Row { header: row == 0 }, &table);
            for _ in 0..columns {
                let path = self.open(Container::Cell, &row);
                self.paragraph(&path, cell, 0);
            }
        }
    }

    /// Text of `min_words` to seven words, with spans over whole words: nested
    /// marks and links, code spans, line breaks on the spaces between words
    /// when `allowed`, and note references at the edges of words outside code
    /// and links when `allowed`.
    fn inline(
        &mut self,
        allowed: Allowed,
        min_words: usize,
    ) -> (String, Vec<(Span, Option<Pending>)>) {
        let words: Vec<&str> = (0..self.rng.between(min_words, 7))
            .map(|_| self.rng.pick(WORDS))
            .collect();
        // Where each word starts and ends, in code points.
        let mut bounds = Vec::new();
        let mut at = 0;
        for word in &words {
            let length = word.chars().count();
            bounds.push((at, at + length));
            at += length + 1;
        }
        let mut spans = Vec::new();
        let inside = Inside {
            link: false,
            strikethrough: false,
            emphasis: 0,
        };
        self.marks(&bounds, 0, words.len(), 0, inside, &mut spans);
        // A code span holds neither a break nor a note reference, and a note
        // reference inside a link's text would undo the link.
        let ranges = |code_only: bool| -> Vec<(usize, usize)> {
            spans
                .iter()
                .filter(|(s, _)| match s.kind {
                    SpanKind::Code => true,
                    SpanKind::Link { .. } => !code_only,
                    _ => false,
                })
                .map(|(s, _)| (s.start, s.end))
                .collect()
        };
        let (code, closed) = (ranges(true), ranges(false));
        let strictly_in = |ranges: &[(usize, usize)], point: usize| {
            ranges
                .iter()
                .any(|&(start, end)| start < point && point < end)
        };
        if allowed.breaks {
            for pair in bounds.windows(2) {
                let space = pair[0].1;
                if self.rng.chance(15) && !strictly_in(&code, space) {
                    let kind = SpanKind::Break {
                        lines: self.rng.between(1, 3) as u32,
                    };
                    spans.push((
                        Span {
                            start: space,
                            end: space + 1,
                            kind,
                        },
                        None,
                    ));
                }
            }
        }
        if allowed.notes && !self.notes.is_empty() {
            for &(start, end) in &bounds {
                for point in [start, end] {
                    if self.rng.chance(8) && !strictly_in(&closed, point) {
                        let label = self.rng.pick(&self.notes);
                        // A reference may differ from its note in case.
                        let marker = if self.rng.chance(30) {
                            label.to_uppercase()
                        } else {
                            label.to_string()
                        };
                        let kind = SpanKind::NoteRef {
                            marker,
                            block: None,
                        };
                        let span = Span {
                            start: point,
                            end: point,
                            kind,
                        };
                        spans.push((span, Some(Pending::Note(label))));
                    }
                }
            }
        }
        // Sorted as the reader sorts them: by start, the longer first, and in
        // the order they open where they cover the same text.
        spans.sort_by_key(|(span, _)| (span.start, std::cmp::Reverse(span.end)));
        (words.join(" "), spans)
    }

    /// Spans over runs of the words `low..high`, nested up to three deep, as
    /// pairs of span and pending target, inside the spans `inside` says.
    fn marks(
        &mut self,
        bounds: &[(usize, usize)],
        low: usize,
        high: usize,
        depth: usize,
        inside: Inside,
        spans: &mut Vec<(Span, Option<Pending>)>,
    ) {
        if depth == 3 {
            return;
        }
        let mut word = low;
        while word < high {
            if !self.rng.chance(25) {
                word += 1;
                continue;
            }
            let end = self.rng.between(word + 1, high);
            let (start, stop) = (bounds[word].0, bounds[end - 1].1);
            let mut pending = None;
            // Emphasis and strong nest two deep here. Deeper, some nestings
            // over punctuation come from no Markdown, so
            // `generated_markdown_reads_back_the_same` tries deeper ones on
            // what the reader makes of Markdown.
            let kind = match self.rng.between(0, 4) {
                0 | 1 if inside.emphasis >= 2 => SpanKind::Code,
                0 => SpanKind::Emphasis,
                1 => SpanKind::Strong,
                2 if !inside.strikethrough => SpanKind::Strikethrough,
                3 => SpanKind::Code,
                4 if !inside.link => {
                    let anchor = !self.anchors.is_empty() && self.rng.chance(25);
                    let (href, to) = if anchor {
                        let text =
                            self.anchors[self.rng.between(0, self.anchors.len() - 1)].clone();
                        pending = Some(Pending::Anchor(text.clone()));
                        (format!("#{text}"), LinkTarget::Nowhere)
                    } else {
                        let href = self.rng.pick(LINKS);
                        (href.to_string(), link_target(href))
                    };
                    SpanKind::Link { href, to }
                }
                _ => SpanKind::Emphasis,
            };
            let emphasis = match kind {
                SpanKind::Emphasis | SpanKind::Strong => inside.emphasis + 1,
                _ => inside.emphasis,
            };
            let inner = match kind {
                SpanKind::Code => None,
                SpanKind::Link { .. } => Some(Inside {
                    link: true,
                    emphasis,
                    ..inside
                }),
                SpanKind::Strikethrough => Some(Inside {
                    strikethrough: true,
                    emphasis,
                    ..inside
                }),
                _ => Some(Inside { emphasis, ..inside }),
            };
            spans.push((
                Span {
                    start,
                    end: stop,
                    kind,
                },
                pending,
            ));
            if let Some(inner) = inner {
                self.marks(bounds, word, end, depth + 1, inner, spans);
            }
            word = end;
        }
    }
}

/// Where the reader finds that a link to `href`, one of [`LINKS`], leads.
fn link_target(href: &str) -> LinkTarget {
    let file = |path: &str| LinkTarget::File {
        path: path.to_string(),
    };
    match href {
        "https://example.org/a_(b)" | "mailto:x@y.z" | "c:\\a\\*b" => LinkTarget::Address,
        "notes.md" | "a(b.md" | "a|b.md" | "&amp;.md" | "dir/a b.md" => file(href),
        "my%20notes.md#part" => file("my notes.md"),
        "x.md?raw=1" => file("x.md"),
        "#" => LinkTarget::Block { block: 0 },
        _ => LinkTarget::Nowhere,
    }
}
