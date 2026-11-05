//! Rebuilds paragraphs from the positioned text lines of PDF pages.
//!
//! The functions here are pure: they take lines with their boxes in top-origin
//! points and return text. [`order_columns`] puts a two-column page into
//! reading order, and
//! [`paragraphs`] joins the remaining lines into paragraphs, footnotes
//! included, and carries a paragraph across a page break.

use std::collections::HashSet;

use crate::normalize::normalize;

/// Lines needed entirely on each side of the page centre to call it two columns.
const COLUMN_MIN_LINES: usize = 4;
/// A gap above this many median pitches starts a paragraph.
const GAP_FACTOR: f32 = 1.5;
/// A left edge this many font sizes right of the column margin starts a paragraph.
const INDENT_FACTOR: f32 = 0.8;
/// A later note line this many font sizes right of the margin makes a note zone
/// hanging-style: its lines hang under a first line that sits at the margin.
const HANGING_FACTOR: f32 = 0.5;
/// A line under this share of the column width, ending a sentence, ends a paragraph.
const SHORT_LINE_FRACTION: f32 = 0.7;
/// Lines at or under this share of the body font size, below the body, are notes.
const NOTE_FONT_RATIO: f32 = 0.85;
/// Left edges within this many points are the same edge.
const LEFT_TOLERANCE: f32 = 1.0;
/// Font sizes within this many points count as one size.
const FONT_TOLERANCE: f32 = 0.25;
/// Vertical steps under this many points are lines on one row, not a pitch.
const MIN_PITCH: f32 = 1.0;
/// A line ending in one of these ends its sentence.
const SENTENCE_END: [char; 6] = ['.', '?', '!', ':', '\u{201D}', '"'];
/// Characters that break a word at the end of a line.
const HYPHENS: [char; 2] = ['-', '\u{2010}'];
const SOFT_HYPHEN: char = '\u{AD}';

/// One line of text with its box in top-origin page points (`top < bottom`).
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The line's text as the PDF gives it.
    pub text: String,
    /// The left edge of its box.
    pub left: f32,
    /// The top edge of its box, measured down from the top of the page.
    pub top: f32,
    /// The right edge of its box.
    pub right: f32,
    /// The bottom edge of its box, measured down from the top of the page.
    pub bottom: f32,
    /// The mean font size of its words, in points.
    pub font_size: f32,
}

impl Line {
    fn width(&self) -> f32 {
        self.right - self.left
    }

    fn middle(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }
}

/// The lines of one page, in the order the PDF reader gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLines {
    /// The physical page, counted from 1.
    pub page: u32,
    /// The page's width in points.
    pub width: f32,
    /// The page's height in points.
    pub height: f32,
    /// The page's lines.
    pub lines: Vec<Line>,
}

/// A paragraph rebuilt from lines. `bbox` is `[left, top, right, bottom]` of
/// its lines on `page`; `page_end` is the page it ends on.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidParagraph {
    /// The paragraph's text, its lines joined.
    pub text: String,
    /// The physical page it starts on.
    pub page: u32,
    /// The physical page it ends on.
    pub page_end: u32,
    /// `[left, top, right, bottom]` of its lines on `page`.
    pub bbox: [f32; 4],
    /// Whether it is a note: smaller type below the page's body text.
    pub note: bool,
}

/// Which part of the page a line belongs to. On a page without columns every
/// line is `Across`.
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Left,
    Right,
    Across,
}

const ZONES: [Zone; 3] = [Zone::Left, Zone::Right, Zone::Across];

fn zone_of(line: &Line, centre: Option<f32>) -> Zone {
    match centre {
        Some(c) if line.right <= c => Zone::Left,
        Some(c) if line.left >= c => Zone::Right,
        _ => Zone::Across,
    }
}

/// The page centre when the lines form two columns, otherwise `None`.
fn two_column_centre(lines: &[Line], page_width: f32) -> Option<f32> {
    let centre = page_width / 2.0;
    let left = lines.iter().filter(|l| l.right <= centre).count();
    let right = lines.iter().filter(|l| l.left >= centre).count();
    (left >= COLUMN_MIN_LINES && right >= COLUMN_MIN_LINES).then_some(centre)
}

/// Puts the lines of a page into reading order.
///
/// A page is two columns when at least four lines lie entirely left of the
/// centre and at least four entirely right of it. The columns start at the
/// first row where a line of each column shares the height of a line of the
/// other. Then the order is: the lines above the columns, the left column, the
/// right column, and the remaining lines that cross the centre, among them any
/// that sit between column lines. Each part runs top to bottom. The lines
/// above the columns are those that cross the centre, and any line of a column
/// that sits above the last of them, such as a short heading over a paragraph
/// that spans the page. Any other page keeps the order it was given.
pub fn order_columns(lines: Vec<Line>, page_width: f32) -> Vec<Line> {
    let Some(centre) = two_column_centre(&lines, page_width) else {
        return lines;
    };
    let zone = |line: &Line| zone_of(line, Some(centre));
    let rows_of = |wanted: Zone| Rows::new(lines.iter().filter(|l| zone(l) == wanted));
    let (left_rows, right_rows) = (rows_of(Zone::Left), rows_of(Zone::Right));
    let shares_a_row = |line: &Line| match zone(line) {
        Zone::Left => right_rows.overlap(line),
        Zone::Right => left_rows.overlap(line),
        Zone::Across => false,
    };
    let top = |counted: &dyn Fn(&Line) -> bool| {
        lines
            .iter()
            .filter(|l| counted(l))
            .map(|l| l.top)
            .reduce(f32::min)
    };
    // Columns that never share a row start at their first line.
    let columns_top = top(&shares_a_row)
        .or_else(|| top(&|l| zone(l) != Zone::Across))
        .unwrap_or(f32::INFINITY);
    let header_bottom = lines
        .iter()
        .filter(|l| zone(l) == Zone::Across && l.middle() < columns_top)
        .map(Line::middle)
        .fold(f32::NEG_INFINITY, f32::max);
    let (mut above, mut left, mut right, mut below) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in lines {
        match zone(&line) {
            Zone::Across if line.middle() < columns_top => above.push(line),
            Zone::Across => below.push(line),
            _ if line.middle() < header_bottom => above.push(line),
            Zone::Left => left.push(line),
            Zone::Right => right.push(line),
        }
    }
    [above, left, right, below]
        .into_iter()
        .flat_map(|mut part| {
            part.sort_by(|a, b| a.top.total_cmp(&b.top));
            part
        })
        .collect()
}

/// The heights the lines of one column take up, to ask whether a line of the
/// other column shares a row with one of them.
struct Rows {
    /// The tops of the lines, ascending.
    tops: Vec<f32>,
    /// `max_bottom[i]` is the lowest bottom among the first `i + 1` lines.
    max_bottom: Vec<f32>,
}

impl Rows {
    fn new<'a>(lines: impl Iterator<Item = &'a Line>) -> Rows {
        let mut spans: Vec<(f32, f32)> = lines.map(|l| (l.top, l.bottom)).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut lowest = f32::NEG_INFINITY;
        Rows {
            tops: spans.iter().map(|span| span.0).collect(),
            max_bottom: spans
                .iter()
                .map(|span| {
                    lowest = lowest.max(span.1);
                    lowest
                })
                .collect(),
        }
    }

    /// Whether one of the lines overlaps `line` in height: it starts above
    /// the bottom of `line` and ends below its top.
    fn overlap(&self, line: &Line) -> bool {
        let starting_above = self.tops.partition_point(|&top| top < line.bottom);
        starting_above > 0 && self.max_bottom[starting_above - 1] > line.top
    }
}

/// The value that most of `values` lie within `tolerance` of; on a tie, the
/// larger value.
fn densest(values: &[f32], tolerance: f32) -> Option<f32> {
    let mut best: Option<(usize, f32)> = None;
    for &value in values {
        let count = values
            .iter()
            .filter(|&&other| (other - value).abs() <= tolerance)
            .count();
        let better = match best {
            None => true,
            Some((best_count, best_value)) => {
                count > best_count || (count == best_count && value > best_value)
            }
        };
        if better {
            best = Some((count, value));
        }
    }
    best.map(|(_, value)| value)
}

fn median(mut values: Vec<f32>) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(SENTENCE_END)
}

/// Whether the line starts at least `factor` font sizes right of `margin`. A
/// line whose font size is not a positive number has no indent to measure.
fn is_right_of(line: &Line, margin: f32, factor: f32) -> bool {
    line.font_size.is_finite()
        && line.font_size > 0.0
        && line.left - margin >= factor * line.font_size
}

/// Where the text of one zone starts and how wide it is.
#[derive(Clone, Copy)]
struct Extent {
    /// The smallest left edge of the zone's lines.
    margin: f32,
    /// From the smallest left to the largest right.
    width: f32,
    /// The zone is notes whose lines hang under a first line at the margin.
    hanging: bool,
}

/// Lines in reading order, measured for the break rules. The margin, the width
/// and the pitch come from these lines alone, so each page and each zone gets
/// its own margin.
struct Flow<'a> {
    lines: &'a [Line],
    centre: Option<f32>,
    extents: [Option<Extent>; 3],
    pitch: Option<f32>,
}

impl<'a> Flow<'a> {
    /// `notes` says the lines are footnotes, whose zones may be hanging-style.
    fn new(lines: &'a [Line], centre: Option<f32>, notes: bool) -> Self {
        let mut extents = [None; 3];
        for zone in ZONES {
            let members: Vec<&Line> = lines
                .iter()
                .filter(|l| zone_of(l, centre) == zone)
                .collect();
            let Some(first) = members.first() else {
                continue;
            };
            let margin = members.iter().map(|l| l.left).fold(f32::INFINITY, f32::min);
            let right = members
                .iter()
                .map(|l| l.right)
                .fold(f32::NEG_INFINITY, f32::max);
            let hanging = notes
                && first.left - margin <= LEFT_TOLERANCE
                && members
                    .iter()
                    .skip(1)
                    .any(|l| is_right_of(l, margin, HANGING_FACTOR));
            extents[zone as usize] = Some(Extent {
                margin,
                width: right - margin,
                hanging,
            });
        }
        let pitches = lines
            .windows(2)
            .map(|pair| pair[1].top - pair[0].top)
            .filter(|pitch| *pitch >= MIN_PITCH)
            .collect();
        Flow {
            lines,
            centre,
            extents,
            pitch: median(pitches),
        }
    }

    fn extent(&self, line: &Line) -> Option<Extent> {
        self.extents[zone_of(line, self.centre) as usize]
    }

    fn is_indented(&self, line: &Line) -> bool {
        self.extent(line)
            .is_some_and(|e| is_right_of(line, e.margin, INDENT_FACTOR))
    }

    /// A line starts a paragraph after a gap over `GAP_FACTOR` pitches. Then
    /// the rule depends on the zone. In a hanging-style note zone, a line at
    /// the margin starts a note and indented lines continue it. Elsewhere, an
    /// indented line starts a paragraph when the previous line sits further
    /// left (so a block quote's lines go on one paragraph), and so does a line
    /// after a short line that ends a sentence.
    fn starts_paragraph(&self, previous: &Line, line: &Line) -> bool {
        let gap = self
            .pitch
            .is_some_and(|pitch| line.top - previous.top > GAP_FACTOR * pitch);
        if gap {
            return true;
        }
        if let Some(extent) = self.extent(line).filter(|e| e.hanging) {
            return line.left - extent.margin <= LEFT_TOLERANCE;
        }
        let indent = self.is_indented(line) && line.left - previous.left > LEFT_TOLERANCE;
        let short_end = ends_sentence(&previous.text)
            && self
                .extent(previous)
                .is_some_and(|e| previous.width() < SHORT_LINE_FRACTION * e.width);
        indent || short_end
    }

    /// Cuts the lines into runs that make one paragraph each.
    fn paragraphs(&self) -> Vec<&'a [Line]> {
        let mut runs = Vec::new();
        let mut start = 0;
        for i in 1..self.lines.len() {
            if self.starts_paragraph(&self.lines[i - 1], &self.lines[i]) {
                runs.push(&self.lines[start..i]);
                start = i;
            }
        }
        if start < self.lines.len() {
            runs.push(&self.lines[start..]);
        }
        runs
    }
}

/// Splits ordered lines into body lines and notes. The body size is the most
/// common font size; a line at most `NOTE_FONT_RATIO` of it that lies below
/// every body-size line is a note. A page whose body size is not a positive
/// number has no notes.
fn split_notes(lines: Vec<Line>) -> (Vec<Line>, Vec<Line>) {
    let sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    let body_size = densest(&sizes, FONT_TOLERANCE).filter(|size| size.is_finite() && *size > 0.0);
    let Some(body_size) = body_size else {
        return (lines, Vec::new());
    };
    let small = |l: &Line| l.font_size <= NOTE_FONT_RATIO * body_size;
    let body_bottom = lines
        .iter()
        .filter(|l| !small(l))
        .map(|l| l.bottom)
        .fold(f32::NEG_INFINITY, f32::max);
    lines
        .into_iter()
        .partition(|l| !(small(l) && l.middle() > body_bottom))
}

/// A paragraph still open: its raw line texts, which are joined when it is done.
struct Draft {
    lines: Vec<String>,
    page: u32,
    page_end: u32,
    bbox: [f32; 4],
    note: bool,
}

impl Draft {
    fn new(run: &[Line], page: u32, note: bool) -> Self {
        let bbox = run.iter().fold(
            [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ],
            |b, l| {
                [
                    b[0].min(l.left),
                    b[1].min(l.top),
                    b[2].max(l.right),
                    b[3].max(l.bottom),
                ]
            },
        );
        Draft {
            lines: run.iter().map(|l| l.text.clone()).collect(),
            page,
            page_end: page,
            bbox,
            note,
        }
    }
}

/// Rebuilds the paragraphs of a book from its pages.
///
/// For each page, lines in `skip` (furniture) and blank lines are dropped, the
/// rest are put in reading order, and footnotes are set apart. A new paragraph
/// starts at a gap over 1.5 median pitches, at a line indented 0.8 font sizes
/// past the column's left margin (the smallest left edge) when the line before
/// it sits further left, or after a short line that ends a sentence. Footnotes
/// that hang under a first line at the margin start a note at each return to
/// the margin instead. A hyphen at a line end is dropped when the line is joined to the
/// next, unless the next line starts with an uppercase letter or a digit or the
/// same compound appears elsewhere in the book; a soft hyphen is always dropped.
/// The last body paragraph of a page runs on to the next page when it lacks
/// terminal punctuation and the next page's first body line is not indented.
/// Notes come after the body paragraphs of their page and never run on.
pub fn paragraphs(pages: &[PageLines], skip: &HashSet<(u32, usize)>) -> Vec<LaidParagraph> {
    let compounds = known_compounds(pages);
    let mut drafts: Vec<Draft> = Vec::new();
    // The last body paragraph of the previous page: its draft and its page.
    let mut carry: Option<(usize, u32)> = None;

    for page in pages {
        let kept: Vec<Line> = page
            .lines
            .iter()
            .enumerate()
            .filter(|(index, line)| {
                !skip.contains(&(page.page, *index)) && !line.text.trim().is_empty()
            })
            .map(|(_, line)| line.clone())
            .collect();
        let centre = two_column_centre(&kept, page.width);
        let (body, notes) = split_notes(order_columns(kept, page.width));

        let body_flow = Flow::new(&body, centre, false);
        let mut last_body = None;
        for (n, run) in body_flow.paragraphs().into_iter().enumerate() {
            let continued = match carry {
                Some((index, previous_page)) if n == 0 => {
                    let open = &drafts[index];
                    let unfinished = open.lines.last().is_some_and(|text| !ends_sentence(text));
                    (previous_page + 1 == page.page
                        && unfinished
                        && !body_flow.is_indented(&run[0]))
                    .then_some(index)
                }
                _ => None,
            };
            match continued {
                Some(index) => {
                    let draft = &mut drafts[index];
                    draft.lines.extend(run.iter().map(|l| l.text.clone()));
                    draft.page_end = page.page;
                    last_body = Some(index);
                }
                None => {
                    drafts.push(Draft::new(run, page.page, false));
                    last_body = Some(drafts.len() - 1);
                }
            }
        }
        carry = last_body.map(|index| (index, page.page));

        let note_flow = Flow::new(&notes, centre, true);
        for run in note_flow.paragraphs() {
            drafts.push(Draft::new(run, page.page, true));
        }
    }

    drafts
        .into_iter()
        .filter_map(|draft| {
            let text = normalize(&join_lines(&draft.lines, &compounds));
            (!text.is_empty()).then_some(LaidParagraph {
                text,
                page: draft.page,
                page_end: draft.page_end,
                bbox: draft.bbox,
                note: draft.note,
            })
        })
        .collect()
}

/// Joins raw line texts into one string, mending words broken at line ends.
fn join_lines(lines: &[String], compounds: &HashSet<String>) -> String {
    let mut text = String::new();
    for line in lines {
        let next = line.trim();
        if next.is_empty() {
            continue;
        }
        text.truncate(text.trim_end().len());
        if text.is_empty() {
            text.push_str(next);
        } else if text.ends_with(SOFT_HYPHEN) {
            text.pop();
            text.push_str(next);
        } else if let Some(stem) = text
            .strip_suffix(HYPHENS)
            .filter(|stem| stem.ends_with(char::is_alphanumeric))
        {
            let stem_len = stem.len();
            let keeps_hyphen = next.starts_with(|c: char| c.is_uppercase() || c.is_numeric())
                || compounds.contains(&compound_key(stem, next));
            if !keeps_hyphen {
                text.truncate(stem_len);
            }
            text.push_str(next);
        } else {
            text.push(' ');
            text.push_str(next);
        }
    }
    text
}

/// The lowercase form `left-right` from the word that ends `left` and the word
/// that starts `right`.
fn compound_key(left: &str, right: &str) -> String {
    let left_start = left
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric())
        .last()
        .map_or(left.len(), |(i, _)| i);
    let right_end = right
        .char_indices()
        .find(|(_, c)| !c.is_alphanumeric())
        .map_or(right.len(), |(i, _)| i);
    format!(
        "{}-{}",
        left[left_start..].to_lowercase(),
        right[..right_end].to_lowercase()
    )
}

/// Every `left-right` that occurs inside a line of the book.
fn known_compounds(pages: &[PageLines]) -> HashSet<String> {
    let mut compounds = HashSet::new();
    for line in pages.iter().flat_map(|page| &page.lines) {
        if !line.text.contains(HYPHENS) {
            continue;
        }
        for word in normalize(&line.text).split(' ') {
            let parts: Vec<&str> = word.split(HYPHENS).collect();
            for pair in parts.windows(2) {
                if pair[0].ends_with(char::is_alphanumeric)
                    && pair[1].starts_with(char::is_alphanumeric)
                {
                    compounds.insert(compound_key(pair[0], pair[1]));
                }
            }
        }
    }
    compounds
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE_W: f32 = 432.0;
    const PAGE_H: f32 = 648.0;
    const BODY_PT: f32 = 10.0;
    const NOTE_PT: f32 = 8.0;
    const PITCH: f32 = 12.0;
    const FIRST_ROW_TOP: f32 = 100.0;
    const RECTO_MARGIN: f32 = 59.9;
    const VERSO_MARGIN: f32 = 58.98;
    const INDENT: f32 = 10.0;
    const HANG: f32 = 8.0;
    const TEXT_WIDTH: f32 = 312.0;
    const SHORT_WIDTH: f32 = 90.0;

    fn line_at(text: &str, left: f32, top: f32, width: f32, font_size: f32) -> Line {
        Line {
            text: text.to_string(),
            left,
            top,
            right: left + width,
            bottom: top + 1.2 * font_size,
            font_size,
        }
    }

    fn row_top(row: u8) -> f32 {
        FIRST_ROW_TOP + f32::from(row) * PITCH
    }

    /// A justified body line that starts at the margin.
    fn full(text: &str, row: u8, margin: f32) -> Line {
        line_at(text, margin, row_top(row), TEXT_WIDTH, BODY_PT)
    }

    /// A body line with a first-line indent that ends at the same right edge.
    fn indented(text: &str, row: u8, margin: f32) -> Line {
        line_at(
            text,
            margin + INDENT,
            row_top(row),
            TEXT_WIDTH - INDENT,
            BODY_PT,
        )
    }

    /// The short last line of a paragraph.
    fn short(text: &str, row: u8, margin: f32) -> Line {
        line_at(text, margin, row_top(row), SHORT_WIDTH, BODY_PT)
    }

    /// A body paragraph of `rows` lines, enough to make 10 pt the usual size of
    /// the page when notes are added.
    fn body_paragraph(rows: u8, margin: f32) -> Vec<Line> {
        (0..rows)
            .map(|row| {
                if row + 1 == rows {
                    short("and so on to its end.", row, margin)
                } else {
                    full("a body line that runs the full width", row, margin)
                }
            })
            .collect()
    }

    /// A footnote line; `row` counts 8 pt lines from the top of the notes.
    fn note_line(text: &str, left: f32, row: u8, width: f32) -> Line {
        line_at(text, left, 480.0 + f32::from(row) * 9.6, width, NOTE_PT)
    }

    fn page(number: u32, lines: Vec<Line>) -> PageLines {
        PageLines {
            page: number,
            width: PAGE_W,
            height: PAGE_H,
            lines,
        }
    }

    fn lay(pages: &[PageLines]) -> Vec<LaidParagraph> {
        paragraphs(pages, &HashSet::new())
    }

    fn texts(paras: &[LaidParagraph]) -> Vec<&str> {
        paras.iter().map(|p| p.text.as_str()).collect()
    }

    fn line_texts(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn vertical_gap_breaks_paragraph() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                full("First paragraph line one", 0, m),
                full("line two", 1, m),
                full("line three", 2, m),
                full("Second paragraph after a gap", 5, m),
                full("and its end", 6, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            [
                "First paragraph line one line two line three",
                "Second paragraph after a gap and its end"
            ]
        );
    }

    #[test]
    fn indent_breaks_paragraph() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                indented("The first paragraph opens", 0, m),
                full("and runs on", 1, m),
                full("for three lines", 2, m),
                indented("The second paragraph opens", 3, m),
                full("and runs on", 4, m),
                full("for three lines", 5, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            [
                "The first paragraph opens and runs on for three lines",
                "The second paragraph opens and runs on for three lines"
            ]
        );
    }

    #[test]
    fn short_last_line_ends_paragraph() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                full("A paragraph that fills its lines", 0, m),
                full("until it reaches", 1, m),
                short("a short last line.", 2, m),
                full("The next paragraph is flush left", 3, m),
                full("and continues", 4, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            [
                "A paragraph that fills its lines until it reaches a short last line.",
                "The next paragraph is flush left and continues"
            ]
        );
    }

    #[test]
    fn short_line_without_terminal_punctuation_does_not_end_paragraph() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                full("A paragraph that fills its lines", 0, m),
                short("with one short line", 1, m),
                full("and then goes on", 2, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            ["A paragraph that fills its lines with one short line and then goes on"]
        );
    }

    #[test]
    fn indented_majority_page_still_breaks() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                indented("The first paragraph opens", 0, m),
                full("and runs on", 1, m),
                indented("The second paragraph opens", 2, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            [
                "The first paragraph opens and runs on",
                "The second paragraph opens"
            ]
        );
    }

    #[test]
    fn block_quote_lines_stay_one_paragraph() {
        let m = RECTO_MARGIN;
        let quote =
            |text: &str, row: u8| line_at(text, m + 20.0, row_top(row), TEXT_WIDTH - 40.0, BODY_PT);
        let pages = vec![page(
            1,
            vec![
                full("A body paragraph introduces", 0, m),
                full("the passage that is quoted", 1, m),
                full("in the lines that follow", 2, m),
                quote("The quoted passage is set in", 3),
                quote("from the margin on both sides", 4),
                quote("and keeps one indent on", 5),
                quote("every one of its lines", 6),
                full("The body resumes at the margin", 9, m),
            ],
        )];

        assert_eq!(
            texts(&lay(&pages)),
            [
                "A body paragraph introduces the passage that is quoted in the lines that follow",
                "The quoted passage is set in from the margin on both sides and keeps one indent on every one of its lines",
                "The body resumes at the margin"
            ]
        );
    }

    #[test]
    fn zero_font_size_page_has_no_notes_and_no_indent_breaks() {
        let m = RECTO_MARGIN;
        for size in [0.0, f32::NAN] {
            let lines = vec![
                line_at("A line of a page", m, row_top(0), TEXT_WIDTH, size),
                line_at("in a font of no size", m, row_top(1), TEXT_WIDTH, size),
                line_at(
                    "with an indented line",
                    m + INDENT,
                    row_top(2),
                    TEXT_WIDTH - INDENT,
                    size,
                ),
                line_at("and a last line", m, row_top(3), TEXT_WIDTH, size),
            ];

            let paras = lay(&[page(1, lines)]);

            assert!(paras.iter().all(|p| !p.note), "size {size}");
            assert_eq!(
                texts(&paras),
                ["A line of a page in a font of no size with an indented line and a last line"],
                "size {size}"
            );
        }
    }

    // Hyphenation

    #[test]
    fn line_end_hyphen_is_joined() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![full("infor-", 0, m), full("mation theory", 1, m)],
        )];

        assert_eq!(texts(&lay(&pages)), ["information theory"]);
    }

    #[test]
    fn known_compound_keeps_hyphen() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![
                short("A self-evident claim.", 0, m),
                full("We hold these self-", 1, m),
                full("evident truths.", 2, m),
            ],
        )];

        let paras = lay(&pages);
        let texts = texts(&paras);
        assert_eq!(texts.len(), 2);
        assert!(texts[1].contains("self-evident truths"));
    }

    #[test]
    fn uppercase_after_hyphen_keeps_it() {
        let m = RECTO_MARGIN;
        let pages = vec![page(1, vec![full("Anglo-", 0, m), full("Saxon", 1, m)])];

        assert_eq!(texts(&lay(&pages)), ["Anglo-Saxon"]);
    }

    #[test]
    fn digit_after_hyphen_keeps_it() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![full("pre-", 0, m), full("1900 reforms", 1, m)],
        )];

        assert_eq!(texts(&lay(&pages)), ["pre-1900 reforms"]);
    }

    #[test]
    fn soft_hyphen_at_line_end_joins() {
        let m = RECTO_MARGIN;
        let pages = vec![page(
            1,
            vec![full("infor\u{AD}", 0, m), full("mation", 1, m)],
        )];
        assert_eq!(texts(&lay(&pages)), ["information"]);

        // Unlike a hyphen, a soft hyphen joins even before an uppercase letter.
        let pages = vec![page(
            1,
            vec![full("Anglo\u{AD}", 0, m), full("Saxon", 1, m)],
        )];
        assert_eq!(texts(&lay(&pages)), ["AngloSaxon"]);
    }

    #[test]
    fn hyphenated_word_is_joined_across_a_page_break() {
        let pages = vec![
            page(
                1,
                vec![
                    indented("The capacity depends on", 0, RECTO_MARGIN),
                    full("the theory of infor-", 1, RECTO_MARGIN),
                ],
            ),
            page(
                2,
                vec![
                    full("mation, which is bounded.", 0, VERSO_MARGIN),
                    full("It follows from this", 1, VERSO_MARGIN),
                ],
            ),
        ];

        let paras = lay(&pages);
        assert_eq!(
            paras[0].text,
            "The capacity depends on the theory of information, which is bounded. It follows from this"
        );
        assert_eq!((paras[0].page, paras[0].page_end), (1, 2));
    }

    // Pages

    #[test]
    fn paragraph_continues_across_pages() {
        let pages = vec![
            page(
                1,
                vec![
                    indented("Shannon showed that the capacity of a", 0, RECTO_MARGIN),
                    full("channel in the presence of noise and the", 1, RECTO_MARGIN),
                    full("capacity is", 2, RECTO_MARGIN),
                ],
            ),
            page(
                2,
                vec![
                    short("bounded by entropy.", 0, VERSO_MARGIN),
                    full("A new paragraph starts here", 1, VERSO_MARGIN),
                ],
            ),
        ];

        let paras = lay(&pages);

        assert_eq!(paras.len(), 2);
        assert_eq!((paras[0].page, paras[0].page_end), (1, 2));
        assert_eq!(
            paras[0].text,
            "Shannon showed that the capacity of a channel in the presence of noise and the capacity is bounded by entropy."
        );
        // The box covers the lines on the first page only.
        assert_eq!(
            paras[0].bbox,
            [
                RECTO_MARGIN,
                row_top(0),
                RECTO_MARGIN + TEXT_WIDTH,
                row_top(2) + 1.2 * BODY_PT
            ]
        );
        assert_eq!((paras[1].page, paras[1].page_end), (2, 2));
    }

    #[test]
    fn terminal_punctuation_stops_a_paragraph_at_the_page_end() {
        let pages = vec![
            page(
                1,
                vec![
                    indented("This paragraph ends exactly at the foot", 0, RECTO_MARGIN),
                    full("of the page.", 1, RECTO_MARGIN),
                ],
            ),
            page(
                2,
                vec![full("A flush line on the next page", 0, VERSO_MARGIN)],
            ),
        ];

        let paras = lay(&pages);

        assert_eq!(paras.len(), 2);
        assert_eq!((paras[0].page, paras[0].page_end), (1, 1));
    }

    #[test]
    fn indented_first_line_on_new_page_starts_new_paragraph() {
        let pages = vec![
            page(
                1,
                vec![
                    indented("Shannon showed that the capacity of a", 0, RECTO_MARGIN),
                    full("capacity is", 1, RECTO_MARGIN),
                ],
            ),
            page(
                2,
                vec![
                    indented("A fresh paragraph begins here", 0, VERSO_MARGIN),
                    full("and runs on", 1, VERSO_MARGIN),
                ],
            ),
        ];

        let paras = lay(&pages);

        assert_eq!(paras.len(), 2);
        assert_eq!((paras[0].page, paras[0].page_end), (1, 1));
        assert_eq!((paras[1].page, paras[1].page_end), (2, 2));
    }

    #[test]
    fn margin_is_measured_per_page() {
        // Recto, verso, and a page with a wide margin: the indent is 10 pt on each.
        let build = |number: u32, margin: f32| {
            page(
                number,
                vec![
                    indented("The first paragraph opens", 0, margin),
                    full("and runs on", 1, margin),
                    short("until it ends.", 2, margin),
                    indented("The second paragraph opens", 3, margin),
                    full("and runs on", 4, margin),
                    short("until it ends too.", 5, margin),
                ],
            )
        };
        let pages = vec![
            build(1, RECTO_MARGIN),
            build(2, VERSO_MARGIN),
            build(3, 90.0),
        ];

        let paras = lay(&pages);

        assert_eq!(paras.len(), 6);
        for (i, p) in paras.iter().enumerate() {
            let number = u32::try_from(i / 2).unwrap() + 1;
            assert_eq!((p.page, p.page_end), (number, number));
        }
        assert_eq!(
            paras[0].text,
            "The first paragraph opens and runs on until it ends."
        );
        assert_eq!(
            paras[5].text,
            "The second paragraph opens and runs on until it ends too."
        );
    }

    // Columns

    /// Twelve lines in two columns, given row by row as pdf_oxide returns them.
    fn two_column_lines() -> Vec<Line> {
        let mut lines = Vec::new();
        for row in 0..6u8 {
            let last = row == 5;
            let top = row_top(row);
            let left_width = if last { 70.0 } else { 165.0 };
            lines.push(line_at(
                &format!("Alpha line {row}{}", if last { "." } else { "" }),
                40.0,
                top,
                left_width,
                BODY_PT,
            ));
            lines.push(line_at(
                &format!("Omega line {row}"),
                227.0,
                top,
                165.0,
                BODY_PT,
            ));
        }
        lines
    }

    #[test]
    fn two_columns_read_left_then_right() {
        let pages = vec![page(1, two_column_lines())];

        let paras = lay(&pages);

        assert!(
            paras
                .iter()
                .all(|p| !(p.text.contains("Alpha") && p.text.contains("Omega")))
        );
        assert_eq!(
            texts(&paras),
            [
                "Alpha line 0 Alpha line 1 Alpha line 2 Alpha line 3 Alpha line 4 Alpha line 5.",
                "Omega line 0 Omega line 1 Omega line 2 Omega line 3 Omega line 4 Omega line 5"
            ]
        );
    }

    #[test]
    fn a_paragraph_runs_on_from_the_left_column_to_the_right_one() {
        let mut lines = two_column_lines();
        // The left column no longer ends on a short sentence.
        lines[10] = line_at("Alpha line 5 and on", 40.0, row_top(5), 165.0, BODY_PT);
        let pages = vec![page(1, lines)];

        let paras = lay(&pages);

        assert_eq!(paras.len(), 1);
        assert!(paras[0].text.starts_with("Alpha line 0 "));
        assert!(paras[0].text.contains("Alpha line 5 and on Omega line 0 "));
    }

    #[test]
    fn order_columns_puts_full_width_lines_around_the_columns() {
        let mut lines = vec![line_at("Title", 40.0, 40.0, 352.0, 14.0)];
        lines.extend(two_column_lines());
        lines.push(line_at("Footer", 40.0, 400.0, 352.0, BODY_PT));

        let ordered = order_columns(lines, PAGE_W);

        let names = line_texts(&ordered);
        assert_eq!(names.len(), 14);
        assert_eq!(names[0], "Title");
        assert_eq!(names[1], "Alpha line 0");
        assert_eq!(names[6], "Alpha line 5.");
        assert_eq!(names[7], "Omega line 0");
        assert_eq!(names[12], "Omega line 5");
        assert_eq!(names[13], "Footer");
    }

    #[test]
    fn a_heading_and_a_paragraph_above_the_columns_are_read_before_them() {
        let mut lines = vec![
            // Short enough to lie left of the centre.
            line_at("Heading", 40.0, 40.0, 80.0, 14.0),
            line_at("A paragraph across the page", 40.0, 64.0, 352.0, BODY_PT),
            line_at("over both columns.", 40.0, 76.0, 200.0, BODY_PT),
        ];
        lines.extend(two_column_lines());

        let ordered = order_columns(lines, PAGE_W);

        let names = line_texts(&ordered);
        assert_eq!(
            names[..5],
            [
                "Heading",
                "A paragraph across the page",
                "over both columns.",
                "Alpha line 0",
                "Alpha line 1"
            ]
        );
        assert_eq!(names[8], "Alpha line 5.");
        assert_eq!(names[9], "Omega line 0");
        assert_eq!(names.len(), 15);
    }

    #[test]
    fn a_column_that_starts_a_row_higher_stays_in_its_column() {
        let lines: Vec<Line> = two_column_lines()
            .into_iter()
            .filter(|l| l.text != "Alpha line 0")
            .collect();

        let ordered = order_columns(lines, PAGE_W);

        let names = line_texts(&ordered);
        assert_eq!(names[0], "Alpha line 1");
        assert_eq!(names[5], "Omega line 0");
    }

    #[test]
    fn fewer_than_four_lines_on_a_side_leave_the_order_alone() {
        let lines: Vec<Line> = two_column_lines()
            .into_iter()
            .filter(|l| !l.text.starts_with("Omega line 3") && !l.text.starts_with("Omega line 4"))
            .filter(|l| !l.text.starts_with("Omega line 5"))
            .collect();
        assert_eq!(lines.len(), 9);

        let ordered = order_columns(lines.clone(), PAGE_W);

        assert_eq!(ordered, lines);
    }

    // Notes

    #[test]
    fn footnotes_become_notes_and_do_not_block_continuation() {
        let m = RECTO_MARGIN;
        let pages = vec![
            page(
                1,
                vec![
                    indented("Shannon showed that the capacity of a", 0, m),
                    full("channel in the presence of noise and the", 1, m),
                    full("capacity is", 2, m),
                    line_at(
                        "1 C. Shannon, A Mathematical Theory of",
                        m,
                        480.0,
                        TEXT_WIDTH,
                        NOTE_PT,
                    ),
                    line_at("Communication (1948), p. 5.", m, 489.6, 120.0, NOTE_PT),
                ],
            ),
            page(2, vec![short("bounded by entropy.", 0, VERSO_MARGIN)]),
        ];

        let paras = lay(&pages);

        assert!(paras.iter().any(|p| p.note && p.page == 1));
        let body = paras.iter().find(|p| !p.note).unwrap();
        assert_eq!((body.page, body.page_end), (1, 2));
        assert_eq!(
            body.text,
            "Shannon showed that the capacity of a channel in the presence of noise and the capacity is bounded by entropy."
        );
        // Notes follow the body paragraphs of their page.
        assert_eq!(paras.len(), 2);
        assert!(paras[1].note);
        assert_eq!(
            paras[1].text,
            "1 C. Shannon, A Mathematical Theory of Communication (1948), p. 5."
        );
        assert_eq!((paras[1].page, paras[1].page_end), (1, 1));
    }

    #[test]
    fn lone_two_line_hanging_note_stays_whole() {
        let m = RECTO_MARGIN;
        let mut lines = body_paragraph(7, m);
        lines.push(note_line(
            "1 A note whose first line sits at the margin and",
            m,
            0,
            TEXT_WIDTH,
        ));
        lines.push(note_line(
            "whose second line hangs under the number.",
            m + HANG,
            1,
            TEXT_WIDTH - HANG,
        ));

        let paras = lay(&[page(1, lines)]);

        assert_eq!(paras.len(), 2);
        assert!(paras[1].note);
        assert_eq!(
            paras[1].text,
            "1 A note whose first line sits at the margin and whose second line hangs under the number."
        );
    }

    #[test]
    fn hanging_notes_split_on_return_to_margin() {
        let m = RECTO_MARGIN;
        let hang = |text: &str, row: u8| note_line(text, m + HANG, row, TEXT_WIDTH - HANG);
        let mut lines = body_paragraph(9, m);
        lines.extend([
            note_line(
                "1 The first note begins at the margin and",
                m,
                0,
                TEXT_WIDTH,
            ),
            hang("goes on in a line that hangs under its", 1),
            hang("number without any full stop", 2),
            note_line(
                "2 The second note begins at the margin and",
                m,
                3,
                TEXT_WIDTH,
            ),
            hang("goes on in one hanging line", 4),
            note_line(
                "3 The third note begins at the margin and",
                m,
                5,
                TEXT_WIDTH,
            ),
            hang("goes on in a line that hangs under", 6),
            hang("its number as well", 7),
        ]);

        let paras = lay(&[page(1, lines)]);

        assert_eq!(paras.len(), 4);
        assert!(!paras[0].note);
        assert!(paras[1..].iter().all(|p| p.note));
        assert_eq!(
            paras[1].text,
            "1 The first note begins at the margin and goes on in a line that hangs under its number without any full stop"
        );
        assert_eq!(
            paras[2].text,
            "2 The second note begins at the margin and goes on in one hanging line"
        );
        assert_eq!(
            paras[3].text,
            "3 The third note begins at the margin and goes on in a line that hangs under its number as well"
        );
    }

    #[test]
    fn notes_with_an_indented_first_line_use_the_body_rules() {
        let m = RECTO_MARGIN;
        let mut lines = body_paragraph(8, m);
        lines.extend([
            note_line(
                "1 The first note opens with an indent",
                m + HANG,
                0,
                TEXT_WIDTH - HANG,
            ),
            note_line("and goes on at the margin", m, 1, TEXT_WIDTH),
            note_line("until it ends.", m, 2, 60.0),
            note_line(
                "2 The second note opens with an indent",
                m + HANG,
                3,
                TEXT_WIDTH - HANG,
            ),
            note_line("and goes on at the margin", m, 4, TEXT_WIDTH),
        ]);

        let paras = lay(&[page(1, lines)]);

        assert_eq!(paras.len(), 3);
        assert_eq!(
            paras[1].text,
            "1 The first note opens with an indent and goes on at the margin until it ends."
        );
        assert_eq!(
            paras[2].text,
            "2 The second note opens with an indent and goes on at the margin"
        );
    }

    // Whole-pipeline behaviour

    #[test]
    fn skipped_lines_are_left_out_of_paragraphs() {
        let pages: Vec<PageLines> = (1..=4u32)
            .map(|n| {
                page(
                    n,
                    vec![
                        line_at("Chapter 3 · Entropy", RECTO_MARGIN, 30.0, 140.0, 9.0),
                        short(&format!("Body text of page {n}."), 0, RECTO_MARGIN),
                        line_at(&(11 + n).to_string(), 200.0, 600.0, 12.0, BODY_PT),
                    ],
                )
            })
            .collect();
        let skip: HashSet<(u32, usize)> = (1..=4).flat_map(|n| [(n, 0), (n, 2)]).collect();

        let paras = paragraphs(&pages, &skip);

        assert_eq!(
            texts(&paras),
            [
                "Body text of page 1.",
                "Body text of page 2.",
                "Body text of page 3.",
                "Body text of page 4."
            ]
        );
    }

    #[test]
    fn paragraph_text_is_normalized() {
        let pages = vec![page(
            1,
            vec![
                full("ﬁnal  ﬂow", 0, RECTO_MARGIN),
                full("of  water", 1, RECTO_MARGIN),
            ],
        )];

        assert_eq!(texts(&lay(&pages)), ["final flow of water"]);
    }
}
