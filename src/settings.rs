//! The constants that set how search, reading and caching behave, and the
//! versions that decide whether a cached index is still current.
//! `docs/search.md` explains each number where it is used.

use std::time::Duration;

/// The most words a passage holds, counted at Unicode word boundaries. A
/// block with more words is split first, between sentences where it can be.
pub const PASSAGE_MAX_WORDS: usize = 220;
/// How many passages hybrid search takes from BM25 before fusing.
pub const LEXICAL_CANDIDATES: usize = 50;
/// How many passages semantic and hybrid search take by cosine similarity,
/// after [`SEMANTIC_FLOOR`].
pub const SEMANTIC_CANDIDATES: usize = 50;
/// The `k` of reciprocal rank fusion: a passage at rank `r` of a list adds
/// `weight / (k + r)` to its fused score.
pub const RRF_K: f32 = 60.0;
/// The weight of the lexical list in reciprocal rank fusion.
pub const BM25_WEIGHT: f32 = 1.25;
/// The weight of the semantic list in reciprocal rank fusion.
pub const VECTOR_WEIGHT: f32 = 1.0;
/// The lowest cosine similarity a passage may have and still be a semantic
/// candidate, so that a query unrelated to the book finds nothing.
pub const SEMANTIC_FLOOR: f32 = 0.55;
/// How many of the best candidates the reranker reorders.
pub const RERANK_CANDIDATES: usize = 30;
/// Candidates the reranker reads at once. Each batch is padded to its longest
/// candidate and ONNX Runtime keeps the largest batch's memory, so all 30 at
/// once held about 0.6 GB more than batches of 4, which rank the same.
pub const RERANK_BATCH: usize = 4;
/// How many hits a search returns when `top_k` is not given.
pub const TOP_K_DEFAULT: usize = 5;
/// The most hits a search may return.
pub const TOP_K_MAX: usize = 20;
/// How many passages one page of `find` holds.
pub const FIND_PAGE: usize = 20;
/// The most characters one page of `read` holds, unless its first passage
/// alone is longer.
pub const READ_MAX_CHARS: usize = 8000;
/// The most passages `read` adds before or after the range it reads.
pub const READ_NEIGHBOURS_MAX: u32 = 10;
/// How long a process waits for another to finish building a book's index
/// before it gives up with [`Error::LockTimeout`](crate::Error::LockTimeout).
pub const LOCK_WAIT: Duration = Duration::from_secs(900);
/// How often a waiting process tries the build lock again.
pub const LOCK_POLL: Duration = Duration::from_millis(200);
/// How long the MCP server keeps a model it has not used, unless
/// `$EBOOK_HERO_MODEL_IDLE` says otherwise. The models hold about 0.6 GB, and
/// loading them again takes about a second.
pub const MODEL_IDLE: Duration = Duration::from_secs(300);
/// How many books a [`Session`](crate::session::Session) keeps open, the most
/// recently used ones.
pub const OPEN_BOOKS: usize = 4;
/// How many passages the embedding model reads at once.
pub const EMBED_BATCH: usize = 8;
/// The text put before a query when it is embedded, as the embedding model's
/// card asks for retrieval queries.
pub const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
/// The embedding model, downloaded from Hugging Face on first use.
pub const EMBEDDING_MODEL: &str = "BAAI/bge-small-en-v1.5";
/// How many numbers the embedding model gives each text.
pub const EMBEDDING_DIM: usize = 384;
/// The reranking model, downloaded from Hugging Face on first use.
pub const RERANKER_MODEL: &str = "jinaai/jina-reranker-v1-turbo-en";
/// The version of the PDF reader. A change to the reader bumps it, so that
/// cached indexes are built again.
pub const PDF_READER_VERSION: u32 = 3;
/// The version of the EPUB reader. A change to the reader bumps it, so that
/// cached indexes are built again.
pub const EPUB_READER_VERSION: u32 = 2;
/// The version of passage packing. A change to how blocks become passages
/// bumps it, so that cached indexes are built again.
pub const PASSAGES_VERSION: u32 = 2;
/// The version of the two analyzers. A change to either bumps it, so that
/// cached indexes are built again.
pub const ANALYZERS_VERSION: u32 = 2;
/// The fewest passages a book needs for a concept graph.
pub const GRAPH_MIN_BOOK_PASSAGES: usize = 20;
/// The version of the concept graph. A cached `graph.json` made by another
/// version is built again.
pub const GRAPH_VERSION: u32 = 2;
/// How many passage ids each concept and link lists unless the full graph is
/// asked for.
pub const GRAPH_PASSAGES_SHOWN: usize = 10;
