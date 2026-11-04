# Furniture detection

A printed page carries text that belongs to the page rather than to the book's argument: running heads, page numbers (folios), a publisher's or repository's stamp at the foot of every page. Typesetters call it furniture. Left in the text, it splits sentences that run across a page break and fills search results with book titles and page numbers.

ebook-hero removes furniture from PDFs with a measure instead of rules. Every line is described in three ways, each description has a length in bits, and the shortest description decides what the line is. The parameters of the descriptions are fitted to the book being read. No threshold, pattern list or trained model is involved.

The code is in `src/extract/pdf_furniture.rs`. EPUB files have no pages and no furniture, so only the PDF reader uses it.

## Where it sits in the PDF reader

The PDF reader (`src/extract/pdf.rs`) turns each page into positioned text lines. Furniture detection reads all pages at once, because it works by comparing pages, and returns the lines that are furniture. Paragraph rebuilding (`paragraphs` in `src/extract/pdf_layout.rs`) skips those lines, and the reader keeps them apart in `Document.furniture`, each with its page and box.

```mermaid
flowchart LR
    A[PDF pages] --> B[Positioned text lines]
    B --> C[Furniture detection]
    C -->|furniture lines| D[Document.furniture]
    B --> E[Paragraph rebuilding]
    C -->|lines to skip| E
    E --> F[Document.blocks]
```

## The idea

Furniture is the part of a page that other pages already tell you. A running head on page 41 repeats the running head on page 39. The folio on page 41 is the folio on page 39 plus two. A sentence of the text, by contrast, is predicted by the words around it on its own page, and no other page holds it.

Video coding uses the same distinction. A frame is coded partly from its own pixels (intra prediction) and partly from the previous frame (inter prediction), and the static background ends up coded from the previous frame because that is cheaper. Here a page is a frame, and furniture is what is cheaper to code from another page.

Minimum description length makes this a decision rule: of several ways to describe the data, prefer the one that describes it in fewer bits. A description must pay for everything it uses, including which other page it copies from, so a copy wins only when the other page really predicts the line.

## Tokens and templates

Each line's text is normalised (`src/normalize.rs`), lowercased and split into tokens:

- a run of letters is a word;
- a run of digits is a number;
- a run of letters that spells a roman numeral in its standard form (1 to 3999, `xiv` but not `iiii`) is a number;
- every other character except whitespace is a token of its own.

A number's token is its kind (`#` for digits, `#r` for roman numerals), and its value is kept apart. The sequence of tokens is the line's template: `352 | Index` and `354 | Index` share the template `# | index`. A description that copies a line copies its template and states its numbers separately.

## Three descriptions of a line

### 1. By its tokens

The tokens are coded one after another, each given the token before it (the start of the line counts as a token, and an end-of-line token closes the line). The probability of token $w$ after token $v$ is

$$P(w \mid v) = \frac{c(v, w) + \beta\, P_1(w)}{c(v) + \beta}, \qquad P_1(w) = \frac{c_\text{page}(w) + \alpha\, P_\text{book}(w)}{n_\text{page} + \alpha}$$

where $c(v, w)$ counts the pair in the rest of the book, $c_\text{page}(w)$ counts $w$ on the rest of the line's page, and $P_\text{book}(w)$ is the Krichevsky–Trofimov estimate over the rest of the book, $(c_\text{book}(w) + 1/2) / (n_\text{book} + V/2)$ for a vocabulary of $V$ tokens. All counts leave the line itself out, so a line never predicts itself; they include the line's own earlier tokens.

This is a token-pair model backed off to a mixture of the page and the book. The page part makes a word cheap on a page where it recurs, as a cache model does. The pair part makes a common sequence cheap, such as `), pp. #–#.` at the end of a reference.

Each number is then coded by how often it occurs in the rest of the book, backed off to writing its digits:

$$P(n) = \frac{c(n) + \gamma\, 2^{-\ell(n)}}{N + \gamma}, \qquad \ell(n) = \text{EliasGamma}(\text{digits}) + \text{digits} \cdot \log_2 10$$

Small numbers that recur, such as note and callout numbers, come cheap. A page number, written about once in the book, costs nearly its digits.

$\alpha$, $\beta$ and $\gamma$ are fitted to the book by maximum likelihood: each is found by golden-section search on its logarithm between $10^{-3}$ and $10^{7}$, and $\alpha$ and $\beta$ are fitted twice in turn.

### 2. As a copy of the line above it

The reference is the nearest line above on the same page whose horizontal extent overlaps the line's own. It must hold the same template. Each number then costs $\log_2 3$ bits to say whether it is the same as above, one more than above, or written out, plus the number's own code in the last case.

This is how a list describes itself: contents-page numbers, numbered callouts down a code listing, index lines. Without this description, the second item of a list that recurs at the same place on the next page would look like furniture.

### 3. As a copy of a line on another page

The reference is the line on page $q$ nearest to the line, measured between their left edges, centres or right edges, whichever pair is closest (right-aligned folios share a right edge, not a left one). It must hold the same template. The copy costs

- $1 + \text{EliasGamma}(|q - p|)$ bits for the offset to page $q$: 2 bits for the next page, 4 bits for two pages away, 10 bits for twenty;
- $\log_2 3$ bits per number to say whether it is the same as the reference's, moved on by the number of pages between them, or written out, plus the number's own code in the last case.

The search tries pages that hold the template, nearest first, and keeps the cheapest copy.

### Positions cancel

All three descriptions place the line in the same way, so the position costs the same in each and drops out of the comparison. Position matters only by choosing the reference: the nearest line on the other page, or the line above in the column. A body line's nearest line on another page is a different sentence, so no copy exists for it.

## The decision

Saying which description a line uses costs bits too. If a share $\pi_m$ of the book's lines uses description $m$, naming it costs $-\log_2 \pi_m$ bits. Each line takes the description that is shortest once that cost is added. The shares are not known in advance, so they are found by hard expectation–maximization:

1. Start with each share at 1/3.
2. Give each line the description with the smallest $L_m(\text{line}) - \log_2 \pi_m$.
3. Recount the shares from those choices: $\pi_m = (\text{lines choosing } m + 1/2) / (\text{lines} + 3/2)$.
4. Repeat until no line changes its choice (at most `EM_ROUNDS` = 64 rounds; a few suffice).

A line is furniture when its choice is the copy from another page. In a book where 5% of lines are furniture, a copy must save about 4 bits over the token description to win; in a book where 12% are, about 3 bits.

## Two lines worked through

From Hall, *Selected Writings on Marxism*, page 365, the folio and running foot `354 | Index`:

| Description | Bits |
|---|---|
| By its tokens | 37.4 |
| As a copy from page −2 (`352 \| Index`): offset 4, number moved on by 2 pages, $\log_2 3$ | 5.6 |

The copy saves 31.8 bits, far above the 4.15 bits this book's shares ask for. The line is furniture.

From Weatherby, *Language Machines*, page 222, a folio `213` at the foot of the page, a place where the nearest other folio is 18 pages back:

| Description | Bits |
|---|---|
| By its tokens | 15.0 |
| As a copy from page −18, the nearest page with a folio at the same place: offset 10, number moved, $\log_2 3$ | 11.6 |

The copy saves 3.4 bits, below the 4.40 bits this book's shares ask for. The folio stays in the text, and the evaluation counts it as a miss.

## Why a measure instead of rules

ebook-hero's previous rule, the position-and-recurrence rule, called a line furniture when it sat in the top or bottom 12% of the page and its text, with numbers masked, recurred there on at least half the pages, or when it was a bare page number there. Each of those numbers fits some layouts and fails on others: a running head that changes with every chapter recurs on far fewer than half the pages, and a repository stamp in the margin sits outside both bands. On 22 randomly drawn books that rule found 42% of what docling labels as furniture; the measure finds 97%. The full results are in [evals/furniture-detection.md](../evals/furniture-detection.md).

## Limits

- The measure compares pages, so it cannot judge a one-page document. A document needs several pages that share furniture.
- A copy needs the same template. When the PDF reader extracts a letter-spaced running head with different spacing on each page (`O NTO LOG Y`, `ON TO LOG Y`), no page holds a copy, and the head stays in the text.
- Text that really does recur at the same place on nearby pages is described as a copy, and is removed: code that repeats across listings, short note and citation lines with a common pattern, a title page printed twice.
- Furniture whose text changes on every page without a number that moves with the page, such as a source note that names a different journal at each chapter opening, has no copy and stays.
- A folio printed somewhere else on a few pages, such as chapter openings, can only copy from a distant page and may stay in the text.
- The search for copies stops at the length of the token description. This is exact while copies are the rarer description, which holds for books.

## Cost

On 22 books (7,235 pages) the whole PDF extraction, including this measure, took 5.0 ms per page with a mean peak memory of 92 MB on an 8-core laptop CPU. See [evals/furniture-detection.md](../evals/furniture-detection.md).

## References

- J. Rissanen. Modeling by shortest data description. *Automatica* 14(5):465–471, 1978.
- P. D. Grünwald. *The Minimum Description Length Principle*. MIT Press, 2007.
- R. E. Krichevsky and V. K. Trofimov. The performance of universal encoding. *IEEE Transactions on Information Theory* 27(2):199–207, 1981.
- P. Elias. Universal codeword sets and representations of the integers. *IEEE Transactions on Information Theory* 21(2):194–203, 1975.
- D. J. C. MacKay and L. C. B. Peto. A hierarchical Dirichlet language model. *Natural Language Engineering* 1(3):289–308, 1995.
- R. Kuhn and R. De Mori. A cache-based natural language model for speech recognition. *IEEE Transactions on Pattern Analysis and Machine Intelligence* 12(6):570–583, 1990.
- G. Celeux and G. Govaert. A classification EM algorithm for clustering and two stochastic versions. *Computational Statistics & Data Analysis* 14(3):315–332, 1992.
- J. Kiefer. Sequential minimax search for a maximum. *Proceedings of the American Mathematical Society* 4(3):502–506, 1953.
- T. Wiegand, G. J. Sullivan, G. Bjøntegaard and A. Luthra. Overview of the H.264/AVC video coding standard. *IEEE Transactions on Circuits and Systems for Video Technology* 13(7):560–576, 2003.
