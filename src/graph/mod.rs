//! A book's concept graph: the phrases that mark its topics, which of them
//! share passages more often than chance, and the communities they form.

pub mod communities;
pub mod phrases;
pub mod topics;

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::passages::Passage;
use crate::settings::{
    GRAPH_CONCEPTS, GRAPH_MIN_BOOK_PASSAGES, GRAPH_MIN_SHARED, GRAPH_NPMI_FLOOR, GRAPH_PARTNERS,
};
use crate::{Error, Result};
use communities::louvain;
use phrases::{Candidate, candidates};
use topics::{passage_topics, topic_count};

/// A book's concept graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphResult {
    /// How many passages the book has.
    pub passages: u32,
    /// How many topics the concepts were chosen with.
    pub topics: u32,
    /// The concepts, in the order the book first mentions them.
    pub concepts: Vec<Concept>,
    /// The links between concepts, by `a` then `b`.
    pub links: Vec<Link>,
    /// The communities, largest first.
    pub communities: Vec<Community>,
}

/// A phrase that marks one of the book's topics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Concept {
    /// Its index in `concepts`.
    pub id: u32,
    /// The form the book most often writes it in, lowercased.
    pub label: String,
    /// The passages it occurs in, in reading order.
    pub passages: Vec<u32>,
    /// The index of its community in `communities`.
    pub community: u32,
}

/// Two concepts that share passages more often than chance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    /// The concept with the lower id.
    pub a: u32,
    /// The concept with the higher id.
    pub b: u32,
    /// Normalized pointwise mutual information: 1 when the two only ever
    /// occur together, 0 when they share passages as often as chance.
    pub npmi: f32,
    /// The passages both occur in, in reading order.
    pub passages: Vec<u32>,
}

/// Concepts more linked to each other than to the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Community {
    /// Its index in `communities`.
    pub id: u32,
    /// Its concepts' ids, ascending.
    pub concepts: Vec<u32>,
}

/// The concept graph of a book's passages, which must be in reading order.
pub fn build(passages: &[Passage]) -> Result<GraphResult> {
    let n = passages.len();
    if n < GRAPH_MIN_BOOK_PASSAGES {
        return Err(Error::InvalidArgument(format!(
            "the book has {n} passages; a concept graph needs at least \
             {GRAPH_MIN_BOOK_PASSAGES}, so use search or find on a book this short"
        )));
    }
    let texts: Vec<&str> = passages.iter().map(|p| p.text.as_str()).collect();
    let selection = select(&texts);
    let mut concepts = selection.concepts;
    concepts.sort_by(|x, y| {
        x.passages[0]
            .cmp(&y.passages[0])
            .then(x.label.cmp(&y.label))
    });

    let links = links(&concepts, n);
    let edges: Vec<(usize, usize, f64)> = links.iter().map(|l| (l.a, l.b, l.npmi)).collect();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (concept, community) in louvain(concepts.len(), &edges).into_iter().enumerate() {
        if groups.len() <= community {
            groups.resize(community + 1, Vec::new());
        }
        groups[community].push(concept);
    }
    // Largest first, ties to the community the book mentions first.
    groups.sort_by(|x, y| y.len().cmp(&x.len()).then(x[0].cmp(&y[0])));
    let mut community_of = vec![0; concepts.len()];
    for (c, group) in groups.iter().enumerate() {
        for &concept in group {
            community_of[concept] = c as u32;
        }
    }

    let id_of = |index: &u32| passages[*index as usize].id;
    Ok(GraphResult {
        passages: n as u32,
        topics: selection.topics,
        concepts: concepts
            .iter()
            .enumerate()
            .map(|(i, c)| Concept {
                id: i as u32,
                label: c.label.clone(),
                passages: c.passages.iter().map(id_of).collect(),
                community: community_of[i],
            })
            .collect(),
        links: links
            .iter()
            .map(|l| Link {
                a: l.a as u32,
                b: l.b as u32,
                npmi: ((l.npmi * 1000.0).round() / 1000.0) as f32,
                passages: l.shared.iter().map(id_of).collect(),
            })
            .collect(),
        communities: groups
            .into_iter()
            .enumerate()
            .map(|(c, group)| Community {
                id: c as u32,
                concepts: group.into_iter().map(|i| i as u32).collect(),
            })
            .collect(),
    })
}

/// A link before it is numbered for output; `shared` holds passage indices.
struct Pair {
    a: usize,
    b: usize,
    npmi: f64,
    shared: Vec<u32>,
}

/// Each concept's `GRAPH_PARTNERS` partners with the highest NPMI above
/// `GRAPH_NPMI_FLOOR`, sharing at least `GRAPH_MIN_SHARED` passages, ties to
/// the lower index. A link exists when either concept keeps the other.
fn links(concepts: &[Candidate], n: usize) -> Vec<Pair> {
    let m = concepts.len();
    let mut shared = vec![vec![Vec::new(); m]; m];
    for (a, x) in concepts.iter().enumerate() {
        for (b, y) in concepts.iter().enumerate().skip(a + 1) {
            let both = intersection(&x.passages, &y.passages);
            shared[b][a].clone_from(&both);
            shared[a][b] = both;
        }
    }
    let score = |a: usize, b: usize| {
        npmi(
            concepts[a].passages.len(),
            concepts[b].passages.len(),
            shared[a][b].len(),
            n,
        )
    };
    let mut kept = BTreeSet::new();
    for (a, with) in shared.iter().enumerate() {
        let mut partners: Vec<(f64, usize)> = (0..m)
            .filter(|&b| b != a && with[b].len() >= GRAPH_MIN_SHARED)
            .map(|b| (score(a, b), b))
            .filter(|&(v, _)| v > GRAPH_NPMI_FLOOR)
            .collect();
        partners.sort_by(|(x, i), (y, j)| y.total_cmp(x).then(i.cmp(j)));
        for (_, b) in partners.into_iter().take(GRAPH_PARTNERS) {
            kept.insert((a.min(b), a.max(b)));
        }
    }
    kept.into_iter()
        .map(|(a, b)| Pair {
            a,
            b,
            npmi: score(a, b),
            shared: shared[a][b].clone(),
        })
        .collect()
}

/// The values in both ascending lists.
fn intersection(x: &[u32], y: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < x.len() && j < y.len() {
        match x[i].cmp(&y[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(x[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Normalized PMI of two phrases in `df_a` and `df_b` of `n` passages that
/// share `shared` of them: `ln(shared·n / (df_a·df_b)) / −ln(shared / n)`.
fn npmi(df_a: usize, df_b: usize, shared: usize, n: usize) -> f64 {
    let joint = shared as f64 / n as f64;
    (shared as f64 * n as f64 / (df_a as f64 * df_b as f64)).ln() / -joint.ln()
}

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
    use crate::passages::PassageLocator;

    fn fake_passages(texts: &[&str]) -> Vec<Passage> {
        texts
            .iter()
            .enumerate()
            .map(|(i, text)| Passage {
                id: i as u32,
                text: text.to_string(),
                section: Vec::new(),
                blocks: Vec::new(),
                locator: PassageLocator::Epub {
                    href: "ch.xhtml".to_string(),
                    para_start: i as u32,
                    para_end: i as u32,
                    print_page: None,
                    under_heading: None,
                },
            })
            .collect()
    }

    #[test]
    fn npmi_is_one_for_phrases_always_together_and_zero_for_chance() {
        assert!((npmi(2, 2, 2, 10) - 1.0).abs() < 1e-12);
        assert!(npmi(4, 5, 2, 10).abs() < 1e-12);
    }

    #[test]
    fn a_book_shorter_than_twenty_passages_is_refused() {
        let error = build(&fake_passages(&["one passage"; 19]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("the book has 19 passages; a concept graph needs at least 20"),
            "{error}"
        );
    }

    #[test]
    fn a_book_whose_phrases_are_all_too_common_has_an_empty_graph() {
        let g = build(&fake_passages(&["the same words everywhere"; 30])).unwrap();
        assert!(g.concepts.is_empty() && g.links.is_empty() && g.communities.is_empty());
        assert_eq!(g.passages, 30);
    }

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
