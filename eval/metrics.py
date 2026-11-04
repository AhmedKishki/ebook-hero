"""SPIKE: scores every variant of a run against the judged pool.

    python3 -I eval/metrics.py RUN_DIR [RUN_DIR ...]

Each RUN_DIR holds run.json and qrels.json ({query_id: {passage_id: 0|1|2}}).
Prints Markdown tables per book and for all books together.
"""

import json
import math
import random
import statistics
import sys
from pathlib import Path

BASELINE = "hybrid"
TYPES = ["keyword", "question", "paraphrase", "broad"]


def dcg(gains):
    return sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(gains))


def ndcg(ranked, rels, k):
    ideal = dcg(sorted(rels.values(), reverse=True)[:k])
    return dcg([rels.get(i, 0) for i in ranked[:k]]) / ideal if ideal else 0.0


def scores(query, rels):
    relevant = {i for i, g in rels.items() if g >= 1}
    out = {}
    for name, r in query["results"].items():
        top, cands = r["top"], r["candidates"]
        first = next((n for n, i in enumerate(top, 1) if i in relevant), None)
        strict = {i: g for i, g in rels.items() if g == 2}
        out[name] = {
            "ndcg5": ndcg(top, rels, 5),
            "ndcg10": ndcg(top, rels, 10),
            "strict10": ndcg(top, strict, 10),
            "p5": sum(i in relevant for i in top[:5]) / 5,
            "r10": len(relevant & set(top[:10])) / len(relevant) if relevant else 0.0,
            "r30": len(relevant & set(cands[:30])) / len(relevant) if relevant else 0.0,
            "mrr": 1 / first if first else 0.0,
            "ms": r["ms"],
            "same_top10": top[:10] == query["results"][BASELINE]["top"][:10],
        }
    return out


def randomization(a, b, rounds=20000, seed=7):
    """Two-sided paired sign-flip test of mean(a - b) = 0."""
    d = [x - y for x, y in zip(a, b)]
    observed = abs(sum(d))
    rng = random.Random(seed)
    hits = sum(
        abs(sum(x if rng.random() < 0.5 else -x for x in d)) >= observed - 1e-12
        for _ in range(rounds)
    )
    return (hits + 1) / (rounds + 1)


def load(run_dir):
    run = json.loads((run_dir / "run.json").read_text())
    qrels = json.loads((run_dir / "qrels.json").read_text())
    rows = []
    for q in run["queries"]:
        rels = {int(k): v for k, v in qrels[q["id"]].items()}
        rows.append({"q": q, "rels": rels, "scores": scores(q, rels)})
    return run, rows


def table(rows, title):
    names = list(rows[0]["scores"])
    print(f"\n### {title} ({len(rows)} queries)\n")
    print("| variant | nDCG@5 | nDCG@10 | strict nDCG@10 | P@5 | R@10 | R@30 cand. | MRR | Δ nDCG@10 vs hybrid | p | same top 10 | median ms |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    base = [r["scores"][BASELINE]["ndcg10"] for r in rows]
    for name in names:
        col = lambda m: [r["scores"][name][m] for r in rows]
        mean = lambda m: statistics.mean(col(m))
        delta = mean("ndcg10") - statistics.mean(base)
        p = randomization(col("ndcg10"), base) if name != BASELINE else float("nan")
        same = sum(col("same_top10"))
        print(f"| {name} | {mean('ndcg5'):.3f} | {mean('ndcg10'):.3f} | {mean('strict10'):.3f} | {mean('p5'):.3f} | "
              f"{mean('r10'):.3f} | {mean('r30'):.3f} | {mean('mrr'):.3f} | "
              f"{delta:+.3f} | {p:.3f} | {same}/{len(rows)} | {statistics.median(col('ms')):.0f} |")


def by_type(rows, title):
    names = list(rows[0]["scores"])
    print(f"\n### {title}: nDCG@10 by query type\n")
    counts = {t: sum(r["q"]["type"] == t for r in rows) for t in TYPES}
    print("| variant | " + " | ".join(f"{t} ({counts[t]})" for t in TYPES) + " |")
    print("|---|" + "---|" * len(TYPES))
    for name in names:
        cells = []
        for t in TYPES:
            vals = [r["scores"][name]["ndcg10"] for r in rows if r["q"]["type"] == t]
            cells.append(f"{statistics.mean(vals):.3f}" if vals else "–")
        print(f"| {name} | " + " | ".join(cells) + " |")


def graph_stats(run, rows, title):
    book = run["book"]
    seeded = [r for r in rows if r["q"]["seeds"]]
    print(f"\n### {title}: the graph itself\n")
    print(f"- {book['passages']} passages, {book['concepts']} concepts, {book['links']} links, "
          f"{book['communities']} communities; {book['coverage']:.0%} of passages hold a concept")
    print(f"- queries naming at least one concept: {len(seeded)}/{len(rows)} "
          + "(" + ", ".join(f"{t} {sum(r['q']['type'] == t for r in seeded)}/"
                            f"{sum(r['q']['type'] == t for r in rows)}" for t in TYPES) + ")")
    hits = [c for r in rows for c in r["q"]["hybrid_top5_concepts"]]
    with_one = sum(bool(c) for c in hits)
    print(f"- hybrid top-5 hits holding a concept: {with_one}/{len(hits)} "
          f"({with_one / len(hits):.0%}), {statistics.mean(len(c) for c in hits):.2f} concepts per hit")
    judged = [len(r["rels"]) for r in rows]
    relevant = [sum(g >= 1 for g in r["rels"].values()) for r in rows]
    print(f"- judged per query: {statistics.mean(judged):.1f}; relevant (grade ≥ 1) per query: "
          f"{statistics.mean(relevant):.1f} (median {statistics.median(relevant)})")


PAIRS = [
    ("hybrid-norerank", "hybrid"),
    ("lexical+expand", "lexical"),
    ("lexical+ppr", "lexical"),
    ("hybrid+ppr-norerank", "hybrid-norerank"),
    ("hybrid+pprfb-norerank", "hybrid-norerank"),
    ("hybrid+ppr", "hybrid"),
    ("hybrid+pprfb", "hybrid"),
    ("hybrid+expand", "hybrid"),
    ("hybrid+diversify", "hybrid"),
]


def pairs(rows, title):
    print(f"\n### {title}: each change against its own baseline, nDCG@10\n")
    print("| change | baseline | Δ all | p | wins/ties/losses | Δ on queries naming a concept | p |")
    print("|---|---|---|---|---|---|---|")
    seeded = [r for r in rows if r["q"]["seeds"]]
    for name, base in PAIRS:
        def delta(rs):
            a = [r["scores"][name]["ndcg10"] for r in rs]
            b = [r["scores"][base]["ndcg10"] for r in rs]
            return statistics.mean(a) - statistics.mean(b), randomization(a, b), a, b
        d, p, a, b = delta(rows)
        w = sum(x > y + 1e-9 for x, y in zip(a, b))
        losses = sum(y > x + 1e-9 for x, y in zip(a, b))
        ds, ps, _, _ = delta(seeded)
        print(f"| {name} | {base} | {d:+.3f} | {p:.3f} | {w}/{len(a) - w - losses}/{losses} | "
              f"{ds:+.3f} ({len(seeded)}) | {ps:.3f} |")


def main():
    everything = []
    for arg in sys.argv[1:]:
        run, rows = load(Path(arg))
        name = Path(run["book"]["path"]).stem
        table(rows, name)
        by_type(rows, name)
        graph_stats(run, rows, name)
        everything += rows
    if len(sys.argv) > 2:
        table(everything, "all books")
        by_type(everything, "all books")
        pairs(everything, "all books")


main()
