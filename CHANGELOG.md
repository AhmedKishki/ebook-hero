# Changelog

Every change to ebook-hero gets a version and an entry here, newest first. Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html): documentation and fixes that keep the behaviour bump the patch number, and new features or changed behaviour bump the minor number while ebook-hero is at 0.x. The entries follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.5.0] - 2026-11-05

### Added

- MCP tasks. The server declares the `io.modelcontextprotocol/tasks` extension, and a client that declares it too gets a task handle for every tool and polls `tasks/get`. A `search` task reports `embedding passages 8 of 166` in its status message. A client that does not declare the extension waits for the result as before.
- Cancellation. `tasks/cancel` and `notifications/cancelled` stop a call while it waits for another process's build lock and between two batches of passages it embeds. A cancelled embedding is dropped and nothing is cached. A stage-1 index build that holds the lock runs to its end.
- Library: `Session::cancellable(Cancel)`, the `cancel::Cancel` flag, `Error::Cancelled`, and `Book::open_cancellable`, `Book::vectors_cancellable`, `Book::graph_cancellable`, `Models::embed_passages_cancellable` and `cache::with_lock_cancellable`. The existing methods are unchanged.
- `settings::TASK_TTL`, one hour, how long a task may run and is kept after it ends.

### Changed

- `Session` keeps its books, hashes and models behind `Arc`s so that a cancellable session can share them. Nothing in its public API changes.

## [0.4.0] - 2026-11-05

### Added

- `docs/usage.md`, the usage reference: installing and the first run, where the caches live, every command with its options, output, errors and a real example, every MCP tool with its arguments and result, the `Session` library API, and recipes.
- A Help tab in the desktop window. It shows the usage reference with a contents list, and above it the help for the command the form has selected, taken from the binary itself.
- `cache list --json`, which prints `{"indexes": [{"sha256", "size_bytes", "title"}]}`, and `cache clear --json`, which prints `{"removed", "skipped_in_use"}`.
- Documentation for every public item of the library, and a `missing_docs` warning for any new one. `ebook-hero help <command>` gives fuller descriptions and examples.

### Changed

- `verify` exits with status 3 when the quote is not in the book, so a script can tell the verdict `none` from an error, which exits with 1. `exact` and `equivalent` still exit with 0, and the MCP tool `verify_quote` still returns the verdict as data. The desktop window shows a `none` verdict as a result, not a failure.
- `find` prints its `next: --cursor N` line on stderr, as `read` already did, so stdout holds only the listing.
- `mcp --json` stops with an error instead of ignoring `--json`.
- `info` and `index` label an EPUB's spine count `Spine items`, not `Chapters`; the JSON field `spine_count` is unchanged.

### Fixed

- `cache clear` and `find` use the singular for one: `removed 1 index`, `1 occurrence in 1 passage`.
- The README and AGENTS.md said a cache root must have mode 0700. They now give the rule the code enforces: a directory of the current user that is not a symbolic link and not writable by group or others.

## [0.3.0] - 2026-11-05

The first version on crates.io.

### Added

- The Graph tab of the desktop window saves the graph as a PNG or SVG image, and a clicked concept keeps its community lit.
- `docs/`, which explains each part of ebook-hero, and an evaluation of furniture detection against docling on 550 pages from 22 books.

### Changed

- PDF furniture is found by comparing code lengths instead of by recurrence in the top and bottom bands of the page: on the evaluation's books it finds 98% of the furniture, where the band rule found 44%.
- Furniture detection and the concept graph's algorithm come from the [furniture-detection](https://crates.io/crates/furniture-detection) and [concept-map](https://crates.io/crates/concept-map) crates, with the same output as before.
- Indexes are kept in the user cache directory, or in `$EBOOK_HERO_INDEXES`, instead of `$TMPDIR/ebook-hero`, which is emptied at boot.
- The MCP server reranks in batches of 4 and drops a model left unused for `EBOOK_HERO_MODEL_IDLE` seconds (300 by default), so an idle server holds about 70 MB instead of about 1.8 GB.

[0.4.0]: https://github.com/AhmedKishki/ebook-hero/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/AhmedKishki/ebook-hero/releases/tag/v0.3.0
