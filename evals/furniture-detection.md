# Furniture detection against docling

This evaluation compares ebook-hero's furniture detection ([docs/furniture-detection.md](../docs/furniture-detection.md)) with docling's layout model on 22 randomly drawn books. Furniture means running heads, folios and repository stamps that the PDF reader must keep out of the text.

On 550 sampled pages, ebook-hero's code-length measure finds 97.3% of the lines docling labels as furniture, and 87.3% of the lines it removes are lines docling labels as furniture. It needs no trained model, and it extracts a page in 5.0 ms with 92 MB of memory, where docling needs 970 ms and 1.86 GB. Docling makes fewer errors: after judging every disagreement, 48 for docling against 86 for the measure. ebook-hero's previous rule, which looked for lines that recur in the top and bottom bands of the page, found 41.9%.

## Setup

### Books

The books come from a research library of 172 PDFs on social theory, political economy and technology. The candidates were its PDFs of 100 pages or more, less the books used while developing the measure (their SHA-256 in [furniture-detection/excluded.sha256](furniture-detection/excluded.sha256)): 31 books. [furniture-detection/pick_books.py](furniture-detection/pick_books.py) drew 22 of them with seed 20261105, keeping only books with a text layer, and 25 random pages from each, 550 pages in all. The list, [furniture-detection/books.tsv](furniture-detection/books.tsv), was frozen with a checksum before any scoring. The measure was not changed after any test run.

The books are copyrighted and are not part of the repository. books.tsv names each by file name and SHA-256, so anyone with the same files can rerun the evaluation.

### Systems

| System | What it is |
|---|---|
| docling 2.135.0 | Layout model (Heron), run without OCR or table structure. Its `page_header` and `page_footer` labels are the reference. |
| Position-and-recurrence rule | ebook-hero 0.2.0, the rule this measure replaces, described below. |
| Code-length measure | ebook-hero with the measure of [docs/furniture-detection.md](../docs/furniture-detection.md). |

### The position-and-recurrence rule

ebook-hero 0.2.0 found furniture with fixed rules on where a line sits and how often it recurs (`furniture` in `src/extract/pdf_layout.rs` at version 0.2.0):

1. Only lines in the top or bottom 12% of the page height can be furniture, measured at the middle of the line (`BAND_FRACTION` = 0.12).
2. A line's key is its text lowercased with each run of digits replaced by `#`, so `Chapter 3 · 41` and `Chapter 3 · 43` share a key.
3. A line in those bands is furniture when its key occurs in the bands on at least half the pages of the book and on at least 3 pages (`FURNITURE_MIN_SHARE` = 0.5, `FURNITURE_MIN_PAGES` = 3).
4. A line in those bands is furniture regardless of recurrence when it is a page number: digits or roman numerals, optionally after the word `page`.

Each of those numbers fits some layouts and fails on others. A running head that changes with each chapter recurs on far fewer than half the pages, so rule 3 misses it. A stamp printed in the side margin, or a running head set lower than 12% of the page, is outside the bands.

### Matching

ebook-hero reports each furniture line with its page and box. docling reports items with boxes, which [furniture-detection/score.py](furniture-detection/score.py) moves from CropBox to MediaBox coordinates. A line and an item match when their boxes share at least half of the smaller box, or when the line's centre lies in the item's box grown by 2 points. Rotated text, whose boxes the two tools draw differently, matches when the letters and digits of one contain those of the other (at least four, not a bare number).

Precision is the share of a system's furniture lines that match a docling item. Recall is the share of docling items that match one of the system's lines. One docling item can cover several lines, so the two are counted in different units.

## Results against docling

| System | Lines removed | Matching a docling item | Precision | Docling items found | Recall |
|---|---|---|---|---|---|
| Position-and-recurrence rule | 345 | 309 | 0.896 | 309 of 738 | 0.419 |
| Code-length measure | 898 | 784 | 0.873 | 718 of 738 | 0.973 |

| Book | Docling items | Position-and-recurrence rule found | Code-length measure lines | Its lines matching | Its items found |
|---|---|---|---|---|---|
| Marx, Capital II | 50 | 25 | 56 | 50 | 50 |
| Hayles, Unthought | 24 | 0 | 25 | 24 | 24 |
| Weatherby, Language Machines | 44 | 23 | 42 | 41 | 41 |
| Healy, Marx and Digital Machines | 22 | 0 | 28 | 22 | 22 |
| Lukács, The Ontology of Social Being 3 | 43 | 0 | 36 | 35 | 35 |
| Jaeggi, Alienation | 23 | 0 | 25 | 22 | 22 |
| Besteman, Militarized Global Apartheid | 0 | 0 | 0 | 0 | 0 |
| Hall, Selected Writings on Marxism | 24 | 0 | 51 | 22 | 21 |
| Hall, Selected Writings on Race and Difference | 27 | 0 | 27 | 26 | 26 |
| Beller, The World Computer | 58 | 48 | 75 | 71 | 58 |
| Sanyal, Rethinking Capitalist Development | 24 | 13 | 38 | 37 | 24 |
| Silver, Forces of Labor | 40 | 25 | 45 | 38 | 38 |
| Bellofiore, Starosta and Thomas, In Marx's Laboratory | 24 | 0 | 40 | 24 | 24 |
| Pellow et al., The Silicon Valley of Dreams | 25 | 0 | 26 | 25 | 25 |
| Corwin, Analog Labor in a Digital World | 65 | 45 | 68 | 64 | 64 |
| Salehabadi, Making and Unmaking E-Waste | 25 | 25 | 25 | 25 | 25 |
| Datta and Hoefsloot, Informational Peripheries | 42 | 21 | 65 | 63 | 42 |
| Neville and Villeneuve, Waste-Site Stories | 23 | 22 | 51 | 43 | 23 |
| Mezzadra and Neilson, The Politics of Operations | 24 | 0 | 28 | 24 | 24 |
| Rubin, Essays on Marx's Theory of Value | 47 | 22 | 54 | 46 | 47 |
| Liboiron and Lepawsky, Discard Studies | 44 | 29 | 58 | 48 | 44 |
| Biel, The Entropy of Capitalism | 40 | 11 | 35 | 34 | 39 |

The position-and-recurrence rule finds none of docling's items in 9 of the 21 books that have any. The code-length measure misses more than 3 docling items only in Lukács.

## Judging the disagreements

docling is a reference, not ground truth: it misses some running heads and stamps and labels some chapter numbers as page headers. Each of the 134 disagreements between the measure and docling was judged from its text and position, and on the rendered page where those left it open, by rules written before the test:

- Furniture: running heads and feet, folios, repository and licence stamps, chapter-opening citation and source notes, blank-page notices, continuation markers.
- Text: headings, chapter numbers and labels, notes and citations, index entries, figure labels, equations, title-page text.

[furniture-detection/verdicts.tsv](furniture-detection/verdicts.tsv) holds every verdict, keyed by book, page and a hash of the line, so it carries no text from the books.

| Disagreement | Lines | Verdict |
|---|---|---|
| Repository stamp ("This content downloaded from …", Hall) | 25 | furniture docling missed |
| Running heads, feet and a folio | 12 | furniture docling missed |
| Chapter-opening citation notes, blank-page notices | 4 | furniture docling missed |
| Note, citation and reference lines | 35 | removed by the measure, wrongly |
| Index entries | 11 | removed by the measure, wrongly |
| Headings and title-page text | 10 | removed by the measure, wrongly |
| Figure labels, equation lines, other text | 17 | removed by the measure, wrongly |
| Chapter labels, chapter numbers, a series title | 7 | docling's label is wrong |
| Letter-spaced running heads (Lukács) | 8 | missed by the measure |
| Notes-section heads and folios, a roman folio | 5 | missed by the measure |

| After judging | Precision | Recall | Errors |
|---|---|---|---|
| docling | 0.991 (731 of 738) | 0.947 (731 of 772) | 48 |
| Code-length measure | 0.919 (825 of 898) | 0.983 (759 of 772) | 86 |
| Position-and-recurrence rule | 0.971 (335 of 345) | 0.441 (335 of 759) | 434 |

A system's reference after judging is docling's items judged furniture plus the furniture that system found and docling missed, which is why the position-and-recurrence rule's reference is smaller. One person judged, knowing which system produced each line, and lines on which the measure and docling agree were not checked.

The two tools fail differently. docling rarely removes text but leaves stamps and some running heads in it. The measure misses little, but it removes short note, citation and index lines whose template recurs at the same place on a nearby page. For search and quotation that error costs more, since it takes real text away.

## Cost

| Measured on one machine | Time per page | Peak memory | Model |
|---|---|---|---|
| ebook-hero, whole extraction of the 22 books (7,235 pages in 36.0 s) | 5.0 ms | 92 MB mean, 394 MB largest | none |
| docling, 550 sampled pages in 534 s, 8 threads | 970 ms | 1.86 GB | Heron layout model |

The machine is an AMD Ryzen 7 4800H (8 cores, 16 threads) with 15 GB of memory. ebook-hero's time covers reading the PDF, rebuilding paragraphs and finding furniture. docling's covers its whole layout pipeline, which also finds tables, figures and reading order that this evaluation does not use. docling's peak memory was measured on one book's 25 pages (37.3 s wall, 189 s CPU).

## Reproducing

With the books in `BOOKS`, poppler-utils and MuPDF installed, and a Python environment with docling 2.135.0:

```sh
cd evals/furniture-detection
python3 -I pick_books.py "$BOOKS" excluded.sha256 > drawn.tsv     # equals books.tsv
python docling_label.py books.tsv "$BOOKS" labels                    # with docling's Python
cargo build --release
python3 -I score.py books.tsv "$BOOKS" labels ../../target/release/ebook-hero --verdicts verdicts.tsv
```

`score.py --disagreements FILE` writes every disagreement with its text for judging. That file quotes the books and stays out of the repository.

## References

- C. Auer et al. Docling Technical Report. arXiv:2408.09869, 2024.
