# ebook-reader part 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the `Document` into schema 3 (each fact stored once, structure as standoff containers and spans), read and write Markdown, compress the cache, and make the window's Help tab the same view as its Book tab.

**Architecture:** Readers in `src/extract/` write a normalised `Document`. `Places` (new, `src/document/places.rs`) derives per-block facts at load. Passages are stored as pieces and assembled in memory, so search, verify and graph keep reading the same `Passage` values. The window draws any `Document` through one pure layout module (`gui/layout.js`) and one view factory (`gui/viewer.js`).

**Tech Stack:** Rust 2024 (serde, serde_json, schemars, scraper, tantivy), new crates `pulldown-cmark` and `lz4_flex`'s `frame` feature; Electron with `node --test`.

**Spec:** `docs/superpowers/specs/2026-11-06-ebook-reader-design.md`. Read it before any task; this plan argues from it and does not repeat its reasons.

## Global Constraints

- Work on branch `ebook-reader`. One commit per task; messages state what changed and why, with no generated-by or co-author trailers. Versions in `Cargo.toml` and `gui/package.json` and the `CHANGELOG.md` entry come in Task 13 only; the branch is squashed into one release commit before the pull request.
- Every task ends with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cd gui && npm test` passing.
- Run one heavy job at a time (a full `cargo test` build never runs beside another build). Run `df -h .` before a full build; if less than 20 GB is free, delete stale test binaries with `cargo clean -p ebook-hero` first.
- `SCHEMA_VERSION` = 3. `MARKDOWN_READER_VERSION` = 1. Bump once on the branch, in the first task that changes the reader: `PDF_READER_VERSION` 3 → 4, `EPUB_READER_VERSION` 2 → 3, `PASSAGES_VERSION` 2 → 3.
- `MAX_MARKDOWN_BYTES` = 64 MiB, in `src/extract/markdown.rs`.
- Cache files: `document.json.lz4`, `passages.json.lz4`, `graph.json.lz4`, LZ4 frame format with content checksum. `manifest.json` stays plain JSON. A file that fails its magic number or checksum gives an error naming the file and saying to run `ebook-hero cache clear`.
- `text` equals `normalize(text)` for every block except `Code`, whose text keeps its lines and indentation without the final newline.
- Span `start` and `end` count Unicode code points of `text`. Spans are well nested, sorted by `start` then longer first. Only `NoteRef` and `PageBreak` may be empty (`start == end`).
- stdout carries only command output or MCP messages; logs go to stderr. Errors name the cause and a remedy. Nothing falls back silently.
- Offsets are UTF-8 bytes inside the library and code points in output.
- Tests that give `Session` a cache root use `tests/common::private_tempdir()`. Test books are generated or public domain.
- Prose in `docs/`, `AGENTS.md` and `CHANGELOG.md` is written by loading and following `/home/ahmed/.claude/skills/human-writing/SKILL.md`.
- A change to an algorithm that `docs/` describes updates that document in the same task.

## Review Focus

1. A Markdown file with CRLF line endings or a byte-order mark reads as the same `Document` as its LF twin, with correct `line` numbers and no `\r` in any text. Test in Task 6.
2. A character outside the Basic Multilingual Plane (an emoji, a CJK extension character) before a span leaves the span on the right characters, in Rust (code points) and in the window (UTF-16). Tests in Tasks 7 and 11.
3. An empty or whitespace-only Markdown file fails like an EPUB without text: `Error::Unreadable` whose message contains "no readable text". Test in Task 6.
4. A text file that is not UTF-8, such as Latin-1 bytes, is `Error::Unsupported`, and the message names the three formats and UTF-8. Test in Task 6.
5. A heading inside an EPUB table cell opens a section that runs on after the table, while the cell's containers close. Test in Task 9.

---

### Task 1: Compressed cache files

**Files:**
- Modify: `Cargo.toml` (dependency `lz4_flex = { version = "0.14", default-features = false, features = ["std", "safe-encode", "safe-decode", "frame"] }`)
- Modify: `src/index/cache.rs` (file constants, layout comment, new read and write functions, `cached_title`)
- Modify: `src/index/book.rs` (document, passages and graph go through the new functions)
- Modify: `docs/usage.md` (section "Where files are kept")
- Test: `src/index/cache.rs` (unit), `tests/cache.rs`

**Interfaces:**
- Produces, in `src/index/cache.rs`:

```rust
pub(crate) const DOCUMENT_FILE: &str = "document.json.lz4";
pub(crate) const PASSAGES_FILE: &str = "passages.json.lz4";
pub(crate) const GRAPH_FILE: &str = "graph.json.lz4";
/// JSON in an LZ4 frame with content checksum, written through a temporary file and a rename.
pub(crate) fn write_lz4_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()>;
/// Errors name the file and say to run `ebook-hero cache clear` when the frame or its checksum fails.
pub(crate) fn read_lz4_json<T: DeserializeOwned>(path: &Path) -> Result<T>;
```

`write_json_atomic` and `read_json` stay for `manifest.json`.

- [ ] **Step 1: Write the failing unit tests in `src/index/cache.rs`**

```rust
#[test]
fn lz4_json_round_trips_and_starts_with_the_frame_magic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.json.lz4");
    write_lz4_json_atomic(&path, &vec!["a", "b"]).unwrap();
    assert_eq!(&fs::read(&path).unwrap()[..4], &[0x04, 0x22, 0x4D, 0x18]);
    assert_eq!(read_lz4_json::<Vec<String>>(&path).unwrap(), ["a", "b"]);
}

#[test]
fn a_damaged_lz4_file_names_itself_and_the_remedy() {
    // write 10 000 bytes of JSON, flip one byte in the middle, read it back
    let message = read_lz4_json::<Vec<String>>(&path).unwrap_err().to_string();
    assert!(message.contains("x.json.lz4"), "{message}");
    assert!(message.contains("ebook-hero cache clear"), "{message}");
}

#[test]
fn a_file_that_is_not_lz4_is_refused_with_the_remedy() {
    // fs::write(&path, b"{\"plain\": true}")
    assert!(read_lz4_json::<serde_json::Value>(&path).unwrap_err().to_string().contains("ebook-hero cache clear"));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib cache::tests::lz4`
Expected: FAIL, `write_lz4_json_atomic` not found.

- [ ] **Step 3: Implement `write_lz4_json_atomic` and `read_lz4_json`**

Use `lz4_flex::frame::{FrameEncoder, FrameDecoder, FrameInfo}` with `FrameInfo::new().content_checksum(true)`. The temporary file is `path` with `.tmp` appended. A decode error becomes `Error::Index(format!("{}: not a readable cache file ({reason}); run \"ebook-hero cache clear\" and try again", path.display()))`.

- [ ] **Step 4: Switch the three cache files, `cached_title` and the layout comment**

`load`, `write_stage` and the graph cache in `src/index/book.rs` use the new functions; `cached_title` decodes through `FrameDecoder`. The layout comment at the top of `cache.rs` names the new files.

- [ ] **Step 5: Write the integration test in `tests/cache.rs`**

```rust
#[test]
fn cache_files_are_lz4_frames_and_the_manifest_stays_plain() {
    // open a generated EPUB through Session::with_cache_root(private_tempdir())
    assert!(dir.join("document.json.lz4").is_file());
    assert!(dir.join("passages.json.lz4").is_file());
    assert!(!dir.join("document.json").exists());
    serde_json::from_slice::<serde_json::Value>(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
}

#[test]
fn a_damaged_cache_file_is_rebuilt() {
    // flip a byte of document.json.lz4, open the book again in a new Session
    // the book opens, and document.json.lz4 decodes again
}
```

- [ ] **Step 6: Run the cache tests, then the whole suite**

Run: `cargo test --test cache && cargo test`
Expected: PASS.

- [ ] **Step 7: Document the files in `docs/usage.md`, then commit**

The "Where files are kept" section names the compressed files and shows `lz4 -dc document.json.lz4` for reading one.

```bash
git add Cargo.toml Cargo.lock src/index/cache.rs src/index/book.rs tests/cache.rs docs/usage.md
git commit -m "Compress the index's JSON files in checksummed LZ4 frames"
```

---

### Task 2: A passage names its blocks

**Files:**
- Modify: `src/read.rs` (`PassageRef`)
- Modify: `gui/book.js` (`open`, drop `passageBlocks`, `bodyOf`, `inPassage`, `PROBE`)
- Modify: `gui/renderer.js` (`showBook` loses the "could not be told apart" note)
- Test: `src/read.rs` (unit), `gui/book.test.js`

**Interfaces:**
- Produces: `PassageRef { passage_id: u32, section: Vec<String>, locator: PassageLocator, blocks: Vec<u32> }`, where `blocks` holds the ids of the blocks the passage covers, in order, each once. The MCP `read` tool's structured output gains the same field.
- Produces: `library.open({ path, passage })` resolves to `{ ok, key, view?, ids? }` with `ids` taken from `read --json`'s `passages[0].blocks`; there is no `precise` any more.

- [ ] **Step 1: Write the failing Rust test**

```rust
#[test]
fn a_read_passage_names_its_blocks_once_each_in_order() {
    // passages built from blocks 0..4, where passage 1 holds pieces of blocks 2 and 3
    let result = read(&book, &params(ReadAt::Passage(1))).unwrap();
    assert_eq!(result.passages[0].blocks, [2, 3]);
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test --lib read::tests::a_read_passage_names_its_blocks`
Expected: FAIL, no field `blocks`.

- [ ] **Step 3: Add `blocks` to `PassageRef`, filled from `passage.blocks`' `block_id`s with repeats dropped**

- [ ] **Step 4: Replace the JS tests that exercised `passageBlocks`**

Delete the tests from "an EPUB passage covers the blocks…" to "when no PDF block can be matched…". Add:

```js
test('a passage marks the blocks read --json names', async () => {
  // fake exec: extract gives three blocks; read gives passages[0].blocks = [1, 2]
  assert.deepEqual(reply.ids, [1, 2]);
  assert.equal('precise' in reply, false);
});
```

The real-binary tests keep their expectations for EPUB and PDF passages.

- [ ] **Step 5: Implement the JS change and run both suites**

Run: `cargo test --lib read && cd gui && npm test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/read.rs gui/book.js gui/book.test.js gui/renderer.js
git commit -m "Let read --json name a passage's blocks, so the window marks them exactly"
```

---

### Task 3: Schema 3 stores each fact once

**Files:**
- Modify: `src/document/mod.rs` (types, JSON form, `SCHEMA_VERSION`)
- Create: `src/document/places.rs`
- Modify: `src/extract/mod.rs` (`assign_sections`)
- Modify: `src/extract/epub.rs`, `src/extract/pdf.rs` (write the new fields)
- Modify: `src/passages.rs` (`build_passages` signature, `same_part`, `locator`)
- Modify: `src/index/book.rs` (`Book.places`)
- Modify: `src/settings.rs` (`PDF_READER_VERSION` 4, `EPUB_READER_VERSION` 3)
- Modify: `gui/book.js` (`MIN_SCHEMA_VERSION` 3, a block's id is its index)
- Modify: `docs/document-model.md`
- Test: `src/document/mod.rs`, `src/document/places.rs`, `src/passages.rs`, `tests/epub.rs`, `tests/pdf.rs`, `gui/book.test.js`

**Interfaces:**
- Produces, in `src/document/mod.rs`:

```rust
pub const SCHEMA_VERSION: u32 = 3;
pub struct Document {
    pub schema_version: u32,
    pub source: Source,
    pub metadata: Metadata,
    pub outline: Vec<OutlineEntry>,
    pub sections: Vec<Section>,
    /// The EPUB's content files in spine order; empty for other formats.
    pub files: Vec<String>,
    /// The printed label of each physical page of a PDF, page 1 first; empty when it defines none.
    pub page_labels: Vec<Option<String>>,
    pub blocks: Vec<Block>,
    pub furniture: Vec<Block>,
    pub warnings: Vec<Warning>,
}
pub struct Section { pub heading: u32, pub parent: Option<u32> }
pub struct Block {
    /// Its position in `blocks`. JSON leaves it out; deserialising sets it.
    pub id: u32,
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<Span>,
    /// Ids of the elements that start in the block, and of empty elements since the block before.
    pub ids: Vec<String>,
    pub section: Option<u32>,
    pub locator: Locator,
}
pub enum Locator {
    Pdf { page: u32, page_end: Option<u32>, bbox: Option<[f32; 4]> },
    Epub { file: u32 },
}
// SpanKind gains:
PageBreak { label: String },
impl Document { pub fn page_label(&self, page: u32) -> Option<&str>; }
```

`orig` goes. JSON leaves out `Block.id` and the locator's `format` tag; the builder chooses `#[serde(from = …, into = …)]` with a representation type, or another mechanism, whichever deserialises faster. Empty `spans` and `ids` and a `None` section are left out.

- Produces, in `src/document/places.rs` (re-exported as `ebook_hero::document::Places`):

```rust
pub struct Places { /* tables built in one pass */ }
impl Places {
    pub fn new(document: &Document) -> Places;
    /// The heading texts above the block, outermost first; a heading's own path ends with its text.
    pub fn section_path(&self, block: &Block) -> &[String];
    /// The block's position among the blocks of its EPUB file, from 1; 0 in a PDF.
    pub fn para(&self, block: u32) -> u32;
    /// The block's number among the non-heading blocks of its section, from 1, as build_passages counts today.
    pub fn section_para(&self, block: u32) -> u32;
    /// The label of the last PageBreak at or before the block's start.
    pub fn print_page(&self, block: u32) -> Option<&str>;
}
```

- Produces: `pub(crate) fn assign_sections(blocks: &mut [Block]) -> Vec<Section>` with today's level rules; `pub fn build_passages(document: &Document, places: &Places) -> Vec<Passage>`; `Book { …, pub places: Places, … }`. `Passage` keeps its fields and their values.
- The EPUB reader turns a change of `RawBlock.print_page` into a `PageBreak` span at offset 0 of the block where it changes; Task 10 makes the position exact.

- [ ] **Step 1: Write the failing schema tests in `src/document/mod.rs`**

```rust
#[test]
fn json_leaves_out_block_ids_and_locator_tags_and_loading_numbers_the_blocks() {
    let json = serde_json::to_value(&sample_document()).unwrap();
    assert!(json["blocks"][1].get("id").is_none());
    assert!(json["blocks"][1]["locator"].get("format").is_none());
    let back: Document = serde_json::from_value(json).unwrap();
    assert_eq!(back.blocks.iter().map(|b| b.id).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(back, sample_document());
}

#[test]
fn a_page_label_comes_from_the_table() {
    // page_labels = [Some("i"), None]
    assert_eq!(doc.page_label(1), Some("i"));
    assert_eq!(doc.page_label(2), None);
    assert_eq!(doc.page_label(9), None);
}
```

- [ ] **Step 2: Write the failing `Places` tests in `src/document/places.rs`**

```rust
#[test]
fn a_section_path_follows_the_parents() {
    // blocks: h1 "One" (0), p (1), h2 "Sub" (2), p (3), h1 "Two" (4), p (5)
    assert_eq!(places.section_path(&doc.blocks[3]), ["One", "Sub"]);
    assert_eq!(places.section_path(&doc.blocks[5]), ["Two"]);
}

#[test]
fn para_counts_within_each_epub_file() {
    // files a, a, a, b, b
    assert_eq!((0..5).map(|b| places.para(b)).collect::<Vec<_>>(), [1, 2, 3, 1, 2]);
}

#[test]
fn a_print_page_carries_on_until_the_next_break_even_into_the_next_file() {
    // PageBreak "45" on block 1, PageBreak "46" on block 3
    assert_eq!([0, 1, 2, 3, 4].map(|b| places.print_page(b)), [None, Some("45"), Some("45"), Some("46"), Some("46")]);
}
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test --lib document`
Expected: compile errors on the new fields and `Places`.

- [ ] **Step 4: Implement the types, the JSON form and `Places`, and make the crate compile**

Readers fill `sections` (from `assign_sections`), `files`, `page_labels` and `ids`. `RawBlock.ids` becomes `Block.ids`. `same_part` compares `section` ids and EPUB `file` ids. `locator` in `src/passages.rs` reads the label from `Document::page_label`, and `para`, `section_para`, `print_page` and the heading text from `Places`. Update every test helper that builds a `Block` or a `Locator`.

- [ ] **Step 5: Keep the integration tests' meaning**

`tests/epub.rs` and `tests/pdf.rs` assert sections through `Places::new(&doc).section_path(block)`, hrefs through `doc.files[file]`, print pages through `places.print_page`, and anchors through `block.ids`. No expected value changes.

- [ ] **Step 6: Run the whole Rust suite**

Run: `cargo test`
Expected: PASS, with the CLI and MCP outputs in `tests/cli.rs` and `tests/mcp.rs` unchanged, so search, read and verify print the same section paths and locators.

- [ ] **Step 7: Update the window for schema 3 and run its tests**

`MIN_SCHEMA_VERSION = 3`; `viewOf` takes a block's id from its index. The fixture in `gui/book.test.js` becomes schema 3.

Run: `cd gui && npm test`
Expected: PASS.

- [ ] **Step 8: Describe schema 3's storage in `docs/document-model.md`, then commit**

Cover each fact stored once, `Places`, sections, files, page labels, ids, `PageBreak`, and what JSON leaves out.

```bash
git add -A src gui tests docs/document-model.md
git commit -m "Store each fact of a Document once, and derive the rest at load"
```

---

### Task 4: Passages are stored as pieces

**Files:**
- Modify: `src/passages.rs` (stored form, assembly)
- Modify: `src/index/book.rs` (`load`, `write_stage`)
- Modify: `src/settings.rs` (`PASSAGES_VERSION` 3)
- Test: `src/passages.rs`, `tests/cache.rs`

**Interfaces:**
- Consumes: `build_passages`, `Places` and the cache functions from Tasks 1 and 3.
- Produces:

```rust
/// `len` bytes of block `block`'s text from `offset`.
#[derive(Serialize, Deserialize)]
pub struct StoredPiece { pub block: u32, pub offset: usize, pub len: usize }
pub fn stored_form(passages: &[Passage]) -> Vec<Vec<StoredPiece>>;
/// Rebuilds exactly what build_passages gave. A piece outside its block is Error::Index naming passages.json.lz4 and the cache-clear remedy.
pub fn assemble(document: &Document, places: &Places, stored: &[Vec<StoredPiece>]) -> Result<Vec<Passage>>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn assembling_the_stored_form_gives_the_built_passages() {
    // a document with two sections, a split oversized block and an EPUB file change
    let built = build_passages(&doc, &places);
    assert_eq!(assemble(&doc, &places, &stored_form(&built)).unwrap(), built);
}

#[test]
fn a_piece_outside_its_block_is_an_index_error_with_the_remedy() {
    let stored = vec![vec![StoredPiece { block: 0, offset: 0, len: 10_000 }]];
    assert!(assemble(&doc, &places, &stored).unwrap_err().to_string().contains("cache clear"));
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib passages::tests::assembl`
Expected: FAIL, `assemble` not found.

- [ ] **Step 3: Implement both, sharing one function with `build_passages` that turns pieces into a `Passage`**

- [ ] **Step 4: Store and load the pieces in `src/index/book.rs`**

`write_stage` writes `stored_form(&passages)`. `load` reads the pieces and builds `places` before `assemble`.

- [ ] **Step 5: Write a size test in `tests/cache.rs` and run the suite**

```rust
#[test]
fn stored_passages_hold_no_text() {
    // open alice.epub; decode passages.json.lz4 as serde_json::Value
    assert!(!text_of(&value).contains("crocodile"));
}
```

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Add the stored form to `docs/document-model.md`, then commit**

```bash
git add src/passages.rs src/index/book.rs src/settings.rs tests/cache.rs docs/document-model.md
git commit -m "Store passages as pieces of blocks, so the book's text is stored once"
```

---

### Task 5: Structure types

**Files:**
- Modify: `src/document/mod.rs` (containers, kinds, spans, links, formats, warnings)
- Modify: `src/passages.rs` (rules, wordless blocks, Markdown locator)
- Modify: `src/read.rs` (`locator_text` for Markdown)
- Modify: `src/cli.rs` (`warning_text` for `BrokenLink`)
- Modify: `docs/document-model.md`
- Test: `src/document/mod.rs`, `src/passages.rs`, `src/read.rs`

**Interfaces:**
- Produces, in `src/document/mod.rs`:

```rust
pub struct Document { …, pub containers: Vec<Container>, … }   // JSON leaves it out when empty
pub struct Block { …, pub within: Vec<u32>, … }               // JSON leaves it out when empty
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Container {
    List { ordered: bool, start: Option<u64> },
    Item, Quote, Table, Row { header: bool }, Cell,
    Note { label: Option<String> },
}
// BlockKind gains:
Code { language: Option<String> }, Rule,
// SpanKind gains:
Emphasis, Strong, Strikethrough, Code,
Link { href: String, to: LinkTarget },
Break { lines: u32 },
NoteRef { marker: String, block: Option<u32> },
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkTarget { Block { block: u32 }, File { path: String }, Address, Nowhere }
// Format gains Markdown; Locator gains:
Markdown { line: u32, line_end: Option<u32> },
// Warning gains:
BrokenLink { href: String, block: u32 },
/// The byte offset of code point `code_point` in `text`; `text.len()` past the end.
pub fn byte_offset(text: &str, code_point: usize) -> usize;
/// Why the block's spans break the rules in Global Constraints, if they do.
pub fn span_errors(block: &Block) -> Vec<String>;
```

- Produces, in `src/passages.rs`: `PassageLocator::Markdown { line_start: u32, line_end: u32, under_heading: Option<HeadingPlace> }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn every_new_variant_round_trips_through_json() { /* one block or span of each */ }

#[test]
fn byte_offset_counts_code_points() {
    assert_eq!(byte_offset("café au", 5), 6);
    assert_eq!(byte_offset("😀a", 1), 4);
    assert_eq!(byte_offset("ab", 9), 2);
}

#[test]
fn span_errors_name_overlaps_order_and_empty_spans() {
    // [0,4) and [2,6) overlap; an empty Emphasis; [3,5) before [0,2)
    assert_eq!(span_errors(&block).len(), 3);
}

#[test]
fn a_rule_closes_the_passage_and_wordless_blocks_go_into_none() {
    // p "one", Rule, p "", p "two" in one section
    assert_eq!(texts(&build_passages(&doc, &places)), ["one", "two"]);
}

#[test]
fn a_markdown_locator_names_the_heading_paragraphs_and_lines() {
    assert_eq!(locator_text(&markdown(3, 5, "search", 260, 284)), "search ¶3–5, lines 260–284");
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib`
Expected: compile errors on the new variants.

- [ ] **Step 3: Implement the types, packing rules and locator text**

Each new `match` arm in readers and consumers either uses the new variant or, where it cannot occur yet, treats it as a paragraph would be treated. A Markdown document is one part for `same_part`. `PASSAGES_VERSION` was already bumped to 3 in Task 4, so it stays.

- [ ] **Step 4: Run the suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Describe containers, the new kinds, spans and links in `docs/document-model.md`, then commit**

Include the Alice table example and the reason the container path was chosen over a tree.

```bash
git add src docs/document-model.md
git commit -m "Add containers, structural block kinds, inline spans and link targets to the Document"
```

---

### Task 6: Markdown reader, blocks and containers

**Files:**
- Modify: `Cargo.toml` (`pulldown-cmark = { version = "0.13", default-features = false }`)
- Create: `src/extract/markdown.rs`
- Modify: `src/extract/mod.rs` (`detect_format`, `extract`)
- Modify: `src/error.rs` (`Unsupported` message)
- Modify: `src/index/cache.rs` (`Manifest.markdown_reader`)
- Modify: `src/settings.rs` (`MARKDOWN_READER_VERSION`)
- Modify: `src/read.rs` (`read page` on Markdown), `src/cli.rs` and `src/mcp.rs` (descriptions)
- Create: `docs/markdown-reading.md`; Modify: `docs/usage.md`
- Test: `src/extract/markdown.rs`, `tests/markdown.rs` (new), `tests/cli.rs`

**Interfaces:**
- Produces:

```rust
pub const MARKDOWN_READER_VERSION: u32 = 1;                // src/settings.rs
pub fn read_markdown(path: &Path, source: Source) -> Result<Document>;          // reads, checks the cap, strips a BOM
pub(crate) fn markdown_document(text: &str, source: Source) -> Result<Document>; // tests call this
pub struct Manifest { …, #[serde(default)] pub markdown_reader: u32, … }
```

- `Error::Unsupported` reads `{path}: unsupported format; expected a PDF, an EPUB or a UTF-8 text file`.
- Inline content goes into `text` as plain normalised text in this task. Task 7 adds its spans.

- [ ] **Step 1: Write the failing unit tests in `src/extract/markdown.rs`**

```rust
fn doc(md: &str) -> Document { markdown_document(md, source()).unwrap() }

#[test]
fn headings_paragraphs_and_lines() {
    let d = doc("# Title\n\nOne\ntwo.\n\n## Sub\n\nThree.\n");
    assert_eq!(kinds_and_texts(&d), [(heading(1), "Title"), (para(), "One two."), (heading(2), "Sub"), (para(), "Three.")]);
    assert_eq!(lines(&d), [(1, None), (3, Some(4)), (6, None), (8, None)]);
    assert_eq!(d.outline.len(), 2);
    assert_eq!(d.metadata.title, None);
}

#[test]
fn a_table_is_containers_and_an_empty_cell_holds_an_empty_paragraph() {
    let d = doc("| Register | Address |\n|---|---|\n| STATUS | 0x1A |\n| CONTROL | |\n");
    assert_eq!(d.containers, [Table, Row { header: true }, Cell, Cell, Row { header: false }, Cell, Cell, Row { header: false }, Cell, Cell]);
    assert_eq!(withins(&d), [vec![0, 1, 2], vec![0, 1, 3], vec![0, 4, 5], vec![0, 4, 6], vec![0, 7, 8], vec![0, 7, 9]]);
    assert_eq!(d.blocks[5].text, "");
}

#[test]
fn nested_lists_quotes_and_numbering() {
    let d = doc("> 3. a\n>    - b\n>\n>    c\n");
    // quote 0, list 1 (ordered, start 3), item 2 holds "a" and "c"; list 3 (unordered), item 4 holds "b"
}

#[test]
fn a_code_block_keeps_its_lines_and_language() {
    let d = doc("```rust\nfn  a() {\n    b\n}\n```\n");
    assert_eq!(d.blocks[0].kind, BlockKind::Code { language: Some("rust".into()) });
    assert_eq!(d.blocks[0].text, "fn  a() {\n    b\n}");
}

#[test]
fn a_rule_and_a_footnote_definition() { /* Rule kind; "[^n]: Note text." gives Note { label: Some("n") } container */ }

#[test]
fn front_matter_and_html_blocks_are_left_out_with_warnings_and_comments_silently() {
    let d = doc("---\ntitle: x\n---\n\n<div>\nhi\n</div>\n\n<!-- c -->\n\nText.\n");
    assert_eq!(texts(&d), ["Text."]);
    assert_eq!(parts(&d.warnings), ["front matter at line 1", "HTML block at line 5"]);
}

// Review Focus 1
#[test]
fn crlf_and_bom_give_the_same_document_as_lf() {
    assert_eq!(read_bytes(b"\xEF\xBB\xBF# T\r\n\r\nA\r\nb.\r\n"), read_bytes(b"# T\n\nA\nb.\n"));
}

// Review Focus 3
#[test]
fn an_empty_markdown_file_has_no_readable_text() {
    let e = markdown_document(" \n\n", source()).unwrap_err();
    assert!(matches!(e, Error::Unreadable { .. }) && e.to_string().contains("no readable text"));
}

#[test]
fn a_file_over_the_cap_fails_with_its_size_and_the_cap() { /* call the cap check with MAX_MARKDOWN_BYTES + 1 */ }
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib extract::markdown`
Expected: FAIL, module missing.

- [ ] **Step 3: Implement the reader**

Use `Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS).into_offset_iter()`. A line-start table built once maps byte ranges to lines by binary search. A container stack gives `within`. `assign_sections` fills `sections`.

- [ ] **Step 4: Detect Markdown and route it in `extract`**

After the PDF marker and the EPUB archive, a file whose bytes are UTF-8 after an optional BOM is `Format::Markdown`. The manifest gains `markdown_reader`, and `expected_stage1` sets it.

- [ ] **Step 5: Write the integration tests in `tests/markdown.rs` and `tests/cli.rs`**

```rust
// Review Focus 4
#[test]
fn a_latin1_text_file_is_unsupported_and_names_utf8() {
    // write b"caf\xE9\n" to a .txt file
    let m = detect_format(&path).unwrap_err().to_string();
    assert!(m.contains("a PDF, an EPUB or a UTF-8 text file"), "{m}");
}

#[test]
fn every_tool_takes_a_markdown_file() {
    // info, outline, search (lexical), find, read passage, verify, graph on docs/usage.md
}

#[test]
fn reading_a_markdown_file_by_page_names_passage_and_outline() { /* error text contains "passage" and "outline" */ }
```

- [ ] **Step 6: Update the descriptions and run the suite**

`src/cli.rs` and `src/mcp.rs` say "PDF, EPUB or Markdown" wherever they say "PDF or EPUB" today (22 places).

Run: `cargo test`
Expected: PASS.

- [ ] **Step 7: Write `docs/markdown-reading.md` and update `docs/usage.md`, then commit**

`docs/usage.md` covers detection, the new `Unsupported` message, the Markdown locator form and `read page` on Markdown.

```bash
git add Cargo.toml Cargo.lock src tests docs
git commit -m "Read Markdown files into Documents with their block structure"
```

---

### Task 7: Markdown reader, inline structure and links

**Files:**
- Modify: `src/extract/markdown.rs`
- Modify: `docs/markdown-reading.md`
- Test: `src/extract/markdown.rs`, `tests/markdown.rs`

**Interfaces:**
- Consumes: the `SpanKind`, `LinkTarget` and `Warning::BrokenLink` types from Task 5, and `normalize` from `src/normalize.rs`.
- Produces: `pub(crate) fn heading_id(text: &str, seen: &mut HashMap<String, u32>) -> String`, a verbatim port of `slugger` in `gui/markdown.js`. Ids go into the heading block's `ids`.

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test]
fn emphasis_strong_code_and_strikethrough_are_spans_over_normalised_text() {
    let b = &doc("Ah,  *that’s*  the **great** `puzzle` ~~x~~.\n").blocks[0];
    assert_eq!(b.text, "Ah, that’s the great puzzle x.");
    assert_eq!(spans(b), [(4, 10, emphasis()), (15, 20, strong()), (21, 27, code()), (28, 29, strike())]);
    assert!(span_errors(b).is_empty());
}

#[test]
fn hard_breaks_are_counted_breaks_on_one_space() {
    let b = &doc("one\\\ntwo\\\n\\\nthree\n").blocks[0];
    assert_eq!(b.text, "one two three");
    assert_eq!(spans(b), [(3, 4, brk(1)), (7, 8, brk(2))]);
}

// Review Focus 2
#[test]
fn spans_count_code_points_after_an_emoji() {
    let b = &doc("😀 *x*\n").blocks[0];
    assert_eq!(spans(b), [(2, 3, emphasis())]);
}

#[test]
fn heading_ids_follow_github_and_repeat_with_numbers() {
    // port the cases of 'headings get GitHub ids…' from gui/markdown.test.js
    assert_eq!(ids(&doc("## Outline\n\n## Outline\n## `cache` list!\n")), [vec!["outline"], vec!["outline-1"], vec!["cache-list"]]);
}

#[test]
fn links_resolve_to_blocks_files_addresses_or_nowhere() {
    let d = doc("# Top\n\n[a](#top) [b](search.md) [c](https://x.org) [d](#gone)\n");
    assert_eq!(targets(&d.blocks[1]), [Block { block: 0 }, File { path: "search.md".into() }, Address, Nowhere]);
    assert_eq!(d.warnings, [Warning::BrokenLink { href: "#gone".into(), block: 1 }]);
}

#[test]
fn a_footnote_reference_is_an_empty_noteref_to_the_note() {
    let d = doc("Claim[^1] goes.\n\n[^1]: Source.\n");
    assert_eq!(d.blocks[0].text, "Claim goes.");
    assert_eq!(spans(&d.blocks[0]), [(5, 5, noteref("1", Some(1)))]);
}

#[test]
fn images_and_inline_tags_leave_their_text_and_nothing_else() {
    assert_eq!(doc("a ![pic](x.png) <kbd>Ctrl</kbd> b\n").blocks[0].text, "a Ctrl b");
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib extract::markdown`
Expected: FAIL on spans, ids and links.

- [ ] **Step 3: Implement spans through normalisation**

Collect a block's raw text pieces with span marks. Normalise once while recording where each raw byte lands, then map the spans through that record and convert them to code points. A run of hard breaks becomes one `Break` whose `lines` counts them.

- [ ] **Step 4: Implement heading ids and link resolution after all blocks are read**

- [ ] **Step 5: Write the guide tests in `tests/markdown.rs` and run the suite**

```rust
#[test]
fn the_usage_guide_has_no_broken_link() {
    let d = extract(Path::new("docs/usage.md"), &sha).unwrap();
    assert!(!d.warnings.iter().any(|w| matches!(w, Warning::BrokenLink { .. })), "{:?}", d.warnings);
}

#[test]
fn every_file_link_in_docs_names_a_file_that_exists() { /* for each docs/*.md, each LinkTarget::File resolves beside it */ }

#[test]
fn every_block_of_every_doc_has_well_formed_spans() { /* span_errors is empty for each docs/*.md */ }
```

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Extend `docs/markdown-reading.md`, then commit**

```bash
git add src/extract/markdown.rs tests/markdown.rs docs/markdown-reading.md
git commit -m "Keep Markdown's inline structure as spans and resolve its links"
```

---

### Task 8: Markdown writer and round trip

**Files:**
- Modify: `src/document/markdown.rs`
- Modify: `docs/usage.md` (what `extract` writes)
- Test: `src/document/markdown.rs`, `tests/markdown.rs`

**Interfaces:**
- Consumes: `markdown_document` from Task 6.
- Produces: `pub fn document_to_markdown(doc: &Document) -> String` keeps its signature. `blocks_to_markdown` becomes private, or takes `&Document`, because containers live on the document.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn text_that_looks_like_markdown_is_escaped_and_reads_back_the_same() {
    for text in ["1. Introduction", "* * *", "# not a heading", "a *b* c", "[x](y)", "a | b", "&amp;", "<b>", "`tick`"] {
        let d = doc_with_paragraph(text);
        assert_eq!(markdown_document(&document_to_markdown(&d), source()).unwrap().blocks[0].text, text);
    }
}

#[test]
fn a_table_without_a_header_gets_an_empty_header_line_and_a_cell_of_two_blocks_one_line() {
    // Table, Row{header:false}, Cell with paragraphs "A" and "B", Cell "C"
    assert_eq!(document_to_markdown(&d), "|  |  |\n| --- | --- |\n| A B | C |\n");
}

#[test]
fn fences_outgrow_the_backticks_in_the_code() { /* code containing ``` gets a fence of four */ }
```

- [ ] **Step 2: Write the round-trip property test in `tests/markdown.rs`**

```rust
fn same_structure(a: &Document, b: &Document) -> bool {
    // same blocks in order with the same kind, text, within and spans, and the same containers; locators ignored
}

#[test]
fn every_doc_reads_back_the_same_after_writing() {
    for path in docs_markdown_files() {
        let d = read(path);
        assert!(same_structure(&d, &markdown_document(&document_to_markdown(&d), d.source.clone()).unwrap()), "{path:?}");
    }
}

#[test]
fn generated_documents_read_back_the_same() {
    // a seeded generator in the test: nested lists and quotes, tables, code, breaks, emphasis, links,
    // footnotes, and text drawn from Markdown's special characters; 500 documents
}
```

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test --lib document::markdown && cargo test --test markdown`
Expected: FAIL.

- [ ] **Step 4: Implement the writer as the spec's list describes, escaping only where a character could start syntax**

- [ ] **Step 5: Run the suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Update `docs/usage.md`, then commit**

```bash
git add src/document/markdown.rs tests/markdown.rs docs/usage.md
git commit -m "Write a Document's structure as Markdown that reads back the same"
```

---

### Task 9: EPUB and PDF block structure

**Files:**
- Modify: `src/document/mod.rs` (remove `BlockKind::ListItem` and `BlockKind::Note`)
- Modify: `src/extract/xhtml.rs`, `src/extract/epub.rs`, `src/extract/pdf.rs`, `src/passages.rs`, `src/document/markdown.rs`, `gui/book.js`
- Modify: `docs/epub-reading.md`, `docs/pdf-reading.md`, `docs/concept-graph.md`
- Test: `src/extract/xhtml.rs`, `src/extract/pdf.rs`, `tests/epub.rs`

**Interfaces:**
- Produces: `pub fn xhtml_blocks(html: &str, containers: &mut Vec<Container>, print_page: &mut Option<String>) -> Vec<RawBlock>`, where `RawBlock` gains `within: Vec<u32>`. Container ids run on across the book's files.
- The PDF reader writes a note's blocks as `Paragraph`s inside a `Note { label: None }` container.

- [ ] **Step 1: Rewrite the XHTML tests that change, and add the new ones**

```rust
#[test]
fn table_cells_are_containers_and_figure_captions_paragraphs() {
    let (got, containers) = blocks_with(r#"<table><tr><td>A</td><td>B</td></tr></table><figure><img src="x.png" alt="pic"/><figcaption>Cap</figcaption></figure>"#);
    assert_eq!(containers, [Table, Row { header: false }, Cell, Cell]);
    assert_eq!(withins_and_texts(&got), [(vec![0, 1, 2], "A"), (vec![0, 1, 3], "B"), (vec![], "Cap")]);
}

#[test]
fn a_header_row_is_in_thead_or_holds_only_th() { /* both forms give Row { header: true } */ }

#[test]
fn lists_nest_and_ordered_lists_keep_their_start() { /* <ol start="3"><li>a<ul><li>b</li></ul></li></ol> */ }

#[test]
fn a_note_keeps_its_paragraphs_apart_in_a_note_container() {
    // replaces the test that expected "One. Two." joined
    assert_eq!(withins_and_texts(&got), [(vec![0], "One."), (vec![0], "Two.")]);
}

#[test]
fn pre_is_code_kept_verbatim_and_hr_a_rule() { /* "<pre>a\n  b</pre><hr/>" */ }

// Review Focus 5
#[test]
fn a_heading_in_a_cell_opens_a_section_that_runs_past_the_table() {
    // epub chapter: <table><tr><td><h2>Inside</h2><p>x</p></td></tr></table><p>after</p>
    assert_eq!(places.section_path(after), ["Inside"]);
    assert!(after.within.is_empty());
}
```

In `src/extract/pdf.rs`, the test that expects `BlockKind::Note` now expects a `Paragraph` inside a `Note { label: None }` container.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib extract`
Expected: FAIL.

- [ ] **Step 3: Implement the container stack in `xhtml.rs` and the note containers in both readers, then remove the two kinds**

Bump `PDF_READER_VERSION` and `EPUB_READER_VERSION` only if Task 3 did not already bump them on this branch.

- [ ] **Step 4: Run the suites**

Run: `cargo test && cd gui && npm test`
Expected: PASS.

- [ ] **Step 5: Update `docs/epub-reading.md`, `docs/pdf-reading.md` and the notes line of `docs/concept-graph.md`, then commit**

```bash
git add src gui docs tests
git commit -m "Keep EPUB lists, quotes, tables, code and notes as containers, and PDF notes too"
```

---

### Task 10: EPUB inline structure and links

**Files:**
- Modify: `src/extract/xhtml.rs` (spans, exact page breaks; the `print_page` parameter goes)
- Modify: `src/extract/epub.rs` (resolve links across files)
- Modify: `docs/epub-reading.md`
- Test: `src/extract/xhtml.rs`, `tests/epub.rs`

**Interfaces:**
- Produces: `pub fn xhtml_blocks(html: &str, containers: &mut Vec<Container>) -> Vec<RawBlock>`; `RawBlock` gains `spans: Vec<Span>`, and its `print_page` and `anchor` fields go. Links leave `xhtml.rs` as `Link { href, to: Nowhere }` with the raw href, and `epub.rs` resolves them once every file is read.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn italics_bold_code_and_deletions_are_spans() {
    let b = &blocks("<p>Ah, <i>that’s</i> the <b>great</b> <code>x</code> <del>y</del>.</p>")[0];
    assert_eq!(spans(b), [(4, 10, emphasis()), (15, 20, strong()), (21, 22, code()), (23, 24, strike())]);
}

#[test]
fn the_crocodile_poem_keeps_its_lines_and_stanza() {
    // the <p class="poem"> of alice.epub chapter II, copied into the test
    assert_eq!(break_spans(b), [(30, 1), (56, 1), (88, 1), (111, 2), (145, 1), (174, 1), (203, 1)]);
}

#[test]
fn a_noteref_is_an_empty_span_and_its_text_stays_out() {
    let b = &blocks(r##"<p>Claim<a epub:type="noteref" href="#n1">1</a> continues.</p>"##)[0];
    assert_eq!(b.text, "Claim continues.");
    assert_eq!(spans(b), [(5, 5, noteref("1", None))]);
}

#[test]
fn a_page_break_inside_a_paragraph_is_a_point_where_it_stands() {
    let b = &blocks(r#"<p>one <span epub:type="pagebreak" title="46"/>two</p>"#)[0];
    assert_eq!(spans(b), [(4, 4, page_break("46"))]);
}
```

In `tests/epub.rs`:

```rust
#[test]
fn links_resolve_across_files_and_warn_only_inside_the_book() {
    // chapter a links to b.xhtml#x (exists), b.xhtml (no fragment), b.xhtml#gone, cover.jpg, https://x.org
    assert_eq!(targets, [Block { block: bx }, Block { block: b_first }, Nowhere, Nowhere, Address]);
    assert_eq!(broken_links(&doc), ["b.xhtml#gone"]);
}

#[test]
fn alice_italics_and_the_poem_survive_extraction() {
    // tests/fixtures/alice.epub: block 73 has an Emphasis over "that’s" at 446..452, block 75 has 7 Break spans
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test --lib extract::xhtml && cargo test --test epub`
Expected: FAIL.

- [ ] **Step 3: Implement the spans, page-break points and link resolution**

Spans pass through normalisation with the same offset mapping as Task 7; share that function rather than writing it twice. Text that was not in the text before this task (note markers) stays out, so `cargo test --test verify` and `tests/known_items.rs` keep passing.

- [ ] **Step 4: Run the suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Update `docs/epub-reading.md`, then commit**

```bash
git add src tests docs/epub-reading.md
git commit -m "Keep EPUB emphasis, line breaks, note references, page breaks and links as spans"
```

---

### Task 11: One view for any Document

**Files:**
- Create: `gui/layout.js` (no DOM; loaded by the window and by `node --test`)
- Create: `gui/layout.test.js`
- Modify: `gui/viewer.js` (`createViewer(element)`), `gui/book.js` (`viewOf`), `gui/renderer.js`, `gui/index.html` (contents column, styles for the new elements, `<script src="layout.js">`)
- Test: `gui/layout.test.js`, `gui/book.test.js`

**Interfaces:**
- Produces, in `gui/layout.js`:

```js
/** The blocks as a tree: { type: 'container', container, children } and { type: 'block', block } nodes, built in one pass by comparing each block's within with a stack. */
layout.nest(view) // view = { containers, blocks: [{ id, kind, text, within, spans }] }
/** The block's text as segments, { text } or { span, children }, nested as the spans are; offsets are converted from code points to UTF-16 once. */
layout.inline(text, spans)
/** The outline as nested entries { title, block, children }. */
layout.contents(outline)
```

- Produces, in `gui/viewer.js`: `createViewer(element, { onLink })` returns `{ show(view), mark(ids), scrollTo(block) }`. `onLink(span)` receives the `Link` or `NoteRef` span that was clicked. `Block` targets and `NoteRef`s scroll inside the viewer; the renderer handles `File` and `Address`.
- Produces: `viewOf(document)` returns `{ title, format, containers, outline, blocks: [{ id, kind, text, within, spans }] }`.

- [ ] **Step 1: Write the failing layout tests**

```js
test('nest opens and closes containers by comparing neighbouring paths', () => {
  // the Registers table: blocks with within [0,1,2] [0,1,3] [0,4,5] [0,4,6]
  assert.deepEqual(shape(layout.nest(view)), ['table', ['row', ['cell', 'Register'], ['cell', 'Address']], ['row', ['cell', 'STATUS'], ['cell', '0x1A']]]);
});

test('two adjacent lists stay apart because their ids differ', () => {});

test('inline nests spans and keeps text outside them', () => {
  assert.deepEqual(flat(layout.inline('Ah, that’s it', [{ start: 4, end: 10, kind: { type: 'emphasis' } }])), ['Ah, ', ['emphasis', 'that’s'], ' it']);
});

// Review Focus 2
test('inline converts code points to UTF-16 after an emoji', () => {
  assert.deepEqual(flat(layout.inline('😀 x', [{ start: 2, end: 3, kind: { type: 'emphasis' } }])), ['😀 ', ['emphasis', 'x']]);
});

test('an empty span is a point that inline still places', () => {
  // NoteRef at 5 in "Claim goes." gives ['Claim', ['note_ref'], ' goes.']
});

test('contents nest the outline by level', () => {});
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd gui && node --test layout.test.js`
Expected: FAIL, `layout.js` missing.

- [ ] **Step 3: Implement `layout.js`, then `createViewer` on top of it**

Text enters the DOM only through `textContent`. Elements: `ol` (with `start`), `ul`, `li`, `blockquote`, `table`, `tr`, `th` in a header row and `td` elsewhere, `aside` for a note, `pre > code`, `hr`, `em`, `strong`, `s`, `code`, `a`, `sup` for a `NoteRef`, and one `br` per counted line of a `Break`. The Book pane gains a contents column fed by `layout.contents`.

- [ ] **Step 4: Update `viewOf` and its tests, then run the window's tests**

Run: `cd gui && npm test`
Expected: PASS.

- [ ] **Step 5: Look at it**

Run `npm start` from `gui/` with a built binary (`cargo build --release` first), open `tests/fixtures/alice.epub` in the Book tab, and check that chapter II shows the poem in lines with a stanza gap and italic words.

- [ ] **Step 6: Commit**

```bash
git add gui
git commit -m "Draw any Document's containers and spans through one view"
```

---

### Task 12: The Help tab is a document

**Files:**
- Modify: `gui/renderer.js` (the Help pane holds a second `createViewer`, opened through `window.ebookHero.openBook` on the guide's path)
- Modify: `gui/help.js` (drop `readGuide` and `DOCS_URL`; add `guidePath()`), `gui/main.js` and `gui/preload.js` (the `guide` handler returns the path), `gui/helpview.js` (command help only), `gui/index.html`
- Delete: `gui/markdown.js`, `gui/markdown.test.js`
- Modify: `AGENTS.md` (lines on `gui/markdown.js`, `gui/help.js`, and the fields `gui/book.js` reads)
- Test: `gui/help.test.js`

**Interfaces:**
- Consumes: `createViewer` and `onLink` from Task 11, and `library.open` from Task 2.
- Produces: `guidePath()` returns the absolute path of `docs/usage.md` in the repository the window runs from. `KEPT_BOOKS` in `gui/book.js` goes from 2 to 3, so the guide and two books stay extracted at once.

- [ ] **Step 1: Rewrite the guide tests in `gui/help.test.js`**

```js
test('the guide is docs/usage.md of this repository', () => {
  assert.equal(help.guidePath(), path.join(__dirname, '..', 'docs', 'usage.md'));
});

test('the real binary extracts the guide with no broken link', { skip: !built }, async () => {
  // library.open({ path: guidePath() }) gives a view whose blocks hold "Using ebook-hero"
});
```

Delete the `readGuide` tests; keep the `commandHelp` and `openLink` tests.

- [ ] **Step 2: Run them and see them fail**

Run: `cd gui && node --test help.test.js`
Expected: FAIL, `guidePath` missing.

- [ ] **Step 3: Implement the change and delete `markdown.js` with its test**

A `File` link opens the linked file in the viewer it was clicked in, resolved against that viewer's document's directory. An `Address` with http or https goes to `openLink`. The contents column comes from the guide's outline, as for any book.

- [ ] **Step 4: Run the window's tests and look at the tab**

Run: `cd gui && npm test`
Expected: PASS. Then run `npm start`, open the Help tab, follow a `#` link and the link to `search.md`, and check that `search.md` opens in the same pane.

- [ ] **Step 5: Update AGENTS.md's architecture lines, then commit**

```bash
git add -A gui AGENTS.md
git commit -m "Open the usage guide in the same view as a book, and drop the window's Markdown renderer"
```

---

### Task 13: Release notes and final verification

**Files:**
- Modify: `Cargo.toml` (version `0.6.0`), `Cargo.lock`, `gui/package.json` (version `0.3.0`), `CHANGELOG.md`, `AGENTS.md` (scope line)

- [ ] **Step 1: Update the scope line in AGENTS.md**

It reads "`ebook-hero` searches and reads one PDF, EPUB or Markdown file per call, as a CLI and a stdio MCP server."

- [ ] **Step 2: Bump the versions and write the changelog entry under 0.6.0, dated 2026-11-06 or the day of the commit**

The entry marks schema 3 as a breaking change of the `Document` and the cache, and lists Markdown reading and writing, containers and spans, LZ4 cache files, exact passage marking and the Help tab.

- [ ] **Step 3: Check that no older name is left**

Run: `grep -rn --exclude-dir=node_modules --exclude-dir=superpowers --include=*.rs --include=*.js --include=*.md "ListItem\|BlockKind::Note\|page_label:\|print_page:\|\.orig\b\|markdown\.js\|document\.json\"" src tests gui docs AGENTS.md`
Expected: no output, or only prose that names an old form on purpose.

- [ ] **Step 4: Run every check from a clean build, one at a time**

Run: `df -h . && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo test -- --ignored && cd gui && npm test`
Expected: all pass. The `--ignored` run needs the models and may download them.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock gui/package.json CHANGELOG.md AGENTS.md
git commit -m "Release 0.6.0: Markdown, structured Documents stored once, compressed cache and one reader view"
```

Squashing the branch into one release commit before the pull request is part of finishing the branch, not of this task.
