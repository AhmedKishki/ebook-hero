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
- Weak structure may be lost: how a structure looks may change, as long as a table stays a table with its cells as they are.
- Each fact is stored once. The `Document` and the index cache hold no second copy of anything.
- Schema 3 may break compatibility with schema 2. ebook-hero is in early development, so a good design matters more here than the additive rule in AGENTS.md.

## Lossless, defined

The `Document` stores the text as data and the structure as metadata over it: `text` holds the characters, and containers and spans point into it by position without adding characters of their own. This is standoff annotation, as in TEI's stand-off markup and the Linguistic Annotation Framework (ISO 24612), and `docs/document-model.md` already states it as "plain text with standoff spans". Passages, search, verify and graph read only `text`, so structure never changes what they see.

Lossless then means two things. Every character the source format treats as text, and every structure it defines that the model names, reaches the `Document`. Writing a `Document` as Markdown and reading it back gives the same `Document`.

Three kinds of difference are outside the promise. The first two carry no information, and the user rules the third out:

- How the Markdown was typed: `*` or `_` for emphasis, `-` or `+` for a list item, where a paragraph's lines wrap. Two files that differ only in these are the same document. Keeping them would need a concrete syntax tree of the file, and a reader has no use for one.
- What `normalize` changes: NFC composition and ligatures, which Unicode defines as equivalent text, dropped soft hyphens, which only hint where to hyphenate, and collapsed whitespace runs, which both formats render as one space outside code blocks and `pre`. Code blocks keep their whitespace for that reason.
- Weak structure, which says how a structure is drawn and not what it is: a column's alignment, the spacing between a list's items, a cell that spans several rows or columns. The model does not store it. Strong structure is what a block is and what it sits in: a heading and its level, a table with its rows and cells, a list with its items, a quote, a note, a code block.

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
- `read` keeps its Markdown output: a passage's text under its `<!-- passage … -->` line.

## Anti-goals

- A branch in the window that only the guide takes. If the Help tab needs code the Book tab does not run, this part has failed.
- Structure that changes what passages, search, verify or graph see. They go on reading `text`.
- Silent loss of text or structure. Whatever of either a reader cannot store becomes a warning.

## The Document, schema 3

`SCHEMA_VERSION` goes from 2 to 3, and schema 3 does not read schema 2. Nothing is lost by that, since the manifest records the schema and a cached index of another schema is built again.

`text` stays the plain text that passages, search, verify and graph read, and the structure sits beside it in `within` and `spans`. Those parts therefore need no change for the new structure. The one exception is a code block, whose text keeps its lines (see Block kinds).

### Each fact once

A fact is stored once, at the level where it holds: the document, a run of blocks, one block, or a point in a block's text. Whatever can be derived from what is stored is computed when the book is loaded, into lookup tables kept in memory, the way a database builds an index from its rows without storing the rows twice. One pass over the blocks builds them, and later lookups take constant time.

Today Alice's index holds its text twice, once in the blocks of `document.json` and again in the passages of `passages.json`. Every block repeats the headings above it as strings and its chapter file's name, though Alice has 875 blocks and only 19 sections and 13 files. The changes:

| fact | stored today | stored in schema 3 |
|---|---|---|
| the book's text | in every block, and again in every passage | in the blocks only |
| a block's section | the heading texts above it, in every block and every passage | `section: Option<u32>`, an index into `sections` |
| an EPUB block's file | its href string, in every block | `file: u32`, an index into `files` |
| an EPUB block's paragraph number | in every block | derived: its position after the first block of its file |
| an element id | the last id before the block, repeated in every block after it | `ids` on the block where the element is |
| an EPUB printed page | the page in effect, repeated in every block | a `PageBreak` span at the break, exact even inside a paragraph |
| a PDF page label | in every block of the page | `page_labels`, one entry per page |
| a block's id | in every block, though it equals the block's position | derived: the position |
| the format of a locator | a tag in every block | derived: `source.format` |
| a passage's text, section and locator | in every passage | derived from its pieces |

`sections` holds one entry per heading, `Section { heading: u32, parent: Option<u32> }`, where `heading` is the heading's block and `parent` the section around it. A block's section path is the chain of parents, and each title is its heading block's text. Sections cannot be containers: a heading inside a table cell opens a section that runs on past the table, which no nesting of containers can hold.

`passages.json` keeps, for each passage, its pieces as block, offset in the block and length. Loading assembles each passage's text from the blocks once, because search, find and verify scan passage texts many times, and that copy lives only in memory.

Simulated on Alice's cached index, `document.json` goes from 392 KB to 258 KB and `passages.json` from 272 KB to 51 KB, so the two files shrink from 664 KB to 309 KB, 53% less.

Output does not change shape. Search hits, read results and the MCP tools still give each passage's section path and locator in full, derived as they are written, so a hit stands alone for whoever reads it.

### Containers

The `Document` gains `containers: Vec<Container>`, each container stored once, its id being its index. A block gets `within: Vec<u32>`, the ids of the containers it sits in, outermost first. JSON leaves either list out when it is empty. Storing each container once keeps a block's path to a few numbers: a table of 1,000 cells would otherwise repeat its table and row in every cell.

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Container {
    /// `start` is the first number of an ordered list, kept because readers
    /// cite an item by its number.
    List { ordered: bool, start: Option<u64> },
    Item,
    Quote,
    Table,
    Row { header: bool },
    Cell,
    /// A footnote or endnote. `label` is the note's id in an EPUB or its
    /// label in Markdown, and `None` in a PDF.
    Note { label: Option<String> },
}
```

Containers are stored in the order they open, so two adjacent lists differ by id. A paragraph in an item of a list inside a quote has `within` like `[0, 1, 3]` for quote 0, list 1 and item 3. A table is built like a list, and its cells hold ordinary blocks:

```markdown
## Registers

| Register | Address |
|----------|---------|
| STATUS   | 0x1A    |
| CONTROL  |         |
```

gives `containers` `[table, row (header), cell, cell, row, cell, cell, row, cell, cell]` with ids 0 to 9, and these blocks, all under the section `Registers`:

| id | kind | text | within | line |
|---|---|---|---|---|
| 0 | heading 2 | Registers | | 1 |
| 1 | paragraph | Register | 0, 1, 2 | 3 |
| 2 | paragraph | Address | 0, 1, 3 | 3 |
| 3 | paragraph | STATUS | 0, 4, 5 | 5 |
| 4 | paragraph | 0x1A | 0, 4, 6 | 5 |
| 5 | paragraph | CONTROL | 0, 7, 8 | 6 |
| 6 | paragraph | (empty) | 0, 7, 9 | 6 |

An EPUB cell holding two paragraphs gives two blocks with the same `within`.

A container exists only through the blocks inside it, so an empty one would vanish. A reader therefore writes an empty `Paragraph` into a container that would otherwise hold no block, such as an empty cell or an empty list item.

Three models of nesting were weighed. A tree, as in Pandoc's AST or mdast, holds the same structure, but a block's id would stop being its index in `blocks`, and every reader of the `Document` would have to walk the tree. Docling's DoclingDocument keeps a flat list with references to parents, so a consumer follows links to learn where a block sits. The path puts that answer on the block, and the view opens and closes containers by comparing the paths of neighbouring blocks in one pass. The containers' kinds sit in one list beside the blocks, so the path costs a few ids per block.

### Block kinds

`BlockKind` gains two variants and loses two:

- `Code { language: Option<String> }` is a code block. Its `text` keeps its lines and indentation as written, without the final newline, since collapsing the whitespace would change the code. Verify still finds quotes in it, because `find_exact` in `src/verify.rs` collapses the whitespace of the text it searches.
- `Rule` is a thematic break with empty text.
- `ListItem` and `Note` go, because the `Item` and `Note` containers now say what they said. A paragraph in a list item is a `Paragraph` inside an `Item`, and a note's blocks keep their own kinds inside a `Note`, so a note can hold a list or a code block.

The reserved field `orig` goes too. No reader writes it, and `normalize` keeps text equivalent, so the original characters carry nothing the text lacks.

### Spans

`Span` stops being reserved. `SpanKind` keeps `Math { latex }`, which stays reserved, and gains:

```rust
Emphasis,
Strong,
Strikethrough,
Code,
/// `href` as written, and where the reader found that it leads.
Link { href: String, to: LinkTarget },
/// Marks the space in `text` that stands for line breaks. `lines` counts
/// them, so 2 leaves a blank line, as between two stanzas of a poem.
Break { lines: u32 },
/// A footnote reference, as a point: `start` equals `end`. `marker` is what
/// the reference shows, such as `1`, and `block` is the note's first block.
NoteRef { marker: String, block: Option<u32> },
/// A printed page break, as a point: the printed page `label` starts here.
PageBreak { label: String },
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
- Only a `NoteRef` or a `PageBreak` may be empty, since each marks a point. A note marker is apparatus and a page break is a position, so neither adds characters to `text`, and a quote that runs across either still verifies.

### Locators

A block's locator keeps only what differs from block to block:

```rust
pub enum Locator {
    Pdf { page: u32, page_end: Option<u32>, bbox: Option<[f32; 4]> },
    Epub { file: u32 },
    Markdown { line: u32, line_end: Option<u32> },
}
```

`Format` gains `Markdown`. A Markdown block's `line` is the line where it starts in the file, counted from 1, and `line_end` the line where it ends when that is a later line. The `Document` gains `files: Vec<String>`, the EPUB's content files in spine order, and `page_labels: Vec<Option<String>>`, the printed label of each physical page of a PDF, empty when the PDF defines none.

A block gains `ids: Vec<String>`, the ids of the elements that start in it and of the empty elements since the block before. A Markdown heading's id goes there too. JSON leaves `ids` out when it is empty. Links resolve through `ids`, and the anchor in effect at a block, which the EPUB locator stores today, is derived when it is needed.

`PassageLocator` gains `Markdown { line_start, line_end, under_heading: Option<HeadingPlace> }`. `locator_text` prints it like the EPUB form, with the heading and its paragraph range followed by the lines, such as `search ¶3–5, lines 260–284`.

### Warnings

`Warning` gains `BrokenLink { href: String, block: u32 }` for a link into the document that leads nowhere. An HTML block or front matter block that the Markdown reader leaves out is reported as `UnreadablePart`, whose part names the kind and the line, such as `HTML block at line 12`.

### Metadata and outline of a Markdown file

CommonMark defines no title, so `metadata.title` is `None` and the window shows the file name. `authors` is empty, and `language`, `page_count` and `spine_count` are `None`. The outline has one entry per heading, with its level and block id.

## The Markdown reader

The reader is `src/extract/markdown.rs`, built on pulldown-cmark, the pure-Rust parser that rustdoc and mdBook use. It turns on tables, strikethrough and footnotes, and reads every other extension's syntax as CommonMark does, so a task list's `[x]` stays text. It also turns on YAML metadata blocks, only so that front matter is recognised and left out: without the option, a `---` line, `title: x` and a second `---` read as a rule and a heading.

### Detection

`detect_format` checks five things in order: the `%PDF-` marker at byte 0, the EPUB archive, UTF-8 text after an optional byte-order mark, the `%PDF-` marker elsewhere in the first 1024 bytes, and otherwise `Unsupported`, whose message names the three formats. Every UTF-8 text is a valid CommonMark document, so a text file is Markdown, and a plain `.txt` file opens as one. The PDF header belongs at the start of the file, and the 1024-byte tolerance exists for binary bytes before a real header, so it applies only to a file that is not text; a Markdown note that mentions `%PDF-` near its start is therefore read as Markdown. No extension is consulted, as now.

### Input cap

A file over `MAX_MARKDOWN_BYTES`, 64 MiB, fails with its size and the cap. Like the EPUB reader's caps, the constant lives in the reader module. 64 MiB of text is about ten million words, more than any book.

The size cap does not bound memory, because GFM pads every table row to the width of its header and pulldown-cmark builds the whole document before it yields events: a 120 KB file can ask for ten million cells. A second cap, `MAX_TABLE_CELLS`, 250,000 cells per file, is checked by a scan of the source before parsing. The scan counts each table's columns times its following lines, which can only over-count, and a file over the cap fails with the table's line, its size and the cap.

### Walk

`Parser::new_ext(source, options).into_offset_iter()` gives each event with its byte range in the file. A table of line starts, built once, turns a range into `line` and `line_end` by binary search. Block events open and close blocks and containers. Inline events append to the open block's text and push and pop a stack of open spans.

A block's text must equal `normalize` of itself, because search and verify rely on it, and its spans must point into that normalised text. The reader therefore collects a block's raw pieces with their span marks, normalises once while recording where each raw offset lands, and moves the spans through that record. A code block is kept verbatim and is not normalised.

Soft breaks become spaces. A hard break becomes a space marked by a `Break` span, and a run of hard breaks becomes one `Break` whose `lines` counts them, because normalising leaves one space for the whole run. A footnote reference becomes an empty `NoteRef` span whose marker is its label. A footnote definition becomes a `Note { label }` container where it stands in the file, and its blocks keep their kinds.

### Heading ids

Heading ids follow GitHub's rule, ported verbatim from `slugger` in `gui/markdown.js`. The id is the heading's text in lower case without characters other than letters, marks, numbers, connector punctuation, spaces and `-`, with each space turned into `-`. A repeated id gets `-1`, `-2` and so on. `docs/usage.md`'s links were written for GitHub, so under this rule they resolve as they do there. The id goes into the heading block's `ids`.

### Links

Once every block is read, the reader resolves targets:

- `#id` leads to `Block`, the block whose `ids` hold it.
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
- GFM pipe tables, as described below
- `---` for a rule
- `\` and a newline for each line of a `Break`
- `*`, `**`, `~~`, code spans with enough backticks, and `[text](href)` links
- `[^label]` references and `[^label]:` definitions, where the label is the target `Note` container's label, or the marker when the note has none

Every table is written as a GFM pipe table, the one table syntax that CommonMark tools share. A pipe table writes each row on one line, so a cell whose content is several blocks, as an EPUB cell can be, is written on one line with its blocks separated by a space. The cell keeps its text and stays a cell, and only how it looks changes. GFM requires a header line, so a table without a header row is written with an empty one. A list gets a blank line between its items only when an item holds more than one block, because Markdown needs one there.

The writer escapes text so that reading the output gives the text back, and it escapes a character only where it could start syntax, so that prose stays readable. Tests decide the exact set. The PDF page comments `<!-- page N -->` stay. The reader drops comments, so they are gone on reading back, which suits a Markdown document because it has no pages.

The tests check one property. For a `Document` D read from Markdown, `read(write(D))` has the same blocks in order, with the same kind, text, `within` and spans. Locators may differ because lines move. The tests run the property on every file in `docs/` and on generated documents.

## The rest of the library

- **Passages.** A `Code` block packs like a paragraph, and so does each cell's paragraph. A block without words, such as a `Rule` or an empty cell, goes into no passage. A `Rule` closes the passage being built, as a heading does, but starts no section, because a rule separates parts of a text. Packing compares section ids and, in an EPUB, file ids, and treats a Markdown file as one part. A passage is stored as its pieces (see Each fact once). `PASSAGES_VERSION` goes up because a rule now closes a passage and the stored form changes.
- **read.** `locator_text` prints the Markdown locator. `read page` on a Markdown file fails in the same way as on an EPUB that marks no printed pages, and its message names `passage` and `outline` instead. Each passage of `read --json` gains `blocks`, the ids of the blocks it covers, so the window marks a passage's blocks exactly in every format.
- **Search, verify and graph.** These read `text`, so they do not change.
- **CLI and MCP.** Descriptions that say "PDF or EPUB" say "PDF, EPUB or Markdown", and `info` prints the Markdown format.

## Cache files

The index directory's JSON files are compressed with LZ4 through `lz4_flex`, which is pure Rust and already in the dependency tree through tantivy: `document.json.lz4`, `passages.json.lz4` and `graph.json.lz4`. Each name gives both layers, JSON inside LZ4, as `.tar.gz` does. Measured on Alice and on the largest cached book, LZ4 shrinks the files about three times, while decompressing adds about 6% to the time spent parsing the JSON. That fits a project that puts speed first better than brotli, whose files are half the size of LZ4's but whose decompression adds about a quarter.

The files use the LZ4 frame format with its content checksum, through `lz4_flex`'s `frame` feature, which adds the small pure-Rust crate `twox-hash`. The frame's magic number makes a file that is not LZ4 fail at once. Without the checksum, a damaged byte inside a passage of text would decompress into a wrong character inside valid JSON, and the book would change without a sign. A file whose magic number or checksum fails gives an error that names the file and tells the user to run `ebook-hero cache clear`. The frame format is also what the `lz4` command reads, so `lz4 -dc document.json.lz4` shows a cached document.

`manifest.json` stays plain JSON, since it is a few hundred bytes and is read first to decide whether the index is current. `extract --format json` keeps printing plain JSON. The tantivy index compresses itself, and `vectors.f32` holds floats that barely compress, so neither changes. Writes stay atomic: the compressed bytes go to a temporary file that is then renamed.

## The EPUB reader

`src/extract/xhtml.rs` keeps a stack of open containers as it walks the DOM, and maps elements as follows:

| XHTML | Document |
|---|---|
| `ul`, `ol` (with `start`), `li` | `List`, `Item` |
| `blockquote` | `Quote` |
| `table`; `tr` (a header row in `thead` or holding only `th`); `th`, `td`, whose `colspan` and `rowspan` are weak structure and dropped | `Table`, `Row`, `Cell` |
| `pre` | `Code` |
| `hr` | `Rule` |
| `br`, and a run of them | one `Break` counting them |
| `em`, `i`; `strong`, `b`; `code`; `del`, `s` | `Emphasis`; `Strong`; `Code`; `Strikethrough` |
| `a href` | `Link`, resolved across the book's files |
| `a epub:type="noteref"` | an empty `NoteRef` whose marker is the element's text, which stays out of `text` as now |
| an element with `epub:type="pagebreak"` | an empty `PageBreak` whose label is the page's title |
| an element's `id` | an entry of the block's `ids` |
| a footnote or endnote element | `Note` container, whose paragraphs stay separate blocks instead of joining into one |

Images stay out, as now. A link is resolved by the rule that a missing target in scope warns and a target outside it does not:

- `file.xhtml#id`, with `file.xhtml` in the spine, leads to the block of that file whose `ids` hold `id`. Without `#id` it leads to the file's first block.
- A spine file with no block for `id` leads `Nowhere` and adds a `BrokenLink` warning, since the book links to its own text and misses.
- A file outside the spine, such as the cover, an image or the navigation document, leads `Nowhere` without a warning, because the model holds none of them.
- An address with a scheme leads to `Address`. The EPUB reader never writes `File`, since every relative link points into the archive.

`EPUB_READER_VERSION` goes up. `table_cells_and_figure_captions_are_paragraphs` changes, because its cells now sit in `Table`, `Row` and `Cell` containers, and the note tests change because a note's paragraphs no longer join.

## The window

- **book.js** sets `MIN_SCHEMA_VERSION` to 3. `viewOf` passes `containers`, each block's `within` and `spans`, and the outline to the page. A passage's blocks come from the `blocks` of `read --json`, so `passageBlocks` and its matching of PDF text go, and the window reads no block locator.
- **viewer.js** becomes a factory, `createViewer(element)`, so that the Book and Help tabs each hold their own instance of one view. It draws the blocks in one pass with a stack of open container elements. For each block it compares the block's `within` with the stack, closes the containers that differ, opens the new ones (`ol` with `start`, `ul`, `li`, `blockquote`, `table`, `tr`, `th` in a header row and `td` elsewhere), and draws the block with its spans as nested inline elements. Text goes in only through `textContent`, so a book cannot add markup to the window. Span offsets count code points, so the view converts them to UTF-16 indexes once per block.
- **Links.** The view acts on `to`. `Block` scrolls to the block, as a passage link does, and a `NoteRef` with a block does the same. `File` opens the file in the same view, resolved against the open document's directory. An `Address` with http or https opens in the browser through `openLink`. Any other address, and `Nowhere`, is drawn as text.
- **Contents.** Both instances build their contents list from the `Document`'s outline, by one rule for every document.
- **Help tab.** The tab opens `docs/usage.md` through `library.open`, as the Book tab opens a book. Beside it the tab keeps the binary's `help <tool>` text, which is command output and not a document. `markdown.js` and `markdown.test.js` are deleted, `helpview.js` keeps only the command help, and `readGuide` and the GitHub base for relative links go.
- **Tests that move.** The check that every `#` link in the guide has its target becomes a Rust test: extracting `docs/usage.md` gives no `BrokenLink` warning, and every relative link in `docs/` names a file that exists.

## Docs, rules and versions

- `docs/document-model.md` describes schema 3: each fact stored once and what is derived at load, containers, sections, the new kinds and spans, the code-point unit of span offsets, the locators and why the container path was chosen over a tree.
- `docs/markdown-reading.md` is new and explains the reader's algorithm.
- `docs/epub-reading.md` describes the structure the EPUB reader now keeps.
- `docs/usage.md` says that Markdown files are read and how they are detected, gives the new `Unsupported` message and the Markdown locator form, and describes what `extract` writes.
- AGENTS.md's scope says "one PDF, EPUB or Markdown file per call". Its lines on `gui/markdown.js` and `gui/help.js` go, and its line on the fields `gui/book.js` reads names `containers`, `blocks[].{kind, text, within, spans}`, `outline` and the `blocks` of a passage in `read --json`. The additive rule stays as written; this change is the exception the user granted, and the changelog marks it as breaking.
- `docs/pdf-reading.md` says that a note is a `Note` container.
- `docs/usage.md`'s section on where files are kept names the compressed cache files and how to read one with `lz4 -dc`, and the layout comment at the top of `src/index/cache.rs` names them too.
- Versions: `SCHEMA_VERSION` 3, `MARKDOWN_READER_VERSION` 1, and `PDF_READER_VERSION`, `EPUB_READER_VERSION` and `PASSAGES_VERSION` up by one. `Cargo.toml` goes to 0.6.0 with a `CHANGELOG.md` entry, and `gui/package.json` goes to 0.3.0.
- Commits. Each step of the order of work is a commit on the branch, and the last one bumps the versions and adds the changelog entry. Before the pull request the commits are squashed into one release commit, so that main receives one change with its version bump, as AGENTS.md requires and as the 0.5.0 branch landed.

## Order of work

Each step leaves `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cd gui && npm test` passing.

1. The schema 3 types, with every `match` updated: sections, files, page labels, `ids` and `PageBreak` stored once, the lookup tables built at load, passages stored as pieces, the cache files in LZ4 frames, and `docs/document-model.md`.
2. The Markdown reader, detection, locators, passages and `read`.
3. The writer and the round-trip tests.
4. The window: the view, the Help tab, and the removal of `markdown.js`.
5. The EPUB reader's structure, and the PDF reader's `Note` containers.
6. The remaining docs, AGENTS.md, versions and changelog.

## Calls made without the user

- The container path on flat blocks, over a tree.
- Sections are a table of headings with parents, not containers.
- Derived facts are computed at load into lookup tables, and passage texts are assembled in memory only.
- `orig` goes, and so do the `ListItem` and `Note` block kinds.
- Printed pages become `PageBreak` points, and element ids live on the block where the element is.
- `read --json` gives each passage's block ids, and the window drops its PDF text matching.
- The cache files use the LZ4 frame format with its content checksum and end in `.json.lz4`, and `manifest.json` stays plain.
- Span offsets count code points.
- Rows and cells are containers, and an empty container holds an empty `Paragraph`.
- Column alignment, list spacing and merged-cell spans count as weak structure and are not stored.
- An ordered list's start number counts as strong structure, since readers cite items by number.
- Emphasis, strong, strikethrough, inline code, links and line breaks count as strong structure.
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
- How JSON leaves out the locator's format tag and the block id, by fields that tell the variants apart or by reading `source.format` first, whichever deserialises faster.
- The CSS for the new elements.
