# ebook-reader, part 1: structured Documents and Markdown

Branch `ebook-reader`, 2026-11-06.

## Why

The window has two readers. The Book tab draws an extracted `Document`, and the Help tab parses `docs/usage.md` with a renderer of its own, `gui/markdown.js`. The user wants one reader: the usage guide is a document like any book, opened by the same code and drawn by the same view, with no branch that only the guide takes. ebook-hero must therefore read Markdown into a `Document`, and the `Document` must hold enough structure to draw the guide's lists, quotes, code, tables, emphasis and links again.

Books need the same structure. The EPUB reader meets `table`, `pre`, `ol` and `blockquote` today and turns them into loose paragraphs, so a book's tables and code reach the window as plain runs of text.

This is the first of three parts on the branch. The second is a library view, opened by the logo, that lists the indexed books and opens them in the window. The third adds bookmarks, annotations, notes and a reading position. Both depend on this part, because the library opens `Document`s and annotations anchor on blocks and offsets. Each later part gets its own design.

## Decided with the user

- The window shows only extracted `Document`s, never the original PDF or EPUB.
- The window stays an Electron app that runs the `ebook-hero` binary, and all new logic is Rust in the library.
- Speed and sturdiness come first.
- The usage guide is read and drawn like any other book, so nothing in the window renders it alone.
- ebook-hero reads Markdown, and the `Document` stores enough structure to reconstruct a document's structure.
- The conversion is lossless with respect to the text and its structure. Images, figures and constructs the model does not support are outside that promise.

## Lossless, defined

The `Document` stores the text as data and the structure as metadata over it: `text` holds the characters, and containers and spans point into it by position without adding characters of their own. This is standoff annotation, as in TEI's stand-off markup and the Linguistic Annotation Framework (ISO 24612), and `docs/document-model.md` already states it as "plain text with standoff spans". Passages, search, verify and graph read only `text`, so structure never changes what they see.

Lossless then means two things. Every character the source format treats as text, and every structure it defines that the model names, reaches the `Document`. Writing a `Document` as Markdown and reading it back gives the same `Document`.

Two kinds of difference are outside the promise, because they carry no information:

- How the Markdown was typed: `*` or `_` for emphasis, `-` or `+` for a list item, where a paragraph's lines wrap. Two files that differ only in these are the same document. Keeping them would need a concrete syntax tree of the file, and a reader has no use for one.
- What `normalize` changes: NFC composition and ligatures, which Unicode defines as equivalent text, dropped soft hyphens, which only hint where to hyphenate, and collapsed whitespace runs, which both formats render as one space outside code blocks and `pre`. Code blocks keep their whitespace for that reason.

## Goals

1. ebook-hero reads a Markdown file into a `Document`, and every command and MCP tool that takes a book takes one.
2. The `Document` stores block structure (nested lists, quotes, tables, code blocks, rules) and inline structure (emphasis, strong, strikethrough, code, links, line breaks, note references).
3. `extract`'s Markdown writer writes that structure back with its text escaped, so that reading the output gives the same blocks, containers and spans.
4. The EPUB reader fills the new structure from XHTML.
5. The Book view draws the structure beside a contents list built from the `Document`'s outline. The Help tab is that view opened on `docs/usage.md`, and `gui/markdown.js` is deleted.

## Non-goals

- The PDF reader stays at headings, paragraphs and notes, since finding tables in a PDF is a problem of its own. Its one change is that a note becomes a `Note` container (see Containers).
- Images and figures. The `Document` gets no image field, and both readers leave an image and its alt text out, as the EPUB reader does now.
- Raw HTML and front matter in Markdown. An inline tag is dropped and the text between tags is kept. An HTML block or a front matter block is left out with a warning that names its line, because it may hold text. A comment is dropped without a warning, since it is not part of the text.
- `extract` reusing the cached document instead of extracting again. Part 2 needs it to open indexed books fast, so it is designed there.
- `read` keeps its output: a passage's text under its `<!-- passage … -->` line.

## Anti-goals

- A branch in the window that only the guide takes. If the Help tab needs code the Book tab does not run, this part has failed.
- Structure that changes what passages, search, verify or graph see. They go on reading `text`.
- Silent loss of text or structure. Whatever of either a reader cannot store becomes a warning.

## The Document, schema 3

`SCHEMA_VERSION` goes from 2 to 3. Every change adds a field or a variant, and every new field has a serde default, so JSON written under schema 2 still deserialises.

`text` stays the plain text that passages, search, verify and graph read, and the structure sits beside it in `within` and `spans`. Those parts therefore need no change for the new structure. The one exception is a code block, whose text keeps its lines (see Block kinds).

### Containers

A block gets `within: Vec<Container>`, the containers it sits in, outermost first, in the way `section` lists the headings above it. JSON leaves `within` out when it is empty.

```rust
pub struct Container {
    pub id: u32,
    pub kind: ContainerKind,
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContainerKind {
    /// `start` is the first number of an ordered list. A tight list has no
    /// blank line between its items, so its paragraphs are drawn without space.
    List { ordered: bool, start: Option<u64>, tight: bool },
    Item,
    Quote,
    Table { align: Vec<Align> },
    Row { header: bool },
    /// `columns` and `rows` count what the cell spans, 1 when it spans one.
    Cell { columns: u32, rows: u32 },
    /// A footnote or endnote. `label` is the note's id in an EPUB or its
    /// label in Markdown, and `None` in a PDF.
    Note { label: Option<String> },
}

#[serde(rename_all = "snake_case")]
pub enum Align { None, Left, Center, Right }
```

Ids count from 0 in the order the containers open, so two adjacent lists differ by id. A paragraph in an item of a list inside a quote has `within` like `[quote 0, list 1, item 3]`, and a cell's paragraph has `within` like `[table 4, row 5, cell 6]`. A table is therefore built like a list, and its cells hold ordinary blocks.

A container exists only through the blocks inside it, so an empty one would vanish. A reader therefore writes an empty `Paragraph` into a container that would otherwise hold no block, such as an empty cell or an empty list item.

Three models of nesting were weighed. A tree, as in Pandoc's AST or mdast, holds the same structure, but a block's id would stop being its index in `blocks`, and every reader of the `Document` would have to walk the tree. Docling's DoclingDocument keeps a flat list with references to parents, so a consumer follows links to learn where a block sits. The path puts that answer on the block, and the view opens and closes containers by comparing the paths of neighbouring blocks in one pass.

### Block kinds

`BlockKind` gains two variants, and two stop being written:

- `Code { language: Option<String> }` is a code block. Its `text` keeps its lines and indentation as written, without the final newline, since collapsing the whitespace would change the code. Verify still finds quotes in it, because `find_exact` in `src/verify.rs` collapses the whitespace of the text it searches.
- `Rule` is a thematic break with empty text.
- `ListItem` and `Note` are no longer written, because the `Item` and `Note` containers now say what they said. A paragraph in a list item is a `Paragraph` inside an `Item`, and a note's blocks keep their own kinds inside a `Note`, so a note can hold a list or a code block. The variants stay so that schema-2 JSON still reads, as the additive rule in AGENTS.md requires.

### Spans

`Span` stops being reserved. `SpanKind` keeps `Math { latex }`, which stays reserved, and gains:

```rust
Emphasis,
Strong,
Strikethrough,
Code,
/// `href` as written, and where the reader found that it leads.
Link { href: String, to: LinkTarget },
/// Marks the space in `text` that stands for a line break.
Break,
/// A footnote reference, as a point: `start` equals `end`. `marker` is what
/// the reference shows, such as `1`, and `block` is the note's first block.
NoteRef { marker: String, block: Option<u32> },
```

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkTarget {
    /// A block of this document.
    Block { block: u32 },
    /// A file beside this one, as a path relative to its directory.
    File { path: String },
    /// An address with a scheme, such as `https:` or `mailto:`.
    Address,
    /// A target the reader could not find, or one outside what the model holds.
    Nowhere,
}
```

The reader decides what a link is, so the window acts on `to` and never on the book's format.

- `start` and `end` count Unicode code points of `text`. `extract --format json` prints the `Document` as it is and the window reads it, and AGENTS.md fixes code points for output. Library code that needs bytes converts through one helper.
- Spans are well nested, so two spans either do not overlap or one contains the other, and they are sorted by `start`, longer first. The view and the writer rely on this order, so each reader's tests check it.
- Only a `NoteRef` may be empty. A note marker is apparatus, so it stays out of `text`, and a quote that runs across it still verifies, as it does in an EPUB today.

### Locators

`Format` gains `Markdown`, and `Locator` gains:

```rust
Markdown { line: u32, line_end: Option<u32>, anchor: Option<String> }
```

`line` is the line where the block starts in the file, counted from 1, and `line_end` is the line where it ends when that is a later line. `anchor` is the id of the last heading at or before the block, so a heading carries its own id, in the role that `anchor` plays in the EPUB locator.

`PassageLocator` gains `Markdown { line_start, line_end, under_heading: Option<HeadingPlace> }`. `locator_text` prints it like the EPUB form, with the heading and its paragraph range followed by the lines, such as `search ¶3–5, lines 260–284`.

### Warnings

`Warning` gains `BrokenLink { href: String, block: u32 }` for a link into the document that leads nowhere. An HTML block or front matter block that the Markdown reader leaves out is reported as `UnreadablePart`, whose part names the kind and the line, such as `HTML block at line 12`.

### Metadata and outline of a Markdown file

CommonMark defines no title, so `metadata.title` is `None` and the window shows the file name. `authors` is empty, and `language`, `page_count` and `spine_count` are `None`. The outline has one entry per heading, with its level and block id.

## The Markdown reader

The reader is `src/extract/markdown.rs`, built on pulldown-cmark, the pure-Rust parser that rustdoc and mdBook use. It turns on tables, strikethrough and footnotes, and reads every other extension's syntax as CommonMark does, so a task list's `[x]` stays text. It also turns on YAML metadata blocks, only so that front matter is recognised and left out: without the option, a `---` line, `title: x` and a second `---` read as a rule and a heading.

### Detection

`detect_format` keeps its order: the `%PDF-` marker in the first 1024 bytes, then the EPUB archive. A file that is neither is Markdown when its bytes are UTF-8 after an optional byte-order mark, because every UTF-8 text is a valid CommonMark document. No extension is consulted, as now. A plain `.txt` file therefore opens as a Markdown document. Any other file stays `Unsupported`, and its message names the three formats.

### Input cap

A file over `MAX_MARKDOWN_BYTES`, 64 MiB, fails with its size and the cap. Like the EPUB reader's caps, the constant lives in the reader module. 64 MiB of text is about ten million words, more than any book.

### Walk

`Parser::new_ext(source, options).into_offset_iter()` gives each event with its byte range in the file. A table of line starts, built once, turns a range into `line` and `line_end` by binary search. Block events open and close blocks and containers. Inline events append to the open block's text and push and pop a stack of open spans.

A block's text must equal `normalize` of itself, because search and verify rely on it, and its spans must point into that normalised text. The reader therefore collects a block's raw pieces with their span marks, normalises once while recording where each raw offset lands, and moves the spans through that record. A code block is kept verbatim and is not normalised.

Soft breaks become spaces. A hard break becomes a space marked by a `Break` span. A footnote reference becomes an empty `NoteRef` span whose marker is its label. A footnote definition becomes a `Note { label }` container where it stands in the file, and its blocks keep their kinds.

### Heading ids

Heading ids follow GitHub's rule, ported verbatim from `slugger` in `gui/markdown.js`. The id is the heading's text in lower case without characters other than letters, marks, numbers, connector punctuation, spaces and `-`, with each space turned into `-`. A repeated id gets `-1`, `-2` and so on. `docs/usage.md`'s links were written for GitHub, so under this rule they resolve as they do there.

### Links

Once every block is read, the reader resolves targets:

- `#id` leads to `Block`, the heading whose id it is.
- A footnote reference leads to the first block of the `Note` container with its label.
- A relative path leads to `File`, and an address with a scheme to `Address`.

A `#id` or footnote reference with no target leads `Nowhere` and adds a `BrokenLink` warning.

### Version

`MARKDOWN_READER_VERSION = 1` goes in `src/settings.rs`. The manifest gains `markdown_reader`, whose serde default of 0 makes an old manifest differ, so its index rebuilds.

## The Markdown writer

`src/document/markdown.rs` writes the new structure:

- list markers nested by indentation, with ordered numbers counted from `start`
- `> ` for each quote level
- fenced code, with a fence longer than the longest backtick run in the code
- GFM tables with their alignment row
- `---` for a rule
- `\` and a newline for a `Break`
- `*`, `**`, `~~`, code spans with enough backticks, and `[text](href)` links
- `[^label]` references and `[^label]:` definitions, where the label is the target `Note` container's label, or the marker when the note has none

A GFM table writes each row on one line, so its cells hold only inline text and cannot span rows or columns. An EPUB table can do both, so the writer writes a table that GFM cannot hold as an HTML table, with `rowspan` and `colspan` kept and each cell's blocks written as escaped text separated by `<br>`. Markdown renderers show it as a table. The Markdown reader leaves HTML out, but no document it reads holds such a table, so the round trip below is unaffected.

The writer escapes text so that reading the output gives the text back, and it escapes a character only where it could start syntax, so that prose stays readable. Tests decide the exact set. The PDF page comments `<!-- page N -->` stay. The reader drops comments, so they are gone on reading back, which suits a Markdown document because it has no pages.

The tests check one property. For a `Document` D read from Markdown, `read(write(D))` has the same blocks in order, with the same kind, text, `within` and spans. Locators may differ because lines move. The tests run the property on every file in `docs/` and on generated documents.

## The rest of the library

- **Passages.** A `Code` block packs like a paragraph, and so does each cell's paragraph. A block without words, such as a `Rule` or an empty cell, goes into no passage. A `Rule` closes the passage being built, as a heading does, but starts no section, because a rule separates parts of a text. `same_part` treats a Markdown file as one part. `PASSAGES_VERSION` goes up because a rule now closes a passage.
- **read.** `locator_text` prints the Markdown locator. `read page` on a Markdown file fails in the same way as on an EPUB that marks no printed pages, and its message names `passage` and `outline` instead.
- **Search, verify and graph.** These read `text`, so they do not change.
- **CLI and MCP.** Descriptions that say "PDF or EPUB" say "PDF, EPUB or Markdown", and `info` prints the Markdown format.

## The EPUB reader

`src/extract/xhtml.rs` keeps a stack of open containers as it walks the DOM, and maps elements as follows:

| XHTML | Document |
|---|---|
| `ul`, `ol` (with `start`), `li` | `List`, `Item` |
| `blockquote` | `Quote` |
| `table`; `tr` (a header row in `thead` or holding only `th`); `th`, `td` with `colspan` and `rowspan` | `Table`, `Row`, `Cell` |
| `pre` | `Code` |
| `hr` | `Rule` |
| `br` | `Break` |
| `em`, `i`; `strong`, `b`; `code`; `del`, `s` | `Emphasis`; `Strong`; `Code`; `Strikethrough` |
| `a href` | `Link`, resolved across the book's files |
| `a epub:type="noteref"` | an empty `NoteRef` whose marker is the element's text, which stays out of `text` as now |
| a footnote or endnote element | `Note` container, whose paragraphs stay separate blocks instead of joining into one |

Images stay out, as now. A link is resolved by the rule that a missing target in scope warns and a target outside it does not:

- `file.xhtml#id`, with `file.xhtml` in the spine, leads to the first block of that file whose locator has `anchor` equal to `id`. Without `#id` it leads to the file's first block.
- A spine file with no block for `id` leads `Nowhere` and adds a `BrokenLink` warning, since the book links to its own text and misses.
- A file outside the spine, such as the cover, an image or the navigation document, leads `Nowhere` without a warning, because the model holds none of them.
- An address with a scheme leads to `Address`. The EPUB reader never writes `File`, since every relative link points into the archive.

`EPUB_READER_VERSION` goes up. `table_cells_and_figure_captions_are_paragraphs` changes, because its cells now sit in `Table`, `Row` and `Cell` containers, and the note tests change because a note's paragraphs no longer join.

## The window

- **book.js** sets `MIN_SCHEMA_VERSION` to 3. `viewOf` passes `within`, `spans` and the outline to the page.
- **viewer.js** becomes a factory, `createViewer(element)`, so that the Book and Help tabs each hold their own instance of one view. It draws the blocks in one pass with a stack of open container elements. For each block it compares the block's `within` with the stack, closes the containers that differ, opens the new ones (`ol` with `start`, `ul`, `li`, `blockquote`, `table`, `tr`, and `th` or `td` with their spans), and draws the block with its spans as nested inline elements. Text goes in only through `textContent`, so a book cannot add markup to the window. Span offsets count code points, so the view converts them to UTF-16 indexes once per block.
- **Links.** The view acts on `to`. `Block` scrolls to the block, as a passage link does, and a `NoteRef` with a block does the same. `File` opens the file in the same view, resolved against the open document's directory. An `Address` with http or https opens in the browser through `openLink`. Any other address, and `Nowhere`, is drawn as text.
- **Contents.** Both instances build their contents list from the `Document`'s outline, by one rule for every document.
- **Help tab.** The tab opens `docs/usage.md` through `library.open`, as the Book tab opens a book. Beside it the tab keeps the binary's `help <tool>` text, which is command output and not a document. `markdown.js` and `markdown.test.js` are deleted, `helpview.js` keeps only the command help, and `readGuide` and the GitHub base for relative links go.
- **Tests that move.** The check that every `#` link in the guide has its target becomes a Rust test: extracting `docs/usage.md` gives no `BrokenLink` warning, and every relative link in `docs/` names a file that exists.

## Docs, rules and versions

- `docs/document-model.md` describes schema 3: containers, the new kinds and spans, the code-point unit of span offsets, the Markdown locator and why the container path was chosen over a tree.
- `docs/markdown-reading.md` is new and explains the reader's algorithm.
- `docs/epub-reading.md` describes the structure the EPUB reader now keeps.
- `docs/usage.md` says that Markdown files are read and how they are detected, gives the new `Unsupported` message and the Markdown locator form, and describes what `extract` writes.
- AGENTS.md's scope says "one PDF, EPUB or Markdown file per call". Its lines on `gui/markdown.js` and `gui/help.js` go, and its line on the fields `gui/book.js` reads gains `within`, `spans` and `outline`.
- `docs/pdf-reading.md` says that a note is a `Note` container.
- Versions: `SCHEMA_VERSION` 3, `MARKDOWN_READER_VERSION` 1, and `PDF_READER_VERSION`, `EPUB_READER_VERSION` and `PASSAGES_VERSION` up by one. `Cargo.toml` goes to 0.6.0 with a `CHANGELOG.md` entry, and `gui/package.json` goes to 0.3.0.
- Commits. Each step of the order of work is a commit on the branch, and the last one bumps the versions and adds the changelog entry. Before the pull request the commits are squashed into one release commit, so that main receives one change with its version bump, as AGENTS.md requires and as the 0.5.0 branch landed.

## Order of work

Each step leaves `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cd gui && npm test` passing.

1. The schema 3 types, with every `match` updated, and `docs/document-model.md`.
2. The Markdown reader, detection, locators, passages and `read`.
3. The writer and the round-trip tests.
4. The window: the view, the Help tab, and the removal of `markdown.js`.
5. The EPUB reader's structure, and the PDF reader's `Note` containers.
6. The remaining docs, AGENTS.md, versions and changelog.

## Calls made without the user

- The container path on flat blocks, over a tree.
- Span offsets count code points.
- Rows and cells are containers, and an empty container holds an empty `Paragraph`.
- The writer writes a table that GFM cannot hold as an HTML table.
- Notes are containers in all three readers, so the PDF reader changes too.
- A note marker is an empty `NoteRef` span and stays out of `text`.
- A link's target is resolved by the reader into `LinkTarget`. A missing target inside the book warns, and a target outside what the model holds does not.
- One commit per step on the branch, squashed into one release commit before the pull request.
- Code blocks keep their text verbatim.
- Any UTF-8 file that is not a PDF or an EPUB is Markdown, with no extension check.
- A 64 MiB cap on a Markdown file.
- A Markdown file has no title.
- Hard breaks are kept as `Break` spans.
- HTML blocks and front matter are left out with a warning, and comments without one.
- A rule closes a passage.
- The Help tab keeps the binary's `help <tool>` text beside the guide.
- A relative link opens the linked file in the window, where it used to open on GitHub.
- The EPUB structure is part of this part, not a later one.

## Left to the builder

- The exact set of characters the writer escapes, decided by the round-trip tests.
- The wording of the Markdown locator text.
- How to test Unicode categories for heading ids: a crate, or tables of their own.
- Whether generated round-trip cases come from a small generator in the tests or from a new dev-dependency.
- The CSS for the new elements.
