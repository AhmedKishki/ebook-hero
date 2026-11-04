"""SPIKE: pools every variant's top passages per query for blind judging.

    python3 -I eval/pool.py RUN_DIR BATCHES

Reads RUN_DIR/run.json and RUN_DIR/passages.json and writes
RUN_DIR/judge-N.json, BATCHES files that each hold some of the queries, with
the union of the variants' top passages in a shuffled order and no word of
which variant found them.
"""

import json
import random
import sys
from pathlib import Path


def main():
    run_dir, batches = Path(sys.argv[1]), int(sys.argv[2])
    run = json.loads((run_dir / "run.json").read_text())
    passages = {p["id"]: p for p in json.loads((run_dir / "passages.json").read_text())}
    pooled = []
    for query in run["queries"]:
        ids = sorted({i for r in query["results"].values() for i in r["top"]})
        random.Random(query["id"]).shuffle(ids)
        pooled.append({
            "query_id": query["id"],
            "query": query["q"],
            "passages": [
                {"id": i, "section": passages[i]["section"], "text": passages[i]["text"]}
                for i in ids
            ],
        })
    total = sum(len(q["passages"]) for q in pooled)
    print(f"{len(pooled)} queries, {total} passages to judge, "
          f"{total / len(pooled):.1f} per query")
    # Whole queries per batch, balanced by passage count.
    loads = [[] for _ in range(batches)]
    for q in sorted(pooled, key=lambda q: -len(q["passages"])):
        min(loads, key=lambda b: sum(len(x["passages"]) for x in b)).append(q)
    for n, batch in enumerate(loads, 1):
        batch.sort(key=lambda q: q["query_id"])
        (run_dir / f"judge-{n}.json").write_text(json.dumps(batch, ensure_ascii=False, indent=1))
        print(f"judge-{n}.json: {len(batch)} queries, "
              f"{sum(len(q['passages']) for q in batch)} passages")


main()
