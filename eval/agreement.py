"""SPIKE: checks the judges against a second judge.

    python3 -I eval/agreement.py make OUT_DIR RUN_DIR:QID,QID,... [RUN_DIR:...]
    python3 -I eval/agreement.py score OUT_DIR RUN_DIR [RUN_DIR ...]

`make` copies the named queries' pooled passages, reshuffled, into
OUT_DIR/judge-x.json. `score` compares OUT_DIR/qrels-x.json with each
RUN_DIR/qrels.json: linear-weighted Cohen's kappa over the shared grades, and
whether the variants rank the same by mean nDCG@10 on those queries under
either judge.
"""

import json
import math
import random
import sys
from pathlib import Path


def make(out, specs):
    batch = []
    for spec in specs:
        run_dir, ids = spec.split(":")
        wanted = set(ids.split(","))
        for f in sorted(Path(run_dir).glob("judge-*.json")):
            for q in json.loads(f.read_text()):
                if q["query_id"] in wanted:
                    q["passages"].sort(key=lambda p: p["id"])
                    random.Random("x" + q["query_id"]).shuffle(q["passages"])
                    batch.append(q)
    out.mkdir(parents=True, exist_ok=True)
    (out / "judge-x.json").write_text(json.dumps(batch, ensure_ascii=False, indent=1))
    print(f"{len(batch)} queries, {sum(len(q['passages']) for q in batch)} passages")


def kappa(pairs):
    cats = [0, 1, 2]
    n = len(pairs)
    w = lambda a, b: 1 - abs(a - b) / 2
    po = sum(w(a, b) for a, b in pairs) / n
    pa = [sum(a == c for a, _ in pairs) / n for c in cats]
    pb = [sum(b == c for _, b in pairs) / n for c in cats]
    pe = sum(w(i, j) * pa[i] * pb[j] for i in cats for j in cats)
    return (po - pe) / (1 - pe)


def ndcg10(ranked, rels):
    dcg = lambda gs: sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(gs))
    ideal = dcg(sorted(rels.values(), reverse=True)[:10])
    return dcg([rels.get(i, 0) for i in ranked[:10]]) / ideal if ideal else 0.0


def score(out, run_dirs):
    second = json.loads((out / "qrels-x.json").read_text())
    pairs, queries = [], []
    for run_dir in map(Path, run_dirs):
        first = json.loads((run_dir / "qrels.json").read_text())
        run = json.loads((run_dir / "run.json").read_text())
        for q in run["queries"]:
            if q["id"] in second:
                a = {int(k): v for k, v in first[q["id"]].items()}
                b = {int(k): v for k, v in second[q["id"]].items()}
                pairs += [(a[k], b[k]) for k in a if k in b]
                queries.append((q, a, b))
    exact = sum(x == y for x, y in pairs) / len(pairs)
    binary = sum((x >= 1) == (y >= 1) for x, y in pairs) / len(pairs)
    print(f"{len(queries)} queries, {len(pairs)} shared grades")
    print(f"exact agreement {exact:.0%}, relevant-or-not agreement {binary:.0%}, "
          f"weighted kappa {kappa(pairs):.2f}")
    names = list(queries[0][0]["results"])
    for label, idx in (("first judges", 1), ("second judge", 2)):
        means = {
            n: sum(ndcg10(q["results"][n]["top"], rels[idx - 1]) for q, *rels in queries) / len(queries)
            for n in names
        }
        order = sorted(names, key=lambda n: -means[n])
        print(f"{label}: " + ", ".join(f"{n} {means[n]:.3f}" for n in order))


if sys.argv[1] == "make":
    make(Path(sys.argv[2]), sys.argv[3:])
else:
    score(Path(sys.argv[2]), sys.argv[3:])
