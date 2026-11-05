//! A book's concept graph: the phrases that mark its topics, which of them
//! share passages more often than chance, and the communities they form. The
//! concept-map crate builds it; this module gives it the types that the MCP
//! tool, `--json` and the desktop window read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::passages::Passage;
use crate::settings::{GRAPH_MIN_BOOK_PASSAGES, GRAPH_PASSAGES_SHOWN};
use crate::{Error, Result};

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
/// built by `concept_map::build`.
pub fn build(passages: &[Passage]) -> Result<GraphResult> {
    let n = passages.len();
    if n < GRAPH_MIN_BOOK_PASSAGES {
        return Err(Error::InvalidArgument(format!(
            "the book has {n} passages; a concept graph needs at least \
             {GRAPH_MIN_BOOK_PASSAGES}, so use search or find on a book this short"
        )));
    }
    let texts: Vec<&str> = passages.iter().map(|p| p.text.as_str()).collect();
    let map = concept_map::build(&texts).map_err(|e| Error::InvalidArgument(e.to_string()))?;
    // The crate numbers passages by their index in `texts`.
    let ids = |indices: Vec<u32>| -> Vec<u32> {
        indices
            .into_iter()
            .map(|i| passages[i as usize].id)
            .collect()
    };
    Ok(GraphResult {
        passages: map.passages,
        topics: map.topics,
        concepts: map
            .concepts
            .into_iter()
            .map(|c| Concept {
                id: c.id,
                label: c.label,
                passage_count: c.passage_count,
                passages: ids(c.passages),
                community: c.community,
            })
            .collect(),
        links: map
            .links
            .into_iter()
            .map(|l| Link {
                a: l.a,
                b: l.b,
                npmi: l.npmi,
                passage_count: l.passage_count,
                passages: ids(l.passages),
            })
            .collect(),
        communities: map
            .communities
            .into_iter()
            .map(|c| Community {
                id: c.id,
                concepts: c.concepts,
            })
            .collect(),
    })
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
}
