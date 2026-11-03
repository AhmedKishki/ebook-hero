//! The entry point every operation goes through: turns a path the user typed
//! into an opened [`Book`], and keeps the last few books and file hashes so a
//! second call about the same book costs almost nothing.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::embed::Models;
use crate::index::book::Book;
use crate::index::cache::{default_cache_root, ensure_private_dir, file_error, sha256_file};
use crate::read::{self, BookInfo, OutlineResult, ReadParams, ReadResult};
use crate::search::find::{self, FindResult};
use crate::search::{self, SearchParams, SearchResult};
use crate::settings::OPEN_BOOKS;
use crate::verify::{self, VerifyResult};
use crate::{Error, Result};

/// A file's size and modification time when it was hashed, and the hash.
type Hashed = (u64, SystemTime, String);

pub struct Session {
    cache_root: PathBuf,
    models: Models,
    /// The most recently used book is last.
    books: Mutex<VecDeque<Arc<Book>>>,
    hashes: Mutex<HashMap<PathBuf, Hashed>>,
}

impl Session {
    pub fn new() -> Session {
        Session::with_cache_root(default_cache_root())
    }

    /// A session that keeps its indexes in `root` and its models in the
    /// default models directory.
    pub fn with_cache_root(root: PathBuf) -> Session {
        Session::with_dirs(root, Models::default_cache_dir())
    }

    /// A session that keeps its indexes in `cache_root` and its models in
    /// `models_dir`. The models load when first used.
    pub fn with_dirs(cache_root: PathBuf, models_dir: PathBuf) -> Session {
        // A root that cannot be used shows up, with its cause, when the first
        // book is opened.
        let _ = ensure_private_dir(&cache_root);
        Session {
            cache_root,
            models: Models::new(models_dir),
            books: Mutex::new(VecDeque::new()),
            hashes: Mutex::new(HashMap::new()),
        }
    }

    pub fn models(&self) -> &Models {
        &self.models
    }

    /// Opens the book at `path`, building its index first if it has none.
    ///
    /// The book is found by the hash of the file's content, so a file that was
    /// replaced is indexed again and a copy under another name reuses the
    /// index. The hash is memoized per path while the file's size and
    /// modification time stay the same. An open book whose cached index was
    /// removed, by `cache clear` for one, is opened and indexed again.
    pub fn book(&self, path: &str) -> Result<Arc<Book>> {
        let path = resolve_path(path)?;
        let sha256 = self.hash(&path)?;
        if let Some(book) = touch(&mut locked(&self.books), &sha256) {
            return Ok(book);
        }
        // The index may take minutes to build, so no lock is held meanwhile.
        let book = Arc::new(Book::open(&self.cache_root, &path, &sha256)?);
        let mut books = locked(&self.books);
        if let Some(existing) = touch(&mut books, &sha256) {
            return Ok(existing);
        }
        books.push_back(Arc::clone(&book));
        while books.len() > OPEN_BOOKS {
            books.pop_front();
        }
        Ok(book)
    }

    /// Every passage of the book at `path` that contains `phrase` exactly; see
    /// [`find::find`].
    pub fn find(&self, path: &str, phrase: &str, cursor: Option<usize>) -> Result<FindResult> {
        let book = self.book(path)?;
        find::find(&book, phrase, cursor)
    }

    /// Searches the book at `path`; see [`search::search`]. The arguments are
    /// checked before the book is opened.
    pub fn search(
        &self,
        path: &str,
        params: SearchParams,
        progress: &dyn Fn(usize, usize),
    ) -> Result<SearchResult> {
        search::validate(&params)?;
        let book = self.book(path)?;
        search::search(&book, &self.models, &params, progress)
    }

    /// Reads passages of the book at `path`; see [`read::read`]. The arguments
    /// are checked before the book is opened.
    pub fn read(&self, path: &str, p: ReadParams) -> Result<ReadResult> {
        read::validate(&p)?;
        let book = self.book(path)?;
        read::read(&book, &p)
    }

    /// The outline of the book at `path`; see [`read::outline`].
    pub fn outline(&self, path: &str) -> Result<OutlineResult> {
        let book = self.book(path)?;
        Ok(read::outline(&book))
    }

    /// A summary of the book at `path`; see [`read::info`].
    pub fn info(&self, path: &str) -> Result<BookInfo> {
        let book = self.book(path)?;
        Ok(read::info(&book))
    }

    /// Checks a quote against the book at `path`; see [`verify::verify_quote`].
    pub fn verify(&self, path: &str, quote: &str, passage_id: Option<u32>) -> Result<VerifyResult> {
        let book = self.book(path)?;
        verify::verify_quote(&book, quote, passage_id)
    }

    /// Builds both stages of the index of the book at `path` and summarizes
    /// the book. Stage 2 embeds every passage the first time, which can take
    /// minutes; `progress(done, total)` reports it.
    pub fn index(&self, path: &str, progress: &dyn Fn(usize, usize)) -> Result<BookInfo> {
        let book = self.book(path)?;
        book.vectors(&self.models, progress)?;
        Ok(read::info(&book))
    }

    fn hash(&self, path: &Path) -> Result<String> {
        let metadata = fs::metadata(path).map_err(|e| file_error(path, e))?;
        if !metadata.is_file() {
            return Err(Error::NotAFile {
                path: path.to_path_buf(),
            });
        }
        let size = metadata.len();
        let modified = metadata.modified()?;
        if let Some((s, m, sha256)) = locked(&self.hashes).get(path)
            && (*s, *m) == (size, modified)
        {
            return Ok(sha256.clone());
        }
        let sha256 = sha256_file(path)?;
        locked(&self.hashes).insert(path.to_path_buf(), (size, modified, sha256.clone()));
        Ok(sha256)
    }

    #[doc(hidden)]
    pub fn open_book_count(&self) -> usize {
        locked(&self.books).len()
    }
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

/// Finds the book with this hash and marks it as the most recently used. A
/// book whose cached index is gone is dropped instead, so that it is opened
/// again.
fn touch(books: &mut VecDeque<Arc<Book>>, sha256: &str) -> Option<Arc<Book>> {
    let position = books.iter().position(|book| book.sha256 == sha256)?;
    let book = books.remove(position)?;
    if !book.is_cached() {
        return None;
    }
    books.push_back(Arc::clone(&book));
    Some(book)
}

/// The data behind these mutexes is never left half-changed, so a lock
/// poisoned by a panicking thread is still safe to use.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Expands a leading `~`, makes the path absolute against the working
/// directory, and follows symlinks to the file itself.
pub fn resolve_path(path: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    resolve_path_in(path, dirs::home_dir().as_deref(), &cwd)
}

/// [`resolve_path`] with the home and working directories given.
#[doc(hidden)]
pub fn resolve_path_in(path: &str, home: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let expanded = match path.strip_prefix('~') {
        Some("") => home_dir(path, home)?.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home_dir(path, home)?.join(&rest[1..]),
        _ => PathBuf::from(path),
    };
    let absolute = cwd.join(expanded);
    fs::canonicalize(&absolute).map_err(|e| file_error(&absolute, e))
}

fn home_dir<'a>(path: &str, home: Option<&'a Path>) -> Result<&'a Path> {
    home.ok_or_else(|| {
        Error::InvalidArgument(format!(
            "cannot expand ~ in {path}: no home directory is known; give an absolute path"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("cwd");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        (tmp, home, cwd)
    }

    #[test]
    fn a_bare_tilde_is_the_home_directory() {
        let (_tmp, home, cwd) = dirs();
        assert_eq!(
            resolve_path_in("~", Some(&home), &cwd).unwrap(),
            fs::canonicalize(&home).unwrap()
        );
    }

    #[test]
    fn a_tilde_with_no_home_asks_for_an_absolute_path() {
        let (_tmp, _home, cwd) = dirs();
        let error = resolve_path_in("~/a.epub", None, &cwd).unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
        assert!(error.to_string().contains("absolute path"), "{error}");
    }

    #[test]
    fn only_a_leading_tilde_slash_is_expanded() {
        let (_tmp, home, cwd) = dirs();
        fs::write(cwd.join("~a.epub"), b"x").unwrap();
        fs::create_dir_all(cwd.join("sub")).unwrap();
        fs::write(cwd.join("sub").join("~"), b"x").unwrap();
        for path in ["~a.epub", "sub/~"] {
            assert_eq!(
                resolve_path_in(path, Some(&home), &cwd).unwrap(),
                fs::canonicalize(cwd.join(path)).unwrap()
            );
        }
    }

    #[test]
    fn a_session_can_be_shared_between_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Session>();
    }
}
