# Concept graph in search: spike results

Run on 2026-11-04 on branch `worktree-graph-search-spike`. Nothing here is wired into the CLI, MCP or window.

## What was compared

Twelve variants of search, all built from the same lexical, semantic and reranker parts as `hybrid` today ([src/search/graph_spike.rs](../src/search/graph_spike.rs)):

| variant | what it does | prior art |
|---|---|---|
| `lexical`, `semantic`, `hybrid-norerank`, `hybrid` | today's pipeline without and with its parts; `hybrid` is the default and the baseline | |
| `lexical+ppr`, `hybrid+ppr(-norerank)` | a third RRF list (weight 1.0, 50 passages): the concepts named in the query seed a personalized PageRank (damping 0.5) over the NPMI links; a passage scores the summed PageRank of its concepts | HippoRAG (Gutiérrez et al. 2024) |
| `hybrid+pprfb(-norerank)` | the same, with half the seed mass from the concepts of the first-stage top 10 | HippoRAG 2's passage seeds; pseudo-relevance feedback |
| `lexical+expand`, `hybrid+expand` | concepts linked to the query's concepts join the BM25 query as optional phrases, boosted 0.5 × NPMI | association-thesaurus expansion (Qiu and Frei 1993) |
| `hybrid+diversify` | xQuAD over the reranked 30, with the communities the query touches as aspects, λ = 0.5 | Santos et al. 2010 |

Annotating each hit with its concepts, which changes no ranking, was measured by coverage.

## Method

- Books: `tests/fixtures/alice.epub` (166 passages) and Veblen's *The Theory of the Leisure Class* from Project Gutenberg (670 passages; not committed).
- 30 queries per book, written before either graph was looked at ([queries/](queries/)): 8 keyword, 8 question, 8 paraphrase (other words than the book's), 6 broad theme.
- Pooling as in TREC: the union of every variant's top 10 per query, 1,242 passages in all, graded 0/1/2 by blind LLM judges (Claude Sonnet) that saw only the query and the passage, shuffled ([qrels/](qrels/)).
- A second judge regraded 10 queries (205 passages): exact agreement 86%, relevant-or-not agreement 95%, linear-weighted κ = 0.85.
- Significance: paired sign-flip randomization test on per-query nDCG@10, 20,000 rounds.

## Results, both books (60 queries)

| variant | nDCG@10 | strict nDCG@10 (grade 2 only) | P@5 | R@30 of reranker window | median ms |
|---|---|---|---|---|---|
| `hybrid` (default) | 0.806 | 0.754 | 0.903 | 0.947 | 970 |
| `hybrid-norerank` | **0.850** | **0.812** | 0.903 | 0.947 | 17 |
| `hybrid+ppr` | 0.807 | 0.754 | 0.900 | 0.932 | 955 |
| `hybrid+pprfb` | 0.810 | 0.763 | 0.897 | 0.921 | 1018 |
| `hybrid+expand` | 0.809 | 0.757 | 0.903 | 0.950 | 977 |
| `hybrid+diversify` | 0.799 | 0.744 | 0.890 | 0.947 | 956 |
| `hybrid+ppr-norerank` | 0.799 | 0.755 | 0.850 | 0.932 | 18 |
| `hybrid+pprfb-norerank` | 0.814 | 0.771 | 0.863 | 0.921 | 18 |
| `semantic` | 0.770 | 0.752 | 0.820 | 0.765 | 13 |
| `lexical` | 0.768 | 0.710 | 0.863 | 0.868 | 5 |
| `lexical+expand` | 0.784 | 0.732 | 0.863 | 0.870 | 0 |
| `lexical+ppr` | 0.714 | 0.654 | 0.807 | 0.866 | 5 |

Each change against the pipeline it changes:

| change | baseline | Δ nDCG@10 | p | wins/ties/losses | Δ on the 40 queries naming a concept |
|---|---|---|---|---|---|
| drop the reranker | `hybrid` | +0.044 | 0.006 | 36/3/21 | +0.021 |
| expansion | `lexical` | +0.016 | 0.028 | 22/26/12 | +0.024 (p 0.027) |
| PageRank list | `lexical` | −0.053 | 0.082 | 23/21/16 | −0.080 |
| PageRank list | `hybrid-norerank` | −0.051 | <0.001 | 11/24/25 | −0.077 |
| PageRank list + feedback | `hybrid-norerank` | −0.036 | 0.007 | 24/4/32 | −0.053 |
| PageRank list | `hybrid` | +0.001 | 0.88 | 9/42/9 | +0.001 |
| PageRank list + feedback | `hybrid` | +0.004 | 0.61 | 15/31/14 | +0.009 |
| expansion | `hybrid` | +0.003 | 0.19 | 4/54/2 | +0.005 |
| diversification | `hybrid` | −0.007 | 0.31 | 18/24/18 | −0.013 |

nDCG@10 by query type:

| variant | keyword (16) | question (16) | paraphrase (16) | broad (12) |
|---|---|---|---|---|
| `hybrid` | 0.862 | 0.796 | 0.835 | 0.707 |
| `hybrid-norerank` | 0.940 | 0.796 | 0.879 | 0.762 |
| `hybrid+pprfb` | 0.856 | 0.787 | 0.865 | 0.705 |
| `hybrid+pprfb-norerank` | 0.906 | 0.706 | 0.859 | 0.774 |
| `lexical` | 0.925 | 0.732 | 0.723 | 0.666 |
| `lexical+expand` | 0.931 | 0.749 | 0.741 | 0.691 |
| `lexical+ppr` | 0.919 | 0.527 | 0.712 | 0.695 |

The weight of the PageRank list in RRF, without the reranker (unjudged passages in the top 10: under 1%):

| weight | `+ppr` vs `hybrid-norerank` | `+pprfb` vs `hybrid-norerank` | `lexical+ppr` vs `lexical` |
|---|---|---|---|
| 0.25 | −0.010 | −0.007 | −0.017 |
| 0.5 | −0.025 | −0.019 | −0.040 |
| 1.0 | −0.051 | −0.036 | −0.053 |

The graph itself: 60 concepts per book, which stand in 99% (Alice) and 97% (Veblen) of passages, 4.5 concepts per top-5 hit. 40 of 60 queries name at least one concept, often a generic one ("class", "culture", "work", "door", "size").

## What the numbers say

1. No graph integration improves on the default. With the reranker, every graph variant is within ±0.01 of `hybrid`, because the graph can only change which 30 passages the reranker sees, and those already hold 95% of the relevant ones.
2. Without the reranker, the PageRank list makes search worse at every weight tried, and most on questions (0.796 → 0.662). Generic seed concepts pull in passages that only share a common word.
3. Expanding the BM25 query with linked concepts is the one change that helps: +0.016 nDCG@10 on lexical search (p = 0.028), +0.024 on queries that name a concept. Inside hybrid it vanishes (+0.003).
4. Diversifying by community costs a little (−0.007) and is not significant.
5. Annotating hits with concepts adds nothing to find: 98% of hits hold a concept, 4.5 each.
6. Not about the graph: dropping the reranker raised nDCG@10 by 0.044 (p = 0.006) and cut the median time from 970 ms to 17 ms. The gain is largest on keyword (+0.078) and broad (+0.055) queries.

## How far to trust them

- 60 queries over two books. Differences under about 0.03 nDCG@10 are within judge noise: on the 10 regraded queries the order of the hybrid variants changed between judges, while hybrid above semantic above lexical held under both.
- The judges are an LLM, not people. The reranker finding (6) contradicts the README's account of the reranker; it should be checked with human grades on a few queries, or another book, before the default changes.
- HippoRAG's gains come from multi-hop questions across many documents, with thousands of entities. A single book with 60 concepts and single-hop questions gives PageRank little to add that BM25 and the embeddings do not already find.

## Reproducing

```sh
cargo build --release --example graph_eval
./target/release/examples/graph_eval BOOK eval/queries/NAME.json RUN_DIR
python3 -I eval/pool.py RUN_DIR N            # then grade RUN_DIR/judge-*.json into qrels-*.json
python3 -I eval/merge.py RUN_DIR             # or copy eval/qrels/NAME.json to RUN_DIR/qrels.json
python3 -I eval/metrics.py RUN_DIR [RUN_DIR ...]
```
