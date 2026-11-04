"""Draw the test books: a seeded random draw of book-length PDFs from a folder.

usage: python3 -I pick_books.py FOLDER EXCLUDE.sha256 > books.tsv

A book is a PDF of 100 pages or more whose middle pages have a text layer.
EXCLUDE.sha256 lists, one per line, the SHA-256 of files to leave out (the
books used while developing the measure). From each book drawn, PAGES pages
are drawn at random. Needs pdfinfo and pdftotext (poppler-utils).
"""
import hashlib, os, random, subprocess, sys

WANT, PAGES, SEED, MIN_PAGES = 22, 25, 20261105, 100

folder, exclude_file = sys.argv[1], sys.argv[2]
exclude = {line.split()[0] for line in open(exclude_file) if line.strip()}


def page_count(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    return next((int(l.split()[1]) for l in info.splitlines() if l.startswith("Pages:")), 0)


def has_text(path, pages):
    middle = pages // 2
    text = subprocess.run(["pdftotext", "-f", str(middle), "-l", str(middle + 2), path, "-"],
                          capture_output=True, text=True).stdout
    return len(text.split()) > 150


candidates = []
for name in sorted(os.listdir(folder)):
    if not name.lower().endswith(".pdf"):
        continue
    path = os.path.join(folder, name)
    sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
    pages = page_count(path)
    if sha not in exclude and pages >= MIN_PAGES:
        candidates.append((sha, pages, name))
candidates.sort()
rng = random.Random(SEED)
rng.shuffle(candidates)

print("sha256\tpages\tsampled_pages\tfile")
drawn = 0
for sha, pages, name in candidates:
    if drawn == WANT:
        break
    if not has_text(os.path.join(folder, name), pages):
        continue
    sample = sorted(rng.sample(range(1, pages + 1), PAGES))
    print(f"{sha}\t{pages}\t{','.join(map(str, sample))}\t{name}")
    drawn += 1
print(f"{len(candidates)} candidates, {drawn} drawn", file=sys.stderr)
