mod common;

use std::sync::Arc;

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::index::book::Book;
use ebook_hero::passages::PassageLocator;
use ebook_hero::search::find::FindHit;
use ebook_hero::search::query::parse_query;
use ebook_hero::search::{Hit, lexical_hits};
use ebook_hero::session::Session;
use ebook_hero::settings::FIND_PAGE;
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
    fn new(paragraphs: &[&str]) -> Fixture {
        let root = private_tempdir();
        let books = TempDir::new().unwrap();
        let path = books.path().join("book.epub");
        let mut builder = EpubBuilder::new(EpubVersion::V3).title("Search Book");
        for (i, text) in paragraphs.iter().enumerate() {
            let href = format!("ch{i}.xhtml");
            builder = builder
                .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
                .toc(&format!("Part {i}"), &href, 1);
        }
        builder.write(&path);
        Fixture {
            session: Session::with_cache_root(root.path().to_path_buf()),
            path: path.to_str().unwrap().to_string(),
            _root: root,
            _books: books,
        }
    }

    fn book(&self) -> Arc<Book> {
        self.session.book(&self.path).unwrap()
    }
}

fn ids(occurrences: &[FindHit]) -> Vec<u32> {
    occurrences.iter().map(|hit| hit.passage_id).collect()
}

/// The text of `hit` that its first `matches` entry covers, counted in code points.
fn highlighted(hit: &Hit, range: [usize; 2]) -> String {
    hit.text
        .chars()
        .skip(range[0])
        .take(range[1] - range[0])
        .collect()
}

const STOPWORD_BOOK: [&str; 3] = [
    "How the river runs to the sea is a story of its own.",
    "Configure the server before you start it.",
    "A note on how it was done, and why, and by whom.",
];

#[test]
fn a_query_of_stopwords_only_finds_nothing() {
    let fixture = Fixture::new(&STOPWORD_BOOK);
    let book = fixture.book();

    let hits = lexical_hits(&book, &parse_query("the of and to a it is"), 10).unwrap();

    assert!(
        hits.is_empty(),
        "{:?}",
        hits.iter().map(|h| h.passage_id).collect::<Vec<_>>()
    );
}

#[test]
fn a_question_matches_only_on_its_content_words() {
    let fixture = Fixture::new(&STOPWORD_BOOK);
    let book = fixture.book();

    let hits = lexical_hits(&book, &parse_query("how to configure"), 10).unwrap();

    assert_eq!(hits.iter().map(|h| h.passage_id).collect::<Vec<_>>(), [1]);
    let words: Vec<String> = hits[0]
        .matches
        .iter()
        .map(|range| highlighted(&hits[0], *range))
        .collect();
    assert_eq!(words, ["Configure"]);
}

#[test]
fn stopwords_are_not_highlighted() {
    let fixture = Fixture::new(&STOPWORD_BOOK);
    let book = fixture.book();

    let hits = lexical_hits(&book, &parse_query("the river and the sea"), 10).unwrap();

    assert_eq!(hits.iter().map(|h| h.passage_id).collect::<Vec<_>>(), [0]);
    let words: Vec<String> = hits[0]
        .matches
        .iter()
        .map(|range| highlighted(&hits[0], *range))
        .collect();
    assert_eq!(words, ["river", "sea"]);
}

#[test]
fn a_quoted_phrase_keeps_its_stopwords() {
    let fixture = Fixture::new(&STOPWORD_BOOK);
    let book = fixture.book();

    let hits = lexical_hits(&book, &parse_query(r#""runs to the sea""#), 10).unwrap();
    assert_eq!(hits.iter().map(|h| h.passage_id).collect::<Vec<_>>(), [0]);
    assert_eq!(highlighted(&hits[0], hits[0].matches[0]), "runs to the sea");

    let none = lexical_hits(&book, &parse_query(r#""runs the sea""#), 10).unwrap();
    assert!(none.is_empty());

    let found = fixture
        .session
        .find(&fixture.path, "how it was", None)
        .unwrap();
    assert_eq!(ids(&found.occurrences), [2]);
}

#[test]
fn lexical_hits_are_ranked_and_located() {
    let fixture = Fixture::new(&[
        "A channel has a capacity measured in bits per second.",
        "Entropy, entropy and more entropy: the entropy of a source.",
        "Shannon defined entropy as the average surprise of a message.",
    ]);
    let book = fixture.book();
    let hits = lexical_hits(&book, &parse_query("entropy"), 10).unwrap();

    assert_eq!(
        hits.iter().map(|h| h.passage_id).collect::<Vec<_>>(),
        [1, 2]
    );
    let first = &hits[0];
    assert!(first.text.contains("ntropy"), "{}", first.text);
    assert!(!first.matches.is_empty());
    assert_eq!(first.lexical_rank, Some(1));
    assert_eq!(first.semantic_rank, None);
    assert_eq!(first.section, ["Part 1"]);
    match &first.locator {
        PassageLocator::Epub { href, .. } => assert_eq!(href, "ch1.xhtml"),
        other => panic!("expected an EPUB locator, got {other:?}"),
    }
    for range in &first.matches {
        assert!(highlighted(first, *range).eq_ignore_ascii_case("entropy"));
    }
    assert_eq!(first.matches.len(), 4);

    assert_eq!(hits[1].lexical_rank, Some(2));
    assert_eq!(hits[1].semantic_rank, None);
    assert!(first.score >= hits[1].score && hits[1].score > 0.0);
}

#[test]
fn lexical_hits_respect_the_limit_and_report_nothing_for_no_match() {
    let fixture = Fixture::new(&["Entropy here.", "More entropy there.", "Nothing relevant."]);
    let book = fixture.book();
    let one = lexical_hits(&book, &parse_query("entropy"), 1).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].lexical_rank, Some(1));
    assert!(
        lexical_hits(&book, &parse_query("zebra"), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        lexical_hits(&book, &parse_query(""), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn hit_ranges_count_code_points_not_bytes() {
    let fixture = Fixture::new(&["Café société: entropy rises."]);
    let book = fixture.book();
    let hits = lexical_hits(&book, &parse_query("entropy"), 10).unwrap();
    assert_eq!(hits[0].matches, [[14, 21]]);
    assert_eq!(highlighted(&hits[0], [14, 21]), "entropy");
}

#[test]
fn hits_serialize_with_the_documented_fields() {
    let fixture = Fixture::new(&["Entropy is a measure."]);
    let book = fixture.book();
    let hits = lexical_hits(&book, &parse_query("entropy"), 10).unwrap();
    let json = serde_json::to_value(&hits[0]).unwrap();
    let object = json.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "lexical_rank",
            "locator",
            "matches",
            "passage_id",
            "score",
            "section",
            "semantic_rank",
            "text"
        ]
    );
    assert_eq!(object["lexical_rank"], 1);
    assert!(object["semantic_rank"].is_null());
    assert_eq!(object["locator"]["format"], "epub");
    assert_eq!(object["matches"], serde_json::json!([[0, 7]]));
}

#[test]
fn find_lists_passages_in_reading_order() {
    let fixture = Fixture::new(&[
        "The mutual information is symmetric.",
        "Nothing here.",
        "Mutual and information, but never adjacent.",
        "Mutual information appears here.",
        "Information mutual is the wrong order.",
        "A last note: MUTUAL   INFORMATION, in capitals. Later, mutual information is back.",
    ]);
    // Passages 0, 3 and 5 hold the phrase; 5 holds it twice.
    let result = fixture
        .session
        .find(&fixture.path, "mutual information", None)
        .unwrap();
    assert_eq!(result.phrase, "mutual information");
    assert_eq!(ids(&result.occurrences), [0, 3, 5]);
    assert_eq!(result.total_passages, 3);
    assert_eq!(result.total_occurrences, 4);
    assert_eq!(result.next_cursor, None);

    let counts: Vec<usize> = result.occurrences.iter().map(|o| o.matches.len()).collect();
    assert_eq!(counts, [1, 1, 2]);
    let book = fixture.book();
    for occurrence in &result.occurrences {
        let passage = &book.passages[occurrence.passage_id as usize];
        assert_eq!(occurrence.section, passage.section);
        assert_eq!(occurrence.locator, passage.locator);
        for [start, end] in &occurrence.matches {
            let text: String = passage
                .text
                .chars()
                .skip(*start)
                .take(end - start)
                .collect();
            assert!(
                text.to_lowercase()
                    .split_whitespace()
                    .eq(["mutual", "information"]),
                "{text:?}"
            );
        }
    }
}

#[test]
fn find_paginates() {
    let texts: Vec<String> = (0..25)
        .map(|i| format!("Passage {i} states the golden rule once."))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let fixture = Fixture::new(&refs);
    assert_eq!(FIND_PAGE, 20);

    let first = fixture
        .session
        .find(&fixture.path, "golden rule", None)
        .unwrap();
    assert_eq!(ids(&first.occurrences), (0..20).collect::<Vec<u32>>());
    assert_eq!(first.next_cursor, Some(20));
    assert_eq!(first.total_passages, 25);
    assert_eq!(first.total_occurrences, 25);

    let second = fixture
        .session
        .find(&fixture.path, "golden rule", first.next_cursor)
        .unwrap();
    assert_eq!(ids(&second.occurrences), (20..25).collect::<Vec<u32>>());
    assert_eq!(second.next_cursor, None);
    assert_eq!(second.total_passages, 25);
    assert_eq!(second.total_occurrences, 25);
}

#[test]
fn a_cursor_beyond_the_end_gives_an_empty_page_with_the_totals() {
    let fixture = Fixture::new(&["The golden rule.", "The golden rule again."]);
    let result = fixture
        .session
        .find(&fixture.path, "golden rule", Some(99))
        .unwrap();
    assert!(result.occurrences.is_empty());
    assert_eq!(result.next_cursor, None);
    assert_eq!(result.total_passages, 2);
    assert_eq!(result.total_occurrences, 2);

    let exactly_at_end = fixture
        .session
        .find(&fixture.path, "golden rule", Some(2))
        .unwrap();
    assert!(exactly_at_end.occurrences.is_empty());
    assert_eq!(exactly_at_end.next_cursor, None);
}

#[test]
fn find_with_no_match_is_empty_not_an_error() {
    let fixture = Fixture::new(&["Some text."]);
    let result = fixture
        .session
        .find(&fixture.path, "no such phrase", None)
        .unwrap();
    assert!(result.occurrences.is_empty());
    assert_eq!(result.total_passages, 0);
    assert_eq!(result.total_occurrences, 0);
    assert_eq!(result.next_cursor, None);
}

#[test]
fn find_ignores_stemming() {
    let fixture = Fixture::new(&[
        "Both entropies are finite.",
        "The entropy is finite.",
        "Entropy, again.",
    ]);
    let result = fixture
        .session
        .find(&fixture.path, "entropy", None)
        .unwrap();
    assert_eq!(ids(&result.occurrences), [1, 2]);
    assert_eq!(result.total_passages, 2);
    // The stemmed search, by contrast, finds all three.
    let hits = lexical_hits(&fixture.book(), &parse_query("entropy"), 10).unwrap();
    assert_eq!(hits.len(), 3);
}

#[test]
fn find_trims_the_phrase() {
    let fixture = Fixture::new(&["The golden rule."]);
    let result = fixture
        .session
        .find(&fixture.path, "  golden rule \n", None)
        .unwrap();
    assert_eq!(result.phrase, "golden rule");
    assert_eq!(result.total_passages, 1);
}

#[test]
fn empty_phrase_is_invalid() {
    let fixture = Fixture::new(&["Some text."]);
    for phrase in ["", "   ", "\t\n", "...", " , "] {
        let error = fixture
            .session
            .find(&fixture.path, phrase, None)
            .unwrap_err();
        assert!(
            matches!(error, Error::InvalidArgument(_)),
            "{phrase:?}: {error}"
        );
    }
}

#[test]
fn find_results_serialize_with_the_documented_fields() {
    let fixture = Fixture::new(&["The golden rule."]);
    let result = fixture
        .session
        .find(&fixture.path, "golden rule", None)
        .unwrap();
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["phrase"], "golden rule");
    assert_eq!(json["total_occurrences"], 1);
    assert_eq!(json["total_passages"], 1);
    assert!(json["next_cursor"].is_null());
    assert_eq!(json["occurrences"][0]["passage_id"], 0);
    assert_eq!(
        json["occurrences"][0]["matches"],
        serde_json::json!([[4, 15]])
    );
    assert_eq!(
        json["occurrences"][0]["section"],
        serde_json::json!(["Part 0"])
    );
    assert_eq!(json["occurrences"][0]["locator"]["format"], "epub");
}
