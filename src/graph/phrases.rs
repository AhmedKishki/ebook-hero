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
}

/// [`phrases_in`] with a memo of the stems already computed, which a whole
/// book reuses.
fn phrases(text: &str, stems: &mut HashMap<String, String>) -> Vec<(String, String)> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for word in words(&lower) {
        match word.filter(|w| is_content(w)) {
            Some(w) => run.push(w),
            None => flush(&mut run, &mut found, stems),
        }
    }
    flush(&mut run, &mut found, stems);
    found
}

/// The text as a sequence of words, with `None` wherever a phrase is cut:
/// at punctuation, digits and every other character that is neither a letter
/// nor whitespace. Whitespace only separates words. A word is a run of letters that may hold `-`, `'` or `’` between two
/// letters; a final possessive `'s` is dropped, and a word that still holds an
/// apostrophe, a contraction such as "don't", is not a word.
fn words(text: &str) -> Vec<Option<&str>> {
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
            out.push(Some(word));
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
    found: &mut Vec<(String, String)>,
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
        found.push((key, run.join(" ")));
    }
    run.clear();
}

/// The phrases of `texts` that occur in at least
/// `GRAPH_CANDIDATE_MIN_PASSAGES` of them and in at most
/// `GRAPH_CANDIDATE_MAX_SHARE` of them, sorted by key. A phrase's label is the
/// form it most often has, ties going to the form seen first.
pub fn candidates(texts: &[&str]) -> Vec<Candidate> {
    #[derive(Default)]
    struct Seen {
        passages: Vec<u32>,
        /// Each form with how often it occurs and when it was first seen.
        forms: HashMap<String, (usize, usize)>,
    }
    let mut stems = HashMap::new();
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut order = 0;
    for (i, text) in texts.iter().enumerate() {
        let i = i as u32;
        for (key, form) in phrases(text, &mut stems) {
            let entry = seen.entry(key).or_default();
            if entry.passages.last() != Some(&i) {
                entry.passages.push(i);
            }
            entry.forms.entry(form).or_insert((0, order)).0 += 1;
            order += 1;
        }
    }
    let most = GRAPH_CANDIDATE_MAX_SHARE * texts.len() as f64;
    let mut kept: Vec<Candidate> = seen
        .into_iter()
        .filter(|(_, s)| {
            s.passages.len() >= GRAPH_CANDIDATE_MIN_PASSAGES && s.passages.len() as f64 <= most
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
