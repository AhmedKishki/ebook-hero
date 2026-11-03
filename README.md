# ebook-hero

ebook-hero searches a PDF or EPUB, reads the text around what it finds, and checks whether a quote really occurs in the book. It runs as a command-line tool for people and as an MCP server over stdio for agents. Each call names one book file: the first call indexes that book, and later calls reuse the index.

## Install

```sh
cargo install --locked --git https://github.com/AhmedKishki/ebook-hero
```

From a clone:

```sh
cargo build --release
sudo install target/release/ebook-hero /usr/local/bin/
```

## Example

These commands run on `tests/fixtures/alice.epub`, the copy of *Alice's Adventures in Wonderland* in this repository, so you can repeat them from a clone. The output is shortened.

`search` returns the passages that best answer a question:

```console
$ ebook-hero search tests/fixtures/alice.epub "why does the Queen want to cut off heads"
1. passage 93 · 4040227324556390537_11-h-8.htm.html ¶11 · Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground  (score 0.907)
    Five and Seven said nothing, but looked at Two. Two began in a low voice, “Why the fact is, you see, Miss, this here ought to have been a red rose-tree, and we put a white one in by mistake; and if the «Queen» was to find it out, we should all have our «heads» «cut» «off», you know. …

2. passage 104 · 4040227324556390537_11-h-8.htm.html ¶65–70 · Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground  (score 0.610)
    When she got back to the Cheshire Cat, she was surprised to find quite a large crowd collected round it: there was a dispute going on between the executioner, the King, and the «Queen», …
```

Each hit gives its passage id, its place in the book (here the EPUB file and paragraphs, in a PDF the page), its chapter and its score. The words that matched the query are marked «like this».

`read` prints a hit in full, as Markdown, from its passage id. To see the text around it, add `--before` or `--after`:

```console
$ ebook-hero read tests/fixtures/alice.epub 104
## Alice’s Adventures in Wonderland › CHAPTER VIII. The Queen’s Croquet-Ground

<!-- passage 104 · 4040227324556390537_11-h-8.htm.html ¶65–70 -->

When she got back to the Cheshire Cat, she was surprised to find quite a large crowd collected round it: …
```

`find` lists every occurrence of an exact phrase, in reading order:

```console
$ ebook-hero find tests/fixtures/alice.epub "off with her head"
4 occurrences in 4 passages
passage 95 · 4040227324556390537_11-h-8.htm.html ¶14–20: …r a moment like a wild beast, screamed “«Off with her head»! Off—” “Nonsense!” said Alice, very lo…
passage 99 · 4040227324556390537_11-h-8.htm.html ¶43–44: …, and shouting “Off with his head!” or “«Off with her head»!” about once in a minute.…
…
```

`verify` checks a quote. `exact` means it occurs as written, `equivalent` means only case, punctuation or spacing differ, and `none` names the passage that shares the longest run of its words:

```console
$ ebook-hero verify tests/fixtures/alice.epub "Off with her head!"
exact: passage 95 · 4040227324556390537_11-h-8.htm.html ¶14–20
$ ebook-hero verify tests/fixtures/alice.epub "off with their heads"
equivalent: passage 96 · 4040227324556390537_11-h-8.htm.html ¶21–29
$ ebook-hero verify tests/fixtures/alice.epub "the Queen was very kind to everyone"
none; closest passage 93 · 4040227324556390537_11-h-8.htm.html ¶11 (3 of 7 words; differs at "very")
```

## How search works

Search ranks passages: runs of whole paragraphs, up to 220 words, that never cross a heading or, in an EPUB, a file. `--mode` chooses how they are ranked:

- `lexical` ranks by BM25 on stemmed words, so "heads" also matches "head". Common words such as "the" and "how" are ignored. It finds the words you typed.
- `semantic` ranks by the cosine similarity between the query and each passage, embedded with bge-small-en-v1.5. It finds passages that say the same thing in other words. Passages below 0.55 are dropped, so an unrelated query returns nothing.
- `hybrid`, the default, merges the top 50 lexical and the top 50 semantic passages by reciprocal rank fusion, with BM25 weighted 1.25, so a passage can rank high for its words or for its meaning. The jina-reranker-v1-turbo-en model then reorders the best 30 by reading the query and each passage together. `--no-rerank` skips this step.

`--top-k` sets how many passages come back: 5 by default, at most 20.

## Command line

```sh
ebook-hero info book.pdf
ebook-hero search book.pdf "how do institutions change"
ebook-hero search book.pdf "institutional change" --mode lexical
ebook-hero search book.pdf "how do rules persist over time" --mode semantic --top-k 10
ebook-hero find book.pdf "social structure"
ebook-hero read book.pdf --page 42
ebook-hero verify book.pdf "a quote you want to check"
ebook-hero extract book.pdf --format markdown > book.md
```

`--page` is the physical page of the PDF, counted from 1, as is `page` in the MCP `read` tool; the printed page label is shown in the locator. `outline`, `read` (by passage id or outline entry), `index` and `cache` are also commands. For `info`, `outline`, `search`, `find`, `read` and `verify`, `--json` prints what the MCP tools `book_info`, `outline`, `search`, `find`, `read` and `verify_quote` return. `ebook-hero <command> --help` lists the flags.

## MCP server

With Claude Code:

```sh
claude mcp add ebook-hero -- ebook-hero mcp
```

For any other client, add this server entry:

```json
{"command": "ebook-hero", "args": ["mcp"]}
```

| Tool | What it does |
|---|---|
| `book_info` | Title, authors, format, size, and whether the semantic index is built. |
| `outline` | The table of contents, with the passage each entry starts at. |
| `search` | The passages that best answer a query: `hybrid` (default), `lexical` or `semantic`. |
| `find` | Every passage that contains an exact phrase, with the total number of occurrences. |
| `read` | Passages as Markdown, by passage id, PDF page or outline entry. |
| `verify_quote` | Whether a quote occurs in the book, and where. |

## Storage

- Indexes are cached in `$TMPDIR/ebook-hero`, a private directory (mode 0700). `ebook-hero cache list` shows them and `ebook-hero cache clear` deletes them.
- The two models live in the user cache directory (`~/.cache/ebook-hero/models` on Linux, `~/Library/Caches/ebook-hero/models` on macOS) or in `$EBOOK_HERO_MODELS` if it is set. The first hybrid or semantic search downloads about 270 MB, then embeds the whole book, which can take minutes. `ebook-hero index book.pdf` does the embedding ahead of time. Lexical search needs neither.
- Logs go to stderr. `EBOOK_HERO_LOG=debug` turns them up (the default is `warn`).

## Limits

- English only.
- PDFs need a text layer. OCR is not supported.
- Formulas and figures are not extracted yet.

## Licence

MIT
