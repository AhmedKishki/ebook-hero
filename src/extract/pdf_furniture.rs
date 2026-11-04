//! Finds the furniture of a PDF: the lines that other pages predict.
//!
//! Every line is described three ways, and its shortest description decides
//! what it is.
//!
//! 1. By its tokens: each token coded given the token before it on the line,
//!    from how often that pair, that token on the page and that token in the
//!    rest of the book occur; then its numbers, from how often each number
//!    occurs in the rest of the book.
//! 2. As a copy of the line above it in its column on its own page, which must
//!    hold the same template (the tokens, numbers left out). Each number is the
//!    same as above, one more, or coded on its own. This is how a list
//!    describes itself.
//! 3. As a copy of the line nearest to it on another page, which must hold the
//!    same template. That costs the offset to the page, and each number is the
//!    same, moved on with the page, or coded on its own.
//!
//! A line's position costs the same in every description, so it cancels: the
//! place matters only by choosing which line is the reference. Which
//! description a line uses costs bits too, by the share of lines that use it,
//! learned from the book by hard expectation–maximization. A line is furniture
//! when its shortest description is the copy from another page.
//!
//! Every parameter (the weights of the token and number codes, the shares of
//! the descriptions) is fitted to the book being read; nothing is a threshold.
//! docs/furniture-detection.md explains the measure and its evaluation.

use std::collections::{HashMap, HashSet};

use super::pdf_layout::{Line, PageLines};
use crate::normalize::normalize;

/// What a line says and where.
struct Text {
    page: u32,
    /// Position of the page in `pages`.
    at: usize,
    index: usize,
    /// Interned tokens, numbers interned as their kind, then end of line.
    tokens: Vec<u32>,
    template: u32,
    numbers: Vec<Number>,
    /// Left edge, centre and right edge.
    x: [f64; 3],
    y: f64,
}

#[derive(Clone, PartialEq)]
struct Number {
    /// The digits as written, or the roman numeral.
    written: String,
    value: Option<i64>,
    /// Bits to write it out digit by digit.
    literal: f64,
}

/// The three descriptions of a line, in the order their shares are kept.
const TOKENS: usize = 0;
const OTHER_PAGE: usize = 2;

/// What end of line is interned as; no token of text can be empty.
const END: &str = "";

/// Rounds of expectation–maximization at most; the shares settle in a few.
const EM_ROUNDS: usize = 64;

/// The `(page, line index)` pairs of the furniture lines.
pub fn furniture(pages: &[PageLines]) -> HashSet<(u32, usize)> {
    let mut vocabulary: HashMap<String, u32> = HashMap::new();
    let mut templates: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut texts = Vec::new();
    for (at, page) in pages.iter().enumerate() {
        for (index, line) in page.lines.iter().enumerate() {
            let text = normalize(&line.text);
            if text.is_empty() {
                continue;
            }
            let (words, numbers) = tokens(&text);
            let mut intern = |w: &str| {
                let next = vocabulary.len() as u32;
                *vocabulary.entry(w.to_string()).or_insert(next)
            };
            let mut tokens: Vec<u32> = words.iter().map(|w| intern(w)).collect();
            let next = templates.len() as u32;
            let template = *templates.entry(tokens.clone()).or_insert(next);
            tokens.push(intern(END));
            texts.push(Text {
                page: page.page,
                at,
                index,
                tokens,
                template,
                numbers,
                x: anchors(line),
                y: f64::from(line.top + line.bottom) / 2.0,
            });
        }
    }
    let numbers = number_bits(&texts);
    let within = within_page_bits(&texts, pages.len(), vocabulary.len(), &numbers.bits);
    let above = column_copy_bits(&texts, pages.len(), &numbers.bits);
    let copied = copy_bits(&texts, pages.len(), &within.bits, &numbers.bits);
    let lengths: Vec<[Option<f64>; 3]> = (0..texts.len())
        .map(|i| [Some(within.bits[i]), above[i], copied[i]])
        .collect();
    let chosen = choose(&lengths);
    texts
        .iter()
        .zip(chosen)
        .filter(|(_, c)| *c == OTHER_PAGE)
        .map(|(t, _)| (t.page, t.index))
        .collect()
}

/// Hard expectation–maximization on the shares of the three descriptions:
/// each line takes the description that is shortest once `-log2(share)` is
/// paid for saying which it is, and the shares are counted again from the
/// choices (Krichevsky–Trofimov), until no line changes.
fn choose(lengths: &[[Option<f64>; 3]]) -> Vec<usize> {
    let lines = lengths.len() as f64;
    let mut shares = [1.0f64 / 3.0; 3];
    let mut chosen: Vec<usize> = vec![usize::MAX; lengths.len()];
    for _ in 0..EM_ROUNDS {
        let next: Vec<usize> = lengths
            .iter()
            .map(|line| {
                (0..3)
                    .filter_map(|m| line[m].map(|bits| (m, bits - shares[m].log2())))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map_or(TOKENS, |(m, _)| m)
            })
            .collect();
        let settled = next == chosen;
        chosen = next;
        for (m, share) in shares.iter_mut().enumerate() {
            let count = chosen.iter().filter(|&&c| c == m).count() as f64;
            *share = (count + 0.5) / (lines + 1.5);
        }
        if settled {
            break;
        }
    }
    chosen
}

fn anchors(line: &Line) -> [f64; 3] {
    let (left, right) = (f64::from(line.left), f64::from(line.right));
    [left, (left + right) / 2.0, right]
}

/// Splits a line into tokens: runs of letters, runs of digits, and single
/// other characters, spaces dropped. A run of digits, or a run of letters that
/// spells a roman numeral the standard way, is a number; its token is its kind
/// and its digits are kept apart.
fn tokens(text: &str) -> (Vec<String>, Vec<Number>) {
    let lower = text.to_lowercase();
    let mut words = Vec::new();
    let mut numbers = Vec::new();
    let mut chars = lower.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let digits = c.is_ascii_digit();
        if !digits && !c.is_alphabetic() {
            words.push(c.to_string());
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(at, next)) = chars.peek() {
            let same = if digits {
                next.is_ascii_digit()
            } else {
                next.is_alphabetic()
            };
            if !same {
                break;
            }
            end = at + next.len_utf8();
            chars.next();
        }
        let run = &lower[start..end];
        if digits {
            words.push("#".to_string());
            numbers.push(Number {
                written: run.to_string(),
                value: run.parse().ok(),
                literal: literal_bits(run.len()),
            });
        } else if let Some(value) = roman_value(run) {
            words.push("#r".to_string());
            numbers.push(Number {
                written: run.to_string(),
                value: Some(value),
                literal: literal_bits(value.to_string().len()),
            });
        } else {
            words.push(run.to_string());
        }
    }
    (words, numbers)
}

/// The value of a lowercase roman numeral written the standard way, up to 3999.
fn roman_value(run: &str) -> Option<i64> {
    if !run.chars().all(|c| "ivxlcdm".contains(c)) {
        return None;
    }
    let digit = |c| match c {
        'i' => 1,
        'v' => 5,
        'x' => 10,
        'l' => 50,
        'c' => 100,
        'd' => 500,
        _ => 1000,
    };
    let values: Vec<i64> = run.chars().map(digit).collect();
    let mut total = 0;
    for (k, &v) in values.iter().enumerate() {
        match values.get(k + 1) {
            Some(&next) if next > v => total -= v,
            _ => total += v,
        }
    }
    (total > 0 && total < 4000 && to_roman(total) == run).then_some(total)
}

fn to_roman(mut n: i64) -> String {
    const TABLE: [(i64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, letters) in TABLE {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    out
}

/// Bits of the Elias gamma code of `n >= 1`.
fn gamma_bits(n: u64) -> f64 {
    (2 * (63 - n.max(1).leading_zeros()) + 1) as f64
}

/// Bits to write out a number of `digits` digits: its length, then each digit.
fn literal_bits(digits: usize) -> f64 {
    gamma_bits(digits as u64) + digits as f64 * 10f64.log2()
}

struct Within {
    bits: Vec<f64>,
}

/// The counts that code one token, the token itself left out.
struct TokenCounts {
    /// The book's share of the token, Krichevsky–Trofimov over the rest.
    book: f64,
    /// The token on the rest of its page, and all tokens there.
    on_page: f64,
    page: f64,
    /// The token after the token before it in the rest of the book, and how
    /// often that token is followed at all.
    after: f64,
    context: f64,
}

/// Bits to describe each line by its tokens, with `alpha` and `beta` fitted to
/// make the book's lines shortest.
///
/// Each token is coded given the token before it on the line (the start of
/// the line counts as a token): `(pairs + beta * unigram) / (context + beta)`,
/// where the unigram mixes the page and the book as `(on the page + alpha *
/// book share) / (page + alpha)`. All counts leave the line out, so a line
/// does not predict itself, but count its own earlier tokens. The last token
/// is the end of the line. Then come the numbers, as [`number_bits`] codes
/// them.
fn within_page_bits(
    texts: &[Text],
    page_count: usize,
    vocabulary: usize,
    numbers: &[Vec<f64>],
) -> Within {
    const START: u32 = u32::MAX;
    let pairs_of = |t: &Text| -> Vec<(u32, u32)> {
        let mut previous = START;
        t.tokens
            .iter()
            .map(|&w| {
                let pair = (previous, w);
                previous = w;
                pair
            })
            .collect()
    };
    let mut book = vec![0u32; vocabulary];
    let mut on_page: Vec<HashMap<u32, u32>> = vec![HashMap::new(); page_count];
    let mut page_total = vec![0u32; page_count];
    let mut pairs: HashMap<(u32, u32), u32> = HashMap::new();
    let mut contexts: HashMap<u32, u32> = HashMap::new();
    for t in texts {
        for &w in &t.tokens {
            book[w as usize] += 1;
            *on_page[t.at].entry(w).or_insert(0) += 1;
        }
        for pair in pairs_of(t) {
            *pairs.entry(pair).or_insert(0) += 1;
            *contexts.entry(pair.0).or_insert(0) += 1;
        }
        page_total[t.at] += t.tokens.len() as u32;
    }
    let book_total = f64::from(book.iter().sum::<u32>());
    let v = vocabulary as f64;

    let mut counts: Vec<Vec<TokenCounts>> = Vec::with_capacity(texts.len());
    let mut fixed: Vec<f64> = Vec::with_capacity(texts.len());
    for (t, line_numbers) in texts.iter().zip(numbers) {
        let n = t.tokens.len();
        let line_pairs = pairs_of(t);
        let mut own: HashMap<u32, u32> = HashMap::new();
        let mut own_pairs: HashMap<(u32, u32), u32> = HashMap::new();
        let mut own_contexts: HashMap<u32, u32> = HashMap::new();
        for (&w, &pair) in t.tokens.iter().zip(&line_pairs) {
            *own.entry(w).or_insert(0) += 1;
            *own_pairs.entry(pair).or_insert(0) += 1;
            *own_contexts.entry(pair.0).or_insert(0) += 1;
        }
        let mut seen: HashMap<u32, u32> = HashMap::new();
        let mut seen_pairs: HashMap<(u32, u32), u32> = HashMap::new();
        let mut seen_contexts: HashMap<u32, u32> = HashMap::new();
        let page_rest = f64::from(page_total[t.at]) - n as f64;
        let book_rest = book_total - n as f64;
        let mut line = Vec::with_capacity(n);
        for (j, (&w, &pair)) in t.tokens.iter().zip(&line_pairs).enumerate() {
            let before = f64::from(seen.get(&w).copied().unwrap_or(0));
            let rest = |all: u32, mine: u32, earlier: f64| f64::from(all - mine) + earlier;
            line.push(TokenCounts {
                book: (rest(book[w as usize], own[&w], before) + 0.5)
                    / (book_rest + j as f64 + 0.5 * v),
                on_page: rest(on_page[t.at].get(&w).copied().unwrap_or(0), own[&w], before),
                page: page_rest + j as f64,
                after: rest(
                    pairs[&pair],
                    own_pairs[&pair],
                    f64::from(seen_pairs.get(&pair).copied().unwrap_or(0)),
                ),
                context: rest(
                    contexts[&pair.0],
                    own_contexts[&pair.0],
                    f64::from(seen_contexts.get(&pair.0).copied().unwrap_or(0)),
                ),
            });
            *seen.entry(w).or_insert(0) += 1;
            *seen_pairs.entry(pair).or_insert(0) += 1;
            *seen_contexts.entry(pair.0).or_insert(0) += 1;
        }
        counts.push(line);
        fixed.push(line_numbers.iter().sum::<f64>());
    }

    let bits_of = |line: &[TokenCounts], alpha: f64, beta: f64| -> f64 {
        line.iter()
            .map(|c| {
                let unigram = (c.on_page + alpha * c.book) / (c.page + alpha);
                -((c.after + beta * unigram) / (c.context + beta)).log2()
            })
            .sum()
    };
    let total = |alpha: f64, beta: f64| -> f64 {
        counts.iter().map(|line| bits_of(line, alpha, beta)).sum()
    };
    // Each weight is fitted on its logarithm with the other held, twice over.
    let (mut alpha, mut beta) = (1.0, 1.0);
    for _ in 0..2 {
        alpha = minimize_log(|a| total(a, beta));
        beta = minimize_log(|b| total(alpha, b));
    }
    Within {
        bits: counts
            .iter()
            .zip(&fixed)
            .map(|(line, f)| bits_of(line, alpha, beta) + f)
            .collect(),
    }
}

/// The positive weight that minimizes `f`, by golden-section search on its
/// logarithm between 1e-3 and 1e7.
fn minimize_log(f: impl Fn(f64) -> f64) -> f64 {
    let (mut lo, mut hi) = ((1e-3f64).ln(), (1e7f64).ln());
    let ratio = (5f64.sqrt() - 1.0) / 2.0;
    let mut a = hi - ratio * (hi - lo);
    let mut b = lo + ratio * (hi - lo);
    let (mut fa, mut fb) = (f(a.exp()), f(b.exp()));
    for _ in 0..48 {
        if fa < fb {
            hi = b;
            b = a;
            fb = fa;
            a = hi - ratio * (hi - lo);
            fa = f(a.exp());
        } else {
            lo = a;
            a = b;
            fa = fb;
            b = lo + ratio * (hi - lo);
            fb = f(b.exp());
        }
    }
    ((lo + hi) / 2.0).exp()
}

/// Bits to describe each line as a copy of the nearest line on another page,
/// for the cheapest such page; `None` when no page offers a copy shorter than
/// the line's description by its tokens.
fn copy_bits(
    texts: &[Text],
    page_count: usize,
    within: &[f64],
    numbers: &[Vec<f64>],
) -> Vec<Option<f64>> {
    let mut by_page: Vec<Vec<usize>> = vec![Vec::new(); page_count];
    let mut pages_with: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, t) in texts.iter().enumerate() {
        by_page[t.at].push(i);
        let list = pages_with.entry(t.template).or_default();
        if list.last() != Some(&t.at) {
            list.push(t.at);
        }
    }
    let nearest = |line: &Text, at: usize| -> Option<usize> {
        by_page[at].iter().copied().min_by(|&a, &b| {
            distance(line, &texts[a])
                .total_cmp(&distance(line, &texts[b]))
                .then(a.cmp(&b))
        })
    };
    let choice = 3f64.log2();
    texts
        .iter()
        .enumerate()
        .map(|(i, line)| {
            // A copy longer than the tokens never wins while copies are the
            // rarer description, so the search stops there; pages are tried
            // nearest first, and the offset only grows.
            let floor = line.numbers.len() as f64 * choice;
            let mut best: Option<f64> = None;
            let bound = |best: Option<f64>| best.map_or(within[i], |b| b.min(within[i]));
            let mut candidates: Vec<usize> = pages_with[&line.template]
                .iter()
                .copied()
                .filter(|&at| at != line.at)
                .collect();
            candidates.sort_by_key(|&at| (at as i64 - line.at as i64).abs());
            for at in candidates {
                let offset = texts[by_page[at][0]].page as i64 - line.page as i64;
                let offset_bits = 1.0 + gamma_bits(offset.unsigned_abs());
                if offset_bits + floor >= bound(best) {
                    break;
                }
                let Some(r) = nearest(line, at) else { continue };
                let reference = &texts[r];
                if reference.template != line.template {
                    continue;
                }
                let bits = offset_bits
                    + line
                        .numbers
                        .iter()
                        .zip(&reference.numbers)
                        .zip(&numbers[i])
                        .map(|((n, m), &written)| from_reference(n, m, -offset, written) + choice)
                        .sum::<f64>();
                if bits < bound(best) {
                    best = Some(bits);
                }
            }
            best
        })
        .collect()
}

/// Bits to describe each line as a copy of the line above it in its column
/// on its own page, the nearest line above whose extent across overlaps its
/// own, when that line holds the same template: for each number, one of
/// three choices, the same as above, one more than above, or written out.
/// This is how a list describes itself: contents page numbers, line callouts,
/// index lines.
fn column_copy_bits(texts: &[Text], page_count: usize, numbers: &[Vec<f64>]) -> Vec<Option<f64>> {
    let mut by_page: Vec<Vec<usize>> = vec![Vec::new(); page_count];
    for (i, t) in texts.iter().enumerate() {
        by_page[t.at].push(i);
    }
    let choice = 3f64.log2();
    texts
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let overlaps = |o: &Text| o.x[0] < line.x[2] && line.x[0] < o.x[2];
            let reference = by_page[line.at]
                .iter()
                .copied()
                .filter(|&k| k != i && texts[k].y < line.y && overlaps(&texts[k]))
                .max_by(|&a, &b| texts[a].y.total_cmp(&texts[b].y).then(b.cmp(&a)))?;
            let above = &texts[reference];
            (above.template == line.template).then(|| {
                line.numbers
                    .iter()
                    .zip(&above.numbers)
                    .zip(&numbers[i])
                    .map(|((n, m), &written)| from_reference(n, m, 1, written) + choice)
                    .sum()
            })
        })
        .collect()
}

/// Bits beyond the choice to give a number from the reference's: none when
/// it is the same or moved on by `step` (the pages between two pages, or one
/// down a list), else `written`, its own code.
fn from_reference(n: &Number, reference: &Number, step: i64, written: f64) -> f64 {
    let moved = n
        .value
        .zip(reference.value)
        .is_some_and(|(a, b)| a == b + step);
    if n.written == reference.written || moved {
        0.0
    } else {
        written
    }
}

struct Numbers {
    /// Bits for each number of each line.
    bits: Vec<Vec<f64>>,
}

/// Bits to write each number: `(as often in the rest of the book + gamma *
/// 2^-digits) / (numbers in the rest of the book + gamma)`, where `digits` is
/// the bits to write it out digit by digit, and gamma makes the book's numbers
/// shortest. Small numbers that recur (note and callout numbers) come cheap;
/// a page number, written about once, costs nearly its digits.
fn number_bits(texts: &[Text]) -> Numbers {
    let mut book: HashMap<&str, u32> = HashMap::new();
    for t in texts {
        for n in &t.numbers {
            *book.entry(n.written.as_str()).or_insert(0) += 1;
        }
    }
    let total: f64 = book.values().map(|&c| f64::from(c)).sum();
    // (others like it, others in all, its digit code), its own line left out.
    let mut counts: Vec<Vec<(f64, f64, f64)>> = Vec::with_capacity(texts.len());
    for t in texts {
        let mut own: HashMap<&str, u32> = HashMap::new();
        for n in &t.numbers {
            *own.entry(n.written.as_str()).or_insert(0) += 1;
        }
        let mut seen: HashMap<&str, u32> = HashMap::new();
        let rest = total - t.numbers.len() as f64;
        let line = t
            .numbers
            .iter()
            .enumerate()
            .map(|(j, n)| {
                let key = n.written.as_str();
                let before = seen.get(key).copied().unwrap_or(0);
                *seen.entry(key).or_insert(0) += 1;
                (
                    f64::from(book[key] - own[key] + before),
                    rest + j as f64,
                    n.literal,
                )
            })
            .collect();
        counts.push(line);
    }
    let bits_of = |(like, all, digits): (f64, f64, f64), gamma: f64| {
        -((like + gamma * (-digits).exp2()) / (all + gamma)).log2()
    };
    let gamma = minimize_log(|g| counts.iter().flatten().map(|&c| bits_of(c, g)).sum());
    Numbers {
        bits: counts
            .iter()
            .map(|line| line.iter().map(|&c| bits_of(c, gamma)).collect())
            .collect(),
    }
}

/// How far apart two lines sit, at whichever of their anchors are closest.
fn distance(a: &Text, b: &Text) -> f64 {
    (0..3)
        .map(|k| (a.x[k] - b.x[k]).hypot(a.y - b.y))
        .fold(f64::INFINITY, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, left: f32, top: f32, width: f32) -> Line {
        Line {
            text: text.to_string(),
            left,
            top,
            right: left + width,
            bottom: top + 10.0,
            font_size: 10.0,
        }
    }

    /// Body lines of varied words, as different from page to page as prose is.
    fn prose(page: u32, rows: std::ops::Range<u32>, left: f32) -> Vec<Line> {
        const WORDS: [&str; 24] = [
            "value", "labour", "time", "machine", "waste", "form", "capital", "measure", "order",
            "matter", "entropy", "market", "power", "nature", "process", "money", "work",
            "history", "relation", "social", "energy", "place", "system", "theory",
        ];
        let mut seed = u64::from(page) * 7919 + 17;
        rows.map(|row| {
            let words: Vec<&str> = (0..8)
                .map(|_| {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    WORDS[(seed >> 33) as usize % WORDS.len()]
                })
                .collect();
            line(&words.join(" "), left, 60.0 + 14.0 * row as f32, 300.0)
        })
        .collect()
    }

    fn page(number: u32, lines: Vec<Line>) -> PageLines {
        PageLines {
            page: number,
            width: 432.0,
            height: 648.0,
            lines,
        }
    }

    #[test]
    fn running_heads_and_folios_are_furniture() {
        let pages: Vec<PageLines> = (1..=12)
            .map(|n| {
                let mut lines = vec![line("On the Nature of Entropy", 60.0, 30.0, 150.0)];
                lines.extend(prose(n, 0..30, 60.0));
                lines.push(line(&(n + 10).to_string(), 210.0, 600.0, 12.0));
                page(n, lines)
            })
            .collect();

        let expected: HashSet<(u32, usize)> = (1..=12).flat_map(|n| [(n, 0), (n, 31)]).collect();
        assert_eq!(furniture(&pages), expected);
    }

    #[test]
    fn a_numbered_list_down_a_column_is_not_furniture() {
        let pages: Vec<PageLines> = (1..=12)
            .map(|n| {
                let first = 4 + n % 5;
                let mut lines = prose(n, 0..30, 80.0);
                lines.extend((0..6).map(|k| {
                    line(
                        &format!("({})", k + 1),
                        50.0,
                        60.0 + 14.0 * (first + k) as f32,
                        14.0,
                    )
                }));
                page(n, lines)
            })
            .collect();

        assert!(furniture(&pages).is_empty());
    }

    #[test]
    fn roman_numerals_are_read_only_in_their_standard_form() {
        assert_eq!(roman_value("xiv"), Some(14));
        assert_eq!(roman_value("mcmxcix"), Some(1999));
        assert_eq!(roman_value("iiii"), None);
        assert_eq!(roman_value("vx"), None);
        assert_eq!(roman_value("ivory"), None);
    }

    #[test]
    fn a_single_page_has_no_furniture() {
        let mut lines = vec![line("On the Nature of Entropy", 60.0, 30.0, 150.0)];
        lines.extend(prose(1, 0..30, 60.0));
        lines.push(line("11", 210.0, 600.0, 12.0));

        assert!(furniture(&[page(1, lines)]).is_empty());
    }
}
