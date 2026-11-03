mod common;

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::passages::PassageLocator;
use ebook_hero::session::Session;
use ebook_hero::verify::{Closest, Elsewhere, Verdict, VerifyResult};
use tempfile::TempDir;

/// A book on disk and a session with a private cache for it.
struct Fixture {
    _root: TempDir,
    _books: TempDir,
    session: Session,
    path: String,
}

impl Fixture {
    /// An EPUB with one chapter per entry of `paragraphs`, each under its own
    /// heading, so that entry `i` is passage `i`.
    fn chapters(paragraphs: &[&str]) -> Fixture {
        let mut builder = EpubBuilder::new(EpubVersion::V3).title("Quote Book");
        for (i, text) in paragraphs.iter().enumerate() {
            let href = format!("ch{i}.xhtml");
            builder = builder
                .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
                .toc(&format!("Part {i}"), &href, 1);
        }
        Fixture::epub(builder)
    }

    /// An EPUB with one chapter whose paragraphs are all in passage 0.
    fn one_passage(paragraphs: &[&str]) -> Fixture {
        let body: String = paragraphs.iter().map(|p| format!("<p>{p}</p>")).collect();
        Fixture::epub(
            EpubBuilder::new(EpubVersion::V3)
                .title("Quote Book")
                .chapter("ch0.xhtml", &format!("<h1>Only</h1>{body}"))
                .toc("Only", "ch0.xhtml", 1),
        )
    }

    fn epub(builder: EpubBuilder) -> Fixture {
        let books = TempDir::new().unwrap();
        let path = books.path().join("book.epub");
        builder.write(&path);
        let root = private_tempdir();
        Fixture {
            session: Session::with_cache_root(root.path().to_path_buf()),
            path: path.to_str().unwrap().to_string(),
            _root: root,
            _books: books,
        }
    }

    fn verify(&self, quote: &str, passage_id: Option<u32>) -> VerifyResult {
        self.session.verify(&self.path, quote, passage_id).unwrap()
    }

    fn locator(&self, passage_id: u32) -> PassageLocator {
        self.session.book(&self.path).unwrap().passages[passage_id as usize]
            .locator
            .clone()
    }
}

fn invalid(result: Result<VerifyResult, Error>) -> String {
    match result {
        Err(Error::InvalidArgument(message)) => message,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn exact_quote() {
    let fixture = Fixture::chapters(&[
        "Unrelated opening words.",
        "Shannon’s entropy measures the uncertainty of a variable.",
        "Unrelated closing words.",
    ]);

    let result = fixture.verify("uncertainty of a variable", None);

    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.locator, Some(fixture.locator(1)));
    // The apostrophe is three bytes but one code point.
    assert_eq!(result.matched, Some([31, 56]));
    assert_eq!(result.closest, None);
    assert_eq!(result.found_elsewhere, None);
}

#[test]
fn exact_quote_in_the_asked_passage() {
    let fixture = Fixture::chapters(&["Alpha beta gamma.", "Delta epsilon zeta."]);

    let result = fixture.verify("epsilon zeta", Some(1));

    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.matched, Some([6, 18]));
    assert_eq!(result.found_elsewhere, None);
}

#[test]
fn a_quote_is_compared_after_the_same_normalization_as_the_text() {
    let fixture = Fixture::chapters(&["The ﬁnal  answer\u{00AD} is here."]);

    let result = fixture.verify("  the final answer is\nhere", None);
    assert_eq!(result.verdict, Verdict::Equivalent, "case differs");

    let result = fixture.verify("final answer  is  here.", None);
    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.matched, Some([4, 25]));
}

#[test]
fn an_exact_quote_may_run_across_paragraphs_of_one_passage() {
    let fixture =
        Fixture::one_passage(&["The first paragraph ends here.", "The second begins now."]);

    let result = fixture.verify("ends here. The second begins", None);

    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(0));
    // "The first paragraph ends here." is 30 code points; a blank line follows.
    assert_eq!(result.matched, Some([20, 49]));
}

#[test]
fn exact_requires_word_boundaries() {
    let fixture = Fixture::chapters(&["entropy measures the rest."]);

    for passage_id in [None, Some(0)] {
        let result = fixture.verify("entropy measure", passage_id);

        assert_eq!(result.verdict, Verdict::None);
        assert_eq!(result.matched, None);
        assert_eq!(result.found_elsewhere, None);
        let closest = result.closest.unwrap();
        assert_eq!(closest.passage_id, 0);
        assert_eq!((closest.matched_words, closest.quote_words), (1, 2));
        assert_eq!(closest.first_difference.as_deref(), Some("measure"));
    }
    // A quote that starts inside a word is rejected the same way.
    let result = fixture.verify("ntropy measures", None);
    assert_eq!(result.verdict, Verdict::None);
}

#[test]
fn exact_allows_punctuation_at_the_edges() {
    let fixture = Fixture::chapters(&[
        "In short, entropy measures.",
        "He said “entropy measures”, twice.",
    ]);

    let result = fixture.verify("entropy measures", None);
    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(0));
    assert_eq!(result.matched, Some([10, 26]));

    let result = fixture.verify("entropy measures", Some(1));
    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.matched, Some([9, 25]));
}

#[test]
fn exact_tries_later_occurrences() {
    let fixture = Fixture::chapters(&["xentropy and entropy."]);

    let result = fixture.verify("entropy", None);

    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(0));
    assert_eq!(result.matched, Some([13, 20]));
}

#[test]
fn case_and_punctuation_are_equivalent() {
    let fixture = Fixture::chapters(&["Before.", "entropy — it is said to measure disorder."]);

    let result = fixture.verify("Entropy, it is said", None);

    assert_eq!(result.verdict, Verdict::Equivalent);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.locator, Some(fixture.locator(1)));
    assert_eq!(result.matched, Some([0, 20]));
    assert_eq!(result.closest, None);
    assert_eq!(result.found_elsewhere, None);
}

#[test]
fn stemming_never_matches() {
    let fixture = Fixture::chapters(&["entropy measures"]);

    for passage_id in [None, Some(0)] {
        let result = fixture.verify("entropies measure", passage_id);

        assert_eq!(result.verdict, Verdict::None);
        assert_eq!(result.matched, None);
        assert_eq!(result.found_elsewhere, None);
    }
    // Only whole words are compared when case or punctuation differ.
    let result = fixture.verify("Entropy measure.", None);
    assert_eq!(result.verdict, Verdict::None);
}

#[test]
fn quote_may_cross_into_the_next_passage() {
    let fixture = Fixture::chapters(&[
        "Opening words.",
        "The sum of all the parts is",
        "greater than the whole. Closing words.",
    ]);

    let result = fixture.verify("parts is greater than the whole", None);

    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.locator, Some(fixture.locator(1)));
    // The range stops where passage 1 stops.
    assert_eq!(result.matched, Some([19, 27]));

    let result = fixture.verify("Parts, is: greater than", Some(1));
    assert_eq!(result.verdict, Verdict::Equivalent);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.matched, Some([19, 27]));
}

#[test]
fn a_quote_inside_the_next_passage_belongs_to_that_passage() {
    let fixture = Fixture::chapters(&["Alpha beta.", "Gamma delta."]);

    let result = fixture.verify("Gamma delta", None);
    assert_eq!(result.passage_id, Some(1));
    assert_eq!(result.matched, Some([0, 11]));

    let result = fixture.verify("Gamma delta", Some(0));
    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.passage_id, Some(0));
    assert_eq!(
        result.found_elsewhere.map(|e| e.passage_id),
        Some(1),
        "the quote is in passage 1, not 0"
    );
}

#[test]
fn a_quote_does_not_span_three_passages() {
    let fixture = Fixture::chapters(&["one two", "three four", "five six"]);

    let result = fixture.verify("two three four five", None);

    assert_eq!(result.verdict, Verdict::None);
}

#[test]
fn found_elsewhere_with_a_wrong_passage_id() {
    let fixture = Fixture::chapters(&[
        "Zero.",
        "One.",
        "Two is about something else.",
        "Three.",
        "Four.",
        "Five holds the wanted sentence.",
    ]);

    let result = fixture.verify("the wanted sentence", Some(2));

    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.passage_id, Some(2));
    assert_eq!(result.locator, Some(fixture.locator(2)));
    assert_eq!(result.matched, None);
    assert_eq!(result.closest, None);
    assert_eq!(
        result.found_elsewhere,
        Some(Elsewhere {
            passage_id: 5,
            locator: fixture.locator(5),
            verdict: Verdict::Exact,
        })
    );
}

#[test]
fn found_elsewhere_reports_an_equivalent_match() {
    let fixture = Fixture::chapters(&["Zero.", "One.", "Two holds The Wanted, Sentence."]);

    let result = fixture.verify("the wanted sentence", Some(0));

    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.passage_id, Some(0));
    assert_eq!(
        result.found_elsewhere.map(|e| (e.passage_id, e.verdict)),
        Some((2, Verdict::Equivalent))
    );
}

#[test]
fn the_first_exact_match_beats_an_earlier_equivalent_one() {
    let fixture = Fixture::chapters(&[
        "Zero.",
        "One says The Wanted, Sentence.",
        "Two says the wanted sentence.",
        "Three says the wanted sentence.",
    ]);

    let result = fixture.verify("the wanted sentence", None);
    assert_eq!(result.verdict, Verdict::Exact);
    assert_eq!(result.passage_id, Some(2));

    let result = fixture.verify("the wanted sentence", Some(0));
    assert_eq!(
        result.found_elsewhere.map(|e| (e.passage_id, e.verdict)),
        Some((2, Verdict::Exact))
    );
}

#[test]
fn none_reports_the_closest_passage() {
    let fixture = Fixture::chapters(&[
        "Nothing about the matter.",
        "Entropy measures the uncertainty of a variable.",
        "Entropy measures the rest.",
    ]);
    let quote = "entropy measures the uncertainty of a banana";
    let closest = Closest {
        passage_id: 1,
        locator: fixture.locator(1),
        matched_words: 6,
        quote_words: 7,
        first_difference: Some("banana".to_string()),
    };

    let result = fixture.verify(quote, None);

    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.passage_id, None);
    assert_eq!(result.locator, None);
    assert_eq!(result.matched, None);
    assert_eq!(result.found_elsewhere, None);
    assert_eq!(result.closest, Some(closest.clone()));

    // With a passage id, the result is about that passage; the closest one
    // is still searched in the whole book.
    let result = fixture.verify(quote, Some(2));
    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.passage_id, Some(2));
    assert_eq!(result.locator, Some(fixture.locator(2)));
    assert_eq!(result.closest, Some(closest));
}

#[test]
fn first_difference_is_the_quote_word_after_the_run() {
    let fixture = Fixture::chapters(&["the uncertainty of a variables"]);
    let result = fixture.verify("measures the uncertainty of a variable", None);
    let closest = result.closest.unwrap();
    assert_eq!(closest.matched_words, 4);
    assert_eq!(closest.quote_words, 6);
    assert_eq!(closest.first_difference.as_deref(), Some("variable"));

    let fixture = Fixture::chapters(&["x the uncertainty of a variable"]);
    let result = fixture.verify("w the uncertainty of a variable", None);
    assert_eq!(result.verdict, Verdict::None);
    let closest = result.closest.unwrap();
    assert_eq!((closest.matched_words, closest.quote_words), (5, 6));
    assert_eq!(closest.first_difference, None);
}

#[test]
fn the_earliest_passage_wins_a_tie_for_the_closest() {
    let fixture = Fixture::chapters(&["Nothing.", "alpha beta gamma", "alpha beta delta"]);

    let result = fixture.verify("alpha beta zeta", None);

    let closest = result.closest.unwrap();
    assert_eq!(closest.passage_id, 1);
    assert_eq!(closest.matched_words, 2);
    assert_eq!(closest.first_difference.as_deref(), Some("zeta"));
}

#[test]
fn no_closest_passage_when_no_word_is_shared() {
    let fixture = Fixture::chapters(&["Alpha beta."]);

    let result = fixture.verify("completely unrelated words", None);

    assert_eq!(result.verdict, Verdict::None);
    assert_eq!(result.closest, None);
}

#[test]
fn blank_quote_and_unknown_passage_are_invalid() {
    let fixture = Fixture::chapters(&["Alpha.", "Beta.", "Gamma."]);

    for quote in ["", "   \n\t", "\u{00AD}\u{200B}"] {
        let message = invalid(fixture.session.verify(&fixture.path, quote, None));
        assert!(message.contains("blank"), "{message}");
    }
    for quote in ["...", "— —", "%"] {
        let message = invalid(fixture.session.verify(&fixture.path, quote, None));
        assert!(message.contains("no words"), "{message}");
    }
    assert_eq!(
        invalid(fixture.session.verify(&fixture.path, "Beta", Some(3))),
        "passage 3 does not exist; the book has 3 passages"
    );
    assert_eq!(
        invalid(
            fixture
                .session
                .verify(&fixture.path, "Beta", Some(u32::MAX))
        ),
        format!(
            "passage {} does not exist; the book has 3 passages",
            u32::MAX
        )
    );
}

#[test]
fn the_result_serializes_with_lowercase_verdicts() {
    let fixture = Fixture::chapters(&["Alpha beta.", "Gamma."]);

    let exact = serde_json::to_value(fixture.verify("Alpha", None)).unwrap();
    assert_eq!(exact["verdict"], "exact");
    assert_eq!(exact["matched"], serde_json::json!([0, 5]));
    assert_eq!(exact["passage_id"], 0);
    assert!(exact["closest"].is_null());

    let equivalent = serde_json::to_value(fixture.verify("alpha", None)).unwrap();
    assert_eq!(equivalent["verdict"], "equivalent");

    let elsewhere = serde_json::to_value(fixture.verify("Gamma", Some(0))).unwrap();
    assert_eq!(elsewhere["verdict"], "none");
    assert_eq!(elsewhere["found_elsewhere"]["verdict"], "exact");
    assert_eq!(elsewhere["found_elsewhere"]["passage_id"], 1);
}
