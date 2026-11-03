//! The tantivy index over passages: BM25 search and exact-phrase lookup.

use std::cmp::Reverse;
use std::fmt::Display;
use std::path::Path;

use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::columnar::{Column, ColumnValues};
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::{
    DocAddress, DocId, Index, IndexReader, ReloadPolicy, Score, Searcher, SegmentReader,
    TantivyDocument, Term,
};

use crate::analysis::{self, EXACT, STEMMED};
use crate::passages::Passage;
use crate::search::query::ParsedQuery;
use crate::{Error, Result};

const PASSAGE_ID: &str = "passage_id";
const TEXT: &str = "text";
const TEXT_EXACT: &str = "text_exact";
const SECTION: &str = "section";

const WRITER_BUDGET_BYTES: usize = 50_000_000;

/// One tantivy document per passage. Nothing is stored: a hit is identified by
/// its `passage_id` fast field and the text is read back from the passages.
///
/// - `text`: the passage text, stemmed, with positions.
/// - `text_exact`: the same text, unstemmed, with positions, for phrases.
/// - `section`: the section path joined with a space, stemmed.
pub struct LexicalIndex {
    reader: IndexReader,
    text: Field,
    text_exact: Field,
    section: Field,
}

impl LexicalIndex {
    /// Writes the index for `passages` into `dir`, which must exist and be
    /// empty, and returns once every background merge has finished.
    pub fn build(dir: &Path, passages: &[Passage]) -> Result<()> {
        let context = format!("build index in {}", dir.display());
        let fail = |e: tantivy::TantivyError| index_error(&context, e);
        let index = Index::create_in_dir(dir, schema()).map_err(fail)?;
        analysis::register(&index);
        let [id, text, text_exact, section] = fields(&index.schema())?;
        let mut writer = index
            .writer::<TantivyDocument>(WRITER_BUDGET_BYTES)
            .map_err(fail)?;
        for passage in passages {
            let mut doc = TantivyDocument::default();
            doc.add_u64(id, u64::from(passage.id));
            doc.add_text(text, &passage.text);
            doc.add_text(text_exact, &passage.text);
            doc.add_text(section, passage.section.join(" "));
            writer.add_document(doc).map_err(fail)?;
        }
        writer.commit().map_err(fail)?;
        writer.wait_merging_threads().map_err(fail)
    }

    pub fn open(dir: &Path) -> Result<LexicalIndex> {
        let context = format!("open index in {}", dir.display());
        let fail = |e: tantivy::TantivyError| index_error(&context, e);
        let index = Index::open_in_dir(dir).map_err(fail)?;
        analysis::register(&index);
        let [_, text, text_exact, section] = fields(&index.schema())?;
        // The index never changes after `build`, so it needs no reload watcher.
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(fail)?;
        Ok(LexicalIndex {
            reader,
            text,
            text_exact,
            section,
        })
    }

    /// Up to `limit` `(passage_id, score)` pairs, best first, ties by ascending id.
    ///
    /// Each word is a `Should` clause on `text` and on `section`; each phrase is
    /// a `Must` clause on `text_exact`. A query with no tokens finds nothing.
    pub fn search(&self, q: &ParsedQuery, limit: usize) -> Result<Vec<(u32, f32)>> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for word in &q.words {
            for token in analysis::tokens(STEMMED, word) {
                for field in [self.text, self.section] {
                    let term = Term::from_field_text(field, &token.text);
                    let query = TermQuery::new(term, IndexRecordOption::WithFreqs);
                    clauses.push((Occur::Should, Box::new(query)));
                }
            }
        }
        for phrase in &q.phrases {
            if let Some(query) = self.phrase_query(phrase) {
                clauses.push((Occur::Must, query));
            }
        }
        if clauses.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();
        // Breaking score ties by passage id inside the collector keeps the
        // cut at `limit` exact, whatever order the segments hold the documents in.
        let by_score_then_id = TopDocs::with_limit(limit).tweak_score(|segment: &SegmentReader| {
            let ids = segment
                .fast_fields()
                .u64(PASSAGE_ID)
                .map(|column| column.first_or_default_col(0))
                .ok();
            move |doc: DocId, score: Score| {
                let id = ids.as_ref().map_or(0, |ids| ids.get_val(doc));
                (score, Reverse(id))
            }
        });
        let top = searcher
            .search(&BooleanQuery::new(clauses), &by_score_then_id)
            .map_err(|e| index_error("search index", e))?;
        let ids = passage_ids(&searcher, top.iter().map(|(_, address)| *address))?;
        Ok(ids
            .into_iter()
            .zip(top)
            .map(|(id, ((score, _), _))| (id, score))
            .collect())
    }

    /// Ids of every passage that contains `phrase`, ascending.
    pub fn phrase_passages(&self, phrase: &str) -> Result<Vec<u32>> {
        let Some(query) = self.phrase_query(phrase) else {
            return Ok(Vec::new());
        };
        let searcher = self.reader.searcher();
        let addresses = searcher
            .search(&query, &DocSetCollector)
            .map_err(|e| index_error("search index", e))?;
        let mut ids = passage_ids(&searcher, addresses.into_iter())?;
        ids.sort_unstable();
        Ok(ids)
    }

    /// The exact-field query for `phrase`; none when it has no tokens. Tokens
    /// keep their analyzer positions, so a token the analyzer dropped (and the
    /// index therefore skipped) still counts as a position between its neighbours.
    fn phrase_query(&self, phrase: &str) -> Option<Box<dyn Query>> {
        let mut terms: Vec<(usize, Term)> = analysis::tokens(EXACT, phrase)
            .into_iter()
            .map(|t| (t.position, Term::from_field_text(self.text_exact, &t.text)))
            .collect();
        if terms.len() < 2 {
            let (_, term) = terms.pop()?;
            return Some(Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)));
        }
        let first = terms[0].0;
        for (position, _) in &mut terms {
            *position -= first;
        }
        Some(Box::new(PhraseQuery::new_with_offset(terms)))
    }
}

fn schema() -> Schema {
    let analyzed = |tokenizer: &str, record: IndexRecordOption| {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(tokenizer)
                .set_index_option(record),
        )
    };
    let mut builder = Schema::builder();
    builder.add_u64_field(PASSAGE_ID, FAST | INDEXED);
    builder.add_text_field(
        TEXT,
        analyzed(STEMMED, IndexRecordOption::WithFreqsAndPositions),
    );
    builder.add_text_field(
        TEXT_EXACT,
        analyzed(EXACT, IndexRecordOption::WithFreqsAndPositions),
    );
    builder.add_text_field(SECTION, analyzed(STEMMED, IndexRecordOption::WithFreqs));
    builder.build()
}

/// `passage_id`, `text`, `text_exact` and `section`, in that order.
fn fields(schema: &Schema) -> Result<[Field; 4]> {
    let field = |name: &str| {
        schema.get_field(name).map_err(|_| {
            Error::Index(format!(
                "the index has no {name} field; it was not built by this version"
            ))
        })
    };
    Ok([
        field(PASSAGE_ID)?,
        field(TEXT)?,
        field(TEXT_EXACT)?,
        field(SECTION)?,
    ])
}

/// The `passage_id` of each document, in the order of `addresses`.
fn passage_ids(
    searcher: &Searcher,
    addresses: impl Iterator<Item = DocAddress>,
) -> Result<Vec<u32>> {
    let columns: Vec<Column<u64>> = searcher
        .segment_readers()
        .iter()
        .map(|segment| segment.fast_fields().u64(PASSAGE_ID))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| index_error("read passage ids", e))?;
    addresses
        .map(|address| {
            let id = columns[address.segment_ord as usize].first(address.doc_id);
            id.and_then(|id| u32::try_from(id).ok()).ok_or_else(|| {
                Error::Index(format!(
                    "read passage ids: document {address:?} has no valid passage_id"
                ))
            })
        })
        .collect()
}

fn index_error(context: impl Display, source: impl Display) -> Error {
    Error::Index(format!("{context}: {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passages::PassageLocator;
    use crate::search::query::parse_query;
    use tempfile::TempDir;

    fn passage(id: u32, text: &str, section: &[&str]) -> Passage {
        Passage {
            id,
            text: text.to_string(),
            section: section.iter().map(|s| s.to_string()).collect(),
            blocks: vec![],
            locator: PassageLocator::Epub {
                href: "c.xhtml".to_string(),
                para_start: 0,
                para_end: 0,
                print_page: None,
                under_heading: None,
            },
        }
    }

    fn index_of(passages: &[Passage]) -> (TempDir, LexicalIndex) {
        let dir = TempDir::new().unwrap();
        LexicalIndex::build(dir.path(), passages).unwrap();
        let index = LexicalIndex::open(dir.path()).unwrap();
        (dir, index)
    }

    fn sample() -> (TempDir, LexicalIndex) {
        index_of(&[
            passage(0, "Entropy measures uncertainty.", &[]),
            passage(1, "Mutual information between channels.", &[]),
            passage(2, "Information is not mutual here.", &[]),
            passage(3, "Shannon proved it.", &["Coding theorem"]),
        ])
    }

    fn ids(hits: &[(u32, f32)]) -> Vec<u32> {
        hits.iter().map(|(id, _)| *id).collect()
    }

    fn words(ws: &[&str]) -> ParsedQuery {
        ParsedQuery {
            words: ws.iter().map(|w| w.to_string()).collect(),
            phrases: vec![],
        }
    }

    fn phrases(ps: &[&str]) -> ParsedQuery {
        ParsedQuery {
            words: vec![],
            phrases: ps.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn stemming_matches_inflections() {
        let (_dir, index) = sample();
        let hits = index.search(&words(&["entropies"]), 5).unwrap();
        assert_eq!(hits.first().map(|h| h.0), Some(0));
    }

    #[test]
    fn phrase_is_required_and_exact() {
        let (_dir, index) = sample();
        let hits = index.search(&phrases(&["mutual information"]), 5).unwrap();
        assert_eq!(ids(&hits), [1]);
    }

    #[test]
    fn section_words_match() {
        let (_dir, index) = sample();
        let hits = index.search(&words(&["theorem"]), 5).unwrap();
        assert!(ids(&hits).contains(&3));
    }

    #[test]
    fn hostile_input_never_errors() {
        let (_dir, index) = sample();
        assert!(index.search(&parse_query(r#"a:b (c) " \ *"#), 5).is_ok());
    }

    #[test]
    fn phrase_passages_are_ascending() {
        let (_dir, index) = sample();
        assert_eq!(index.phrase_passages("mutual information").unwrap(), [1]);
    }

    #[test]
    fn phrase_passages_lists_every_match_in_ascending_order() {
        let texts: Vec<Passage> = (0..30)
            .rev()
            .map(|id| passage(id, "see the mutual information here", &[]))
            .collect();
        let (_dir, index) = index_of(&texts);
        assert_eq!(
            index.phrase_passages("Mutual  Information").unwrap(),
            (0..30).collect::<Vec<u32>>()
        );
    }

    #[test]
    fn a_phrase_with_no_tokens_matches_nothing() {
        let (_dir, index) = sample();
        assert!(index.phrase_passages("").unwrap().is_empty());
        assert!(index.phrase_passages("...").unwrap().is_empty());
    }

    #[test]
    fn a_one_token_phrase_is_exact() {
        let (_dir, index) = sample();
        let hits = index.search(&phrases(&["entropy"]), 5).unwrap();
        assert_eq!(ids(&hits), [0]);
        assert!(
            index
                .search(&phrases(&["entropies"]), 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_query_without_tokens_returns_nothing() {
        let (_dir, index) = sample();
        assert!(index.search(&ParsedQuery::default(), 5).unwrap().is_empty());
        assert!(index.search(&parse_query("... --"), 5).unwrap().is_empty());
        assert!(index.search(&phrases(&["..."]), 5).unwrap().is_empty());
    }

    #[test]
    fn a_zero_limit_returns_nothing() {
        let (_dir, index) = sample();
        assert!(index.search(&words(&["entropy"]), 0).unwrap().is_empty());
    }

    #[test]
    fn words_add_score_to_a_phrase_match() {
        let (_dir, index) = sample();
        let q = ParsedQuery {
            words: vec!["channels".to_string()],
            phrases: vec!["mutual information".to_string()],
        };
        let hits = index.search(&q, 5).unwrap();
        assert_eq!(ids(&hits), [1]);
        let phrase_only = index.search(&phrases(&["mutual information"]), 5).unwrap();
        assert!(hits[0].1 > phrase_only[0].1);
    }

    #[test]
    fn results_are_best_first_with_ties_by_ascending_id() {
        let (_dir, index) = index_of(&[
            passage(7, "entropy", &[]),
            passage(3, "entropy", &[]),
            passage(5, "entropy entropy entropy", &[]),
        ]);
        let hits = index.search(&words(&["entropy"]), 10).unwrap();
        assert_eq!(ids(&hits), [5, 3, 7]);
        assert!(hits[0].1 > hits[1].1);
        assert_eq!(hits[1].1, hits[2].1);
        assert_eq!(ids(&index.search(&words(&["entropy"]), 2).unwrap()), [5, 3]);
    }

    #[test]
    fn the_cut_at_the_limit_keeps_the_lowest_ids_among_equal_scores() {
        // Enough documents for the writer's threads to spread them over segments.
        let passages: Vec<Passage> = (0..3000)
            .rev()
            .map(|id| passage(id, "entropy", &[]))
            .collect();
        let (_dir, index) = index_of(&passages);
        let hits = index.search(&words(&["entropy"]), 10).unwrap();
        assert_eq!(ids(&hits), (0..10).collect::<Vec<u32>>());
    }

    #[test]
    fn a_phrase_across_a_dropped_long_token_still_matches() {
        let long = "x".repeat(45);
        let text = format!("see foo {long} bar now");
        let (_dir, index) = index_of(&[passage(0, &text, &[]), passage(1, "foo bar", &[])]);
        let phrase = format!("foo {long} bar");
        assert_eq!(index.phrase_passages(&phrase).unwrap(), [0]);
        assert_eq!(index.phrase_passages("foo bar").unwrap(), [1]);
    }

    #[test]
    fn build_refuses_a_directory_that_already_holds_an_index() {
        let (dir, _index) = sample();
        let again = LexicalIndex::build(dir.path(), &[passage(0, "x", &[])]);
        assert!(matches!(again, Err(crate::Error::Index(_))));
    }

    #[test]
    fn open_fails_on_a_directory_without_an_index() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            LexicalIndex::open(dir.path()),
            Err(crate::Error::Index(_))
        ));
    }

    #[test]
    fn an_index_of_no_passages_searches_empty() {
        let (_dir, index) = index_of(&[]);
        assert!(index.search(&words(&["entropy"]), 5).unwrap().is_empty());
        assert!(index.phrase_passages("a b").unwrap().is_empty());
    }
}
