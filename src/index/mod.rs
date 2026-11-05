//! A book's index and the cache that holds it. Stage 1 is the document, its
//! passages and their lexical index; stage 2 is the passages' embeddings,
//! made only when a semantic or hybrid search first needs them.

pub mod book;
pub mod cache;
pub mod lexical;
pub mod vectors;
