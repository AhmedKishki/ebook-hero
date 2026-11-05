# AGENTS.md

## Scope

- `ebook-hero` searches and reads one PDF or EPUB per call, as a CLI and a stdio MCP server.
- Searching several books in one call is out of scope.

## Commands

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo test -- --ignored` runs the tests that need the embedding and reranker models; the first run downloads them.
- `cd gui && npm install && npm test` tests the desktop window's form-to-command logic; `npm start` opens the window.

## Architecture

- Readers in `src/extract/` turn a file into a `Document` (`src/document/`). Everything after them reads a `Document` and never changes it.
- `passages`, `index`, `search`, `read`, `verify` and `graph` hold the logic. `cli.rs` and `mcp.rs` parse arguments and format output only. Both call `Session`.
- Two algorithms come from crates of their own: the PDF reader calls `furniture-detection` to find page furniture, and `graph` calls `concept-map` to build the concept graph and converts its result into ebook-hero's output types. Each crate's own `docs/` explains its algorithm; `docs/furniture-detection.md` and `docs/concept-graph.md` here explain how ebook-hero uses them.
- `src/settings.rs` holds the search, read and limit constants and the version numbers. Input caps, writer budgets and reader heuristics live in the module that uses them.
- `docs/` explains each part's algorithm and `evals/` holds evaluations against other tools, each with its harness and frozen inputs.
- `gui/` is an Electron window that runs the built `ebook-hero` binary and shows what it prints. It does not link the library, and its logic is finding that binary and turning the form into one command (`gui/runner.js`), reading the book for its Book tab (`gui/book.js`), reading the concept graph for its Graph tab and saving its image (`gui/graph.js`), and drawing that graph as a standalone SVG with the colours and community names the tab shares (`gui/graphdraw.js`, loaded by the window and by `node --test`). `gui/launch.js` starts Electron; `npm start` and the `ebook-hero-gui` command both run it.
- `gui/book.js` reads `ebook-hero extract --format json` and `read --json`: `schema_version`, `metadata.title`, `source.format`, `blocks[].{id, kind, text, locator}` and a passage's `locator`. Renaming or removing one of these breaks the window, so bump `MIN_SCHEMA_VERSION` there with it.
- `gui/graph.js` reads `ebook-hero graph --json --full`: `passages`, `concepts[].{id, label, passage_count, passages, community}`, `links[].{a, b, npmi, passages}` and `communities[].{id, concepts}`. Renaming or removing one of these breaks the window.

## Rules

- A `Document` schema change is additive and bumps `SCHEMA_VERSION`.
- A change to a reader, to passage packing, or to an analyzer bumps its version in `src/settings.rs`, so cached indexes rebuild. Moving to a version of `furniture-detection` whose output differs bumps `PDF_READER_VERSION`, and to a version of `concept-map` whose graphs differ bumps `GRAPH_VERSION`.
- stdout carries only MCP messages or command output. Logs go to stderr.
- Errors name the cause and a remedy. Nothing falls back silently.
- `--json` output equals the structured result of the matching MCP tool.
- Offsets are UTF-8 bytes inside the library and Unicode code points in output.
- Tests that give `Session` a cache root use `tests/common::private_tempdir()`; the root must be mode 0700.
- Test books are generated or public-domain. Commit no other book.
- A change to an algorithm that `docs/` describes updates that document in the same commit.
- A commit message states what changed and why, with no generated-by or co-author trailers.
