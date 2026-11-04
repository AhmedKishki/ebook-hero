"""SPIKE: scores the sensitivity runs against the judged pool of the main run.

    python3 -I eval/sensitivity.py RUNS_DIR

Unjudged passages count as not relevant; the share of them in the top 10 is
printed so the bias can be seen.
"""

import json
import math
import random
import statistics
import sys
from pathlib import Path

BASE = {"hybrid+ppr-norerank": "hybrid-norerank", "hybrid+pprfb-norerank": "hybrid-norerank",
        "lexical+ppr": "lexical"}


def ndcg10(ranked, rels):
    dcg = lambda gs: sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(gs))
    ideal = dcg(sorted(rels.values(), reverse=True)[:10])
    return dcg([rels.get(i, 0) for i in ranked[:10]]) / ideal if ideal else 0.0


def randomization(d, rounds=20000):
    rng = random.Random(7)
    observed = abs(sum(d))
    hits = sum(abs(sum(x if rng.random() < 0.5 else -x for x in d)) >= observed - 1e-12
               for _ in range(rounds))
    return (hits + 1) / (rounds + 1)


runs = Path(sys.argv[1])
print("| weight | change | baseline | Δ nDCG@10 | p | unjudged in top 10 |")
print("|---|---|---|---|---|---|")
for w in ["0.25", "0.5", "1.0"]:
    scores, unjudged = {}, {}
    for book in ["alice", "veblen"]:
        qrels = json.loads((runs / book / "qrels.json").read_text())
        src = runs / book if w == "1.0" else runs / f"{book}-w{w}"
        for q in json.loads((src / "run.json").read_text())["queries"]:
            rels = {int(k): v for k, v in qrels[q["id"]].items()}
            for name, r in q["results"].items():
                if name not in BASE and name not in BASE.values():
                    continue
                scores.setdefault(name, []).append(ndcg10(r["top"], rels))
                unjudged.setdefault(name, []).append(
                    sum(i not in rels for i in r["top"][:10]) / max(1, len(r["top"][:10])))
    for name, base in BASE.items():
        d = [a - b for a, b in zip(scores[name], scores[base])]
        print(f"| {w} | {name} | {base} | {statistics.mean(d):+.3f} | {randomization(d):.3f} | "
              f"{statistics.mean(unjudged[name]):.1%} |")
