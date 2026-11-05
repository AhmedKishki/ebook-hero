//! A flag that asks work in progress to stop.
//!
//! Cancellation is cooperative: the work looks at the flag at the points
//! where it can stop without leaving anything half written, and returns
//! [`Error::Cancelled`](crate::error::Error::Cancelled). Work that is past
//! such a point, such as building a book's stage-1 index, runs to its end.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A shared flag. Clones share one flag, so a caller keeps one clone to
/// cancel with and hands another to the work.
///
/// ```
/// use ebook_hero::cancel::Cancel;
///
/// let cancel = Cancel::new();
/// let handed_over = cancel.clone();
/// assert!(!handed_over.is_cancelled());
/// cancel.cancel();
/// assert!(handed_over.is_cancelled());
/// ```
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag that is not set. A flag nobody keeps a clone of is never set,
    /// which is what a caller that does not cancel passes.
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Sets the flag. It stays set.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether [`cancel`](Cancel::cancel) was called on this flag or a clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `Err(Error::Cancelled)` once the flag is set, else `Ok(())`, so work
    /// writes `cancel.check()?` at a point where it can stop.
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err(crate::error::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
