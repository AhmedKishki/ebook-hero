"""Score ebook-hero's furniture lines against docling's page headers and footers.

usage: python3 -I score.py books.tsv BOOK_DIR DOCLING_DIR EBOOK_HERO
                           [--verdicts verdicts.tsv] [--disagreements out.tsv]

For each book, runs `EBOOK_HERO extract --format json` in a fresh index cache and
compares the furniture lines on the sampled pages with docling's `page_header`
and `page_footer` items (from docling_label.py). A line and an item match when
their boxes share at least half of the smaller box, when the line's centre lies
in the item's box grown by 2 points, or, for rotated text whose boxes disagree,
when one's letters and digits contain the other's (four or more, not a bare
number).

Prints per book and in total: precision (furniture lines that match a docling
item) and recall (docling items matched by a furniture line). With --verdicts,
also prints the scores after the judged disagreements: a line judged furniture
counts as correct and joins the reference, a docling item judged text leaves it.
--disagreements writes every disagreement, with its text, for judging; that file
quotes the books and is not for publishing.
"""
import hashlib, json, os, subprocess, sys, tempfile

books, book_dir, docling_dir, binary = sys.argv[1:5]
options = dict(zip(sys.argv[5::2], sys.argv[6::2]))
FURNITURE = {"page_header", "page_footer"}
PAD = 2.0


def words(text):
    return " ".join("".join(c if c.isalnum() else " " for c in text.lower()).split())


def key(sha, page, text):
    digest = hashlib.sha1(words(text)[:60].encode()).hexdigest()[:16]
    return f"{sha[:12]}\t{page}\t{digest}"


def centre(b):
    return ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2)


def overlap(p, q):
    w = min(p[2], q[2]) - max(p[0], q[0])
    h = min(p[3], q[3]) - max(p[1], q[1])
    if w <= 0 or h <= 0:
        return 0.0
    area = lambda b: max(b[2] - b[0], 1e-6) * max(b[3] - b[1], 1e-6)
    return w * h / min(area(p), area(q))


def same(line_box, line_text, item):
    c, b = centre(line_box), item["bbox"]
    if b[0] - PAD <= c[0] <= b[2] + PAD and b[1] - PAD <= c[1] <= b[3] + PAD:
        return True
    if overlap(line_box, b) >= 0.5:
        return True
    a, t = words(line_text).replace(" ", ""), words(item["text"]).replace(" ", "")
    return len(a) >= 4 and not a.isdigit() and (a in t or (len(t) >= 4 and t in a))


def crop_shift(path, page):
    """Docling's boxes are in CropBox space and ebook-hero's in MediaBox space."""
    out = subprocess.run(["pdfinfo", "-box", "-f", str(page), "-l", str(page), path],
                         capture_output=True, text=True).stdout
    box = {}
    for l in out.splitlines():
        parts = l.split()
        if len(parts) >= 7 and parts[2] in ("MediaBox:", "CropBox:"):
            box[parts[2]] = [float(v) for v in parts[3:7]]
    if "MediaBox:" not in box or "CropBox:" not in box:
        return 0.0, 0.0
    media, crop = box["MediaBox:"], box["CropBox:"]
    return crop[0] - media[0], media[3] - crop[3]


def furniture_lines(path):
    with tempfile.TemporaryDirectory() as cache:   # created with mode 0700, as ebook-hero requires
        env = dict(os.environ, EBOOK_HERO_INDEXES=cache)
        run = subprocess.run([binary, "extract", "--format", "json", path], env=env,
                             capture_output=True, text=True)
    if run.returncode:
        sys.exit(f"{path}: {run.stderr.strip()}")
    return json.loads(run.stdout)


verdicts = {}
if "--verdicts" in options:
    for line in list(open(options["--verdicts"]))[1:]:
        sha12, page, digest, _, verdict, _ = line.rstrip("\n").split("\t")
        verdicts[f"{sha12}\t{page}\t{digest}"] = verdict == "furniture"

totals = dict(lines=0, agree=0, items=0, found=0, judged_right=0, judged_items=0, judged_found=0, unjudged=0)
disagreements = []
print(f"{'book':44} {'lines':>5} {'agree':>5} {'items':>5} {'found':>5}")
for line in list(open(books))[1:]:
    sha, _, sample, name = line.rstrip("\n").split("\t")
    path = os.path.join(book_dir, name)
    sample = [int(p) for p in sample.split(",")]
    labels = json.load(open(os.path.join(docling_dir, f"{sha}.json")))
    doc = furniture_lines(path)
    ours = {}
    for block in doc["furniture"]:
        loc = block["locator"]
        if loc["page"] in sample:
            ours.setdefault(loc["page"], []).append((loc["bbox"], block["text"]))
    row = dict.fromkeys(totals, 0)
    for page in sample:
        dx, dy = crop_shift(path, page)
        items = [dict(i, bbox=[i["bbox"][0] + dx, i["bbox"][1] + dy, i["bbox"][2] + dx, i["bbox"][3] + dy])
                 for i in labels[str(page)] if i["label"] in FURNITURE and i["text"].strip()]
        lines = ours.get(page, [])
        for box, text in lines:
            row["lines"] += 1
            if any(same(box, text, i) for i in items):
                row["agree"] += 1
                row["judged_right"] += 1
                continue
            k = key(sha, page, text)
            verdict = verdicts.get(k)
            row["unjudged"] += verdict is None
            if verdict:
                row["judged_right"] += 1
                row["judged_items"] += 1
                row["judged_found"] += 1
            disagreements.append((k, "ours-only", page, box, text, name))
        for item in items:
            row["items"] += 1
            if any(same(box, text, item) for box, text in lines):
                row["found"] += 1
                row["judged_items"] += 1
                row["judged_found"] += 1
                continue
            k = key(sha, page, item["text"])
            verdict = verdicts.get(k)
            row["unjudged"] += verdict is None
            if verdict is not False:
                row["judged_items"] += 1
            disagreements.append((k, "docling-only", page, item["bbox"], item["text"], name))
    for k in totals:
        totals[k] += row[k]
    print(f"{name[:44]:44} {row['lines']:5d} {row['agree']:5d} {row['items']:5d} {row['found']:5d}")

ratio = lambda a, b: a / b if b else 1.0
t = totals
print(f"\nagainst docling: precision {ratio(t['agree'], t['lines']):.3f} ({t['agree']}/{t['lines']})"
      f"  recall {ratio(t['found'], t['items']):.3f} ({t['found']}/{t['items']})")
if verdicts:
    print(f"after judging:   precision {ratio(t['judged_right'], t['lines']):.3f} ({t['judged_right']}/{t['lines']})"
          f"  recall {ratio(t['judged_found'], t['judged_items']):.3f} ({t['judged_found']}/{t['judged_items']})"
          f"  unjudged {t['unjudged']}")
if "--disagreements" in options:
    with open(options["--disagreements"], "w") as f:
        f.write("sha12\tpage\ttext_sha1\tkind\tbbox\ttext\tfile\n")
        for k, kind, page, box, text, name in disagreements:
            f.write(f"{k}\t{kind}\t{[round(v) for v in box]}\t{text}\t{name}\n")
