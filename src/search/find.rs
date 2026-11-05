//! `find`: every passage that contains an exact phrase, in reading order.

use schemars::JsonSchema;
use serde::Serialize;

use crate::analysis::{EXACT, tokens};
use crate::index::book::Book;
use crate::passages::PassageLocator;
use crate::search::highlight::{phrase_ranges, to_char_ranges};
use crate::search::passage;
use crate::settings::FIND_PAGE;
use crate::{Error, Result};

/// A passage that contains the phrase. `matches` are `[start, end]` Unicode
/// code-point ranges of the passage text, one per occurrence.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FindHit {
    /// The passage's id, which `read` takes.
    pub passage_id: u32,
    /// The texts of the headings above it, outermost first.
    pub section: Vec<String>,
    /// Where it is in the book.
    pub locator: PassageLocator,
    /// One range per occurrence of the phrase in the passage's text.
    pub matches: Vec<[usize; 2]>,
}

/// One page of the passages that contain `phrase`. The totals count the whole
/// book, not the page. `next_cursor` is the cursor of the next page, if any.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FindResult {
    /// The phrase, trimmed.
    pub phrase: String,
    /// How many times the phrase occurs in the whole book.
    pub total_occurrences: usize,
    /// How many passages of the whole book hold it.
    pub total_passages: usize,
    /// The passages on this page, in reading order, at most [`FIND_PAGE`].
    pub occurrences: Vec<FindHit>,
    /// The cursor of the next page; `None` on the last page.
    pub next_cursor: Option<usize>,
}

/// Finds every occurrence of `phrase`, matched exactly (no stemming, any case)
/// as a run of whole words. Pages hold `FIND_PAGE` passages; `cursor` is the
/// index of the first passage of the page among all passages that match, in
/// reading order, and starts at 0.
pub fn find(book: &Book, phrase: &str, cursor: Option<usize>) -> Result<FindResult> {
    let phrase = phrase.trim();
    if tokens(EXACT, phrase).is_empty() {
        return Err(Error::InvalidArgument(
            "phrase must contain at least one word".to_string(),
        ));
    }
    let mut matching = Vec::new();
    let mut total_occurrences = 0;
    for id in book.lexical.phrase_passages(phrase)? {
        let passage = passage(book, id)?;
        let ranges = phrase_ranges(&passage.text, phrase);
        if ranges.is_empty() {
            continue;
        }
        total_occurrences += ranges.len();
        matching.push((passage, ranges));
    }
    let start = cursor.unwrap_or(0).min(matching.len());
    let end = (start + FIND_PAGE).min(matching.len());
    let occurrences = matching[start..end]
        .iter()
        .map(|(passage, ranges)| FindHit {
            passage_id: passage.id,
            section: passage.section.clone(),
            locator: passage.locator.clone(),
            matches: to_char_ranges(&passage.text, ranges),
        })
        .collect();
    Ok(FindResult {
        phrase: phrase.to_string(),
        total_occurrences,
        total_passages: matching.len(),
        occurrences,
        next_cursor: (end < matching.len()).then_some(end),
    })
}
