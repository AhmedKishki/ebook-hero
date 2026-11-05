//! Searches and reads one PDF or EPUB per call: lexical, semantic and hybrid
//! search, every occurrence of a phrase, passages by id, page or outline
//! entry, quote verification and a concept graph. The `ebook-hero` binary
//! offers these as a command line and as an MCP server, and both call this
//! library.
//!
//! [`Session`](session::Session) is the entry point. It takes the path of a
//! book, builds the book's index in a cache directory on the first call, and
//! answers later calls from that index. Each of its methods returns the value
//! that the matching MCP tool returns as its structured result, which the
//! matching command prints with `--json`:
//!
//! | `Session` method | MCP tool | Command |
//! |---|---|---|
//! | [`info`](session::Session::info) | `book_info` | `info` |
//! | [`outline`](session::Session::outline) | `outline` | `outline` |
//! | [`search`](session::Session::search) | `search` | `search` |
//! | [`find`](session::Session::find) | `find` | `find` |
//! | [`read`](session::Session::read) | `read` | `read` |
//! | [`verify`](session::Session::verify) | `verify_quote` | `verify` |
//! | [`graph`](session::Session::graph) | `concept_graph` | `graph` |
//! | [`index`](session::Session::index) | | `index` |
//!
//! ```
//! use ebook_hero::read::locator_text;
//! use ebook_hero::session::Session;
//!
//! # fn main() -> ebook_hero::Result<()> {
//! # let tmp = tempfile::tempdir()?;
//! # let cache_dir = tmp.path();
//! let session = Session::with_cache_root(cache_dir.join("indexes"));
//! let found = session.find("tests/fixtures/alice.epub", "off with her head", None)?;
//! for hit in &found.occurrences {
//!     println!("passage {} · {}", hit.passage_id, locator_text(&hit.locator));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The modules follow the path of a book through the library: [`extract`]
//! reads the file into a [`document::Document`], [`passages`] packs its text
//! into passages, [`index`] caches them with a lexical index and their
//! embeddings, and [`search`], [`read`], [`verify`] and [`graph`] answer
//! from them. [`cli`] and [`mcp`] are the two front ends.
//!
//! The [usage reference](https://github.com/AhmedKishki/ebook-hero/blob/main/docs/usage.md)
//! documents every command, every MCP tool and this library.

#![warn(missing_docs)]

pub mod analysis;
pub mod cancel;
pub mod cli;
pub mod document;
pub mod embed;
pub mod error;
pub mod extract;
pub mod graph;
pub mod index;
pub mod mcp;
pub mod normalize;
pub mod passages;
pub mod read;
pub mod search;
pub mod session;
pub mod settings;
pub mod verify;

pub use error::{Error, Result};
