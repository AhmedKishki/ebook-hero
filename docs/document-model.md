# The Document model

The two readers in `src/extract/` turn a PDF or an EPUB into a `Document`, the type defined in `src/document/mod.rs`. Passage packing, the indexes, search, reading, quote verification and the concept graph all start from that value and never change it. Code after the readers therefore handles one shape of book, whatever format the file had. Only the locators keep each format's own way of saying where a piece of text sits.

`docs/pdf-reading.md` and `docs/epub-reading.md` describe how each reader fills the model.

## Where the model sits

```mermaid
flowchart LR
    pdf["PDF file"] --> pdfr["src/extract/pdf.rs"]
    epub["EPUB file"] --> epubr["src/extract/epub.rs"]
    pdfr --> doc(["Document"])
    epubr --> doc
    doc --> cache["document.json.lz4 in the index cache"]
    doc --> extract["ebook-hero extract: JSON or Markdown"]
    doc --> direct["book_info, outline, read"]
    doc --> places(["Places, built at load"])
    doc --> passages["build_passages in src/passages.rs"]
    places --> passages
    passages --> tools["search, find, read, verify_quote, concept_graph"]
```

`src/index/book.rs` runs the reader once per book, packs the blocks into passages and writes the document and the passages to the user cache as `document.json.lz4` and `passages.json.lz4`, in a directory named by the file's SHA-256. Later calls load the cached `Document` instead of reading the file again, and loading builds the book's `Places` from it and the passages from the stored pieces (see [Each fact once](#each-fact-once)). `ebook-hero extract` runs the reader on every call and prints the `Document`: as JSON with `--format json` or `--json`, otherwise as Markdown.

## Structure

```mermaid
classDiagram
    class Document {
        schema_version: u32
        source: Source
        metadata: Metadata
        outline: Vec~OutlineEntry~
        sections: Vec~Section~
        files: Vec~String~
        page_labels: Vec~Option~String~~
        blocks: Vec~Block~
        furniture: Vec~Block~
        warnings: Vec~Warning~
    }
    class Source {
        sha256: String
        file_name: String
        format: Format
    }
    class Format {
        <<enumeration>>
        Pdf
        Epub
    }
    class Metadata {
        title: Option~String~
        authors: Vec~String~
        language: Option~String~
        page_count: Option~u32~
        spine_count: Option~u32~
    }
    class OutlineEntry {
        id: u32
        title: String
        level: u8
        block_id: Option~u32~
    }
    class Section {
        heading: u32
        parent: Option~u32~
    }
    class Block {
        id: u32
        kind: BlockKind
        text: String
        spans: Vec~Span~
        ids: Vec~String~
        section: Option~u32~
        locator: Locator
    }
    class BlockKind {
        <<enumeration>>
        Heading: level
        Paragraph
        ListItem
        Note: label
    }
    class Span {
        start: usize
        end: usize
        kind: SpanKind
    }
    class SpanKind {
        <<enumeration>>
        Math: latex
        PageBreak: label
    }
    class Locator {
        <<enumeration>>
        Pdf: page, page_end, bbox
        Epub: file
    }
    class Warning {
        <<enumeration>>
        PagesWithoutText: pages
        UnreadableContent: href, reason
        UnreadablePart: part, reason
    }
    Document *-- Source
    Source *-- Format
    Document *-- Metadata
    Document *-- OutlineEntry
    Document *-- Section
    Document *-- Block : blocks, furniture
    Document *-- Warning
    Block *-- BlockKind
    Block *-- Span
    Block *-- Locator
    Span *-- SpanKind
    OutlineEntry ..> Block : block_id
    Section ..> Block : heading
    Section ..> Section : parent
    Block ..> Section : section
```

A `Document` holds the text as two lists of blocks: `blocks` for the reading text and `furniture` for the lines the reader took out of it. Beside them sit facts about the file (`source`, `metadata`), the table of contents (`outline`), the tables that blocks point into (`sections`, `files`, `page_labels`) and the parts the reader could not read (`warnings`). The JSON form uses the field names above, except where [The JSON form](#the-json-form) says a value is left out. `BlockKind`, `SpanKind` and `Warning` carry their variant in a `type` field, in snake case.

### Source

| Field | Content |
|---|---|
| `sha256` | The SHA-256 of the file's bytes as 64 hex digits, from `sha256_file` in `src/index/cache.rs`. It names the book's cache directory. |
| `file_name` | The last component of the path, converted to UTF-8 with invalid bytes replaced. |
| `format` | `"pdf"` or `"epub"`, decided from the file's content by `detect_format` in `src/extract/mod.rs`. |

### Metadata

| Field | PDF | EPUB |
|---|---|---|
| `title` | The Info dictionary's `Title`, normalized; null when missing or empty | The package's main title, normalized; null when empty |
| `authors` | The Info dictionary's `Author` as one entry, not split into names | Every creator in order, normalized, empty ones dropped |
| `language` | Always null | The package's main language, trimmed |
| `page_count` | The number of pages | Always null |
| `spine_count` | Always null | The number of spine entries, skipped ones included |

### Blocks

A block is one unit of reading text: a heading, a paragraph, a list item or a note.

`id` numbers the blocks from 0 in reading order, so a block's id equals its index in `blocks`. JSON therefore leaves it out, and loading sets it from the position. `OutlineEntry.block_id`, `Section.heading` and the `BlockRange.block_id` of a passage in `src/passages.rs` refer to blocks by this number. `first_passage` in `src/read.rs` binary-searches the passages by block id, which works because ids rise in reading order.

`kind` is a `BlockKind`:

| Variant | JSON | Written by |
|---|---|---|
| `Heading { level }` | `{"type": "heading", "level": 2}` | EPUB: `h1` to `h6`, with the tag's level. PDF: a bookmark's heading, with the bookmark's depth, the top level being 1. |
| `Paragraph` | `{"type": "paragraph"}` | Both readers |
| `ListItem` | `{"type": "list_item"}` | EPUB only: text directly inside an `li` |
| `Note { label }` | `{"type": "note", "label": "n1"}` | EPUB: footnotes and endnotes, labelled with the note element's id. PDF: notes found by the layout, always without a label. |

The kind decides what happens downstream. `build_passages` packs paragraphs, list items and notes into passages. A heading opens a section, through which it reaches the passages under it as their section path.

`text` is the block's text after `normalize` in `src/normalize.rs`: Latin ligatures expanded, soft hyphens, zero-width spaces, byte-order marks and non-whitespace control characters dropped, composed to NFC, every whitespace run collapsed to one space, and trimmed. It is a single line without markup.

`section` is the index in `sections` of the section the block is in. A block before the first heading has none, and [Sections](#sections) explains how the others get theirs. `locator` says where the block is in the file, with the fields that [Locators](#locators) lists.

`ids` holds the element ids of an EPUB block: the ids of the elements that start in the block, its own included, and of the empty elements met since the block before. The EPUB reader resolves a table-of-contents fragment to the first block whose `ids` hold it. The PDF reader writes none.

`spans` marks ranges of `text` without changing it. A `Span` covers `start..end`, counted in Unicode code points of `text`, and its `SpanKind` says what the range is. The EPUB reader writes one kind, `PageBreak { label }`, an empty span (`start` equals `end`) at the point where a page of the printed edition starts. It puts each break at offset 0 of the first block whose text starts on the new page. `Math { latex }` is reserved for formulas, and no reader writes it.

An EPUB note as JSON, the first block on printed page 45:

```json
{
  "kind": { "type": "note", "label": "n1" },
  "text": "1 Source.",
  "spans": [{ "start": 0, "end": 0, "kind": { "type": "page_break", "label": "45" } }],
  "ids": ["n1"],
  "section": 3,
  "locator": { "file": 2 }
}
```

### Sections

`sections` holds one `Section` per heading, in reading order. `heading` is the heading's block. `parent` is the section around it, always an earlier one, or null at the top. A block's `section` names the innermost section it is in, which for a heading is the section it opens. A block's section path follows the chain of parents up from its section and lists it outermost first, each title taken from a heading block's text. [`Places`](#places) builds these paths when a book is loaded.

`assign_sections` in `src/extract/mod.rs` builds the sections for both readers once all blocks exist. It walks the blocks in order with a stack of open sections. A heading of level n closes every open section of level n or deeper and opens its own inside the one left on top, which becomes its `parent`. Blocks before the first heading are in no section. Levels the book skips open nothing, so a book whose chapters are all `h2` gets one top-level section per chapter.

The test `skipped_levels_replace_deeper_entries` shows the rule through the paths that result:

| Heading | Level | Section path |
|---|---|---|
| A | 1 | `["A"]` |
| C | 3 | `["A", "C"]` |
| C2 | 3 | `["A", "C2"]` |
| B | 2 | `["A", "B"]` |
| D | 4 | `["A", "B", "D"]` |
| B2 | 2 | `["A", "B2"]` |

The levels come from the heading blocks, not from the outline. An EPUB heading has the level of its tag, and a PDF heading the depth of its bookmark. In `tests/fixtures/sample.pdf` the bookmarks give three headings, and `sections` reads `[{"heading": 3, "parent": null}, {"heading": 7, "parent": 0}, {"heading": 16, "parent": null}]`. Block 7, `Joint entropy`, opens its section inside the one of `Entropy`, so a paragraph under it has the path `["Entropy", "Joint entropy"]`.

### Locators

A `Locator` has one variant per format and holds only what differs from one block to the next. What many blocks share sits once in the `Document`, and [`Places`](#places) derives what follows from it.

`Locator::Pdf`:

| Field | Content |
|---|---|
| `page` | The 1-based physical page where the block starts |
| `page_end` | The page where the block ends, set only when that is a later page |
| `bbox` | `[left, top, right, bottom]` of the block's lines on `page`, in points measured from the top-left corner of the page's media box, so `top` is less than `bottom`; null for a heading the reader inserts because no paragraph on the bookmark's page matched it |

A page's printed label sits in `Document.page_labels`, one entry per physical page with page 1 first, filled from the PDF's page-label ranges. The list is empty when the PDF defines no ranges or they could not be read. `Document::page_label(page)` reads it and gives `None` for a page the list does not hold.

`Locator::Epub`:

| Field | Content |
|---|---|
| `file` | The index in `Document.files` of the block's content document |

`files` lists the content documents the reader read, in spine order, each as its path relative to the package document's directory, percent-decoded and stripped of control characters, such as `text/ch 1.xhtml`. A document skipped with a warning is not in it, while one that gave no text keeps its entry, so `files` can name a document that holds no block.

`Places` derives the other EPUB facts a locator needs:

- `para`, the block's 1-based position among the blocks of its content document, restarting at 1 in every document;
- `print_page`, the printed page in effect where the block's text starts: the label of the last `PageBreak` at or before the block's start, in that block or an earlier one, so a page carries from one content document to the next and is absent before the first break.

The passage locator built from `para` in `src/passages.rs` adds a paragraph count from the nearest heading, which `Places::section_para` gives. `locator_text` in `src/read.rs` prints that count, or the file and the `para` range for text before the first heading.

The model stores no anchor, the last element id at or before a block's start, because a link into the book resolves through the `ids` of the block that holds its target.

A PDF locator as JSON:

```json
{
  "page": 2,
  "page_end": 3,
  "bbox": [72.0, 96.5, 523.3, 240.1]
}
```

### Outline

`outline` lists the table of contents in reading order.

| Field | Content |
|---|---|
| `id` | The entry's position in the list, from 0 |
| `title` | The entry's text, normalized; entries with an empty title are dropped before numbering |
| `level` | The depth in the tree, the top level being 1; a depth above 255 is stored as 255 |
| `block_id` | The block where the entry's text starts, or null when the reader could not place it |

The PDF reader takes the bookmarks that lead to a page of the book. For each one it marks the matching paragraph as a `Heading` or inserts a new `Heading` block, so every PDF entry points at a heading. The EPUB reader takes the navigation document or the NCX and points each entry at the block that carries the target's element id, or at the first block of the target file. An EPUB entry can point at a paragraph, and its level need not equal the tag level of the heading it points at.

The `outline` tool lists these entries. Reading by outline entry starts at the entry's first passage and stops before the first passage of the next entry at the same level or nearer the top (`entry_range` in `src/read.rs`).

### Furniture

`furniture` holds the lines the reader took out of the reading text. Only the PDF reader fills it. `furniture` from the furniture-detection crate picks the lines that other pages predict, such as running heads and page numbers ([furniture-detection.md](furniture-detection.md)), and `build_document` in `src/extract/pdf.rs` makes each into a block:

- kind `Paragraph`, text normalized, lines whose text is empty left out;
- no `section`;
- a PDF locator with the page and the line's box, and `page_end` null;
- ids counting from 0 inside `furniture`, independent of the ids in `blocks`, which loading sets as it does for `blocks`.

The EPUB reader writes an empty list. Nothing after the readers reads `furniture`, so these lines never enter passages, search hits or the Markdown output. They appear only in `document.json.lz4` and in the output of `ebook-hero extract --format json`. `docs/furniture-detection.md` explains how the PDF reader finds them.

### Warnings

| Variant | JSON `type` | Fields | Written by |
|---|---|---|---|
| `PagesWithoutText` | `pages_without_text` | `pages` | PDF: the pages that were read and hold no text |
| `UnreadableContent` | `unreadable_content` | `href`, `reason` | EPUB: a spine item that was skipped, named by its href, or by its `idref` when the manifest lacks it |
| `UnreadablePart` | `unreadable_part` | `part`, `reason` | PDF: a part left out or replaced, named `page 7`, `page labels`, `outline` or `word spacing` |

A warning means the reader left part of the book out and read the rest. A book with no text at all is an error instead: `Unreadable` with "no readable text" for an EPUB, and `NoTextLayer` ("no text layer; OCR is not supported") or `Unreadable` for a PDF. The warnings reach the `book_info` result (`BookInfo.warnings` in `src/read.rs`) and the output of `ebook-hero info`.

## Each fact once

A fact is stored once, at the level where it holds: the document, a run of blocks, one block, or a point in a block's text. What follows from the stored facts is computed when a book is loaded, so neither `document.json.lz4` nor the output of `extract --format json` repeats it. The output of search, read and verify loses nothing by this: each passage carries its full section path and locator, which `build_passages` derives as it builds the passages.

| Fact | Stored as |
|---|---|
| a block's section | `section`, an index into `sections` |
| a section's path | derived: the chain of `parent`s, titled by the heading blocks |
| an EPUB block's file | `file` in the locator, an index into `files` |
| an EPUB block's paragraph number | derived: its position among the blocks of its file |
| an EPUB printed page | a `PageBreak` span where the page starts |
| the printed page at a block | derived: the last `PageBreak` at or before the block's start |
| a PDF page's label | `page_labels`, one entry per page |
| an element id | `ids` on the block where the element is |
| a block's id | derived: its position in `blocks` |
| a locator's format | derived: `source.format`, and the locator's own fields |
| a passage's text, section and locator | derived: `passages.json.lz4` stores its pieces as a block, an offset and a length |

Alice's 875 blocks lie in 19 sections and 14 content documents, so a block holds two small numbers where it would otherwise repeat the heading texts above it and its document's name. Its document takes 304 KB as `extract --format json` prints it, against 523 KB when every block carried these facts.

### Places

`Places` in `src/document/places.rs` holds what is derived. `Places::new` builds it in one pass over the sections and one over the blocks, so every lookup afterwards takes constant time:

| Method | Gives |
|---|---|
| `section_path(block)` | the heading texts above the block, outermost first; a heading's own path ends with its text |
| `para(block)` | the block's position among the blocks of its EPUB file, from 1; 0 in a PDF |
| `section_para(block)` | the block's number among the non-heading blocks of its section, from 1; 0 for a heading |
| `print_page(block)` | the label of the last `PageBreak` at or before the block's start |

`Places` keeps its own copies of the paths and labels it returns, so it can sit beside the `Document` in `Book` (`src/index/book.rs`), which builds it whenever it loads a book. `build_passages` takes it with the document and reads from it each passage's section path and EPUB locator. A PDF page's label needs no table of its own, because `page_labels` already holds one entry per page.

### Passages

`passages.json.lz4` stores each passage as its pieces and no text. A piece is `len` bytes of the text of block `block`, from byte `offset`, and `StoredPiece` in `src/passages.rs` is those three numbers. Every piece is part of a block, and `document.json.lz4` already holds the text of every block, so storing a passage's text would store it twice. Alice's `passages.json.lz4` takes 7.2 KB, against 119 KB when each passage also carried its text.

`stored_form` turns the passages `build_passages` made into pieces, and `assemble` turns them back when a book is loaded. `assemble` cuts each piece out of its block, joins the pieces with a blank line, and takes the section path and the locator from `Places`. `build_passages` makes a passage with the same function, so a loaded passage equals the built one in its text, its `BlockRange`s, its section and its locator, and nothing after loading sees that passages are stored as pieces. Loading assembles every text once, because search, find and verify scan the passage texts many times; that copy lives only in memory.

The pieces come from a file on disk, so `assemble` checks them. A piece in a block that does not exist, a piece that runs past the end of its block or starts or ends inside a character, and a passage without pieces are each an `Error::Index` that names `passages.json.lz4` and says to run `ebook-hero cache clear`. Opening a book handles that error like any cache file that does not load: it logs a warning and builds the index again. The stored form is part of what `PASSAGES_VERSION` versions, so an index made before pieces, which stores each passage's text, is built again when its book is opened.

### The JSON form

Serde writes each field under its name, except where a value is derived or empty:

- A block's `id` is left out, and `numbered` in `src/document/mod.rs` restores it: serde reads the `blocks` and `furniture` lists through it, and it numbers each list from 0.
- A locator carries no `format` tag, since its fields tell the variants apart: a PDF locator has `page`, an EPUB locator `file`. `Locator` is written untagged, and its hand-written `Deserialize` reads every field a locator may have into `LocatorFields` in one pass and picks the variant from the fields present. A locator with both `page` and `file`, or neither, does not load. Serde's untagged enums would buffer each locator and try one variant after the other, which is slower and fails with a message that names no field.
- `spans`, `ids` and a block's `section` are left out when they are empty or absent, and take their defaults when the JSON lacks them.

## Design principles

### Readers write it, nothing rewrites it

AGENTS.md sets the boundary: readers turn a file into a `Document`, and everything after them reads it and never changes it. The code keeps to it: `build_passages` takes `&Document` with its `Places`, `read_passages` takes `&Document`, and the cache stores the `Document` as the reader wrote it. `Places` is derived from the document and never written back into it. A change in what downstream code sees must therefore come from a reader, and AGENTS.md requires every reader change to bump `PDF_READER_VERSION` or `EPUB_READER_VERSION` in `src/settings.rs`, which rebuilds the cached indexes.

### Flat blocks

`blocks` is one list in reading order. Structure travels as data on each block (`kind`) and as numbers that point into lists: a block's `section` into `sections`, and `Section.heading`, `OutlineEntry.block_id` and `BlockRange.block_id` into `blocks`. A consumer walks one sequence: `build_passages` packs consecutive blocks, and `gui/book.js` draws the book as "its text in order". The two formats reach this shape from opposite ends. The EPUB reader flattens a tree of elements, and the PDF reader builds paragraphs up from positioned lines.

### Plain text with standoff spans

`text` holds plain characters with no markup. A span marks a range of `text` by offset and leaves the characters alone. A `PageBreak` marks the point where a printed page starts and adds no character, so search, passage packing and quote verification read the same characters at the same offsets whether a book marks its pages or not. A future `Math` span could carry LaTeX for a formula in the same way. `src/normalize.rs` is "shared by every reader and by quote verification", so a quote and the text it is checked against pass through the same function.

### Locators per format

A PDF places a block by physical page and box, and an EPUB by content document. The locator holds only these, while the printed label, the paragraph number and the printed page come from the document's tables and from `Places`. An EPUB has no physical pages, and a PDF has no files or element ids, so the model gives each format its own variant and no shared fields. Each consumer handles both variants explicitly. `PassageLocator` in `src/passages.rs` keeps the split. `locator_text` in `src/read.rs` prints `p. 12–13 (xii–xiii)` for a PDF and `CHAPTER I ¶3–5, print p. 45` for an EPUB. `gui/book.js` handles neither variant, because `read --json` gives each passage the ids of its blocks (`PassageRef.blocks` in `src/read.rs`, taken from the passage's `BlockRange`s) and the window marks those.

### Furniture kept apart

Running heads and page numbers left in `blocks` would enter passages and match searches. Dropped altogether, they would leave no trace of what the reader decided. A separate list keeps them out of every downstream step and keeps them visible in the JSON output.

### Additive schema changes with a version

`SCHEMA_VERSION` in `src/document/mod.rs` is 3, and every reader writes it into `Document.schema_version`. AGENTS.md requires a schema change to be additive and to bump `SCHEMA_VERSION`. Additive means a change adds fields or variants and leaves existing names and meanings alone. Version 2 was additive: commit 85c7cd9 added `Warning::UnreadablePart` and changed nothing else. Version 3 is the one exception the user allowed. It moves the facts that blocks repeated into the tables of [Each fact once](#each-fact-once), so it removes and changes block fields, and schema 3 does not read a schema 2 document.

Two consumers check the version, so a document of another schema is never read as this one.

- The cache. `Manifest` in `src/index/cache.rs` records `schema_version` with `tool_version`, `pdf_reader`, `epub_reader`, `passages` and `analyzers`. `is_current_stage1` reuses a cached index only when every one of them equals the running build's value, so a bump rebuilds each cached book the next time it is opened.
- The desktop window. `gui/book.js` accepts a `Document` whose `schema_version` is at least `MIN_SCHEMA_VERSION` (3) and otherwise asks for a rebuilt binary. It takes a block's id from its index, since JSON leaves the id out. Because changes are additive, a newer `Document` still carries every field the window reads. Renaming or removing one of those fields breaks the window, and AGENTS.md asks for `MIN_SCHEMA_VERSION` to be bumped with it.

### Byte offsets inside, code points outside

AGENTS.md fixes two units: offsets inside the library count UTF-8 bytes, and ranges in output count Unicode code points.

Inside, `BlockRange` in `src/passages.rs` maps passage text to block text by byte offsets: `passage.text[start..end]` is `block.text[block_offset..block_offset + end - start]`. `match_ranges` and `phrase_ranges` in `src/search/highlight.rs` return byte ranges too. Rust slices a `str` by byte offset, so these ranges index the text with no conversion.

On the way out, `to_char_ranges` in `src/search/highlight.rs` turns byte ranges into `[start, end]` pairs of code points. It produces the `matches` of `search` and `find` hits and the `matched` range of `verify_quote`, and each of these tools' descriptions ends "Ranges are Unicode code points." A code-point count stays the same whatever encoding a client stores the text in, while a byte offset holds only for UTF-8. In `café au lait`, `é` takes two bytes and one code point, so `au` starts at byte 6 and at code point 5.

The `Document` holds one pair of offsets itself, `Span.start` and `Span.end`. They count code points of the block's `text`, like the ranges in output, because `extract --format json` prints them for other programs to read.

## Markdown rendering

`src/document/markdown.rs` writes a `Document` as Markdown for `ebook-hero extract` when JSON is not asked for. `document_to_markdown` writes `# {title}` when `metadata.title` is known, then the blocks. `blocks_to_markdown` separates blocks with a blank line and ends the text with one newline. With no blocks it writes nothing but the title line, if there is a title.

| Block | Markdown |
|---|---|
| `Heading { level }` | `#` repeated `level` times, clamped to 1 to 6, a space, the text |
| `Paragraph` | The text |
| `ListItem` | `- ` and the text |
| `Note { label }` | `> [label] text`, or `> text` without a label; each line of the text gets its own `> ` |

A PDF block whose `page` differs from the previous PDF block's is preceded by `<!-- page N -->`, where N is the physical page. EPUB blocks get no page comments, printed pages included. The renderer writes nothing for `furniture`, `outline`, `warnings`, `sections`, `files` or `page_labels`, nor for a block's `section`, `ids` or `spans`.

From the tests, a PDF with the title "Test Book" and one paragraph renders as:

```markdown
# Test Book

<!-- page 1 -->

One.
```

An EPUB list item, a labelled note and an unlabelled note render as:

```markdown
- First

> [1] See chapter 2.

> Aside.
```

The `read` tool does not use this file. It writes its own Markdown from passages (`render` in `src/read.rs`), with a `<!-- passage N · locator -->` comment before each passage.

The title and a level-1 heading both get one `#`, so a book whose chapters are `h1` shows its chapter headings at the title's level. Block text is written without escaping. A paragraph that starts with `# ` or `1. ` renders as a heading or a numbered list in a Markdown viewer, and text such as `<b>` is read as HTML.

## Limits

- Four kinds of block. Table cells, figure captions, block quotes, code and verse have no kind of their own. The EPUB reader writes them as paragraphs, and `normalize` turns the line breaks of code and verse into spaces.
- No images and no formulas. The model has no field for an image, and the `Math` span that would hold a formula has no writer.
- No inline markup. Emphasis, links and superscript markup leave no trace in `text`. Characters such as `²` survive, since `normalize` keeps them.
- No link from a note reference to its note. The EPUB reader deletes `noteref` elements, an EPUB `Note` keeps only the note element's id as `label`, and a PDF note has no label.
- One box per block. `bbox` covers the block's lines on its first page, and the part of a paragraph that runs on to `page_end` has no box.
- Levels are `u8`. An outline deeper than 255 levels is capped, and the Markdown renderer writes at most six `#`.
- Missing per-format facts. A PDF never has a `language`, an EPUB never has a `page_count`, and an EPUB without page-break markers has no page numbers at all.
- Page breaks at block starts. The EPUB reader puts each `PageBreak` at offset 0 of the first block whose text starts on the new page, so a page that starts inside a paragraph is marked at the next block.

## References

- F. Yergeau. "UTF-8, a transformation format of ISO 10646." RFC 3629, Internet Engineering Task Force, November 2003. https://www.rfc-editor.org/rfc/rfc3629. The encoding whose bytes the library's offsets count.
- T. Bray, editor. "The JavaScript Object Notation (JSON) Data Interchange Format." RFC 8259, Internet Engineering Task Force, December 2017. https://www.rfc-editor.org/rfc/rfc8259. The format of the JSON in `document.json.lz4` and of `ebook-hero extract --format json`.
- John MacFarlane. "CommonMark Spec", version 0.31.2, 28 January 2024. https://spec.commonmark.org/0.31.2/. ATX headings of one to six `#`, block quotes, bullet list items and HTML comments, which the Markdown renderer writes.
- ISO 32000-2:2020. "Document management — Portable document format — Part 2: PDF 2.0." International Organization for Standardization, 2020. The page labels, document outline, Info dictionary and media box behind `Locator::Pdf`, `OutlineEntry` and `Metadata`.
- Matt Garrish, Ivan Herman and Dave Cramer, editors. "EPUB 3.3." W3C Recommendation, 25 May 2023. https://www.w3.org/TR/epub-33/. The package document, spine and navigation document behind `Locator::Epub`, `OutlineEntry` and `Metadata`.
