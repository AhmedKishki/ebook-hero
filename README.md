# ebook-hero

Search and read one PDF or EPUB per call, from the command line or as an MCP server over stdio.

## Install

```sh
cargo install --locked --git https://github.com/AhmedKishki/ebook-hero
```

From a clone:

```sh
cargo build --release
sudo install target/release/ebook-hero /usr/local/bin/
```

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
