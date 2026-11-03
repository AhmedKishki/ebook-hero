//! Hybrid search against a real book: *Alice's Adventures in Wonderland*
//! (Project Gutenberg #11, public domain). Each query describes a scene
//! without quoting it, and the passage that holds the scene must come back.

mod common;

use common::private_tempdir;
use ebook_hero::embed::Models;
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::Session;

const ALICE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alice.epub");
const TOP_K: usize = 5;

/// A query and a phrase from the passage that answers it.
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

#[test]
#[ignore = "needs the embedding and reranker models"]
fn hybrid_search_finds_the_known_passages() {
    let root = private_tempdir();
    let session = Session::with_dirs(root.path().to_path_buf(), Models::default_cache_dir());
    let book = session.book(ALICE).unwrap();

    let mut failures = Vec::new();
    for (query, phrase) in KNOWN_ITEMS {
        let expected = book
            .passages
            .iter()
            .find(|passage| passage.text.contains(phrase))
            .unwrap_or_else(|| panic!("no passage contains {phrase:?}"))
            .id;
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
        let found: Vec<u32> = result.hits.iter().map(|hit| hit.passage_id).collect();
        if !found.contains(&expected) {
            failures.push(format!(
                "{query:?}: passage {expected} (holds {phrase:?}) is not in the top {TOP_K}: {found:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
