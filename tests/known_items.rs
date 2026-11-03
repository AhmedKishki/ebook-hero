//! Hybrid search against a real book: *Alice's Adventures in Wonderland*
//! (Project Gutenberg #11, public domain). Each query describes a scene
//! without quoting it, and a passage that holds the scene must come back.

mod common;

use common::private_tempdir;
use ebook_hero::embed::Models;
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::Session;

const ALICE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alice.epub");
const TOP_K: usize = 5;
const MIN_FOUND: usize = 4;

/// A query and a phrase from a passage that answers it.
const KNOWN_ITEMS: [(&str, &str); 5] = [
    ("a bottle with a label telling her to drink it", "DRINK ME"),
    (
        "a cat that vanishes until only its smile remains",
        "grin without a cat",
    ),
    (
        "the queen orders everyone to be beheaded",
        "Off with her head",
    ),
    (
        "a tea party where it is always the same time",
        "always six o",
    ),
    (
        "the knave accused of stealing pastries",
        "stole those tarts",
    ),
];

/// A query is found when any passage that holds its phrase is among the top
/// `TOP_K` hits: the phrase may occur in several passages, and each is a
/// correct answer. The check holds if at least `MIN_FOUND` of the queries are
/// found.
#[test]
#[ignore = "needs the embedding and reranker models"]
fn hybrid_search_finds_the_known_passages() {
    let root = private_tempdir();
    let session = Session::with_dirs(root.path().to_path_buf(), Models::default_cache_dir());
    let book = session.book(ALICE).unwrap();

    let mut found = 0;
    let mut report = Vec::new();
    for (query, phrase) in KNOWN_ITEMS {
        let holders: Vec<u32> = book
            .passages
            .iter()
            .filter(|passage| passage.text.contains(phrase))
            .map(|passage| passage.id)
            .collect();
        assert!(!holders.is_empty(), "no passage contains {phrase:?}");
        let result = session
            .search(
                ALICE,
                SearchParams {
                    query: query.to_string(),
                    mode: Mode::Hybrid,
                    top_k: TOP_K,
                    rerank: true,
                },
                &|_, _| {},
            )
            .unwrap();
        let rank = result
            .hits
            .iter()
            .position(|hit| holders.contains(&hit.passage_id));
        match rank {
            Some(index) => {
                found += 1;
                report.push(format!("{query:?}: rank {}", index + 1));
            }
            None => report.push(format!("{query:?}: not in top {TOP_K}")),
        }
    }
    assert!(
        found >= MIN_FOUND,
        "{found} of {} queries found, at least {MIN_FOUND} needed:\n{}",
        KNOWN_ITEMS.len(),
        report.join("\n")
    );
}

/// `find` needs no model, so this test is not ignored.
#[test]
fn find_counts_a_repeated_phrase() {
    let root = private_tempdir();
    let session = Session::with_cache_root(root.path().to_path_buf());
    let found = session.find(ALICE, "Off with her head", None).unwrap();
    assert!(
        found.total_occurrences >= 2,
        "{} occurrences",
        found.total_occurrences
    );
}
