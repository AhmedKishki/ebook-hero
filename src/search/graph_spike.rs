//! SPIKE, not part of the tool: the ways the concept graph could take part in
//! search, run side by side by `examples/graph_eval.rs`.
//!
//! 1. A third ranked list for reciprocal rank fusion, after HippoRAG
//!    (Gutiérrez et al. 2024): concepts named in the query seed a personalized
//!    PageRank over the NPMI links, and a passage scores the sum of its
//!    concepts' PageRank. The feedback variant adds seeds from the concepts of
//!    the first-stage top hits.
//! 2. Query expansion with an association thesaurus (Qiu and Frei 1993): the
//!    concepts linked to the query's concepts join the BM25 query as boosted
//!    optional clauses.
//! 3. xQuAD diversification (Santos et al. 2010) of the reranked candidates,
//!    with the communities the query touches as its aspects.
//! 4. Annotation: the concepts in each hit, which changes no ranking.

use std::collections::HashMap;
use std::time::Instant;

use crate::Result;
use crate::analysis::stem_word;
use crate::embed::Models;
use crate::graph::GraphResult;
use crate::graph::phrases::stemmed_runs;
use crate::index::book::Book;
use crate::search::fusion::rrf;
use crate::search::query::parse_query;
use crate::search::{Hit, fuse, lexical_hits, passage, rerank_hits, semantic_hits};
use crate::settings::{
    BM25_WEIGHT, LEXICAL_CANDIDATES, RERANK_CANDIDATES, RRF_K, SEMANTIC_CANDIDATES, VECTOR_WEIGHT,
};

/// How many passages the graph list holds, like the lexical and semantic lists.
pub const GRAPH_CANDIDATES: usize = 50;
/// The graph list's weight in reciprocal rank fusion.
pub const GRAPH_WEIGHT: f32 = 1.0;
/// The probability of following a link rather than jumping back to a seed;
/// HippoRAG uses 0.5.
pub const DAMPING: f64 = 0.5;
pub const PPR_ITERATIONS: usize = 50;
/// How many first-stage hits seed the feedback variant.
pub const FEEDBACK_HITS: usize = 10;
/// An expansion phrase's boost is this times its NPMI with the query concept.
pub const EXPANSION_BOOST: f32 = 0.5;
/// xQuAD's trade-off between relevance (0) and coverage of aspects (1).
pub const XQUAD_LAMBDA: f64 = 0.5;
/// How many passages each variant reports.
pub const DEPTH: usize = 10;

/// The concept graph rearranged for search.
pub struct Concepts {
    pub labels: Vec<String>,
    keys: Vec<Vec<String>>,
    counts: Vec<usize>,
    community: Vec<u32>,
    adjacent: Vec<Vec<(usize, f64)>>,
    in_passage: Vec<Vec<usize>>,
    passages: usize,
}

impl Concepts {
    /// `graph` must be the full graph, not the abridged one.
    pub fn new(graph: &GraphResult) -> Concepts {
        let m = graph.concepts.len();
        let n = graph.passages as usize;
        let mut in_passage = vec![Vec::new(); n];
        for concept in &graph.concepts {
            for &p in &concept.passages {
                in_passage[p as usize].push(concept.id as usize);
            }
        }
        let mut adjacent = vec![Vec::new(); m];
        for link in &graph.links {
            let (a, b, w) = (link.a as usize, link.b as usize, f64::from(link.npmi));
            adjacent[a].push((b, w));
            adjacent[b].push((a, w));
        }
        Concepts {
            labels: graph.concepts.iter().map(|c| c.label.clone()).collect(),
            keys: graph
                .concepts
                .iter()
                .map(|c| c.label.split_whitespace().map(stem_word).collect())
                .collect(),
            counts: graph
                .concepts
                .iter()
                .map(|c| c.passage_count as usize)
                .collect(),
            community: graph.concepts.iter().map(|c| c.community).collect(),
            adjacent,
            in_passage,
            passages: n,
        }
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// The concepts that stand in passage `id`.
    pub fn of_passage(&self, id: u32) -> &[usize] {
        self.in_passage.get(id as usize).map_or(&[], Vec::as_slice)
    }

    /// The share of the book's passages that hold at least one concept.
    pub fn coverage(&self) -> f64 {
        let covered = self.in_passage.iter().filter(|c| !c.is_empty()).count();
        covered as f64 / self.passages.max(1) as f64
    }

    /// A concept's specificity: rarer concepts weigh more as seeds.
    fn idf(&self, c: usize) -> f64 {
        (self.passages as f64 / self.counts[c].max(1) as f64).ln()
    }

    /// The concepts whose stemmed words occur, in order and adjacent, inside
    /// one run of the query's content words, each weighted by its idf.
    pub fn query_seeds(&self, query: &str) -> Vec<(usize, f64)> {
        let runs = stemmed_runs(query);
        (0..self.len())
            .filter(|&c| {
                let key = &self.keys[c];
                runs.iter()
                    .any(|run| run.len() >= key.len() && run.windows(key.len()).any(|w| w == key))
            })
            .map(|c| (c, self.idf(c)))
            .collect()
    }

    /// The concepts of the first `FEEDBACK_HITS` of `ids`, each weighted by
    /// its idf over the rank of every hit it stands in.
    pub fn feedback_seeds(&self, ids: &[u32]) -> Vec<(usize, f64)> {
        let mut weights: HashMap<usize, f64> = HashMap::new();
        for (rank, &id) in (1..).zip(ids.iter().take(FEEDBACK_HITS)) {
            for &c in self.of_passage(id) {
                *weights.entry(c).or_default() += self.idf(c) / f64::from(rank);
            }
        }
        let mut seeds: Vec<(usize, f64)> = weights.into_iter().collect();
        seeds.sort_by_key(|s| s.0);
        seeds
    }

    /// Personalized PageRank from `seeds` over the NPMI-weighted links. A
    /// concept with no links sends its mass back to the seeds.
    pub fn pagerank(&self, seeds: &[(usize, f64)]) -> Vec<f64> {
        let m = self.len();
        let reset = normalized(seeds, m);
        let out: Vec<f64> = self
            .adjacent
            .iter()
            .map(|a| a.iter().map(|x| x.1).sum())
            .collect();
        let mut p = reset.clone();
        for _ in 0..PPR_ITERATIONS {
            let mut next = vec![0.0; m];
            let mut dangling = 0.0;
            for i in 0..m {
                if out[i] == 0.0 {
                    dangling += p[i];
                    continue;
                }
                for &(j, w) in &self.adjacent[i] {
                    next[j] += DAMPING * p[i] * w / out[i];
                }
            }
            for i in 0..m {
                next[i] += ((1.0 - DAMPING) + DAMPING * dangling) * reset[i];
            }
            p = next;
        }
        p
    }

    /// The passages ranked by the summed PageRank of their concepts, best
    /// first and lowest id first among equals; those with none are left out.
    pub fn graph_list(&self, rank: &[f64], limit: usize) -> Vec<u32> {
        let mut scored: Vec<(u32, f64)> = (0..self.passages as u32)
            .map(|id| (id, self.of_passage(id).iter().map(|&c| rank[c]).sum()))
            .filter(|&(_, s)| s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(limit);
        scored.into_iter().map(|s| s.0).collect()
    }

    /// The labels linked to the seeds that are not seeds themselves, each
    /// boosted by `EXPANSION_BOOST` times its highest NPMI with a seed.
    pub fn expansions(&self, seeds: &[(usize, f64)]) -> Vec<(String, f32)> {
        let mut best: HashMap<usize, f64> = HashMap::new();
        for &(s, _) in seeds {
            for &(c, w) in &self.adjacent[s] {
                if seeds.iter().all(|x| x.0 != c) {
                    let e = best.entry(c).or_default();
                    *e = e.max(w);
                }
            }
        }
        let mut out: Vec<(usize, f64)> = best.into_iter().collect();
        out.sort_by_key(|x| x.0);
        out.into_iter()
            .map(|(c, w)| (self.labels[c].clone(), EXPANSION_BOOST * w as f32))
            .collect()
    }

    /// xQuAD: picks `k` of `hits`, trading their (min-max normalized) score
    /// against covering the communities in `aspects` not yet covered.
    pub fn diversify(&self, hits: &[Hit], aspects: &[(usize, f64)], k: usize) -> Vec<u32> {
        let mut weight: HashMap<u32, f64> = HashMap::new();
        for &(c, w) in aspects {
            *weight.entry(self.community[c]).or_default() += w;
        }
        let total: f64 = weight.values().sum();
        if total == 0.0 {
            return hits.iter().take(k).map(|h| h.passage_id).collect();
        }
        let (lo, hi) = hits.iter().fold((f32::MAX, f32::MIN), |(lo, hi), h| {
            (lo.min(h.score), hi.max(h.score))
        });
        let relevance = |h: &Hit| {
            if hi > lo {
                f64::from((h.score - lo) / (hi - lo))
            } else {
                1.0
            }
        };
        // How much of a passage's concepts fall in each community.
        let share = |id: u32, a: u32| {
            let cs = self.of_passage(id);
            if cs.is_empty() {
                0.0
            } else {
                cs.iter().filter(|&&c| self.community[c] == a).count() as f64 / cs.len() as f64
            }
        };
        let mut uncovered: HashMap<u32, f64> = weight.keys().map(|&a| (a, 1.0)).collect();
        let mut left: Vec<&Hit> = hits.iter().collect();
        let mut chosen = Vec::new();
        while chosen.len() < k && !left.is_empty() {
            let gain = |h: &Hit| {
                let diversity: f64 = weight
                    .iter()
                    .map(|(&a, &w)| w / total * share(h.passage_id, a) * uncovered[&a])
                    .sum();
                (1.0 - XQUAD_LAMBDA) * relevance(h) + XQUAD_LAMBDA * diversity
            };
            let (best, _) = left
                .iter()
                .enumerate()
                .map(|(i, h)| (i, gain(h)))
                .fold((0, f64::MIN), |acc, x| if x.1 > acc.1 { x } else { acc });
            let hit = left.remove(best);
            for (&a, u) in &mut uncovered {
                *u *= 1.0 - share(hit.passage_id, a);
            }
            chosen.push(hit.passage_id);
        }
        chosen
    }
}

fn normalized(seeds: &[(usize, f64)], m: usize) -> Vec<f64> {
    let mut v = vec![0.0; m];
    let total: f64 = seeds.iter().map(|s| s.1).sum();
    if total > 0.0 {
        for &(c, w) in seeds {
            v[c] += w / total;
        }
    }
    v
}

/// Two seed sets mixed half and half, each normalized first.
fn mixed(a: &[(usize, f64)], b: &[(usize, f64)], m: usize) -> Vec<(usize, f64)> {
    let (x, y) = (normalized(a, m), normalized(b, m));
    (0..m)
        .map(|c| (c, x[c] + y[c]))
        .filter(|s| s.1 > 0.0)
        .collect()
}

/// The search variants the experiment compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Lexical,
    Semantic,
    HybridNoRerank,
    Hybrid,
    LexicalPpr,
    HybridPprNoRerank,
    HybridPpr,
    HybridFeedbackNoRerank,
    HybridFeedback,
    LexicalExpand,
    HybridExpand,
    HybridDiversify,
}

impl Variant {
    pub const ALL: [Variant; 12] = [
        Variant::Lexical,
        Variant::Semantic,
        Variant::HybridNoRerank,
        Variant::Hybrid,
        Variant::LexicalPpr,
        Variant::HybridPprNoRerank,
        Variant::HybridPpr,
        Variant::HybridFeedbackNoRerank,
        Variant::HybridFeedback,
        Variant::LexicalExpand,
        Variant::HybridExpand,
        Variant::HybridDiversify,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Variant::Lexical => "lexical",
            Variant::Semantic => "semantic",
            Variant::HybridNoRerank => "hybrid-norerank",
            Variant::Hybrid => "hybrid",
            Variant::LexicalPpr => "lexical+ppr",
            Variant::HybridPprNoRerank => "hybrid+ppr-norerank",
            Variant::HybridPpr => "hybrid+ppr",
            Variant::HybridFeedbackNoRerank => "hybrid+pprfb-norerank",
            Variant::HybridFeedback => "hybrid+pprfb",
            Variant::LexicalExpand => "lexical+expand",
            Variant::HybridExpand => "hybrid+expand",
            Variant::HybridDiversify => "hybrid+diversify",
        }
    }
}

/// What one variant returned for one query.
pub struct Outcome {
    /// The first `DEPTH` passage ids.
    pub top: Vec<u32>,
    /// The first `RERANK_CANDIDATES` ids before reranking, or before the cut
    /// when the variant does not rerank.
    pub candidates: Vec<u32>,
    pub millis: f64,
}

/// Runs `variant` for `query`.
pub fn run(
    book: &Book,
    models: &Models,
    concepts: &Concepts,
    query: &str,
    variant: Variant,
) -> Result<Outcome> {
    let started = Instant::now();
    let parsed = parse_query(query);
    let nothing = |_: usize, _: usize| {};
    let lexical = || lexical_hits(book, &parsed, LEXICAL_CANDIDATES);
    let semantic = || semantic_hits(book, models, query, SEMANTIC_CANDIDATES, &nothing);
    let expanded = || -> Result<Vec<Hit>> {
        let extra = concepts.expansions(&concepts.query_seeds(query));
        let found = book.lexical.search_with(&parsed, &extra, LEXICAL_CANDIDATES)?;
        hits_of(book, found.into_iter().map(|f| f.0))
    };
    let rerank = |hits: Vec<Hit>| rerank_hits(hits, |docs| models.rerank(query, docs));
    let with_graph = |lexical: &[Hit], semantic: &[Hit], seeds: &[(usize, f64)]| {
        let graph = concepts.graph_list(&concepts.pagerank(seeds), GRAPH_CANDIDATES);
        let lists: Vec<(Vec<u32>, f32)> = vec![
            (ids(lexical), BM25_WEIGHT),
            (ids(semantic), VECTOR_WEIGHT),
            (graph, graph_weight()),
        ];
        let borrowed: Vec<(&[u32], f32)> = lists
            .iter()
            .filter(|l| !l.0.is_empty())
            .map(|(l, w)| (l.as_slice(), *w))
            .collect();
        hits_of(book, rrf(&borrowed, RRF_K).into_iter().map(|f| f.0))
    };

    let (candidates, hits) = match variant {
        Variant::Lexical => (lexical()?, None),
        Variant::Semantic => (semantic()?, None),
        Variant::HybridNoRerank => (fuse(lexical()?, semantic()?), None),
        Variant::Hybrid => {
            let fused = fuse(lexical()?, semantic()?);
            (fused.clone(), Some(rerank(fused)?))
        }
        Variant::LexicalPpr => (
            with_graph(&lexical()?, &[], &concepts.query_seeds(query))?,
            None,
        ),
        Variant::HybridPprNoRerank | Variant::HybridPpr => {
            let fused = with_graph(&lexical()?, &semantic()?, &concepts.query_seeds(query))?;
            let reranked = (variant == Variant::HybridPpr)
                .then(|| rerank(fused.clone()))
                .transpose()?;
            (fused, reranked)
        }
        Variant::HybridFeedbackNoRerank | Variant::HybridFeedback => {
            let (l, s) = (lexical()?, semantic()?);
            let first = fuse(l.clone(), s.clone());
            let seeds = mixed(
                &concepts.query_seeds(query),
                &concepts.feedback_seeds(&ids(&first)),
                concepts.len(),
            );
            let fused = with_graph(&l, &s, &seeds)?;
            let reranked = (variant == Variant::HybridFeedback)
                .then(|| rerank(fused.clone()))
                .transpose()?;
            (fused, reranked)
        }
        Variant::LexicalExpand => (expanded()?, None),
        Variant::HybridExpand => {
            let fused = fuse(expanded()?, semantic()?);
            (fused.clone(), Some(rerank(fused)?))
        }
        Variant::HybridDiversify => {
            let fused = fuse(lexical()?, semantic()?);
            let reranked = rerank(fused.clone())?;
            let window = &reranked[..reranked.len().min(RERANK_CANDIDATES)];
            let mut aspects = concepts.query_seeds(query);
            if aspects.is_empty() {
                aspects = concepts.feedback_seeds(&ids(window));
            }
            let top = concepts.diversify(window, &aspects, DEPTH);
            return Ok(Outcome {
                top,
                candidates: ids(&fused).into_iter().take(RERANK_CANDIDATES).collect(),
                millis: started.elapsed().as_secs_f64() * 1000.0,
            });
        }
    };
    let ranked = hits.as_ref().unwrap_or(&candidates);
    Ok(Outcome {
        top: ids(ranked).into_iter().take(DEPTH).collect(),
        candidates: ids(&candidates).into_iter().take(RERANK_CANDIDATES).collect(),
        millis: started.elapsed().as_secs_f64() * 1000.0,
    })
}

/// `GRAPH_WEIGHT`, or `$SPIKE_GRAPH_WEIGHT` for the sensitivity runs.
fn graph_weight() -> f32 {
    std::env::var("SPIKE_GRAPH_WEIGHT")
        .ok()
        .and_then(|w| w.parse().ok())
        .unwrap_or(GRAPH_WEIGHT)
}

fn ids(hits: &[Hit]) -> Vec<u32> {
    hits.iter().map(|h| h.passage_id).collect()
}

/// Bare hits for `ids`: their text, for the reranker, and nothing else.
fn hits_of(book: &Book, ids: impl Iterator<Item = u32>) -> Result<Vec<Hit>> {
    ids.map(|id| {
        let p = passage(book, id)?;
        Ok(Hit {
            passage_id: id,
            text: p.text.clone(),
            section: p.section.clone(),
            locator: p.locator.clone(),
            matches: Vec::new(),
            score: 0.0,
            lexical_rank: None,
            semantic_rank: None,
        })
    })
    .collect()
}
