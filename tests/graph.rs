mod common;

use std::fs;
use std::path::PathBuf;

use common::private_tempdir;
use ebook_hero::graph::GraphResult;
use ebook_hero::session::Session;
use serde_json::Value;

const ALICE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alice.epub");

fn community_of(graph: &GraphResult, label: &str) -> u32 {
    let concept = graph.concepts.iter().find(|c| c.label == label);
    concept
        .unwrap_or_else(|| panic!("no concept {label:?}: {graph:#?}"))
        .community
}

#[test]
fn the_graph_of_alice_is_the_same_twice_and_groups_its_characters() {
    let (one, two) = (private_tempdir(), private_tempdir());
    let first = Session::with_cache_root(one.path().to_path_buf())
        .graph(ALICE)
        .unwrap();
    let second = Session::with_cache_root(two.path().to_path_buf())
        .graph(ALICE)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        community_of(&first, "mock turtle"),
        community_of(&first, "gryphon")
    );
    assert_eq!(
        community_of(&first, "hatter"),
        community_of(&first, "march hare")
    );
}

#[test]
fn every_link_is_backed_by_the_passages_it_names() {
    let root = private_tempdir();
    let graph = Session::with_cache_root(root.path().to_path_buf())
        .graph(ALICE)
        .unwrap();
    assert!(!graph.links.is_empty());
    for link in &graph.links {
        let (a, b) = (
            &graph.concepts[link.a as usize],
            &graph.concepts[link.b as usize],
        );
        assert!(link.a < link.b);
        for p in &link.passages {
            assert!(a.passages.contains(p) && b.passages.contains(p), "{link:?}");
        }
        let n = f64::from(graph.passages);
        let shared = link.passages.len() as f64;
        let (da, db) = (a.passages.len() as f64, b.passages.len() as f64);
        let npmi = (shared * n / (da * db)).ln() / -(shared / n).ln();
        assert!(
            (f64::from(link.npmi) - npmi).abs() < 6e-4,
            "{link:?}: {npmi}"
        );
    }
}

#[test]
fn the_cached_graph_is_read_and_a_stale_or_corrupt_one_rebuilt() {
    let root = private_tempdir();
    let fresh = || Session::with_cache_root(root.path().to_path_buf());
    let session = fresh();
    let built = session.graph(ALICE).unwrap();
    let file: PathBuf = session.book(ALICE).unwrap().dir.join("graph.json");
    let write = |version: u64, label: &str| {
        let mut cached: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        cached["graph_version"] = version.into();
        cached["result"]["concepts"][0]["label"] = label.into();
        fs::write(&file, serde_json::to_vec(&cached).unwrap()).unwrap();
    };

    write(1, "planted");
    assert_eq!(fresh().graph(ALICE).unwrap().concepts[0].label, "planted");

    write(0, "planted");
    assert_eq!(fresh().graph(ALICE).unwrap(), built);

    fs::write(&file, "not json").unwrap();
    assert_eq!(fresh().graph(ALICE).unwrap(), built);
}

#[test]
fn a_removed_cache_directory_asks_for_another_call() {
    let root = private_tempdir();
    let session = Session::with_cache_root(root.path().to_path_buf());
    let book = session.book(ALICE).unwrap();
    fs::remove_dir_all(&book.dir).unwrap();
    let error = book.graph().unwrap_err().to_string();
    assert!(error.contains("call again"), "{error}");
}
