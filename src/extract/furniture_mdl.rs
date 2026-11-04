//! SPIKE, throwaway: furniture found by minimum description length.
//!
//! Each line is `(template, x, y, p)`: its text lowercased with each digit run
//! read as `#`, the middle of its box, and its page. A pattern is a template at
//! one place on the page, with a rule for its numbers. A pattern is kept when
//! coding its lines through it takes fewer bits than coding them through a
//! model of the whole book's layout, counting the cost of the pattern itself
//! and of saying which pages it appears on. The lines of kept patterns are
//! furniture.
//!
//! The text of a line costs the same under both codes, since both share one
//! dictionary of templates, so only position, numbers and pages decide.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::pdf_layout::PageLines;
use crate::normalize::normalize;

/// The smallest spread a pattern's position may have, in points: the noise of
/// coordinates rounded to whole points, 1/√12.
const SIGMA_FLOOR: f64 = 0.288_675_134_594_812_9;
/// Refits of a pattern's place before it is judged.
const REFITS: usize = 8;
/// Numbers longer than this are not read as numbers.
const MAX_DIGITS: usize = 9;

struct Occurrence {
    page: u32,
    index: usize,
    x: f64,
    y: f64,
    numbers: Vec<i64>,
    /// Bits to code the position under the book's layout model.
    null_position: f64,
    /// Bits to code the numbers digit by digit.
    null_numbers: f64,
}

/// What a kept or rejected pattern costs and saves, for the debug log.
struct Judgement {
    template: String,
    members: Vec<usize>,
    mu: (f64, f64),
    sigma: f64,
    position_gain: f64,
    number_gain: f64,
    presence: f64,
    parameters: f64,
}

impl Judgement {
    fn net(&self) -> f64 {
        self.position_gain + self.number_gain - self.presence - self.parameters
    }
}

/// The `(page, line index)` pairs of the lines that kept patterns code.
pub fn furniture(pages: &[PageLines]) -> HashSet<(u32, usize)> {
    let page_count = pages.len().max(1) as f64;
    let mut occurrences = Vec::new();
    let mut templates: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut heights = Vec::new();
    for page in pages {
        for (index, line) in page.lines.iter().enumerate() {
            let text = normalize(&line.text);
            if text.is_empty() {
                continue;
            }
            let (template, numbers) = template_of(&text);
            heights.push((line.bottom - line.top).abs() as f64);
            templates.entry(template).or_default().push(occurrences.len());
            occurrences.push(Occurrence {
                page: page.page,
                index,
                x: f64::from(line.left),
                y: f64::from((line.top + line.bottom) / 2.0),
                null_numbers: numbers.iter().map(|n| literal_number_bits(*n)).sum(),
                numbers,
                null_position: 0.0,
            });
        }
    }
    if occurrences.is_empty() {
        return HashSet::new();
    }
    layout_model(&mut occurrences, pages);
    heights.sort_by(f64::total_cmp);
    let em = heights[heights.len() / 2].max(1.0);
    let template_bits = (templates.len().max(2) as f64).log2();

    let debug = std::env::var_os("EBOOK_HERO_FURNITURE_DEBUG").is_some();
    let mut found = HashSet::new();
    let mut judged = Vec::new();
    for (template, group) in &templates {
        let mut remaining: Vec<usize> = group.clone();
        loop {
            let pages_left: HashSet<u32> = remaining.iter().map(|&i| occurrences[i].page).collect();
            if pages_left.len() < 2 {
                break;
            }
            let Some(judgement) = fit(template, &remaining, &occurrences, em, page_count, template_bits) else {
                break;
            };
            let keep = judgement.net() > 0.0;
            if keep {
                for &m in &judgement.members {
                    found.insert((occurrences[m].page, occurrences[m].index));
                }
                let members: HashSet<usize> = judgement.members.iter().copied().collect();
                remaining.retain(|i| !members.contains(i));
            }
            if debug {
                judged.push((keep, judgement));
            }
            if !keep {
                break;
            }
        }
    }
    if debug {
        judged.sort_by(|a, b| b.1.net().total_cmp(&a.1.net()));
        for (keep, j) in &judged {
            if *keep || j.members.len() >= 3 {
                eprintln!(
                    "{} {:>9.1} bits  n={:<4} at ({:>6.1}, {:>6.1}) σ={:>6.2}  position {:>8.1}  numbers {:>7.1}  pages {:>7.1}  model {:>5.1}  {:?}",
                    if *keep { "KEEP" } else { "drop" },
                    j.net(),
                    j.members.len(),
                    j.mu.0,
                    j.mu.1,
                    j.sigma,
                    j.position_gain,
                    j.number_gain,
                    j.presence,
                    j.parameters,
                    j.template
                );
            }
        }
    }
    found
}

/// The template of a line, and the numbers its digit runs hold.
fn template_of(text: &str) -> (String, Vec<i64>) {
    let mut template = String::with_capacity(text.len());
    let mut numbers = Vec::new();
    let mut digits = String::new();
    let mut flush = |digits: &mut String, template: &mut String, numbers: &mut Vec<i64>| {
        if !digits.is_empty() {
            template.push('#');
            if digits.len() <= MAX_DIGITS {
                numbers.push(digits.parse().unwrap_or(0));
            }
            digits.clear();
        }
    };
    for c in text.to_lowercase().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut digits, &mut template, &mut numbers);
            template.push(c);
        }
    }
    flush(&mut digits, &mut template, &mut numbers);
    (template, numbers)
}

/// Bits to write `n` digit by digit: an Elias gamma code of its length, then
/// log2 10 bits per digit.
fn literal_number_bits(n: i64) -> f64 {
    let digits = n.max(1).to_string().len() as f64;
    2.0 * digits.log2().floor() + 1.0 + digits * 10f64.log2()
}

/// The book's layout model: how often lines sit at each whole-point x and y,
/// by the Krichevsky–Trofimov estimator, x and y independent. Sets each
/// occurrence's `null_position`.
fn layout_model(occurrences: &mut [Occurrence], pages: &[PageLines]) {
    let width = pages.iter().map(|p| p.width).fold(1.0, f32::max) as f64;
    let height = pages.iter().map(|p| p.height).fold(1.0, f32::max) as f64;
    let span_x = occurrences.iter().map(|o| o.x).fold(width, f64::max).ceil() as usize + 1;
    let span_y = occurrences.iter().map(|o| o.y).fold(height, f64::max).ceil() as usize + 1;
    let bin = |v: f64, span: usize| (v.max(0.0) as usize).min(span - 1);
    let mut xs = vec![0usize; span_x];
    let mut ys = vec![0usize; span_y];
    for o in occurrences.iter() {
        xs[bin(o.x, span_x)] += 1;
        ys[bin(o.y, span_y)] += 1;
    }
    let total = occurrences.len() as f64;
    for o in occurrences.iter_mut() {
        let px = (xs[bin(o.x, span_x)] as f64 + 0.5) / (total + 0.5 * span_x as f64);
        let py = (ys[bin(o.y, span_y)] as f64 + 0.5) / (total + 0.5 * span_y as f64);
        o.null_position = -px.log2() - py.log2();
    }
}

/// Bits to code a position `d2` square points from the pattern's place under an
/// isotropic Gaussian of spread `sigma`, at whole-point precision.
fn gaussian_bits(d2: f64, sigma: f64) -> f64 {
    let s2 = sigma * sigma;
    ((2.0 * std::f64::consts::PI * s2).log2() + d2 / (2.0 * s2) * std::f64::consts::LOG2_E).max(0.0)
}

/// Fits one pattern of `template` among `remaining` and judges it. The seed is
/// the em-sized cell, with its neighbours, that holds the template on the most
/// pages; the place and spread are then refitted, each page keeping at most the
/// one occurrence that the pattern codes in fewer bits than the layout model.
fn fit(
    template: &str,
    remaining: &[usize],
    occurrences: &[Occurrence],
    em: f64,
    page_count: f64,
    template_bits: f64,
) -> Option<Judgement> {
    let cell = |o: &Occurrence| ((o.x / em).floor() as i64, (o.y / em).floor() as i64);
    let mut cells: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for &i in remaining {
        cells.entry(cell(&occurrences[i])).or_default().push(i);
    }
    let mut best: Option<(usize, Vec<usize>)> = None;
    for &(cx, cy) in cells.keys() {
        let mut around = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(v) = cells.get(&(cx + dx, cy + dy)) {
                    around.extend_from_slice(v);
                }
            }
        }
        let pages: HashSet<u32> = around.iter().map(|&i| occurrences[i].page).collect();
        if best.as_ref().is_none_or(|(n, _)| pages.len() > *n) {
            best = Some((pages.len(), around));
        }
    }
    let (_, seed) = best?;
    let (mut mu, mut sigma) = place(&seed, occurrences);
    let mut members: Vec<usize> = Vec::new();
    for _ in 0..REFITS {
        let mut per_page: HashMap<u32, (f64, usize)> = HashMap::new();
        for &i in remaining {
            let o = &occurrences[i];
            let cost = gaussian_bits((o.x - mu.0).powi(2) + (o.y - mu.1).powi(2), sigma);
            if cost < o.null_position {
                let entry = per_page.entry(o.page).or_insert((f64::INFINITY, i));
                if cost < entry.0 || (cost == entry.0 && i < entry.1) {
                    *entry = (cost, i);
                }
            }
        }
        let mut next: Vec<usize> = per_page.values().map(|&(_, i)| i).collect();
        next.sort_unstable();
        if next.len() < 2 {
            return None;
        }
        let converged = next == members;
        members = next;
        (mu, sigma) = place(&members, occurrences);
        if converged {
            break;
        }
    }

    let n = members.len() as f64;
    let position_gain: f64 = members
        .iter()
        .map(|&i| {
            let o = &occurrences[i];
            o.null_position - gaussian_bits((o.x - mu.0).powi(2) + (o.y - mu.1).powi(2), sigma)
        })
        .sum();
    let number_gain = number_gain(&members, occurrences, page_count);
    let presence = if n < page_count {
        n * (page_count / n).log2() + (page_count - n) * (page_count / (page_count - n)).log2()
    } else {
        0.0
    };
    let parameters = 3.0 * 0.5 * n.log2() + template_bits;
    Some(Judgement {
        template: template.to_string(),
        members,
        mu,
        sigma,
        position_gain,
        number_gain,
        presence,
        parameters,
    })
}

/// The mean position of `members` and their isotropic spread.
fn place(members: &[usize], occurrences: &[Occurrence]) -> ((f64, f64), f64) {
    let n = members.len() as f64;
    let mx = members.iter().map(|&i| occurrences[i].x).sum::<f64>() / n;
    let my = members.iter().map(|&i| occurrences[i].y).sum::<f64>() / n;
    let var = members
        .iter()
        .map(|&i| (occurrences[i].x - mx).powi(2) + (occurrences[i].y - my).powi(2))
        .sum::<f64>()
        / (2.0 * n);
    ((mx, my), var.sqrt().max(SIGMA_FLOOR))
}

/// Bits saved by coding one number of each member as its page plus a constant
/// offset, the offset that most members share, instead of digit by digit. The
/// rule costs the offset and the share of members it fits; it saves nothing
/// when it fits fewer than two.
fn number_gain(members: &[usize], occurrences: &[Occurrence], page_count: f64) -> f64 {
    let mut offsets: BTreeMap<i64, usize> = BTreeMap::new();
    for &i in members {
        let o = &occurrences[i];
        let distinct: HashSet<i64> = o.numbers.iter().map(|n| n - i64::from(o.page)).collect();
        for c in distinct {
            *offsets.entry(c).or_insert(0) += 1;
        }
    }
    let Some((&offset, &fits)) = offsets.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) else {
        return 0.0;
    };
    if fits < 2 {
        return 0.0;
    }
    let n = members.len() as f64;
    let q = fits as f64 / n;
    let mut null = 0.0;
    let mut pattern = 0.0;
    for &i in members {
        let o = &occurrences[i];
        null += o.null_numbers;
        let target = i64::from(o.page) + offset;
        match o.numbers.iter().position(|&m| m == target) {
            Some(at) => {
                pattern += -q.log2();
                pattern += o
                    .numbers
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k != at)
                    .map(|(_, &m)| literal_number_bits(m))
                    .sum::<f64>();
            }
            None => {
                pattern += if q < 1.0 { -(1.0 - q).log2() } else { 0.0 };
                pattern += o.null_numbers;
            }
        }
    }
    let rule = (2.0 * page_count + 1.0).log2() + 0.5 * n.log2();
    (null - pattern - rule).max(0.0)
}
