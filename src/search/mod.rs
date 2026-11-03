//! Search over one book: the query syntax, match highlighting, lexical and
//! semantic hits, their fusion and reranking, and `find`.

pub mod find;
pub mod fusion;
pub mod highlight;
pub mod query;

use std::collections::HashMap;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::embed::Models;
use crate::index::book::Book;
use crate::passages::{Passage, PassageLocator};
use crate::search::fusion::rrf;
use crate::search::highlight::{match_ranges, to_char_ranges};
use crate::search::query::{ParsedQuery, parse_query};
use crate::settings::{
    BM25_WEIGHT, LEXICAL_CANDIDATES, RERANK_CANDIDATES, RRF_K, SEMANTIC_CANDIDATES, SEMANTIC_FLOOR,
    TOP_K_MAX, VECTOR_WEIGHT,
};
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
///
/// A book with no passages, or a `limit` of 0, has nothing to rank, so it is
/// answered without embedding anything.
pub fn semantic_hits(
    book: &Book,
    models: &Models,
    query: &str,
    limit: usize,
    progress: &dyn Fn(usize, usize),
) -> Result<Vec<Hit>> {
    if limit == 0 || book.passages.is_empty() {
        return Ok(Vec::new());
    }
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

/// Searches `book` as `params` say: validates them, finds the passages in the
/// requested mode, reranks them if asked to, and returns the best `top_k`.
///
/// `Lexical` never loads a model and ignores `rerank`. The other modes may
/// embed the whole book on first use, which can take minutes;
/// `progress(done, total)` reports it. `score` is the number the hits are
/// ordered by: BM25 for `Lexical`, cosine similarity for `Semantic`, the fused
/// score for `Hybrid`, and the reranker's score when `rerank` applies.
pub fn search(
    book: &Book,
    models: &Models,
    params: &SearchParams,
    progress: &dyn Fn(usize, usize),
) -> Result<SearchResult> {
    let query = validate(params)?;
    let mut hits = match params.mode {
        Mode::Lexical => lexical_hits(book, &parse_query(query), params.top_k)?,
        Mode::Semantic => semantic_hits(book, models, query, SEMANTIC_CANDIDATES, progress)?,
        Mode::Hybrid => {
            let lexical = lexical_hits(book, &parse_query(query), LEXICAL_CANDIDATES)?;
            let semantic = semantic_hits(book, models, query, SEMANTIC_CANDIDATES, progress)?;
            fuse(lexical, semantic)
        }
    };
    if params.rerank && params.mode != Mode::Lexical {
        hits = rerank_hits(hits, |docs| models.rerank(query, docs))?;
    }
    hits.truncate(params.top_k);
    Ok(SearchResult {
        query: query.to_string(),
        mode: params.mode,
        hits,
    })
}

/// Checks `params` and returns its query without the surrounding whitespace.
pub(crate) fn validate(params: &SearchParams) -> Result<&str> {
    if !(1..=TOP_K_MAX).contains(&params.top_k) {
        return Err(Error::InvalidArgument(format!(
            "top_k must be between 1 and {TOP_K_MAX}, not {}",
            params.top_k
        )));
    }
    let query = params.query.trim();
    if query.is_empty() {
        return Err(Error::InvalidArgument(
            "query must not be empty".to_string(),
        ));
    }
    Ok(query)
}

/// The lexical and semantic hits as one list, best first, each hit keeping
/// the ranks it had in the lists that hold it and scored by its fused score.
fn fuse(lexical: Vec<Hit>, semantic: Vec<Hit>) -> Vec<Hit> {
    let lexical_ids: Vec<u32> = lexical.iter().map(|hit| hit.passage_id).collect();
    let semantic_ids: Vec<u32> = semantic.iter().map(|hit| hit.passage_id).collect();
    let fused = rrf(
        &[(&lexical_ids, BM25_WEIGHT), (&semantic_ids, VECTOR_WEIGHT)],
        RRF_K,
    );
    // Both lists highlight the same query, so a hit in both has the same
    // `matches` whichever list it is taken from.
    let mut by_id: HashMap<u32, Hit> = lexical
        .into_iter()
        .map(|hit| (hit.passage_id, hit))
        .collect();
    for hit in semantic {
        by_id
            .entry(hit.passage_id)
            .and_modify(|found| found.semantic_rank = hit.semantic_rank)
            .or_insert(hit);
    }
    fused
        .into_iter()
        .filter_map(|(id, score)| {
            let mut hit = by_id.remove(&id)?;
            hit.score = score;
            Some(hit)
        })
        .collect()
}

/// Reorders the first `RERANK_CANDIDATES` of `hits` by the score `score`
/// gives their texts, highest first and lowest passage id first among equals,
/// and makes that score the hit's `score`. Later hits keep their order after
/// them.
fn rerank_hits(
    mut hits: Vec<Hit>,
    score: impl FnOnce(&[&str]) -> Result<Vec<f32>>,
) -> Result<Vec<Hit>> {
    let tail = hits.split_off(hits.len().min(RERANK_CANDIDATES));
    let scores = {
        let docs: Vec<&str> = hits.iter().map(|hit| hit.text.as_str()).collect();
        score(&docs)?
    };
    for (hit, score) in hits.iter_mut().zip(scores) {
        hit.score = score;
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.passage_id.cmp(&b.passage_id))
    });
    hits.extend(tail);
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

    fn hit(id: u32, score: f32, lexical_rank: Option<u32>, semantic_rank: Option<u32>) -> Hit {
        Hit {
            passage_id: id,
            text: format!("passage {id}"),
            section: Vec::new(),
            locator: PassageLocator::Epub {
                href: "ch.xhtml".to_string(),
                para_start: 0,
                para_end: 0,
                print_page: None,
            },
            matches: Vec::new(),
            score,
            lexical_rank,
            semantic_rank,
        }
    }

    fn ids(hits: &[Hit]) -> Vec<u32> {
        hits.iter().map(|hit| hit.passage_id).collect()
    }

    #[test]
    fn a_fused_hit_keeps_the_rank_it_had_in_each_list() {
        let lexical = vec![
            hit(10, 9.0, Some(1), None),
            hit(20, 5.0, Some(2), None),
            hit(30, 1.0, Some(3), None),
        ];
        let semantic = vec![hit(20, 0.9, None, Some(1)), hit(40, 0.8, None, Some(2))];
        let fused = fuse(lexical, semantic);

        assert_eq!(ids(&fused), [20, 10, 30, 40]);
        let ranks: Vec<_> = fused
            .iter()
            .map(|hit| (hit.lexical_rank, hit.semantic_rank))
            .collect();
        assert_eq!(
            ranks,
            [
                (Some(2), Some(1)),
                (Some(1), None),
                (Some(3), None),
                (None, Some(2)),
            ]
        );
        let expected = 1.25 / 62.0 + 1.0 / 61.0;
        assert!(
            (fused[0].score - expected).abs() < 1e-6,
            "{}",
            fused[0].score
        );
        assert!((fused[3].score - 1.0 / 62.0).abs() < 1e-6);
    }

    #[test]
    fn fusing_with_an_empty_list_keeps_the_other_in_order() {
        let semantic = vec![hit(5, 0.9, None, Some(1)), hit(3, 0.8, None, Some(2))];
        let fused = fuse(Vec::new(), semantic);
        assert_eq!(ids(&fused), [5, 3]);
        assert_eq!(fused[0].lexical_rank, None);
        assert!(fuse(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn reranked_hits_follow_the_reranker_and_take_its_scores() {
        let hits = vec![
            hit(1, 0.9, None, Some(1)),
            hit(2, 0.8, None, Some(2)),
            hit(3, 0.7, None, Some(3)),
        ];
        let reranked = rerank_hits(hits, |docs| {
            assert_eq!(docs, ["passage 1", "passage 2", "passage 3"]);
            Ok(vec![-1.0, 4.0, 2.5])
        })
        .unwrap();
        assert_eq!(ids(&reranked), [2, 3, 1]);
        let scores: Vec<f32> = reranked.iter().map(|hit| hit.score).collect();
        assert_eq!(scores, [4.0, 2.5, -1.0]);
        // The ranks from the first stage are not rewritten.
        assert_eq!(reranked[0].semantic_rank, Some(2));
    }

    #[test]
    fn reranker_ties_break_by_passage_id() {
        let hits = vec![
            hit(9, 0.9, None, Some(1)),
            hit(4, 0.8, None, Some(2)),
            hit(6, 0.7, None, Some(3)),
        ];
        let reranked = rerank_hits(hits, |_| Ok(vec![1.0, 1.0, 1.0])).unwrap();
        assert_eq!(ids(&reranked), [4, 6, 9]);
    }

    #[test]
    fn only_the_first_candidates_are_reranked_and_the_rest_follow_in_order() {
        let total = RERANK_CANDIDATES + 5;
        let hits: Vec<Hit> = (0..total as u32)
            .map(|id| hit(id, 1.0 - id as f32 / 100.0, None, Some(id + 1)))
            .collect();
        let reranked = rerank_hits(hits, |docs| {
            assert_eq!(docs.len(), RERANK_CANDIDATES);
            // The reranker prefers the later passages of the block.
            Ok((0..docs.len()).map(|i| i as f32).collect())
        })
        .unwrap();

        let block = RERANK_CANDIDATES as u32;
        let expected: Vec<u32> = (0..block).rev().chain(block..total as u32).collect();
        assert_eq!(ids(&reranked), expected);
        assert_eq!(reranked[0].score, (RERANK_CANDIDATES - 1) as f32);
        // Hits past the block keep the score they came with.
        let last = reranked.last().unwrap();
        assert_eq!(last.score, 1.0 - (total as f32 - 1.0) / 100.0);
    }

    #[test]
    fn nothing_to_rerank_is_fine() {
        let reranked = rerank_hits(Vec::new(), |docs| {
            assert!(docs.is_empty());
            Ok(Vec::new())
        })
        .unwrap();
        assert!(reranked.is_empty());
    }

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
