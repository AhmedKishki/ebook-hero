//! Search over one book: the query syntax, match highlighting, lexical hits
//! and `find`.

pub mod find;
pub mod highlight;
pub mod query;

use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::embed::Models;
use crate::index::book::Book;
use crate::passages::{Passage, PassageLocator};
use crate::search::highlight::{match_ranges, to_char_ranges};
use crate::search::query::{ParsedQuery, parse_query};
use crate::settings::SEMANTIC_FLOOR;
use crate::{Error, Result};

/// How `search` finds passages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Lexical and semantic results fused.
    #[default]
    Hybrid,
    /// Stemmed word and exact phrase matching only.
    Lexical,
    /// Embedding similarity only.
    Semantic,
}

impl FromStr for Mode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Mode> {
        match s.to_lowercase().as_str() {
            "hybrid" => Ok(Mode::Hybrid),
            "lexical" => Ok(Mode::Lexical),
            "semantic" => Ok(Mode::Semantic),
            _ => Err(Error::InvalidArgument(format!(
                "unknown mode {s:?}; expected hybrid, lexical or semantic"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchParams {
    pub query: String,
    pub mode: Mode,
    pub top_k: usize,
    pub rerank: bool,
}

/// One passage returned by a search.
///
/// `matches` are `[start, end]` Unicode code-point ranges of `text`. The ranks
/// are 1-based positions in the lexical and semantic result lists, or `None`
/// when the passage was not in that list.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Hit {
    pub passage_id: u32,
    pub text: String,
    pub section: Vec<String>,
    pub locator: PassageLocator,
    pub matches: Vec<[usize; 2]>,
    pub score: f32,
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SearchResult {
    pub query: String,
    pub mode: Mode,
    pub hits: Vec<Hit>,
}

/// The best `limit` passages for `q` by BM25, best first, with `lexical_rank`
/// counting from 1 and `score` the BM25 score.
pub fn lexical_hits(book: &Book, q: &ParsedQuery, limit: usize) -> Result<Vec<Hit>> {
    let found = book.lexical.search(q, limit)?;
    let mut hits = Vec::with_capacity(found.len());
    for (rank, (id, score)) in (1..).zip(found) {
        let passage = passage(book, id)?;
        let ranges = match_ranges(&passage.text, q);
        hits.push(Hit {
            passage_id: id,
            text: passage.text.clone(),
            section: passage.section.clone(),
            locator: passage.locator.clone(),
            matches: to_char_ranges(&passage.text, &ranges),
            score,
            lexical_rank: Some(rank),
            semantic_rank: None,
        });
    }
    Ok(hits)
}

/// The `limit` passages most similar in meaning to `query`, best first, with
/// `semantic_rank` counting from 1 and `score` the cosine similarity. Passages
/// below `SEMANTIC_FLOOR` are left out, so there may be fewer, or none.
///
/// The first call for a book embeds all its passages, which can take minutes;
/// `progress(done, total)` reports it. `matches` highlight the query's words
/// the way lexical hits do.
pub fn semantic_hits(
    book: &Book,
    models: &Models,
    query: &str,
    limit: usize,
    progress: &dyn Fn(usize, usize),
) -> Result<Vec<Hit>> {
    let vectors = book.vectors(models, progress)?;
    let embedded = models.embed_query(query)?;
    let q = parse_query(query);
    let mut hits = Vec::new();
    for (rank, (id, score)) in (1..).zip(vectors.nearest(&embedded, limit, SEMANTIC_FLOOR)) {
        let passage = passage(book, id)?;
        let ranges = match_ranges(&passage.text, &q);
        hits.push(Hit {
            passage_id: id,
            text: passage.text.clone(),
            section: passage.section.clone(),
            locator: passage.locator.clone(),
            matches: to_char_ranges(&passage.text, &ranges),
            score,
            lexical_rank: None,
            semantic_rank: Some(rank),
        });
    }
    Ok(hits)
}

/// The passage with this id. Passage ids are positions in `book.passages`, so a
/// missing one means the index and the passages do not belong together.
fn passage(book: &Book, id: u32) -> Result<&Passage> {
    book.passages.get(id as usize).ok_or_else(|| {
        Error::Index(format!(
            "passage {id} is in the index but not in the book; the cache is inconsistent"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_defaults_to_hybrid() {
        assert_eq!(Mode::default(), Mode::Hybrid);
    }

    #[test]
    fn mode_parses_case_insensitively() {
        assert_eq!("hybrid".parse::<Mode>().unwrap(), Mode::Hybrid);
        assert_eq!("Lexical".parse::<Mode>().unwrap(), Mode::Lexical);
        assert_eq!("SEMANTIC".parse::<Mode>().unwrap(), Mode::Semantic);
    }

    #[test]
    fn an_unknown_mode_lists_the_three() {
        let error = "fuzzy".parse::<Mode>().unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
        let message = error.to_string();
        for name in ["hybrid", "lexical", "semantic"] {
            assert!(message.contains(name), "{message}");
        }
        assert!(message.contains("fuzzy"), "{message}");
    }

    #[test]
    fn mode_serializes_in_lowercase() {
        assert_eq!(serde_json::to_string(&Mode::Hybrid).unwrap(), r#""hybrid""#);
        assert_eq!(
            serde_json::from_str::<Mode>(r#""semantic""#).unwrap(),
            Mode::Semantic
        );
    }
}
