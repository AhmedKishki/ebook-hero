//! Step 2 of the concept graph: the passages divided into topics by
//! non-negative matrix factorization (NMF) of their TF-IDF weights.

use std::collections::{BTreeMap, HashMap};

use crate::analysis::{STEMMED, tokens};
use crate::settings::{NMF_MAX_ITERATIONS, NMF_SEED, NMF_TOLERANCE};

/// A word in fewer passages than this says nothing about topics.
const MIN_PASSAGES: usize = 2;
/// A word in more than this share of the passages is in every topic.
const MAX_SHARE: f64 = 0.5;
/// Keeps the multiplicative updates from dividing by zero.
const EPSILON: f64 = 1e-10;
/// How many iterations pass between two looks at the loss.
const CHECK_EVERY: usize = 10;

/// The number of topics for `n` passages: the square root, rounded, and at
/// least two.
pub fn topic_count(n: usize) -> usize {
    ((n as f64).sqrt().round() as usize).max(2)
}

/// The topic, below `k`, of each of `texts`: the topic it has most of after
/// NMF. A book with no word that can tell topics apart has every passage in
/// topic 0.
pub fn passage_topics(texts: &[&str], k: usize) -> Vec<usize> {
    let (rows, vocabulary) = tfidf(texts);
    if vocabulary == 0 || rows.iter().all(Vec::is_empty) {
        return vec![0; texts.len()];
    }
    let w = nmf(&rows, vocabulary, k);
    w.chunks(k)
        .map(|row| {
            row.iter()
                .enumerate()
                .fold(
                    (0, f64::MIN),
                    |best, (t, &x)| if x > best.1 { (t, x) } else { best },
                )
                .0
        })
        .collect()
}

/// Each passage as sparse `(column, weight)` pairs, sorted by column, and the
/// size of the vocabulary. A weight is `(1 + ln tf) × (ln((1 + n) / (1 + df))
/// + 1)` over the `STEMMED` tokens, and each row has unit length.
fn tfidf(texts: &[&str]) -> (Vec<Vec<(usize, f64)>>, usize) {
    let n = texts.len();
    let counts: Vec<HashMap<String, usize>> = texts
        .iter()
        .map(|text| {
            let mut tf = HashMap::new();
            for token in tokens(STEMMED, text) {
                *tf.entry(token.text).or_insert(0) += 1;
            }
            tf
        })
        .collect();
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for tf in &counts {
        for word in tf.keys() {
            *df.entry(word).or_insert(0) += 1;
        }
    }
    // Columns in word order, so that the same book always gets the same matrix.
    let columns: HashMap<&str, (usize, usize)> = df
        .into_iter()
        .filter(|&(_, d)| d >= MIN_PASSAGES && d as f64 <= MAX_SHARE * n as f64)
        .enumerate()
        .map(|(column, (word, d))| (word, (column, d)))
        .collect();
    let rows = counts
        .iter()
        .map(|tf| {
            let mut row: Vec<(usize, f64)> = tf
                .iter()
                .filter_map(|(word, &count)| {
                    let &(column, d) = columns.get(word.as_str())?;
                    let idf = ((1 + n) as f64 / (1 + d) as f64).ln() + 1.0;
                    Some((column, (1.0 + (count as f64).ln()) * idf))
                })
                .collect();
            row.sort_by_key(|&(column, _)| column);
            let norm = row.iter().map(|(_, x)| x * x).sum::<f64>().sqrt();
            if norm > 0.0 {
                row.iter_mut().for_each(|(_, x)| *x /= norm);
            }
            row
        })
        .collect();
    (rows, columns.len())
}

/// W (n × k, row-major) of the factorization `X ≈ W H` with W, H ≥ 0, by Lee
/// and Seung's multiplicative updates. H is kept transposed (vocabulary × k)
/// so that every inner loop runs over contiguous memory.
fn nmf(rows: &[Vec<(usize, f64)>], vocabulary: usize, k: usize) -> Vec<f64> {
    let n = rows.len();
    let total: f64 = rows.iter().flatten().map(|(_, x)| x).sum();
    let scale = 2.0 * (total / (n * vocabulary) as f64 / k as f64).sqrt();
    let mut random = SplitMix64(NMF_SEED);
    let mut w: Vec<f64> = (0..n * k).map(|_| scale * random.next_f64()).collect();
    let mut ht: Vec<f64> = (0..vocabulary * k)
        .map(|_| scale * random.next_f64())
        .collect();
    let squared_x: f64 = rows.iter().flatten().map(|(_, x)| x * x).sum();
    let mut previous: Option<f64> = None;

    for iteration in 1..=NMF_MAX_ITERATIONS {
        // H ← H ∘ (Wᵀ X) ⊘ (Wᵀ W H), in transposed form.
        let wtw = gram(&w, k);
        let mut xtw = vec![0.0; vocabulary * k];
        for (i, row) in rows.iter().enumerate() {
            let wi = &w[i * k..(i + 1) * k];
            for &(c, x) in row {
                for (out, &wit) in xtw[c * k..(c + 1) * k].iter_mut().zip(wi) {
                    *out += x * wit;
                }
            }
        }
        let denominator = times(&ht, &wtw, k);
        for ((h, num), den) in ht.iter_mut().zip(&xtw).zip(&denominator) {
            *h *= num / (den + EPSILON);
        }

        // W ← W ∘ (X Hᵀ) ⊘ (W H Hᵀ).
        let hht = gram(&ht, k);
        let xht = x_times(rows, &ht, k);
        let denominator = times(&w, &hht, k);
        for ((wv, num), den) in w.iter_mut().zip(&xht).zip(&denominator) {
            *wv *= num / (den + EPSILON);
        }

        if iteration % CHECK_EVERY == 0 {
            let loss = loss(rows, &w, &ht, k, squared_x);
            if let Some(before) = previous
                && (before <= 0.0 || (before - loss) / before < NMF_TOLERANCE)
            {
                break;
            }
            previous = Some(loss);
        }
    }
    w
}

/// `‖X − W H‖²`, from `‖X‖²`, `X Hᵀ`, `Wᵀ W` and `H Hᵀ` without forming `W H`.
fn loss(rows: &[Vec<(usize, f64)>], w: &[f64], ht: &[f64], k: usize, squared_x: f64) -> f64 {
    let xht = x_times(rows, ht, k);
    let cross: f64 = w.iter().zip(&xht).map(|(a, b)| a * b).sum();
    let wtw = gram(w, k);
    let hht = gram(ht, k);
    let model: f64 = wtw.iter().zip(&hht).map(|(a, b)| a * b).sum();
    (squared_x - 2.0 * cross + model).max(0.0)
}

/// `Mᵀ M` (k × k) of a row-major matrix with `k` columns.
fn gram(m: &[f64], k: usize) -> Vec<f64> {
    let mut out = vec![0.0; k * k];
    for row in m.chunks(k) {
        for (s, &a) in row.iter().enumerate() {
            if a == 0.0 {
                continue;
            }
            for (out, &b) in out[s * k..(s + 1) * k].iter_mut().zip(row) {
                *out += a * b;
            }
        }
    }
    out
}

/// `M S` for a row-major matrix with `k` columns and a k × k matrix `S`.
fn times(m: &[f64], s: &[f64], k: usize) -> Vec<f64> {
    let mut out = vec![0.0; m.len()];
    for (row, out) in m.chunks(k).zip(out.chunks_mut(k)) {
        for (j, &a) in row.iter().enumerate() {
            if a == 0.0 {
                continue;
            }
            for (o, &b) in out.iter_mut().zip(&s[j * k..(j + 1) * k]) {
                *o += a * b;
            }
        }
    }
    out
}

/// `X Hᵀ` (n × k) for the sparse rows of X and H stored transposed.
fn x_times(rows: &[Vec<(usize, f64)>], ht: &[f64], k: usize) -> Vec<f64> {
    let mut out = vec![0.0; rows.len() * k];
    for (row, out) in rows.iter().zip(out.chunks_mut(k)) {
        for &(c, x) in row {
            for (o, &h) in out.iter_mut().zip(&ht[c * k..(c + 1) * k]) {
                *o += x * h;
            }
        }
    }
    out
}

/// SplitMix64 (Steele, Lea and Flood, 2014): a small generator whose sequence
/// depends only on its seed, so NMF starts the same way on every machine.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_count_is_the_rounded_square_root_and_at_least_two() {
        assert_eq!(
            (topic_count(1), topic_count(20), topic_count(2392)),
            (2, 4, 49)
        );
    }

    #[test]
    fn nmf_separates_two_disjoint_vocabularies() {
        let a = "entropy channel capacity coding noise";
        let b = "queen croquet hedgehog flamingo garden";
        let texts: Vec<&str> = (0..20).map(|i| if i % 2 == 0 { a } else { b }).collect();
        let topics = passage_topics(&texts, 2);
        for (i, topic) in topics.iter().enumerate() {
            assert_eq!(*topic == topics[0], i % 2 == 0, "passage {i}: {topics:?}");
        }
    }

    #[test]
    fn the_same_input_gives_the_same_topics() {
        let texts: Vec<String> = (0..30)
            .map(|i| format!("word{} shared{} common", i % 7, i % 3))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        assert_eq!(passage_topics(&refs, 4), passage_topics(&refs, 4));
    }

    #[test]
    fn a_book_with_no_usable_word_puts_every_passage_in_topic_zero() {
        // Every word is in all passages, so the vocabulary is empty.
        assert_eq!(passage_topics(&["same words"; 10], 3), vec![0; 10]);
    }
}
