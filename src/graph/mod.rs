//! A book's concept graph: the phrases that mark its topics, which of them
//! share passages more often than chance, and the communities they form.

pub mod communities;
pub mod phrases;
pub mod topics;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{Block, BlockKind};
use crate::passages::Passage;
use crate::settings::{
    GRAPH_CONCEPTS, GRAPH_MIN_BOOK_PASSAGES, GRAPH_MIN_SHARED, GRAPH_NPMI_FLOOR, GRAPH_PARTNERS,
    GRAPH_PASSAGES_SHOWN,
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
    /// How many of them the graph was built from: those with text outside
    /// notes, tables of contents, lists of figures, tables or abbreviations,
    /// bibliographies and indexes. Link NPMI counts these passages.
    pub passages_read: u32,
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
    /// How many passages it stands in as a phrase of its own, cut off from
    /// other words by stopwords or punctuation.
    pub passage_count: u32,
    /// Those passages' ids, in reading order: all of them when the full graph
    /// was asked for, else the first ten. `find` lists every mention of the
    /// words, including those inside longer phrases.
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
    /// How many passages both concepts stand in.
    pub passage_count: u32,
    /// Those passages' ids, in reading order: all of them when the full graph
    /// was asked for, else the first ten.
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

impl GraphResult {
    /// This graph with each concept's and link's passages cut to their first
    /// `GRAPH_PASSAGES_SHOWN`; `passage_count` still says how many there are.
    pub fn abridged(&self) -> GraphResult {
        let mut short = self.clone();
        for concept in &mut short.concepts {
            concept.passages.truncate(GRAPH_PASSAGES_SHOWN);
        }
        for link in &mut short.links {
            link.passages.truncate(GRAPH_PASSAGES_SHOWN);
        }
        short
    }
}

/// The concept graph of a book's passages, which must be in reading order,
/// cut from `blocks`. The graph reads each passage without its notes and
/// apparatus, and leaves out the passages that nothing else is left of.
pub fn build(blocks: &[Block], passages: &[Passage]) -> Result<GraphResult> {
    let left_out: HashSet<u32> = blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Note { .. }) || b.apparatus.is_some())
        .map(|b| b.id)
        .collect();
    let read: Vec<(u32, Cow<str>)> = passages
        .iter()
        .map(|p| (p.id, text_outside(p, &left_out)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    let (n, r) = (passages.len(), read.len());
    if r < GRAPH_MIN_BOOK_PASSAGES {
        return Err(Error::InvalidArgument(if r == n {
            format!(
                "the book has {n} passages; a concept graph needs at least \
                 {GRAPH_MIN_BOOK_PASSAGES}, so use search or find on a book this short"
            )
        } else {
            format!(
                "{r} of the book's {n} passages have text outside notes, contents, \
                 lists, bibliography and index; a concept graph needs at least \
                 {GRAPH_MIN_BOOK_PASSAGES}, so use search or find on this book"
            )
        }));
    }
    let texts: Vec<&str> = read.iter().map(|(_, text)| text.as_ref()).collect();
    let selection = select(&texts);
    let mut concepts = selection.concepts;
    concepts.sort_by(|x, y| {
        x.passages[0]
            .cmp(&y.passages[0])
            .then(x.label.cmp(&y.label))
    });

    let links = links(&concepts, r);
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

    let id_of = |index: &u32| read[*index as usize].0;
    Ok(GraphResult {
        passages: n as u32,
        passages_read: r as u32,
        topics: selection.topics,
        concepts: concepts
            .iter()
            .enumerate()
            .map(|(i, c)| Concept {
                id: i as u32,
                label: c.label.clone(),
                passage_count: c.passages.len() as u32,
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
                passage_count: l.shared.len() as u32,
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

/// The text of `passage` without the pieces of the blocks in `left_out`, the
/// pieces left joined by a blank line.
fn text_outside<'a>(passage: &'a Passage, left_out: &HashSet<u32>) -> Cow<'a, str> {
    if !passage
        .blocks
        .iter()
        .any(|range| left_out.contains(&range.block_id))
    {
        return Cow::Borrowed(&passage.text);
    }
    Cow::Owned(
        passage
            .blocks
            .iter()
            .filter(|range| !left_out.contains(&range.block_id))
            .map(|range| &passage.text[range.start..range.end])
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
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
    use crate::document::{Apparatus, BlockKind, Locator};
    use crate::passages::{BlockRange, PassageLocator};

    /// A piece of a passage: its text, kind and apparatus.
    type Piece<'a> = (&'a str, BlockKind, Option<Apparatus>);

    /// A book with one passage of `pieces`, a block each, per entry of
    /// `passages`.
    fn fake_book(passages: &[Vec<Piece>]) -> (Vec<Block>, Vec<Passage>) {
        let mut blocks = Vec::new();
        let mut out = Vec::new();
        for (i, pieces) in passages.iter().enumerate() {
            let mut text = String::new();
            let mut ranges = Vec::new();
            for (piece, kind, apparatus) in pieces {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                let id = blocks.len() as u32;
                ranges.push(BlockRange {
                    block_id: id,
                    block_offset: 0,
                    start: text.len(),
                    end: text.len() + piece.len(),
                });
                text.push_str(piece);
                blocks.push(Block {
                    id,
                    kind: kind.clone(),
                    text: piece.to_string(),
                    orig: None,
                    spans: Vec::new(),
                    section: Vec::new(),
                    apparatus: *apparatus,
                    locator: Locator::Epub {
                        href: "ch.xhtml".to_string(),
                        anchor: None,
                        para: id + 1,
                        print_page: None,
                    },
                });
            }
            out.push(Passage {
                id: i as u32,
                text,
                section: Vec::new(),
                blocks: ranges,
                locator: PassageLocator::Epub {
                    href: "ch.xhtml".to_string(),
                    para_start: i as u32,
                    para_end: i as u32,
                    print_page: None,
                    under_heading: None,
                },
            });
        }
        (blocks, out)
    }

    /// A book of one paragraph per passage.
    fn fake_passages(texts: &[&str]) -> (Vec<Block>, Vec<Passage>) {
        let passages: Vec<Vec<Piece>> = texts
            .iter()
            .map(|text| vec![(*text, BlockKind::Paragraph, None)])
            .collect();
        fake_book(&passages)
    }

    fn build_of((blocks, passages): (Vec<Block>, Vec<Passage>)) -> Result<GraphResult> {
        build(&blocks, &passages)
    }

    #[test]
    fn npmi_is_one_for_phrases_always_together_and_zero_for_chance() {
        assert!((npmi(2, 2, 2, 10) - 1.0).abs() < 1e-12);
        assert!(npmi(4, 5, 2, 10).abs() < 1e-12);
    }

    #[test]
    fn a_book_shorter_than_twenty_passages_is_refused() {
        let error = build_of(fake_passages(&["one passage"; 19]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("the book has 19 passages; a concept graph needs at least 20"),
            "{error}"
        );
    }

    #[test]
    fn a_book_whose_phrases_are_all_too_common_has_an_empty_graph() {
        let g = build_of(fake_passages(&["the same words everywhere"; 30])).unwrap();
        assert!(g.concepts.is_empty() && g.links.is_empty() && g.communities.is_empty());
        assert_eq!(g.passages, 30);
    }

    #[test]
    fn notes_and_apparatus_stay_out_of_the_graph() {
        // 10 passages of index, then 30 of text that each hold 3 of 30 words,
        // and 6 of which also hold a note. Read whole, the index's "zzindex"
        // and the notes' "london" and "routledge" would be concepts.
        let index = ("zzindex, 12", BlockKind::Paragraph, Some(Apparatus::Index));
        let note = ("London: Routledge.", BlockKind::Note { label: None }, None);
        let texts: Vec<String> = (0..30)
            .map(|i| {
                (0..30)
                    .filter(|j| (i + 30 - j) % 30 < 3)
                    .map(word)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect();
        let mut passages: Vec<Vec<Piece>> = vec![vec![index]; 10];
        for (i, text) in texts.iter().enumerate() {
            let mut pieces = vec![(text.as_str(), BlockKind::Paragraph, None)];
            if i % 5 == 0 {
                pieces.push(note.clone());
            }
            passages.push(pieces);
        }

        let g = build_of(fake_book(&passages)).unwrap();

        assert_eq!((g.passages, g.passages_read), (40, 30));
        let labels: Vec<&str> = g.concepts.iter().map(|c| c.label.as_str()).collect();
        for left_out in ["zzindex", "london", "routledge"] {
            assert!(!labels.contains(&left_out), "{left_out}: {labels:?}");
        }
        let first = g.concepts.iter().find(|c| c.label == word(0)).unwrap();
        assert_eq!(first.passages, [10, 11, 12]);
    }

    #[test]
    fn a_book_with_too_little_text_outside_its_apparatus_is_refused() {
        let index = ("apple, 12", BlockKind::Paragraph, Some(Apparatus::Index));
        let mut passages: Vec<Vec<Piece>> = vec![vec![("text", BlockKind::Paragraph, None)]; 15];
        passages.extend(vec![vec![index]; 10]);
        let error = build_of(fake_book(&passages)).unwrap_err().to_string();
        assert!(
            error.contains(
                "15 of the book's 25 passages have text outside notes, contents, \
                 lists, bibliography and index; a concept graph needs at least 20"
            ),
            "{error}"
        );
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
