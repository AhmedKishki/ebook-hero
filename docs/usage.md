# Using ebook-hero

ebook-hero answers questions about one PDF or EPUB at a time: which passages answer a query, where a phrase occurs, what the text around a place says, whether a quote really is in the book, and which of the book's concepts occur together. One binary is both a command line and an MCP server over stdio for AI assistants. Both call the `ebook-hero` crate, which a Rust program can also use as a library. Each call names one book file. The first call indexes that book, and later calls reuse the index.

This document is the usage reference: it covers installing, every command, every MCP tool, the library's entry point, and recipes for common tasks. How each part works inside is explained in the other files of `docs/`, such as [search.md](search.md) and [quote-verification.md](quote-verification.md).

The examples run on `tests/fixtures/alice.epub`, the copy of *Alice's Adventures in Wonderland* in the repository, so they can be repeated from a clone. Their output is real, shortened with `…` where it is long.

## Install and first run

```sh
cargo install --locked ebook-hero
```

This needs Rust 1.92 or later. The build downloads a prebuilt ONNX Runtime, the library that runs the models semantic search uses, and installs the `ebook-hero` binary in `~/.cargo/bin`. From a clone, `cargo build --release` builds the same binary as `target/release/ebook-hero`.

### What the first run does

The first command about a book reads the file, cuts its text into passages and builds a lexical index of them. `info`, `outline`, `find`, `read`, `verify`, `graph` and lexical search need nothing more, so they load no model and work offline. For `alice.epub` the first command takes well under a second.

Semantic and hybrid search need two models:

- the embedding model `BAAI/bge-small-en-v1.5`, which turns a text into a vector;
- the reranker `jinaai/jina-reranker-v1-turbo-en`, which reorders the best candidates.

The first semantic or hybrid search on a machine downloads them from Hugging Face, about 270 MB together. The first such search of each book then embeds all its passages with the embedding model, which can take minutes for a long book, and prints its progress on standard error. Hybrid is the default mode, so a plain `ebook-hero search` is such a search. `ebook-hero index book.pdf` does the download and the embedding ahead of time.

### Where files are kept

| What | Where | Set by |
|---|---|---|
| Indexes | `ebook-hero/indexes` in the user cache directory: `~/.cache/ebook-hero/indexes` on Linux, `~/Library/Caches/ebook-hero/indexes` on macOS | `EBOOK_HERO_INDEXES` |
| Models | `ebook-hero/models` in the user cache directory | `EBOOK_HERO_MODELS` |

A system without a user cache directory gets both in the temporary directory instead.

Each book has its own directory in the index cache, named for the SHA-256 of the file's bytes. A copy of a book under another name therefore reuses its index, and a file that changes is indexed again. A new version of ebook-hero rebuilds an index, embeddings included, the first time it opens the book.

The indexes are loaded without being checked, so whoever can write to their directory could plant one. ebook-hero therefore refuses an index directory that is a symbolic link, belongs to another user, or lets group or others write to it:

```console
$ mkdir -m 777 shared
$ EBOOK_HERO_INDEXES=shared ebook-hero info tests/fixtures/alice.epub
error: index error: cache directory shared is not private (its mode 777 lets group or others write to it); set EBOOK_HERO_INDEXES to a private directory
```

A directory that does not exist yet is created with mode 0700, provided its parent exists. [`cache list`](#cache) shows the cached indexes and [`cache clear`](#cache) deletes them.

### Environment variables

| Variable | Default | Effect |
|---|---|---|
| `EBOOK_HERO_INDEXES` | `ebook-hero/indexes` in the user cache directory | where indexes are kept |
| `EBOOK_HERO_MODELS` | `ebook-hero/models` in the user cache directory | where the models are downloaded to and loaded from |
| `EBOOK_HERO_MODEL_IDLE` | `300` | how many seconds the MCP server keeps an unused model loaded; a whole number of at least 1 |
| `EBOOK_HERO_LOG` | `error,ebook_hero=warn` | which log lines reach standard error, as a `tracing` filter such as `debug` |

## Passages and locators

Every command names passages by their ids and places them by their locators.

A passage is a run of whole paragraphs, list items and notes of at most 220 words, which never crosses a heading or, in an EPUB, a file. Passages are numbered from 0 in reading order. `search`, `find`, `outline`, `verify` and `graph` give passages by this id, and `read` takes it, so any passage a command names can be opened. A heading goes into no passage. Each passage carries the path of headings above it, its section, such as `Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground`.

A locator says where a passage is, as a citation would:

- In a PDF it gives the physical pages, counted from 1, then the printed page labels when the PDF defines them: `p. 12–13 (xii–xiii)`.
- In an EPUB it gives the nearest heading above and the paragraphs counted from it: `CHAPTER VIII. The Queen’s Croquet-Ground ¶13–19`. When the book marks its printed pages, `, print p. 45` follows. Text before the first heading is placed by its file instead: `cover.xhtml ¶1`.

With `--json`, and in MCP results, a locator is an object whose `format` says which kind it is. These are real locators from `tests/fixtures/sample.pdf` and `alice.epub`:

```json
{"format": "pdf", "page_start": 2, "page_end": 2, "label_start": "1", "label_end": "1"}
```

```json
{
  "format": "epub",
  "href": "4040227324556390537_11-h-8.htm.html",
  "para_start": 14,
  "para_end": 20,
  "print_page": null,
  "under_heading": {
    "text": "CHAPTER VIII. The Queen’s Croquet-Ground",
    "para_start": 13,
    "para_end": 19
  }
}
```

An EPUB locator counts paragraphs twice. `para_start` and `para_end` count the blocks of the file `href`, heading included, while `under_heading` counts from the heading, which is the count the text form prints. `under_heading` is `null` for text before the book's first heading.

Ranges in results, such as a search hit's `matches` or a verified quote's `matched`, are `[start, end]` pairs that count Unicode code points of the passage's text, with `end` exclusive. A JavaScript string counts UTF-16 units instead, which differ from code points only for characters outside the Basic Multilingual Plane, such as emoji.

## Commands

```text
ebook-hero [--json] <command> [arguments]
```

These rules hold for every command:

- `<path>` is the book: an absolute path, a path relative to the working directory, or one that starts with `~`. Symbolic links are followed. The format comes from the file's content, so the extension does not matter.
- `--json` may stand anywhere on the line. For `info`, `outline`, `search`, `find`, `read`, `verify` and `graph` it prints, pretty-printed, the structured result of the MCP tool `book_info`, `outline`, `search`, `find`, `read`, `verify_quote` or `concept_graph`. `index` then prints what `info` prints, and `extract` prints the extracted document as `--format json` does. `cache list` and `cache clear` print the objects that [cache](#cache) describes. `mcp` refuses it, because its standard output carries only protocol messages.
- Standard output carries only the result. Errors, embedding progress, the `next: --cursor` line of `find` and `read`, and log lines go to standard error.
- `ebook-hero help <command>` and `ebook-hero <command> --help` print a command's options and, for most commands, examples. `-h` prints a summary, and `--version` prints the version.

The exit status says how the command ended:

| Status | When |
|---|---|
| 0 | The command ran, also when it found nothing: `search` then prints `no results`, and `find` prints `0 occurrences in 0 passages`. |
| 1 | The command failed. It printed `error: ` and the cause on standard error. |
| 2 | The arguments did not parse, such as a missing argument or an unknown mode. The usage was printed on standard error. |
| 3 | `verify` ran and gave the verdict `none`: the quote is not in the book, or not in the passage that `--passage-id` names. |

Every command that opens a book can fail in these ways:

| Message | Cause |
|---|---|
| `<path>: file not found` | Nothing exists at the path. |
| `<path>: not a regular file` | The path is a directory or another kind of file. |
| `<path>: unsupported format; expected a PDF or an EPUB` | The content is neither. |
| `<path>: could not be read: <reason>` | The file cannot be opened or parsed, such as a PDF whose reason is `password required`. |
| `<path>: no text layer; OCR is not supported` | The PDF has no text, as a scan without OCR has none. |
| `index error: cache directory <dir> is not private (<reason>); set EBOOK_HERO_INDEXES to a private directory` | See [Where files are kept](#where-files-are-kept). |
| `index build for <sha256> is held by process <pid>; waited <n>s` | Another process was still building the same book's index after 15 minutes. |

The sections below list only the errors a command adds to these.

### info

```text
ebook-hero info <path>
```

Summarizes the book: its title, authors, language, format and size, its number of passages, and whether its passages are embedded yet. `info` takes no options besides `--json`.

The text form prints one field per line. `Pages` is the page count of a PDF, and `Spine items` the number of entries in an EPUB's spine, the list of its files in reading order. Spine items include covers and front matter, and a long chapter can be split across several, so the count is not a chapter count: `alice.epub` has 14 for its 12 chapters. `Semantic index` is `ready` once the passages are embedded and `not built` before. Each part of the file that the reader had to leave out adds a line that starts with `Warning:` and names the part.

```console
$ ebook-hero info tests/fixtures/alice.epub
Title: Alice's Adventures in Wonderland
Authors: Lewis Carroll
Language: en
Format: EPUB
Spine items: 14
Passages: 166
Semantic index: not built
```

With `--json` it prints the [`book_info`](#book_info) result:

```console
$ ebook-hero info tests/fixtures/alice.epub --json
{
  "title": "Alice's Adventures in Wonderland",
  "authors": [
    "Lewis Carroll"
  ],
  "language": "en",
  "format": "epub",
  "page_count": null,
  "spine_count": 14,
  "passage_count": 166,
  "lexical_ready": true,
  "semantic_ready": false,
  "warnings": []
}
```

### outline

```text
ebook-hero outline <path>
```

Lists the book's table of contents, each entry with the passage where its text starts. Each line gives the entry's id in brackets, which `read --outline` takes, its title, and its first passage with that passage's locator. Entries are indented two spaces per level below the top. An entry whose text the reader could not place prints `no text` instead of a passage, and a book without a table of contents prints nothing.

```console
$ ebook-hero outline tests/fixtures/alice.epub
[0] Alice’s Adventures in Wonderland — passage 1, Contents ¶1–24
[1] THE MILLENNIUM FULCRUM EDITION 3.0 — passage 1, Contents ¶1–24
[2] Contents — passage 1, Contents ¶1–24
[3] CHAPTER I. Down the Rabbit-Hole — passage 2, CHAPTER I. Down the Rabbit-Hole ¶1–2
[4] CHAPTER II. The Pool of Tears — passage 15, CHAPTER II. The Pool of Tears ¶1–4
…
[15] THE FULL PROJECT GUTENBERG™ LICENSE — passage 150, THE FULL PROJECT GUTENBERG™ LICENSE ¶1–4
```

With `--json` it prints the [`outline`](#outline-1) result: `{"entries": [...]}`, each entry with `id`, `title`, `level` (the top level is 1), `passage_id` and `locator`, the last two `null` for an entry without text.

### search

```text
ebook-hero search <path> <query> [--mode <mode>] [--top-k <n>] [--no-rerank]
```

Finds the passages that best answer a query, best first.

| Option | Default | Meaning |
|---|---|---|
| `--mode <mode>` | `hybrid` | How to rank the passages: `hybrid`, `lexical` or `semantic`, in any case. |
| `--top-k <n>` | `5` | How many passages to return, from 1 to 20. |
| `--no-rerank` | off | Skip the reranker in hybrid and semantic mode. |

The query is words, "quoted phrases" or a question. A word matches its other forms, so "heads" also finds "head", and common words such as "the" and "how" are ignored. A quoted phrase must occur exactly, as whole words in any case. `--mode` chooses how the passages are ranked:

- `lexical` ranks by BM25 on the words. It finds the words you typed and loads no model. A quoted phrase must occur in every hit, and a query made only of common words finds nothing.
- `semantic` ranks by how close each passage's meaning is to the query's, as the embedding model measures it, and drops passages below a cosine similarity of 0.55. An unrelated query therefore returns nothing.
- `hybrid` merges the 50 best lexical and the 50 best semantic passages by their ranks, so a passage can rank high for its words or for its meaning. A quoted phrase binds only the lexical half, so a hybrid hit need not contain it.

In semantic and hybrid mode the reranker then reads the query with each of the best 30 passages and reorders them, unless `--no-rerank` is given. [search.md](search.md) gives the formulas.

The text form numbers the hits. Each hit gives its passage id, its place in the book and its score, then the whole passage indented by four spaces, with the words that matched the query marked «like this»:

```console
$ ebook-hero search tests/fixtures/alice.epub "why does the Queen want to cut off heads"
1. passage 93 · Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground ¶10  (score 0.907)
    Five and Seven said nothing, but looked at Two. Two began in a low voice, “Why the fact is, you see, Miss, this here ought to have been a red rose-tree, and we put a white one in by mistake; and if the «Queen» was to find it out, we should all have our «heads» «cut» «off», you know. …

2. passage 104 · Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground ¶64–69  (score 0.610)
    When she got back to the Cheshire Cat, she was surprised to find quite a large crowd collected round it: there was a dispute going on between the executioner, the King, and the «Queen», …
…
```

Because this was the book's first hybrid search, standard error had shown `embedding 8/166` to `embedding 166/166` before the hits. On a terminal the count updates on one line.

A score only orders the hits of one search. After reranking it is the reranker's logit, which can be negative; otherwise it is the BM25 score in lexical mode, the cosine similarity in semantic mode, and the fused score in hybrid mode. A lexical search with a quoted phrase therefore shows BM25 scores:

```console
$ ebook-hero search tests/fixtures/alice.epub '"mock turtle" soup' --mode lexical --top-k 2
1. passage 127 · Alice’s Adventures in Wonderland › CHAPTER X. The Lobster Quadrille ¶72–76  (score 10.425)
    “Beautiful «Soup», so rich and green, Waiting in a hot tureen! …

    “Chorus again!” cried the Gryphon, and the «Mock Turtle» had just begun to repeat it, when a cry of “The trial’s beginning!” was heard in the distance.
    …

2. passage 126 · Alice’s Adventures in Wonderland › CHAPTER X. The Lobster Quadrille ¶66–71  (score 10.123)
…
```

With `--json` it prints the [`search`](#search-1) result. Each hit carries the passage's whole `text`, its `section`, `locator`, the `matches` that the text form marks, its `score`, and the ranks it had in the lexical and the semantic list, `null` when it was not in one:

```console
$ ebook-hero search tests/fixtures/alice.epub "why does the Queen want to cut off heads" --top-k 1 --json
{
  "query": "why does the Queen want to cut off heads",
  "mode": "hybrid",
  "hits": [
    {
      "passage_id": 93,
      "text": "Five and Seven said nothing, but looked at Two. …",
      "section": [
        "Alice’s Adventures in Wonderland",
        "CHAPTER VIII. The Queen’s Croquet-Ground"
      ],
      "locator": {
        "format": "epub",
        "href": "4040227324556390537_11-h-8.htm.html",
        "para_start": 11,
        "para_end": 11,
        "print_page": null,
        "under_heading": {
          "text": "CHAPTER VIII. The Queen’s Croquet-Ground",
          "para_start": 10,
          "para_end": 10
        }
      },
      "matches": [
        [
          202,
          207
        ],
        …
      ],
      "score": 0.9066033,
      "lexical_rank": 2,
      "semantic_rank": null
    }
  ]
}
```

Errors:

- `top_k must be between 1 and 20, not 21` and `query must not be empty` exit with 1.
- An unknown mode, such as `--mode fuzzy`, exits with 2: `error: invalid value 'fuzzy' for '--mode <MODE>': unknown mode "fuzzy"; expected hybrid, lexical or semantic`.
- A model that cannot be downloaded or run gives `model <name> is unavailable (<reason>); models are cached in <dir>; use mode=lexical to search without it`.

### find

```text
ebook-hero find <path> <phrase> [--cursor <n>]
```

Lists every occurrence of an exact phrase, in reading order. The phrase matches as a run of whole words in any case, without stemming, and punctuation between the words does not count, so "off with her head" finds "Off with her head!" but not "off with her heads".

| Option | Default | Meaning |
|---|---|---|
| `--cursor <n>` | the first page | Where to continue, from the `next: --cursor` line of the previous page. |

The text form starts with the number of occurrences and passages in the whole book. One line per occurrence follows, with its passage, its locator, and up to 40 characters on each side of the match:

```console
$ ebook-hero find tests/fixtures/alice.epub "off with her head"
4 occurrences in 4 passages
passage 95 · CHAPTER VIII. The Queen’s Croquet-Ground ¶13–19: …r a moment like a wild beast, screamed “«Off with her head»! Off—” “Nonsense!” said Alice, very lo…
passage 99 · CHAPTER VIII. The Queen’s Croquet-Ground ¶42–43: …, and shouting “Off with his head!” or “«Off with her head»!” about once in a minute.…
passage 111 · CHAPTER IX. The Mock Turtle’s Story ¶36–42: …, and shouting “Off with his head!” or “«Off with her head»!” Those whom she sentenced were taken i…
passage 145 · CHAPTER XII. Alice’s Evidence ¶54–60: …rning purple. “I won’t!” said Alice. “«Off with her head»!” the Queen shouted at the top of her v…
```

A page holds the occurrences of 20 passages. When more are left, `find` prints `next: --cursor <n>` on standard error, where `n` counts matching passages, not passage ids. The line tells how to go on and is not part of the listing, so a pipe receives only the totals and the occurrences, while a terminal shows the line after them. A cursor past the last page prints the totals and nothing else.

```console
$ ebook-hero find tests/fixtures/alice.epub "the queen"
72 occurrences in 33 passages
passage 1 · Contents ¶1–24: …R VII. A Mad Tea-Party CHAPTER VIII. «The Queen»’s Croquet-Ground CHAPTER IX. The Mock…
…
passage 111 · CHAPTER IX. The Mock Turtle’s Story ¶36–42: …ite unhappy at the number of executions «the Queen» had ordered.…
next: --cursor 20
$ ebook-hero find tests/fixtures/alice.epub "the queen" --cursor 20
72 occurrences in 33 passages
passage 112 · CHAPTER IX. The Mock Turtle’s Story ¶43–47: …at the picture.) “Up, lazy thing!” said «the Queen», “and take this young lady to see the M…
…
```

With `--json` it prints the [`find`](#find-1) result: the trimmed `phrase`, `total_occurrences` and `total_passages` for the whole book, the page's `occurrences`, each with `passage_id`, `section`, `locator` and one `matches` range per occurrence, and `next_cursor`, `null` on the last page.

A phrase without a letter or digit, such as `...`, fails with `phrase must contain at least one word`.

### read

```text
ebook-hero read <path> (<passage-id> | --page <n> | --outline <id>) [--before <n>] [--after <n>] [--cursor <n>]
```

Prints passages as Markdown, by one of three addresses, of which exactly one must be given:

| Address | Reads |
|---|---|
| `<passage-id>` | that passage |
| `--page <n>` | every passage on the physical page `n` of a PDF, counted from 1; the printed page label is in the locator |
| `--outline <id>` | the passages of that [`outline`](#outline) entry, up to the next entry at the same level or above |

| Option | Default | Meaning |
|---|---|---|
| `--before <n>` | `0` | Passages to add before the range, at most 10. |
| `--after <n>` | `0` | Passages to add after the range, at most 10. |
| `--cursor <n>` | the start of the range | The passage to continue at, from the `next: --cursor` line of the previous call. |

Each passage is a `<!-- passage N · locator -->` comment followed by its text. The first passage, and each one whose section differs from the one before it, is headed by its section path as a `##` heading:

```console
$ ebook-hero read tests/fixtures/alice.epub 99
## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground

<!-- passage 99 · CHAPTER VIII. The Queen’s Croquet-Ground ¶42–43 -->

The chief difficulty Alice found at first was in managing her flamingo: she succeeded in getting its body tucked away, comfortably enough, under her arm, …

The players all played at once without waiting for turns, quarrelling all the while, and fighting for the hedgehogs; and in a very short time the Queen was in a furious passion, and went stamping about, and shouting “Off with his head!” or “Off with her head!” about once in a minute.
```

One call prints at most about 8000 characters, and always at least one passage. When the range does not fit, the call prints `next: --cursor <id>` on standard error, and the same command with `--cursor <id>` added prints the next part. Chapter VIII of `alice.epub` takes two calls:

```console
$ ebook-hero read tests/fixtures/alice.epub --outline 10
## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground

<!-- passage 92 · CHAPTER VIII. The Queen’s Croquet-Ground ¶1–9 -->
…
<!-- passage 98 · CHAPTER VIII. The Queen’s Croquet-Ground ¶41 -->
…
next: --cursor 99
$ ebook-hero read tests/fixtures/alice.epub --outline 10 --cursor 99
## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground

<!-- passage 99 · CHAPTER VIII. The Queen’s Croquet-Ground ¶42–43 -->
…
<!-- passage 105 · CHAPTER VIII. The Queen’s Croquet-Ground ¶70–71 -->
…
```

The cursor is a passage id, and it must lie inside the range the other arguments name. A PDF can be read by page:

```console
$ ebook-hero read tests/fixtures/sample.pdf --page 2
## Entropy

<!-- passage 1 · p. 2 (1) -->

Entropy measures how much surprise a source of messages carries. …

## Entropy › Joint entropy

<!-- passage 2 · p. 2–3 (1–2) -->

Joint entropy is the entropy of two variables taken together. …
```

With `--json` it prints the [`read`](#read-1) result: the `markdown` above, the `passages` it holds, each with `passage_id`, `section` and `locator`, and `next_cursor`, `null` when the range is done.

The command and the MCP tool share these messages, so they name the arguments by their MCP names:

| Message | Cause |
|---|---|
| `passage 500 does not exist; the book has 166 passages` | The passage id is too large. |
| `page addressing applies to PDFs; use passage_id or outline_id` | `--page` was given for an EPUB. |
| `page 99 has no text` | No passage lies on that page. |
| `outline entry 99 does not exist; the book has 16 entries` | The outline id is too large. |
| `this book has no outline; use passage_id or page` | `--outline` was given for a PDF without a table of contents; for an EPUB the message ends `use passage_id`. |
| `outline entry <id> has no text after it` | The entry points at no passage. |
| `after is 11; it must be at most 10` | `--before` or `--after` is above 10. |
| `cursor 5 is not inside the passages 92–105 being read; use the next_cursor of the previous call` | The cursor lies outside the range. |

No address, or more than one, is a usage error with exit status 2.

### verify

```text
ebook-hero verify <path> <quote> [--passage-id <id>]
```

Checks whether a quote occurs in the book, and where it starts. The answer is one of three verdicts:

- `exact`: the quote occurs as written, case included, from the start of a word to the end of a word;
- `equivalent`: the book has the same words in the same order, and only case, punctuation or spacing differ;
- `none`: neither.

| Option | Default | Meaning |
|---|---|---|
| `--passage-id <id>` | the whole book | Look in this passage first, as a citation names it. |

Without `--passage-id` the whole book is searched, and an exact match anywhere beats an equivalent one. With it, that passage alone decides the verdict. A quote may run on from one passage into the next and belongs to the passage it starts in. Words are never stemmed, so a quote that changes one word or one word's form gets `none`.

```console
$ ebook-hero verify tests/fixtures/alice.epub "Off with her head!"
exact: passage 95 · CHAPTER VIII. The Queen’s Croquet-Ground ¶13–19
$ ebook-hero verify tests/fixtures/alice.epub "off with their heads"
equivalent: passage 96 · CHAPTER VIII. The Queen’s Croquet-Ground ¶20–28
$ ebook-hero verify tests/fixtures/alice.epub "the Queen was very kind to everyone"
none; closest passage 93 · CHAPTER VIII. The Queen’s Croquet-Ground ¶10 (3 of 7 words; differs at "very")
```

After `none`, the passage that shares the longest run of words with the quote is named, with the length of that run and the first quote word after it. A quote that shares no word with any passage prints `none; no passage shares a word with the quote`. A cited passage that does not hold the quote gives `none` and a second line that names the passage that does:

```console
$ ebook-hero verify tests/fixtures/alice.epub "Off with her head!" --passage-id 93
none
found in passage 95 · CHAPTER VIII. The Queen’s Croquet-Ground ¶13–19
```

The exit status is 0 after `exact` or `equivalent` and 3 after `none`. An error exits with 1, so a script can tell `none` from a check that failed without reading the output:

```console
$ ebook-hero verify tests/fixtures/alice.epub "the Queen was very kind to everyone" > /dev/null
$ echo $?
3
```

With `--json` it exits with the same status and prints the [`verify_quote`](#verify_quote) result, which also gives the matched range:

| Field | With `exact` or `equivalent` | With `none` |
|---|---|---|
| `verdict` | `"exact"` or `"equivalent"` | `"none"` |
| `passage_id`, `locator` | the passage the quote starts in | the cited passage, or `null` |
| `matched` | `[start, end]` of the quote in that passage's text | `null` |
| `found_elsewhere` | `null` | with a cited passage that lacks the quote: the `passage_id`, `locator` and `verdict` of the passage that holds it |
| `closest` | `null` | when the quote is nowhere: `passage_id`, `locator`, `matched_words`, `quote_words` and `first_difference`, or `null` when no passage shares a word |

```console
$ ebook-hero verify tests/fixtures/alice.epub "the Queen was very kind to everyone" --json
{
  "verdict": "none",
  "passage_id": null,
  "locator": null,
  "matched": null,
  "closest": {
    "passage_id": 93,
    "locator": {
      "format": "epub",
      …
    },
    "matched_words": 3,
    "quote_words": 7,
    "first_difference": "very"
  },
  "found_elsewhere": null
}
```

A quote that is blank, has no letter or digit, or has more than 440 words is refused, as is a passage id the book does not have: `quote has no words; give letters or digits to check`, `passage 99999 does not exist; the book has 166 passages`. [quote-verification.md](quote-verification.md) explains the matching rules.

### graph

```text
ebook-hero graph <path> [--full]
```

Lists the book's concepts, the links between them and the communities they form. A concept is a phrase that marks one of the book's topics, such as "march hare". Two concepts are linked when they stand in the same passages more often than chance predicts, and a link's NPMI says how much more: 1 when the two only ever occur together, 0 when they meet as often as chance would have them. A community is a group of concepts more linked to each other than to the rest. The graph comes from the text alone, without the outline or a model, so it takes seconds. It is cached with the index, so later calls only load it.

| Option | Default | Meaning |
|---|---|---|
| `--full` | off | With `--json`, list every passage of each concept and link, not only the first ten. |

The text form starts with a summary line. Each community follows, numbered from 1 and headed by up to five of its concepts, those with most passages. Each concept line gives its number of passages, the first of them, and the concepts it links to with their NPMI, highest first:

```console
$ ebook-hero graph tests/fixtures/alice.epub
60 concepts in 8 communities · 166 passages · 13 topics

Community 1: queen, thought alice, king, added, court, …
  queen          30 passages, first 66    king 0.60, majesty 0.52, knave 0.47, shouted 0.43, duchess 0.22
  thought alice  25 passages, first 2     trial 0.23, size 0.17, write 0.17
…
Community 8: time, hatter, march hare, dormouse
  time        47 passages, first 3    found 0.18, march hare 0.17, hatter 0.15
  hatter      18 passages, first 77   march hare 0.92, dormouse 0.76, court 0.38, time 0.15
  march hare  17 passages, first 77   hatter 0.92, dormouse 0.72, majesty 0.24, time 0.17
  dormouse    13 passages, first 80   hatter 0.76, march hare 0.72, court 0.48
```

A concept's passages are those where it stands as a phrase of its own, so `find` can list more mentions of its words. With `--json` it prints the [`concept_graph`](#concept_graph) result, where communities are numbered from 0.

A book of fewer than 20 passages is refused: `the book has 6 passages; a concept graph needs at least 20, so use search or find on a book this short`. [concept-graph.md](concept-graph.md) explains how concepts, links and communities are chosen.

### extract

```text
ebook-hero extract <path> [--format markdown|json]
```

Prints the whole book as the reader extracted it.

| Option | Default | Meaning |
|---|---|---|
| `--format <format>` | `markdown` | `markdown` prints the text with the book's headings as Markdown headings; `json` prints the document: metadata, outline, and every block with its kind, section and locator. `--json` is the same as `--format json`. |

`extract` reads the file on every call and neither reads nor writes the index, so it works on a book that was never indexed. The Markdown starts with the title as a `#` heading. A heading of level n gets n `#`, at most six, a list item starts with `- `, and a note is a `>` quote, with its label in brackets when it has one. A PDF marks each new page with `<!-- page N -->`. Running heads, page numbers and other lines the PDF reader set apart as page furniture are left out.

```console
$ ebook-hero extract tests/fixtures/alice.epub
# Alice's Adventures in Wonderland

## The Project Gutenberg eBook of Alice's Adventures in Wonderland

This eBook is for the use of anyone anywhere in the United States and most other parts of the world at no cost and with almost no restrictions whatsoever. …
…
## CHAPTER I. Down the Rabbit-Hole
…
```

The JSON is the document model: `schema_version`, `source` (`sha256`, `file_name`, `format`), `metadata`, `outline`, `blocks`, `furniture` and `warnings`. A block looks like this:

```json
{"id": 41, "kind": {"type": "paragraph"}, "text": "In another moment down went Alice after it, never once considering how in the world she was to get out again.", "section": ["Alice’s Adventures in Wonderland", "CHAPTER I. Down the Rabbit-Hole"], "locator": {"format": "epub", "href": "4040227324556390537_11-h-1.htm.html", "anchor": "chap01", "para": 5, "print_page": null}}
```

[document-model.md](document-model.md) describes every field. Changes to the model only add fields or variants, and each one raises `schema_version`.

### index

```text
ebook-hero index <path>
```

Builds the whole index of the book, embeddings included, then prints what `info` prints. It does ahead of time what the first semantic or hybrid search would do, downloading the models first if they are not cached. While it embeds, standard error shows `embedding <done>/<total>`, one line per batch of 8 passages, or one updated line on a terminal. A book whose passages are embedded already is done at once.

```console
$ ebook-hero index tests/fixtures/alice.epub
Title: Alice's Adventures in Wonderland
Authors: Lewis Carroll
Language: en
Format: EPUB
Spine items: 14
Passages: 166
Semantic index: ready
```

In a cache where `alice.epub` was not embedded yet, standard error first showed `embedding 8/166` to `embedding 166/166`. After the search above it was embedded already, and `index` printed this at once. A model that cannot be downloaded or run fails as in [search](#search).

### cache

```text
ebook-hero cache list
ebook-hero cache clear
```

`cache list` prints one line per cached index, sorted by hash: the SHA-256 of the book's file, the size of the index in megabytes, and the book's title. It prints nothing when the cache is empty.

`cache clear` deletes every cached index and prints how many it removed: `removed 1 index`, or `removed <n> indexes` for any other number. An index that another process is building or embedding is left alone, and the line then ends with ` (<n> skipped: in use)`. The empty `.lock` files beside the indexes stay, since deleting them could let two processes build one book at once.

After the examples above, the cache holds two books:

```console
$ ebook-hero cache list
08a2de3a58226311915bd87056e59ed4ee49a284dcb5e722fef415bca4032a4b 1.1 MB Alice's Adventures in Wonderland
79344fac38920106022acdf970fe94855b1f5f5e826756fdd4d4418bbdae18e5 0.0 MB Sample Book
$ ebook-hero cache clear
removed 2 indexes
```

The lexical index of a book differs a little in size from one build to the next, so a rebuilt index can list a slightly different size. A book whose index was cleared is indexed again by the next command about it.

With `--json` each command prints an object:

| Command | Field | What it holds |
|---|---|---|
| `cache list` | `indexes[]` | one object per cached index, sorted by hash |
| `cache list` | `indexes[].sha256` | the SHA-256 of the book's file |
| `cache list` | `indexes[].size_bytes` | the bytes the index takes |
| `cache list` | `indexes[].title` | the book's title, or `null` when it has none |
| `cache clear` | `removed` | how many indexes it deleted |
| `cache clear` | `skipped_in_use` | how many it left alone because another process was building or embedding them |

An empty cache gives `"indexes": []`. On a cache that holds the same two books, the commands print:

```console
$ ebook-hero cache list --json
{
  "indexes": [
    {
      "sha256": "08a2de3a58226311915bd87056e59ed4ee49a284dcb5e722fef415bca4032a4b",
      "size_bytes": 1111734,
      "title": "Alice's Adventures in Wonderland"
    },
    {
      "sha256": "79344fac38920106022acdf970fe94855b1f5f5e826756fdd4d4418bbdae18e5",
      "size_bytes": 35864,
      "title": "Sample Book"
    }
  ]
}
$ ebook-hero cache clear --json
{
  "removed": 2,
  "skipped_in_use": 0
}
```

### mcp

```text
ebook-hero mcp
```

Serves the tools of the [MCP reference](#mcp-reference) over standard input and output until the client disconnects. An MCP client starts this command itself; standard output then carries only protocol messages, and logs go to standard error. Started by hand with nothing to talk to, it stops at once:

```console
$ ebook-hero mcp < /dev/null
error: I/O error: connection closed: initialize request
```

An `EBOOK_HERO_MODEL_IDLE` that is not a whole number of at least 1 stops it before it starts: `error: I/O error: EBOOK_HERO_MODEL_IDLE is "five"; set it to a whole number of seconds, at least 1, or unset it to keep a model 300 seconds after its last use`.

`--json` chooses the form of a command's output, and the protocol fixes the form of this one's, so `mcp --json` fails before the server starts:

```console
$ ebook-hero mcp --json
error: --json does not apply to mcp, whose standard output carries only MCP messages; run ebook-hero mcp without --json
```

## MCP reference

The MCP server gives an AI assistant the commands above as seven tools. Each client starts its own server process, which keeps the four books it used last open for the next calls.

### Registering the server

With Claude Code:

```sh
claude mcp add ebook-hero -- ebook-hero mcp
```

Any other client takes a server entry in its `mcpServers` configuration:

```json
{
  "mcpServers": {
    "ebook-hero": {
      "command": "ebook-hero",
      "args": ["mcp"]
    }
  }
}
```

A client that does not search the `PATH` needs the binary's absolute path as `command`, such as `/home/me/.cargo/bin/ebook-hero`.

When a client connects, the server tells it how to use the tools:

> ebook-hero searches and reads one PDF or EPUB per call; pass the book's path to every tool. Use `search` for questions and `find` to list every occurrence of a phrase. Expand a hit with `read`. Check a quote with `verify_quote` before using it. `concept_graph` lists the book's concepts, which of them link and the communities they form. Ranges are Unicode code points.

### Calls and results

Every tool takes `path`, the book: an absolute path, a path relative to the server's working directory, or one that starts with `~`. The client chooses the server's working directory, so an absolute path is the safe choice. An argument the tool does not know is refused.

A successful call returns the result twice: as `structuredContent`, and as compact JSON in its one text block. The result equals what the matching command prints with `--json`, so the command line can show what a tool returns. A failed call is a tool result with `isError` set to `true` and the message as its text, which the assistant can read and act on:

```json
{"content":[{"type":"text","text":"give exactly one of passage_id, page, outline_id"}],"isError":true}
```

Each call runs on its own thread, so a search that waits for embedding does not hold up other calls. When a `search` request carries a `progressToken`, the server sends progress while it embeds:

```json
{"jsonrpc":"2.0","method":"notifications/progress","params":{"message":"embedding passages","progress":8.0,"progressToken":"t1","total":166.0}}
```

A client that cancels a request, with `notifications/cancelled`, also stops the call; see [Tasks and cancelling](#tasks-and-cancelling).

The two models take about 0.6 GB once loaded. The server drops them after `EBOOK_HERO_MODEL_IDLE` seconds without a search, 300 by default, and the next search loads them again, in about a second.

### Tasks and cancelling

A call can take minutes: the first one on a book builds its index, and the first semantic or hybrid `search` embeds all its passages. The server therefore supports the MCP tasks extension (`io.modelcontextprotocol/tasks`, SEP-2663) for all seven tools.

- A client that declares the extension when it connects gets a task handle back at once, with `resultType` `"task"` and a `taskId`. It polls `tasks/get` until `status` is `completed`, `failed` or `cancelled`. A `completed` task holds the result the tool would have returned, in `result`, including a tool error with `isError` set. While a `search` embeds, `statusMessage` says how far it is, as in `embedding passages 8 of 166`.
- A client that does not declare the extension waits for the result, as before. Such a client may still send `notifications/progress` tokens, which only `search` answers.
- `tasks/cancel` asks the task to stop. The call stops at the two points where it can without leaving anything half written: while it waits for another process's build lock, and between two batches of passages it embeds. The embeddings made so far are dropped, nothing is cached, and the task ends `cancelled`; the next `search` embeds the book again. A stage-1 index build that already holds the lock runs to its end, and so does a call that is past those points, so a cancelled task can still end `completed`.
- A task is kept for one hour (`TASK_TTL`) after it starts, and again after it ends so that the client can read the result. One that runs longer ends `failed` and its call is cancelled.
- A request cancelled with `notifications/cancelled` stops the same way. Its call ends with the tool error `cancelled before it finished; call again to retry`, which a client that has already given up on the request does not see.

Tasks are held in memory, so they end with the server process.

### book_info

Summarizes the book. It is the MCP form of [`info`](#info).

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |

It returns:

| Field | What it holds |
|---|---|
| `title` | the title, or `null` when the file gives none |
| `authors` | the authors, in the order the file lists them |
| `language` | the language an EPUB declares; always `null` for a PDF |
| `format` | `"pdf"` or `"epub"` |
| `page_count`, `spine_count` | the pages of a PDF or the spine entries of an EPUB, each `null` for the other format |
| `passage_count` | the number of passages, whose ids run from 0 to one less |
| `lexical_ready` | always `true` |
| `semantic_ready` | whether the passages are embedded |
| `warnings` | the parts the reader left out, each an object whose `type` is `pages_without_text` (with `pages`), `unreadable_content` (with `href` and `reason`) or `unreadable_part` (with `part` and `reason`) |

```json
{"path": "/home/me/books/alice.epub"}
```

```json
{"title":"Alice's Adventures in Wonderland","authors":["Lewis Carroll"],"language":"en","format":"epub","page_count":null,"spine_count":14,"passage_count":166,"lexical_ready":true,"semantic_ready":true,"warnings":[]}
```

### outline

Lists the table of contents, each entry with the passage where its text starts. It is the MCP form of [`outline`](#outline).

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |

It returns `entries` in reading order. Each has an `id`, which [`read`](#read-1) takes as `outline_id`, a `title`, a `level`, the top level being 1, and the `passage_id` and `locator` of its first passage, both `null` when the entry has no text.

```json
{"path": "/home/me/books/alice.epub"}
```

```json
{
  "entries": [
    …
    {
      "id": 3,
      "title": "CHAPTER I. Down the Rabbit-Hole",
      "level": 1,
      "passage_id": 2,
      "locator": {"format": "epub", "href": "4040227324556390537_11-h-1.htm.html", …}
    },
    …
  ]
}
```

### search

Finds the passages that best answer a query. It is the MCP form of [`search`](#search), whose section explains the modes and scores.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |
| `query` | string | required | Words, "quoted phrases" or a question. |
| `mode` | `"hybrid"`, `"lexical"` or `"semantic"` | `"hybrid"` | How to rank the passages. |
| `top_k` | integer | `5` | How many hits to return, from 1 to 20. |
| `rerank` | boolean | `true` | Whether the reranker reorders the best candidates; lexical mode ignores it. |

It returns the trimmed `query`, the `mode`, and `hits`, best first, each with `passage_id`, `text`, `section`, `locator`, `matches`, `score`, `lexical_rank` and `semantic_rank`. A score orders the hits of one call and means nothing across calls. The first semantic or hybrid search of a book embeds all its passages, which can take minutes; running [`ebook-hero index`](#index) beforehand avoids the wait.

```json
{"path": "/home/me/books/alice.epub", "query": "why does the Queen want to cut off heads", "top_k": 1}
```

```json
{"query":"why does the Queen want to cut off heads","mode":"hybrid","hits":[{"passage_id":93,"text":"Five and Seven said nothing, but looked at Two. …","section":["Alice’s Adventures in Wonderland","CHAPTER VIII. The Queen’s Croquet-Ground"],"locator":{…},"matches":[[202,207],[251,256],…],"score":0.9066033,"lexical_rank":2,"semantic_rank":null}]}
```

### find

Lists every passage that contains an exact phrase, in reading order. It is the MCP form of [`find`](#find).

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |
| `phrase` | string | required | The phrase, matched as whole words in any case, without stemming. |
| `cursor` | integer | the first page | Where to continue: the `next_cursor` of the previous page. |

It returns `phrase`, `total_occurrences` and `total_passages` for the whole book, `occurrences`, the passages of this page, at most 20, each with `passage_id`, `section`, `locator` and one `matches` range per occurrence, and `next_cursor`, `null` on the last page.

```json
{"path": "/home/me/books/alice.epub", "phrase": "off with her head"}
```

```json
{"phrase":"off with her head","total_occurrences":4,"total_passages":4,"occurrences":[{"passage_id":95,"section":["Alice’s Adventures in Wonderland","CHAPTER VIII. The Queen’s Croquet-Ground"],"locator":{…},"matches":[[1015,1032]]},…],"next_cursor":null}
```

### read

Reads passages as Markdown. It is the MCP form of [`read`](#read), with `outline_id` where the command has `--outline`.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |
| `passage_id` | integer | | Read this passage. |
| `page` | integer | | Read the passages on this physical page of a PDF, counted from 1. |
| `outline_id` | integer | | Read the passages of this outline entry, by the `id` that `outline` gives. |
| `before` | integer | `0` | Passages to add before the range, at most 10. |
| `after` | integer | `0` | Passages to add after the range, at most 10. |
| `cursor` | integer | the start of the range | The passage to continue at: the `next_cursor` of the previous call. |

Exactly one of `passage_id`, `page` and `outline_id` must be given. The result holds at most about 8000 characters of `markdown`, the `passages` it holds, each with `passage_id`, `section` and `locator`, and `next_cursor`. When `next_cursor` is set, the same call with `cursor` set to it gives the next part.

```json
{"path": "/home/me/books/alice.epub", "passage_id": 99}
```

```json
{"markdown":"## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground\n\n<!-- passage 99 · CHAPTER VIII. The Queen’s Croquet-Ground ¶42–43 -->\n\nThe chief difficulty Alice found at first was in managing her flamingo: …\n","passages":[{"passage_id":99,"section":["Alice’s Adventures in Wonderland","CHAPTER VIII. The Queen’s Croquet-Ground"],"locator":{…}}],"next_cursor":null}
```

### verify_quote

Checks whether a quote occurs in the book, and where. It is the MCP form of [`verify`](#verify), whose section explains the verdicts and the fields of the result.

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |
| `quote` | string | required | The text to look for, at most 440 words. |
| `passage_id` | integer | the whole book | Look in this passage first, as a citation names it. |

It returns `verdict`, `passage_id`, `locator`, `matched`, `closest` and `found_elsewhere`. A `none` verdict is an ordinary result without `isError`, although the command exits with 3 for it.

```json
{"path": "/home/me/books/alice.epub", "quote": "Off with her head!"}
```

```json
{"verdict":"exact","passage_id":95,"locator":{"format":"epub","href":"4040227324556390537_11-h-8.htm.html","para_start":14,"para_end":20,"print_page":null,"under_heading":{"text":"CHAPTER VIII. The Queen’s Croquet-Ground","para_start":13,"para_end":19}},"matched":[1015,1033],"closest":null,"found_elsewhere":null}
```

`matched` counts code points of passage 95's text, the text that `read` prints under the passage's comment line.

### concept_graph

Builds or loads the book's concept graph. It is the MCP form of [`graph`](#graph).

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | required | The book. |
| `full` | boolean | `false` | List every passage of each concept and link, not only the first ten. |

It returns:

| Field | What it holds |
|---|---|
| `passages`, `topics` | the number of passages in the book, and of the topics the concepts were chosen with |
| `concepts[]` | `id`, `label`, `passage_count`, `passages` (ids in reading order) and `community`, in the order the book first mentions them |
| `links[]` | `a` (the lower concept id), `b`, `npmi` to three decimals, and the `passage_count` and `passages` that both concepts stand in |
| `communities[]` | `id`, from 0, and `concepts`, ascending; the largest community comes first |

Unless `full` is `true`, each `passages` list holds the first ten ids, while `passage_count` gives the whole number. Every id can be given to `read` as `passage_id`.

```json
{"path": "/home/me/books/alice.epub"}
```

```json
{
  "passages": 166,
  "topics": 13,
  "concepts": [
    {"id": 0, "label": "copy", "passage_count": 10, "passages": [0, 149, 150, 152, 153, 154, 155, 156, 157, 160], "community": 5},
    …
    {"id": 40, "label": "hatter", "passage_count": 18, "passages": [77, 78, 79, 80, 81, 82, 83, 84, 85, 86], "community": 7},
    …
  ],
  "links": [
    …
    {"a": 40, "b": 41, "npmi": 0.924, "passage_count": 16, "passages": [77, 78, 79, 80, 81, 82, 83, 84, 85, 86]},
    …
  ],
  "communities": [
    …
    {"id": 7, "concepts": [12, 40, 41, 42]}
  ]
}
```

## Library reference

The `ebook-hero` crate is the library behind the command line and the MCP server. Its entry point is `Session`, and the API documentation of every type is on [docs.rs](https://docs.rs/ebook-hero).

```sh
cargo add ebook-hero
```

In code the crate is `ebook_hero`.

### Creating a session

A `Session` opens books by path, keeps their indexes in a cache directory, and loads the models when a search first needs them. It keeps the four most recently used books open, and it remembers a file's hash while the file's size and modification time stay the same, so a second call about a book reads neither the file nor the index again. A session is `Send` and `Sync`, so threads can share one, as the MCP server does.

| Constructor | Indexes in | Models in |
|---|---|---|
| `Session::new() -> Session` | `$EBOOK_HERO_INDEXES`, else `ebook-hero/indexes` in the user cache directory | `$EBOOK_HERO_MODELS`, else `ebook-hero/models` in the user cache directory |
| `Session::with_cache_root(root: PathBuf) -> Session` | `root` | as `new` |
| `Session::with_dirs(cache_root: PathBuf, models_dir: PathBuf) -> Session` | `cache_root` | `models_dir` |

`Session::default()` is `Session::new()`. The command line and the MCP server use `new`.

The cache root must be private, for the reason given in [Where files are kept](#where-files-are-kept). On Unix it must be a directory, not a symbolic link, that belongs to the current user and that group and others cannot write to, as mode 0700 ensures. A root that does not exist is created with mode 0700 when the session is made, provided its parent exists. No constructor fails: a root that cannot be used makes every method that opens a book return `Error::Index`, whose message names the cause.

### Methods

Every method that takes a `path` accepts the same paths as the command line. A method that matches an MCP tool returns `ebook_hero::Result<T>`, where `T` serializes through serde to that tool's result, so `serde_json::to_string(&result)` gives the JSON shown in the [MCP reference](#mcp-reference).

| Method | Returns | MCP tool |
|---|---|---|
| `info(&self, path: &str)` | `BookInfo` | `book_info` |
| `outline(&self, path: &str)` | `OutlineResult` | `outline` |
| `search(&self, path: &str, params: SearchParams, progress: &dyn Fn(usize, usize))` | `SearchResult` | `search` |
| `find(&self, path: &str, phrase: &str, cursor: Option<usize>)` | `FindResult` | `find` |
| `read(&self, path: &str, p: ReadParams)` | `ReadResult` | `read` |
| `verify(&self, path: &str, quote: &str, passage_id: Option<u32>)` | `VerifyResult` | `verify_quote` |
| `graph(&self, path: &str, full: bool)` | `GraphResult` | `concept_graph` |
| `index(&self, path: &str, progress: &dyn Fn(usize, usize))` | `BookInfo` | none; the command `index` |
| `cancellable(&self, cancel: Cancel)` | `Session` | none |
| `book(&self, path: &str)` | `Arc<Book>` | none |
| `models(&self)` | `&Models` | none |

The parameters that are not plain values are two structs:

- `SearchParams { query: String, mode: Mode, top_k: usize, rerank: bool }`, with `Mode::Hybrid`, `Mode::Lexical` or `Mode::Semantic`, from `ebook_hero::search`. Nothing in the struct is optional: `top_k` must be from 1 to 20, and the command line's defaults are `Mode::Hybrid`, which is `Mode::default()`, 5 and `true`.
- `ReadParams { at: ReadAt, before: u32, after: u32, cursor: Option<u32> }`, with `ReadAt::Passage(id)`, `ReadAt::Page(page)` or `ReadAt::Outline(id)`, from `ebook_hero::read`.

`search` and `index` call `progress(done, total)` after each batch of passages they embed, which happens on the first semantic or hybrid search of a book. Lexical search never calls it, so `&|_, _| {}` serves there.

`cancellable` returns a session that shares this one's open books and models and whose calls end with `Error::Cancelled` once `cancel`, an `ebook_hero::cancel::Cancel`, is set from another thread: `let cancel = Cancel::new(); let session = session.cancellable(cancel.clone());`. A call stops while it waits for another process's build lock, and between two batches of passages it embeds, and nothing is cached from a cancelled embedding. A call that holds the lock for a stage-1 build runs to its end. The MCP server uses it for tasks.

`book` opens the book and returns its `Book`, with the extracted `document` and the `passages`; the other methods call it first. `models` gives the session's `Models`, whose `release_idle(idle)` drops the models when they have gone unused for `idle`.

Every method blocks until it is done, which can be minutes while a book is indexed or embedded, so async code calls it on a blocking thread, such as `tokio::task::spawn_blocking`.

### Errors

Every error is an `ebook_hero::Error`, whose `Display` is the message the command line prints after `error: `. Every method that opens a book can return:

| Variant | When |
|---|---|
| `NotFound { path }`, `NotAFile { path }` | `path` names no regular file. |
| `Unsupported { path }` | The file is neither a PDF nor an EPUB. |
| `Unreadable { path, reason }` | The file cannot be opened or parsed. |
| `NoTextLayer { path }` | The PDF has no text. |
| `InvalidArgument(message)` | `path` starts with `~` and no home directory is known. |
| `Index(message)` | The cache root is not private, or the index cannot be written. |
| `LockTimeout { sha256, pid, waited_secs }` | Another process was still building the same book's index after 900 seconds. |
| `Cancelled` | The session came from `cancellable` and its `Cancel` was set before the call finished. |
| `Io(error)` | Any other input or output error. |

The methods add these:

| Method | Adds |
|---|---|
| `search` | `InvalidArgument` for a `top_k` outside 1 to 20 or a blank query, checked before the book is opened; `ModelUnavailable { model, cache, reason }` when a model cannot be downloaded or run |
| `find` | `InvalidArgument` for a phrase without a word |
| `read` | `InvalidArgument` for `before` or `after` above 10, checked before the book is opened, and for a passage, page or outline entry that does not exist or has no text, a page of an EPUB, or a cursor outside the range |
| `verify` | `InvalidArgument` for a quote that is blank, has no letter or digit or has more than 440 words, or a `passage_id` the book does not have |
| `graph` | `InvalidArgument` for a book of fewer than 20 passages |
| `index` | `ModelUnavailable` |

Semantic and hybrid `search`, `graph` and `index` also return `Index` when another process deletes the book's cache directory while the call runs, as `ebook-hero cache clear` does. Calling again opens the book anew.

### Example

This program finds a passage, reads it with one passage on each side, and checks a quote:

```rust
use ebook_hero::read::{ReadAt, ReadParams, locator_text};
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::Session;
use ebook_hero::verify::Verdict;

fn main() -> ebook_hero::Result<()> {
    let book = "tests/fixtures/alice.epub";
    let session = Session::new();

    // Lexical search loads no model.
    let params = SearchParams {
        query: "flamingo hedgehog".to_string(),
        mode: Mode::Lexical,
        top_k: 1,
        rerank: false,
    };
    let hit = &session.search(book, params, &|_, _| {})?.hits[0];
    println!("passage {} · {}", hit.passage_id, locator_text(&hit.locator));

    // The hit with one passage on each side, as Markdown.
    let around = ReadParams {
        at: ReadAt::Passage(hit.passage_id),
        before: 1,
        after: 1,
        cursor: None,
    };
    let page = session.read(book, around)?;
    println!("{} characters of Markdown", page.markdown.chars().count());

    // Is the quote in the book, and where?
    let checked = session.verify(book, "Off with his head!", None)?;
    match checked.passage_id {
        Some(id) if checked.verdict != Verdict::None => {
            println!("{:?} in passage {id}", checked.verdict);
        }
        _ => println!("not in the book"),
    }
    Ok(())
}
```

Run from a clone of the repository, it prints:

```text
passage 103 · CHAPTER VIII. The Queen’s Croquet-Ground ¶61–63
3705 characters of Markdown
Exact in passage 85
```

`read::locator_text` prints a locator as the command line does. Beside `Session`, three functions serve programs that work with a whole book: `extract::extract` reads a file into a `Document` without the cache, `document::markdown::document_to_markdown` writes one as `extract` prints it, and `index::cache::sha256_file` gives the hash that `extract` takes.

## Recipes

### Find where a quote is

`verify` says whether a quote is in the book and where it starts, and `read` then shows it in context:

```console
$ ebook-hero verify tests/fixtures/alice.epub "Off with her head!"
exact: passage 95 · CHAPTER VIII. The Queen’s Croquet-Ground ¶13–19
$ ebook-hero read tests/fixtures/alice.epub 95
```

A quote recalled from memory often differs by a word. `none` then names the closest passage and the first word that differs, which is the place to read. A citation that names a passage is checked against that passage with `--passage-id`; a quote that is in the book but not there gets `none` and a `found in passage` line. `verify` reports one place, the first exact match in reading order or else the first equivalent one, and `find` lists every occurrence of a phrase:

```console
$ ebook-hero find tests/fixtures/alice.epub "off with her head"
4 occurrences in 4 passages
…
```

### Read around a search hit

A hit gives a passage id, which `read` takes with `--before` and `--after` to add the passages on each side:

```console
$ ebook-hero search tests/fixtures/alice.epub "flamingo hedgehog" --mode lexical --top-k 1
1. passage 103 · Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground ¶61–63  (score 10.849)
…
$ ebook-hero read tests/fixtures/alice.epub 103 --before 1 --after 1
## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground

<!-- passage 102 · CHAPTER VIII. The Queen’s Croquet-Ground ¶50–60 -->
…
<!-- passage 103 · CHAPTER VIII. The Queen’s Croquet-Ground ¶61–63 -->
…
<!-- passage 104 · CHAPTER VIII. The Queen’s Croquet-Ground ¶64–69 -->
…
```

To read the whole chapter instead, find its entry with `outline` and read it with `--outline`, following `next: --cursor` until the chapter is done.

### Export a book as Markdown

```sh
ebook-hero extract book.pdf > book.md
ebook-hero extract book.pdf --format json > book.json
```

The Markdown keeps the headings, paragraphs, list items and notes, and drops page furniture. The JSON adds every block's kind, section and locator, and keeps the furniture in a list of its own. A program that already has a session can write the cached document instead, which is the same document:

```rust
use ebook_hero::document::markdown::document_to_markdown;
use ebook_hero::session::Session;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("give the path of a book")?;
    let book = Session::new().book(&path)?;
    print!("{}", document_to_markdown(&book.document));
    Ok(())
}
```

For `alice.epub` this prints the same bytes as `ebook-hero extract`.

### Draw the concept graph

The [concept-map](https://crates.io/crates/concept-map) crate, which builds ebook-hero's graphs, writes a graph in Graphviz's DOT language with `GraphResult::to_dot`. Its graph has the same fields as ebook-hero's, so serde turns one into the other. In a new project:

```sh
cargo new book-map
cd book-map
cargo add ebook-hero concept-map serde_json
```

`src/main.rs`:

```rust
use ebook_hero::session::Session;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("give the path of a book")?;
    let session = Session::new();
    let graph = session.graph(&path, false)?;
    // The two crates' graphs have the same fields, so serde turns one into the other.
    let map: concept_map::GraphResult = serde_json::from_value(serde_json::to_value(&graph)?)?;
    print!("{}", map.to_dot());
    Ok(())
}
```

Graphviz then draws it:

```sh
cargo run --release -- /home/me/books/alice.epub > alice.dot
dot -Tsvg alice.dot -o alice.svg
```

The picture shows each concept as a circle in its community's colour, sized by its passages, joins linked concepts with lines that thicken with their NPMI, and names each community in a legend. `to_dot` reads no passage lists, so the abridged graph serves. The output of `ebook-hero graph book.epub --json` converts the same way: `serde_json::from_reader(std::io::stdin())` reads it into a `concept_map::GraphResult`.

### Use it from an AI assistant over MCP

Register the server once, as in [Registering the server](#registering-the-server). The assistant can then work with any book whose path a request gives it:

> In /home/me/books/alice.epub, why does the Queen want to cut off heads? Quote the book.

The server's instructions tell the assistant to use `search` for such a question, `read` to expand a hit, and `verify_quote` to check a quote before using it. `find` suits a request for every mention of a name, and `concept_graph` a request for an overview of a book the assistant has not read.

A first hybrid search embeds the whole book while the assistant waits. Running `ebook-hero index` on the book beforehand removes that wait:

```sh
ebook-hero index /home/me/books/alice.epub
```
