//! Step 1 of the concept graph: candidate phrases, found by RAKE (Rose and
//! others, 2010) as the runs of words between stopwords and punctuation.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::analysis::stem_word;
use crate::settings::{
    GRAPH_CANDIDATE_MAX_SHARE, GRAPH_CANDIDATE_MIN_PASSAGES, GRAPH_PHRASE_MAX_WORDS,
};

/// The SMART system's 571 stopwords, one per line, as Appendix 11 of Lewis,
/// Yang, Rose and Li, "RCV1: A New Benchmark Collection for Text
/// Categorization Research", JMLR 5 (2004), republishes them.
const SMART_STOPWORDS: &str = include_str!("smart_stopwords.txt");

/// A word with fewer letters than this cuts a phrase, like a stopword.
const MIN_WORD_LETTERS: usize = 3;

/// A one-word phrase that a numeral follows in more than this share of its
/// occurrences labels chapters, figures or tables, as "figure" does in
/// "Figure 2.1", and is no candidate.
const LABEL_SHARE: f64 = 0.5;

/// A phrase that may become a concept. `passages` are indices into the texts
/// it was found in, ascending.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub key: String,
    pub label: String,
    pub passages: Vec<u32>,
}

fn stopwords() -> &'static HashSet<&'static str> {
    static STOPWORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    STOPWORDS.get_or_init(|| {
        SMART_STOPWORDS
            .lines()
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .collect()
    })
}

/// The phrases RAKE takes from `text`, in text order, each as its key (the
/// stemmed words joined by spaces) and its lowercased form.
pub fn phrases_in(text: &str) -> Vec<(String, String)> {
    phrases(text, &mut HashMap::new())
        .into_iter()
        .map(|p| (p.key, p.form))
        .collect()
}

/// One occurrence of a phrase.
struct Found {
    key: String,
    form: String,
    /// Whether a numeral follows it, as "2.1" follows "figure" in "figure 2.1".
    numbered: bool,
}

/// [`phrases_in`] with a memo of the stems already computed, which a whole
/// book reuses, and whether a numeral follows each phrase.
fn phrases(text: &str, stems: &mut HashMap<String, String>) -> Vec<Found> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    let mut run_end = 0;
    for word in words(&lower) {
        match word.filter(|(w, _)| is_content(w)) {
            Some((w, end)) => {
                run.push(w);
                run_end = end;
            }
            None => flush(
                &mut run,
                numeral_follows(&lower, run_end),
                &mut found,
                stems,
            ),
        }
    }
    flush(
        &mut run,
        numeral_follows(&lower, run_end),
        &mut found,
        stems,
    );
    found
}

/// Whether a numeral follows byte `at` of `text` past whitespace, and past a
/// period before it: "figure 2" and "fig. 2", but not "realism17" or
/// "realism.17", whose digits mark a note.
fn numeral_follows(text: &str, at: usize) -> bool {
    let after = &text[at..];
    let after = after.strip_prefix('.').unwrap_or(after);
    let rest = after.trim_start();
    rest.len() < after.len() && rest.starts_with(char::is_numeric)
}

/// The text as a sequence of words, each with the byte where it ends, and
/// `None` wherever a phrase is cut: at punctuation, digits and every other
/// character that is neither a letter nor whitespace. Whitespace only
/// separates words. A word is a run of letters that may hold `-`, `'` or `’`
/// between two letters; a final possessive `'s` is dropped, and a word that
/// still holds an apostrophe, a contraction such as "don't", is not a word.
fn words(text: &str) -> Vec<Option<(&str, usize)>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].1.is_whitespace() {
            i += 1;
            continue;
        }
        if !chars[i].1.is_alphabetic() {
            if out.last() != Some(&None) {
                out.push(None);
            }
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let mut j = i + 1;
        while j < chars.len() {
            let c = chars[j].1;
            let joins = matches!(c, '-' | '\'' | '’')
                && chars
                    .get(j + 1)
                    .is_some_and(|(_, next)| next.is_alphabetic());
            if c.is_alphabetic() || joins {
                j += 1;
            } else {
                break;
            }
        }
        let end = chars.get(j).map_or(text.len(), |(at, _)| *at);
        let word = &text[start..end];
        let word = word
            .strip_suffix("'s")
            .or_else(|| word.strip_suffix("’s"))
            .unwrap_or(word);
        if word.contains(['\'', '’']) {
            out.push(None);
        } else {
            out.push(Some((word, end)));
        }
        i = j;
    }
    out
}

fn is_content(word: &str) -> bool {
    word.chars().filter(|c| c.is_alphabetic()).count() >= MIN_WORD_LETTERS
        && !stopwords().contains(word)
}

/// Ends the current run: a run of 1 to `GRAPH_PHRASE_MAX_WORDS` words is a
/// phrase, a longer one is dropped.
fn flush(
    run: &mut Vec<&str>,
    numbered: bool,
    found: &mut Vec<Found>,
    stems: &mut HashMap<String, String>,
) {
    if !run.is_empty() && run.len() <= GRAPH_PHRASE_MAX_WORDS {
        let key = run
            .iter()
            .map(|w| {
                stems
                    .entry((*w).to_string())
                    .or_insert_with(|| stem_word(w))
                    .clone()
            })
            .collect::<Vec<_>>()
            .join(" ");
        found.push(Found {
            key,
            form: run.join(" "),
            numbered,
        });
    }
    run.clear();
}

/// The phrases of `texts` that occur in at least
/// `GRAPH_CANDIDATE_MIN_PASSAGES` of them and in at most
/// `GRAPH_CANDIDATE_MAX_SHARE` of them, sorted by key, without the one-word
/// phrases that a numeral follows in more than `LABEL_SHARE` of their
/// occurrences. A phrase's label is the form it most often has, ties going to
/// the form seen first.
pub fn candidates(texts: &[&str]) -> Vec<Candidate> {
    #[derive(Default)]
    struct Seen {
        passages: Vec<u32>,
        /// Each form with how often it occurs and when it was first seen.
        forms: HashMap<String, (usize, usize)>,
        occurrences: usize,
        numbered: usize,
    }
    let mut stems = HashMap::new();
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut order = 0;
    for (i, text) in texts.iter().enumerate() {
        let i = i as u32;
        for found in phrases(text, &mut stems) {
            let entry = seen.entry(found.key).or_default();
            if entry.passages.last() != Some(&i) {
                entry.passages.push(i);
            }
            entry.forms.entry(found.form).or_insert((0, order)).0 += 1;
            entry.occurrences += 1;
            entry.numbered += usize::from(found.numbered);
            order += 1;
        }
    }
    let most = GRAPH_CANDIDATE_MAX_SHARE * texts.len() as f64;
    let is_label = |key: &str, s: &Seen| {
        !key.contains(' ') && s.numbered as f64 > LABEL_SHARE * s.occurrences as f64
    };
    let mut kept: Vec<Candidate> = seen
        .into_iter()
        .filter(|(key, s)| {
            s.passages.len() >= GRAPH_CANDIDATE_MIN_PASSAGES
                && s.passages.len() as f64 <= most
                && !is_label(key, s)
        })
        .map(|(key, s)| {
            let label = s
                .forms
                .into_iter()
                .max_by(|(_, (n1, first1)), (_, (n2, first2))| n1.cmp(n2).then(first2.cmp(first1)))
                .map(|(form, _)| form)
                .expect("a kept phrase was seen at least once");
            Candidate {
                key,
                label,
                passages: s.passages,
            }
        })
        .collect();
    kept.sort_by(|a, b| a.key.cmp(&b.key));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaces(text: &str) -> Vec<String> {
        phrases_in(text)
            .into_iter()
            .map(|(_, surface)| surface)
            .collect()
    }

    #[test]
    fn rake_cuts_at_stopwords_punctuation_and_digits() {
        assert_eq!(
            surfaces("The transcendental realism of science, in 1975, rejects positivism."),
            ["transcendental realism", "science", "rejects positivism"]
        );
    }

    #[test]
    fn possessives_are_dropped_and_contractions_cut() {
        assert_eq!(
            surfaces("Kant’s critique doesn't answer Hume's problem"),
            ["kant critique", "answer hume problem"]
        );
    }

    #[test]
    fn accented_and_hyphenated_words_stay_whole() {
        assert_eq!(
            surfaces("naïve realism — labor-power and Gödel"),
            ["naïve realism", "labor-power", "gödel"]
        );
    }

    #[test]
    fn runs_longer_than_three_words_are_not_candidates() {
        assert!(phrases_in("red green blue yellow").is_empty());
    }

    #[test]
    fn a_word_a_numeral_mostly_follows_is_a_label_and_no_candidate() {
        // "figure" comes before a number in 3 of its 4 passages and "fig" in
        // all 3, once past a period; "theory" in 1 of 3. "world war" is
        // always numbered but has two words.
        let mut texts = vec![
            "see figure 2.1 here",
            "as figure 3 is",
            "figure 4, then",
            "in the figure, then",
            "see fig. 2 here",
            "as fig. 3 is",
            "in fig. 5 again",
            "theory 1 is here",
            "the theory, then",
            "a theory, again",
            "world war 2 began",
            "the world war 2, then",
            "in world war 2 again",
        ];
        texts.extend(["filler"; 7]);
        let got: Vec<String> = candidates(&texts).into_iter().map(|c| c.label).collect();
        assert_eq!(got, ["theory", "world war"]);
    }

    #[test]
    fn candidates_keep_phrases_in_three_passages_and_at_most_thirty_percent() {
        // 10 passages: "mock turtle" in 3 (kept), "gryphon" in 2 (too rare),
        // "queen" in 4 (too common: more than 30%).
        let mut texts = vec!["mock turtle and the queen"; 3];
        texts.extend(["gryphon and the queen", "gryphon"]);
        texts.extend(["filler"; 5]);
        let got: Vec<String> = candidates(&texts).into_iter().map(|c| c.label).collect();
        assert_eq!(got, ["mock turtle"]);
    }
}
