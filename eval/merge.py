"""SPIKE: merges RUN_DIR/qrels-N.json into RUN_DIR/qrels.json and checks that
every pooled passage of every query was graded 0, 1 or 2.

    python3 -I eval/merge.py RUN_DIR
"""

import json
import sys
from pathlib import Path

run_dir = Path(sys.argv[1])
merged = {}
for f in sorted(run_dir.glob("qrels-*.json")):
    for qid, grades in json.loads(f.read_text()).items():
        merged.setdefault(qid, {}).update({str(k): v for k, v in grades.items()})
problems = []
for f in sorted(run_dir.glob("judge-*.json")):
    for q in json.loads(f.read_text()):
        grades = merged.get(q["query_id"], {})
        for p in q["passages"]:
            g = grades.get(str(p["id"]))
            if g not in (0, 1, 2):
                problems.append(f"{q['query_id']} passage {p['id']}: {g!r}")
if problems:
    sys.exit("ungraded or invalid:\n" + "\n".join(problems))
(run_dir / "qrels.json").write_text(json.dumps(merged, indent=1, sort_keys=True))
counts = [0, 0, 0]
for grades in merged.values():
    for g in grades.values():
        counts[g] += 1
print(f"{len(merged)} queries, {sum(counts)} grades: 0={counts[0]} 1={counts[1]} 2={counts[2]}")
