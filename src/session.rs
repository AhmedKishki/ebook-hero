//! The entry point every operation goes through: turns a path the user typed
//! into an opened [`Book`], and keeps the last few books and file hashes so a
//! second call about the same book costs almost nothing.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::embed::Models;
use crate::graph::GraphResult;
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

/// The entry point every operation goes through. A session opens books by
/// path, keeps their indexes in a cache directory, and loads the embedding
/// and reranking models when a search first needs them.
///
/// The command line makes one session per command, and the MCP server one for
/// its whole life. A session is `Send` and `Sync`, so threads can share it.
/// It keeps the [`OPEN_BOOKS`] most recently used books open, and it
/// remembers each file's hash while the file's size and modification time
/// stay the same, so a second call about a book neither reads the file nor
/// loads its index again.
///
/// Each method that takes a `path` accepts an absolute path, a path relative
/// to the working directory, or one that starts with `~`, and follows
/// symbolic links to the file. The first call about a book extracts its text
/// and builds its index in the cache; later calls, in this process or
/// another, load the index. A book is found in the cache by the SHA-256 of
/// its bytes, so a copy under another name reuses the index and a changed
/// file is indexed again. A method that matches an MCP tool returns the value
/// that tool returns as its structured result.
///
/// # The cache root
///
/// The indexes in the cache root are loaded without being checked, so on Unix
/// the root must be a directory, not a symbolic link, that belongs to the
/// current user and that group and others cannot write to, as mode 0700
/// ensures. A root that does not exist is created with mode 0700 when the
/// session is made; its parent must exist. A root that fails these checks
/// makes every method that opens a book fail with [`Error::Index`], whose
/// message names the cause.
///
/// # Errors
///
/// Every method that opens a book can fail with:
///
/// - [`Error::NotFound`] or [`Error::NotAFile`] when `path` names no file;
/// - [`Error::Unsupported`] when the file is neither a PDF nor an EPUB;
/// - [`Error::Unreadable`] when the file cannot be read, such as a PDF that
///   needs a password, or [`Error::NoTextLayer`] when a PDF holds no text;
/// - [`Error::InvalidArgument`] when `path` starts with `~` and no home
///   directory is known;
/// - [`Error::Index`] when the cache root is not private or the index cannot
///   be written;
/// - [`Error::LockTimeout`] when another process has been building the same
///   book's index for longer than [`LOCK_WAIT`](crate::settings::LOCK_WAIT).
///
/// Each method lists the errors it adds to these.
///
/// # Examples
///
/// ```
/// use ebook_hero::session::Session;
///
/// # fn main() -> ebook_hero::Result<()> {
/// # let tmp = tempfile::tempdir()?;
/// # let cache_dir = tmp.path();
/// // A root that does not exist yet is created with mode 0700.
/// let session = Session::with_cache_root(cache_dir.join("indexes"));
/// let info = session.info("tests/fixtures/alice.epub")?;
/// assert_eq!(info.title.as_deref(), Some("Alice's Adventures in Wonderland"));
/// assert_eq!(info.passage_count, 166);
/// # Ok(())
/// # }
/// ```
pub struct Session {
    cache_root: PathBuf,
    models: Models,
    /// The most recently used book is last.
    books: Mutex<VecDeque<Arc<Book>>>,
    hashes: Mutex<HashMap<PathBuf, Hashed>>,
}

impl Session {
    /// A session that keeps its indexes in [`default_cache_root`]:
    /// `$EBOOK_HERO_INDEXES` if it is set, else `ebook-hero/indexes` in the
    /// user's cache directory, such as `~/.cache` on Linux. Its models go in
    /// [`Models::default_cache_dir`]. The command line and the MCP server use
    /// this session.
    pub fn new() -> Session {
        Session::with_cache_root(default_cache_root())
    }

    /// A session that keeps its indexes in `root` and its models in the
    /// default models directory, [`Models::default_cache_dir`]. See
    /// [the cache root](Session#the-cache-root) for what `root` must be.
    pub fn with_cache_root(root: PathBuf) -> Session {
        Session::with_dirs(root, Models::default_cache_dir())
    }

    /// A session that keeps its indexes in `cache_root` and its models in
    /// `models_dir`. The models load when first used.
    ///
    /// Nothing fails here: a `cache_root` that cannot be used makes the first
    /// call that opens a book fail, with the cause in the error.
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

    /// The embedding and reranking models this session loads for semantic
    /// and hybrid search. [`Models::release_idle`] gives their memory back
    /// between searches, as the MCP server does.
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
    ///
    /// # Errors
    ///
    /// The errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    /// let book = session.book("tests/fixtures/alice.epub")?;
    /// assert_eq!(book.passages.len(), 166);
    /// assert_eq!(book.document.source.file_name, "alice.epub");
    /// # Ok(())
    /// # }
    /// ```
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
    ///
    /// The phrase matches as a run of whole words, in any case and without
    /// stemming. The passages come in reading order,
    /// [`FIND_PAGE`](crate::settings::FIND_PAGE) to a page, and the totals
    /// count the whole book. `cursor` is the `next_cursor` of the previous
    /// page, or `None` for the first page; a cursor past the last page gives
    /// an empty page.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] when `phrase` holds no word, and the errors
    /// of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    /// let found = session.find("tests/fixtures/alice.epub", "off with her head", None)?;
    /// assert_eq!(found.total_occurrences, 4);
    /// assert_eq!(found.occurrences[0].passage_id, 95);
    /// assert_eq!(found.next_cursor, None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn find(&self, path: &str, phrase: &str, cursor: Option<usize>) -> Result<FindResult> {
        let book = self.book(path)?;
        find::find(&book, phrase, cursor)
    }

    /// Searches the book at `path`; see [`search::search`]. The arguments are
    /// checked before the book is opened.
    ///
    /// Semantic and hybrid search need the passages embedded. The first such
    /// search of a book embeds them all, which can take minutes, and calls
    /// `progress(done, total)` after each batch; the first one on a machine
    /// also downloads the two models, about 270 MB. Lexical search needs
    /// neither and never calls `progress`.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidArgument`] when `top_k` is not between 1 and
    ///   [`TOP_K_MAX`](crate::settings::TOP_K_MAX) or the query is blank;
    /// - [`Error::ModelUnavailable`] when a model cannot be downloaded,
    ///   loaded or run, in semantic and hybrid mode;
    /// - [`Error::Index`] when another process removes the book's cache
    ///   directory while a semantic or hybrid search runs; another call opens
    ///   the book again;
    /// - the errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// Lexical search loads no model:
    ///
    /// ```
    /// use ebook_hero::search::{Mode, SearchParams};
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    ///
    /// let params = SearchParams {
    ///     query: r#""mock turtle" soup"#.to_string(),
    ///     mode: Mode::Lexical,
    ///     top_k: 2,
    ///     rerank: false,
    /// };
    /// let result = session.search("tests/fixtures/alice.epub", params, &|_, _| {})?;
    /// assert_eq!(result.hits.len(), 2);
    /// assert_eq!(result.hits[0].passage_id, 127);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Hybrid search, the default mode, downloads the models on first use and
    /// reports the embedding:
    ///
    /// ```no_run
    /// use ebook_hero::read::locator_text;
    /// use ebook_hero::search::{Mode, SearchParams};
    /// use ebook_hero::session::Session;
    ///
    /// # fn main() -> ebook_hero::Result<()> {
    /// let session = Session::new();
    /// let params = SearchParams {
    ///     query: "why does the Queen want to cut off heads".to_string(),
    ///     mode: Mode::Hybrid,
    ///     top_k: 5,
    ///     rerank: true,
    /// };
    /// let progress = |done, total| eprintln!("embedding {done}/{total}");
    /// let result = session.search("book.epub", params, &progress)?;
    /// for hit in &result.hits {
    ///     println!("passage {} · {}", hit.passage_id, locator_text(&hit.locator));
    /// }
    /// # Ok(())
    /// # }
    /// ```
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

    /// Reads passages of the book at `path` as Markdown; see [`read::read`].
    /// The arguments are checked before the book is opened.
    ///
    /// A page of the result holds at most
    /// [`READ_MAX_CHARS`](crate::settings::READ_MAX_CHARS) characters, and
    /// always at least one passage. When the range does not fit,
    /// `next_cursor` names the passage to continue at: pass it as `cursor`
    /// with the other parameters unchanged.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidArgument`] when `before` or `after` is above
    ///   [`READ_NEIGHBOURS_MAX`](crate::settings::READ_NEIGHBOURS_MAX); when
    ///   the passage, page or outline entry does not exist or has no text;
    ///   when a page is asked of an EPUB; or when `cursor` lies outside the
    ///   range being read;
    /// - the errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// use ebook_hero::read::{ReadAt, ReadParams};
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    ///
    /// // Passage 99 with one passage on each side.
    /// let params = ReadParams {
    ///     at: ReadAt::Passage(99),
    ///     before: 1,
    ///     after: 1,
    ///     cursor: None,
    /// };
    /// let page = session.read("tests/fixtures/alice.epub", params)?;
    /// let ids: Vec<u32> = page.passages.iter().map(|p| p.passage_id).collect();
    /// assert_eq!(ids, [98, 99, 100]);
    /// assert!(page.markdown.contains("<!-- passage 99 · "));
    /// # Ok(())
    /// # }
    /// ```
    pub fn read(&self, path: &str, p: ReadParams) -> Result<ReadResult> {
        read::validate(&p)?;
        let book = self.book(path)?;
        read::read(&book, &p)
    }

    /// The concept graph of the book at `path`, built and cached on the first
    /// call; see [`crate::graph::build`]. Unless `full`, each concept and link
    /// lists only its first passages; see [`GraphResult::abridged`].
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidArgument`] when the book has fewer than
    ///   [`GRAPH_MIN_BOOK_PASSAGES`](crate::settings::GRAPH_MIN_BOOK_PASSAGES) passages;
    /// - [`Error::Index`] when another process removes the book's cache
    ///   directory while the call runs; another call opens the book again;
    /// - the errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    /// let graph = session.graph("tests/fixtures/alice.epub", false)?;
    /// let hatter = graph.concepts.iter().find(|c| c.label == "hatter").unwrap();
    /// assert_eq!(hatter.passage_count, 18);
    /// assert_eq!(hatter.passages.len(), 10);
    /// # Ok(())
    /// # }
    /// ```
    pub fn graph(&self, path: &str, full: bool) -> Result<GraphResult> {
        let book = self.book(path)?;
        let graph = book.graph()?;
        Ok(if full {
            graph.clone()
        } else {
            graph.abridged()
        })
    }

    /// The outline of the book at `path`; see [`read::outline`]. A book
    /// without a table of contents has no entries.
    ///
    /// # Errors
    ///
    /// The errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    /// let outline = session.outline("tests/fixtures/alice.epub")?;
    /// let chapter = &outline.entries[3];
    /// assert_eq!(chapter.title, "CHAPTER I. Down the Rabbit-Hole");
    /// assert_eq!(chapter.passage_id, Some(2));
    /// # Ok(())
    /// # }
    /// ```
    pub fn outline(&self, path: &str) -> Result<OutlineResult> {
        let book = self.book(path)?;
        Ok(read::outline(&book))
    }

    /// A summary of the book at `path`; see [`read::info`].
    ///
    /// # Errors
    ///
    /// The errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    /// let info = session.info("tests/fixtures/alice.epub")?;
    /// assert_eq!(info.authors, ["Lewis Carroll"]);
    /// assert!(info.lexical_ready);
    /// # Ok(())
    /// # }
    /// ```
    pub fn info(&self, path: &str) -> Result<BookInfo> {
        let book = self.book(path)?;
        Ok(read::info(&book))
    }

    /// Checks a quote against the book at `path`; see [`verify::verify_quote`].
    ///
    /// Without `passage_id` the whole book is searched, and the first exact
    /// match wins over any equivalent one. With it, that passage is checked
    /// first, and a quote found only in another passage gets the verdict
    /// `none` with `found_elsewhere` set.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidArgument`] when the quote is blank, holds no letter
    ///   or digit, or has more than 440 words, or when `passage_id` names no
    ///   passage of the book;
    /// - the errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```
    /// use ebook_hero::verify::Verdict;
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// # let tmp = tempfile::tempdir()?;
    /// # let session = Session::with_cache_root(tmp.path().join("indexes"));
    ///
    /// let book = "tests/fixtures/alice.epub";
    /// let checked = session.verify(book, "Off with her head!", None)?;
    /// assert_eq!(checked.verdict, Verdict::Exact);
    /// assert_eq!(checked.passage_id, Some(95));
    ///
    /// let misread = session.verify(book, "the Queen was very kind to everyone", None)?;
    /// assert_eq!(misread.verdict, Verdict::None);
    /// let closest = misread.closest.unwrap();
    /// assert_eq!(closest.first_difference.as_deref(), Some("very"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn verify(&self, path: &str, quote: &str, passage_id: Option<u32>) -> Result<VerifyResult> {
        let book = self.book(path)?;
        verify::verify_quote(&book, quote, passage_id)
    }

    /// Builds both stages of the index of the book at `path` and summarizes
    /// the book. Stage 2 embeds every passage the first time, which can take
    /// minutes; `progress(done, total)` reports it.
    ///
    /// # Errors
    ///
    /// - [`Error::ModelUnavailable`] when the embedding model cannot be
    ///   downloaded, loaded or run;
    /// - [`Error::Index`] when another process removes the book's cache
    ///   directory while the call runs; another call opens the book again;
    /// - the errors of [opening a book](Session#errors).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ebook_hero::session::Session;
    /// # fn main() -> ebook_hero::Result<()> {
    /// let session = Session::new();
    /// let info = session.index("book.epub", &|done, total| {
    ///     eprintln!("embedding {done}/{total}");
    /// })?;
    /// assert!(info.semantic_ready);
    /// # Ok(())
    /// # }
    /// ```
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
        let modified = metadata.modified().map_err(|e| file_error(path, e))?;
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
