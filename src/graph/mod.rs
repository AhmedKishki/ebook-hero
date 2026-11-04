//! A book's concept graph: the phrases that mark its topics, which of them
//! share passages more often than chance, and the communities they form.

pub mod phrases;
pub mod topics;

use crate::settings::GRAPH_CONCEPTS;
use phrases::{Candidate, candidates};
use topics::{passage_topics, topic_count};

/// Steps 1 to 3 of the graph: a book's concepts, the one with the most
/// mutual information with the topics first, and how many topics there were.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub topics: u32,
    pub concepts: Vec<Candidate>,
}

/// The `GRAPH_CONCEPTS` candidate phrases of `texts` that best predict a
/// passage's topic, ties going to the phrase in more passages, then to the
/// lower key.
pub fn select(texts: &[&str]) -> Selection {
    let k = topic_count(texts.len());
    let topic_of = passage_topics(texts, k);
    let mut sizes = vec![0; k];
    for &topic in &topic_of {
        sizes[topic] += 1;
    }
    let mut scored: Vec<(f64, Candidate)> = candidates(texts)
        .into_iter()
        .map(|c| (mutual_information(&c.passages, &topic_of, &sizes), c))
        .collect();
    scored.sort_by(|(a, x), (b, y)| {
        b.total_cmp(a)
            .then(y.passages.len().cmp(&x.passages.len()))
            .then(x.key.cmp(&y.key))
    });
    scored.truncate(GRAPH_CONCEPTS);
    Selection {
        topics: k as u32,
        concepts: scored.into_iter().map(|(_, c)| c).collect(),
    }
}

/// `I(T; G)` in nats, between whether a passage is one of `occurs` and its
/// topic in `topic_of`, given how many passages each topic has.
fn mutual_information(occurs: &[u32], topic_of: &[usize], topic_sizes: &[usize]) -> f64 {
    let n = topic_of.len() as f64;
    let mut inside = vec![0; topic_sizes.len()];
    for &p in occurs {
        inside[topic_of[p as usize]] += 1;
    }
    let p_in = occurs.len() as f64 / n;
    let mut mi = 0.0;
    for (&size, &with) in topic_sizes.iter().zip(&inside) {
        let p_topic = size as f64 / n;
        for (count, p_t) in [(with, p_in), (size - with, 1.0 - p_in)] {
            if count > 0 {
                let joint = count as f64 / n;
                mi += joint * (joint / (p_t * p_topic)).ln();
            }
        }
    }
    mi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutual_information_matches_hand_values() {
        let topic_of = [0, 0, 1, 1];
        let sizes = [2, 2];
        let mi = mutual_information(&[0, 1], &topic_of, &sizes);
        assert!((mi - std::f64::consts::LN_2).abs() < 1e-12, "{mi}");
        assert!(mutual_information(&[0, 2], &topic_of, &sizes).abs() < 1e-12);
    }

    /// A word of letters only, different for each `j`.
    fn word(j: usize) -> String {
        let letter = |x: usize| char::from(b'a' + (x % 26) as u8);
        format!("zz{}{}", letter(j / 26), letter(j))
    }

    #[test]
    fn select_keeps_the_most_informative_concepts_up_to_the_cap() {
        // 100 passages and 80 one-word phrases, each in 3 neighbouring passages.
        let texts: Vec<String> = (0..100)
            .map(|i| {
                (0..80)
                    .filter(|j| (i + 100 - j) % 100 < 3)
                    .map(word)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let s = select(&refs);
        assert_eq!(s.topics, 10);
        assert_eq!(s.concepts.len(), GRAPH_CONCEPTS);
        let topic_of = passage_topics(&refs, 10);
        let mut sizes = vec![0; 10];
        topic_of.iter().for_each(|&t| sizes[t] += 1);
        let mi: Vec<f64> = s
            .concepts
            .iter()
            .map(|c| mutual_information(&c.passages, &topic_of, &sizes))
            .collect();
        assert!(mi.windows(2).all(|w| w[0] >= w[1]), "{mi:?}");
    }
}
