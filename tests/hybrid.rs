mod common;

use std::path::PathBuf;

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::embed::Models;
use ebook_hero::search::fusion::rrf;
use ebook_hero::search::query::parse_query;
use ebook_hero::search::{Hit, Mode, SearchParams, lexical_hits, semantic_hits};
use ebook_hero::session::Session;
use ebook_hero::settings::{
    BM25_WEIGHT, LEXICAL_CANDIDATES, RRF_K, SEMANTIC_CANDIDATES, TOP_K_MAX, VECTOR_WEIGHT,
};
use tempfile::TempDir;

/// An EPUB with one chapter per entry of `paragraphs`, each under its own
/// heading, so that entry `i` is passage `i`.
struct Fixture {
    _root: TempDir,
    _books: TempDir,
    session: Session,
    path: String,
}

impl Fixture {
    /// A fixture whose session looks for models in `models_dir`.
    fn new(paragraphs: &[&str], models_dir: PathBuf) -> Fixture {
        let root = private_tempdir();
        let books = TempDir::new().unwrap();
        let path = books.path().join("book.epub");
        let mut builder = EpubBuilder::new(EpubVersion::V3).title("Hybrid Book");
        for (i, text) in paragraphs.iter().enumerate() {
            let href = format!("ch{i}.xhtml");
            builder = builder
                .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
                .toc(&format!("Part {i}"), &href, 1);
        }
        builder.write(&path);
        Fixture {
            session: Session::with_dirs(root.path().to_path_buf(), models_dir),
            path: path.to_str().unwrap().to_string(),
            _root: root,
            _books: books,
        }
    }

    fn search(
        &self,
        query: &str,
        mode: Mode,
        top_k: usize,
        rerank: bool,
    ) -> ebook_hero::Result<ebook_hero::search::SearchResult> {
        self.session.search(
            &self.path,
            SearchParams {
                query: query.to_string(),
                mode,
                top_k,
                rerank,
            },
            &|_, _| {},
        )
    }
}

/// A models directory that is a regular file, so that no model can be loaded.
fn unusable_models() -> (tempfile::NamedTempFile, PathBuf) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_path_buf();
    (file, path)
}

const ENTROPY: [&str; 4] = [
    "A channel has a capacity measured in bits per second.",
    "Entropy, entropy and more entropy: the entropy of a source.",
    "Shannon defined entropy as the average surprise of a message.",
    "Entropy also appears in thermodynamics.",
];

fn ids(hits: &[Hit]) -> Vec<u32> {
    hits.iter().map(|hit| hit.passage_id).collect()
}

fn invalid(result: ebook_hero::Result<ebook_hero::search::SearchResult>) -> String {
    match result {
        Err(error @ Error::InvalidArgument(_)) => error.to_string(),
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn top_k_bounds_are_enforced() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    for mode in [Mode::Lexical, Mode::Semantic, Mode::Hybrid] {
        for top_k in [0, TOP_K_MAX + 1] {
            let message = invalid(fixture.search("entropy", mode, top_k, false));
            assert!(message.contains("top_k"), "{message}");
            assert!(message.contains(&TOP_K_MAX.to_string()), "{message}");
        }
    }
    // Both ends of the range are allowed.
    for top_k in [1, TOP_K_MAX] {
        let result = fixture
            .search("entropy", Mode::Lexical, top_k, false)
            .unwrap();
        assert!(result.hits.len() <= top_k);
    }
}

#[test]
fn blank_query_is_invalid() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    for mode in [Mode::Lexical, Mode::Semantic, Mode::Hybrid] {
        for query in ["", "  ", " \t\n "] {
            let message = invalid(fixture.search(query, mode, 5, false));
            assert!(message.contains("query"), "{message}");
        }
    }
}

#[test]
fn arguments_are_checked_before_the_book_is_opened() {
    let root = private_tempdir();
    let session = Session::with_dirs(root.path().to_path_buf(), PathBuf::from("/nonexistent"));
    let params = |query: &str, top_k: usize| SearchParams {
        query: query.to_string(),
        mode: Mode::Lexical,
        top_k,
        rerank: false,
    };
    for bad in [params("entropy", 0), params(" ", 5)] {
        match session.search("/nonexistent/book.epub", bad, &|_, _| {}) {
            Err(Error::InvalidArgument(_)) => {}
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }
    // With valid arguments the missing book is what is reported.
    match session.search("/nonexistent/book.epub", params("entropy", 5), &|_, _| {}) {
        Err(Error::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn punctuation_only_query_returns_no_hits() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    let result = fixture.search("?!", Mode::Lexical, 5, false).unwrap();
    assert!(result.hits.is_empty(), "{:?}", result.hits);
    assert_eq!(result.query, "?!");
    assert_eq!(result.mode, Mode::Lexical);
}

#[test]
fn lexical_mode_never_loads_models() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    // `rerank` is ignored in lexical mode, so it needs no reranker either.
    for rerank in [false, true] {
        let result = fixture
            .search("  entropy  ", Mode::Lexical, 2, rerank)
            .unwrap();
        assert_eq!(result.query, "entropy", "the query is trimmed");
        assert_eq!(result.mode, Mode::Lexical);
        assert_eq!(result.hits.len(), 2);
        let book = fixture.session.book(&fixture.path).unwrap();
        let direct = lexical_hits(&book, &parse_query("entropy"), 2).unwrap();
        assert_eq!(
            result.hits, direct,
            "lexical search is lexical_hits as it is"
        );
    }
}

#[test]
fn lexical_mode_returns_at_most_top_k() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    let all = fixture.search("entropy", Mode::Lexical, 10, false).unwrap();
    assert_eq!(all.hits.len(), 3);
    let two = fixture.search("entropy", Mode::Lexical, 2, false).unwrap();
    assert_eq!(ids(&two.hits), ids(&all.hits[..2]));
}

#[test]
fn the_other_modes_name_the_lexical_remedy_without_models() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&ENTROPY, unusable);
    for mode in [Mode::Semantic, Mode::Hybrid] {
        let error = fixture.search("entropy", mode, 5, false).unwrap_err();
        assert!(matches!(error, Error::ModelUnavailable { .. }), "{error}");
        assert!(error.to_string().contains("mode=lexical"), "{error}");
    }
}

const HARBOUR: [&str; 5] = [
    "The council voted to widen the harbour road and to levy a new toll on freight carts.",
    "There was a bottle labelled drink me.",
    "Tides rise and fall twice a day because of the gravitational pull of the moon.",
    "The recipe calls for two cups of flour, a pinch of salt and three eggs.",
    "Interest rates on long-term bonds moved little after the central bank met.",
];

fn models_fixture(paragraphs: &[&str]) -> Fixture {
    Fixture::new(paragraphs, Models::default_cache_dir())
}

#[test]
#[ignore = "needs the embedding model"]
fn hybrid_hits_carry_both_ranks() {
    let fixture = models_fixture(&HARBOUR);
    let result = fixture
        .search("tides and the moon", Mode::Hybrid, 5, false)
        .unwrap();
    assert_eq!(result.mode, Mode::Hybrid);
    assert_eq!(result.query, "tides and the moon");
    let first = &result.hits[0];
    assert_eq!(first.passage_id, 2, "{:?}", result.hits);
    assert_eq!(first.lexical_rank, Some(1));
    assert_eq!(first.semantic_rank, Some(1));
    assert!(!first.matches.is_empty(), "the query words are highlighted");

    for hit in &result.hits {
        assert!(hit.lexical_rank.is_some() || hit.semantic_rank.is_some());
        let score = hit
            .lexical_rank
            .map_or(0.0, |r| BM25_WEIGHT / (RRF_K + r as f32))
            + hit
                .semantic_rank
                .map_or(0.0, |r| VECTOR_WEIGHT / (RRF_K + r as f32));
        assert!((hit.score - score).abs() < 1e-6, "{hit:?}");
    }
    assert!(
        result
            .hits
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score)
    );
}

#[test]
#[ignore = "needs the embedding model"]
fn rerank_false_keeps_fused_order() {
    let fixture = models_fixture(&HARBOUR);
    let query = "a drink with a label";
    let result = fixture.search(query, Mode::Hybrid, 5, false).unwrap();

    let book = fixture.session.book(&fixture.path).unwrap();
    let lexical = lexical_hits(&book, &parse_query(query), LEXICAL_CANDIDATES).unwrap();
    let semantic = semantic_hits(
        &book,
        fixture.session.models(),
        query,
        SEMANTIC_CANDIDATES,
        &|_, _| {},
    )
    .unwrap();
    let lexical_ids = ids(&lexical);
    let semantic_ids = ids(&semantic);
    let fused = rrf(
        &[(&lexical_ids, BM25_WEIGHT), (&semantic_ids, VECTOR_WEIGHT)],
        RRF_K,
    );
    assert!(!fused.is_empty());
    let expected: Vec<(u32, f32)> = fused.into_iter().take(5).collect();
    let actual: Vec<(u32, f32)> = result
        .hits
        .iter()
        .map(|h| (h.passage_id, h.score))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(result.hits[0].passage_id, 1);
}

#[test]
#[ignore = "needs the embedding and reranker models"]
fn rerank_orders_hits_by_reranker_score() {
    let fixture = models_fixture(&HARBOUR);
    let query = "tides and the moon";
    let plain = fixture.search(query, Mode::Hybrid, 5, false).unwrap();
    let reranked = fixture.search(query, Mode::Hybrid, 5, true).unwrap();

    let mut plain_ids = ids(&plain.hits);
    let mut reranked_ids = ids(&reranked.hits);
    plain_ids.sort_unstable();
    reranked_ids.sort_unstable();
    assert!(plain_ids.len() > 1, "{:?}", plain.hits);
    assert_eq!(reranked_ids, plain_ids, "the same passages, reordered");
    assert_eq!(reranked.hits[0].passage_id, 2, "{:?}", reranked.hits);
    assert!(
        reranked
            .hits
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score)
    );
    for hit in &reranked.hits {
        let before = plain
            .hits
            .iter()
            .find(|p| p.passage_id == hit.passage_id)
            .unwrap();
        assert_ne!(hit.score, before.score, "the reranker's score replaces it");
        assert_eq!(hit.lexical_rank, before.lexical_rank);
        assert_eq!(hit.semantic_rank, before.semantic_rank);
        assert_eq!(hit.matches, before.matches);
    }

    let top_two = fixture.search(query, Mode::Hybrid, 2, true).unwrap();
    assert_eq!(ids(&top_two.hits), ids(&reranked.hits[..2]));
}

#[test]
#[ignore = "needs the embedding and reranker models"]
fn semantic_mode_reranks_its_hits_too() {
    let fixture = models_fixture(&HARBOUR);
    let query = "gravitational pull of the moon on the tides";
    let plain = fixture.search(query, Mode::Semantic, 5, false).unwrap();
    let reranked = fixture.search(query, Mode::Semantic, 5, true).unwrap();
    assert_eq!(ids(&plain.hits), [2], "{:?}", plain.hits);
    assert_eq!(ids(&reranked.hits), [2]);
    assert_ne!(reranked.hits[0].score, plain.hits[0].score);
    assert_eq!(reranked.hits[0].semantic_rank, Some(1));
}

#[test]
#[ignore = "needs the embedding model"]
fn unrelated_query_returns_nothing_in_semantic_mode() {
    let fixture = models_fixture(&HARBOUR);
    let result = fixture
        .search("quantum chromodynamics lattice", Mode::Semantic, 5, true)
        .unwrap();
    assert!(result.hits.is_empty(), "{:?}", result.hits);
    assert_eq!(result.mode, Mode::Semantic);
}
