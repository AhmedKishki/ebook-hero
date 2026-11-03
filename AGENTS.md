# AGENTS.md

## Scope

- `ebook-hero` searches and reads one PDF or EPUB per call, as a CLI and a stdio MCP server.
- Searching several books in one call is out of scope.

## Commands

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo test -- --ignored` runs the tests that need the embedding and reranker models; the first run downloads them.

## Architecture

- Readers in `src/extract/` turn a file into a `Document` (`src/document/`). Everything after them reads a `Document` and never changes it.
- `passages`, `index`, `search`, `read`, and `verify` hold the logic. `cli.rs` and `mcp.rs` parse arguments and format output only. Both call `Session`.
- `src/settings.rs` holds the search, read and limit constants and the version numbers. Input caps, writer budgets and reader heuristics live in the module that uses them.

## Rules

- A `Document` schema change is additive and bumps `SCHEMA_VERSION`.
- A change to a reader, to passage packing, or to an analyzer bumps its version in `src/settings.rs`, so cached indexes rebuild.
- stdout carries only MCP messages or command output. Logs go to stderr.
- Errors name the cause and a remedy. Nothing falls back silently.
- `--json` output equals the structured result of the matching MCP tool.
- Offsets are UTF-8 bytes inside the library and Unicode code points in output.
- Tests that give `Session` a cache root use `tests/common::private_tempdir()`; the root must be mode 0700.
- Test books are generated or public-domain. Commit no other book.
- A commit message states what changed and why, with no generated-by or co-author trailers.
