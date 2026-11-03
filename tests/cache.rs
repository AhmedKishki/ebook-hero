mod common;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Barrier, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use common::{EpubBuilder, EpubVersion};
use ebook_hero::Error;
use ebook_hero::index::book::Book;
use ebook_hero::index::cache::{Manifest, build_count, expected_stage1, sha256_file, with_lock};
use ebook_hero::session::{Session, resolve_path, resolve_path_in};
use ebook_hero::settings::OPEN_BOOKS;
use tempfile::TempDir;

/// `build_count` is process-wide and the tests run in parallel, so every test
/// that builds a book holds this lock and sees only its own builds.
static BUILDS: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    BUILDS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes an EPUB whose title, and so whose hash, is `title`.
fn write_book(path: &Path, title: &str) {
    EpubBuilder::new(EpubVersion::V3)
        .title(title)
        .chapter(
            "ch1.xhtml",
            &format!("<h1>{title}</h1><p>Words about {title} and nothing else.</p>"),
        )
        .toc(title, "ch1.xhtml", 1)
        .write(path);
}

struct Setup {
    root: TempDir,
    books: TempDir,
}

impl Setup {
    fn new() -> Setup {
        Setup {
            root: TempDir::new().unwrap(),
            books: TempDir::new().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// Writes `<title>.epub` and returns its path and hash.
    fn book(&self, title: &str) -> (PathBuf, String) {
        let path = self.books.path().join(format!("{title}.epub"));
        write_book(&path, title);
        let sha = sha256_file(&path).unwrap();
        (path, sha)
    }

    fn session(&self) -> Session {
        Session::with_cache_root(self.root().to_path_buf())
    }
}

fn str_of(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn builds_once_then_reuses() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Builds Once");
    let before = build_count();
    let first = Book::open(setup.root(), &path, &sha).unwrap();
    let second = Book::open(setup.root(), &path, &sha).unwrap();
    assert_eq!(build_count() - before, 1);
    assert!(setup.root().join(&sha).join("manifest.json").is_file());
    assert_eq!(first.sha256, sha);
    assert_eq!(first.path, path);
    assert_eq!(first.dir, setup.root().join(&sha));
    assert_eq!(
        first.document.metadata.title.as_deref(),
        Some("Builds Once")
    );
    assert!(!first.passages.is_empty());
    assert_eq!(first.passages, second.passages);
    assert!(
        !first
            .lexical
            .phrase_passages("nothing else")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manifest_mismatch_rebuilds() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Mismatch");
    Book::open(setup.root(), &path, &sha).unwrap();
    let manifest_path = setup.root().join(&sha).join("manifest.json");
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest.tool_version = "0.0.0".to_string();
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let before = build_count();
    Book::open(setup.root(), &path, &sha).unwrap();
    assert_eq!(build_count() - before, 1);
    let restored: Manifest = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(restored.tool_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(restored, expected_stage1());
}

#[test]
fn damaged_cache_is_rebuilt() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Damaged");
    Book::open(setup.root(), &path, &sha).unwrap();
    fs::remove_file(setup.root().join(&sha).join("passages.json")).unwrap();

    let before = build_count();
    let book = Book::open(setup.root(), &path, &sha).unwrap();
    assert_eq!(build_count() - before, 1);
    assert!(!book.passages.is_empty());
}

#[test]
fn leftovers_are_removed_before_a_rebuild() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Leftovers");
    let stale = setup.root().join(&sha);
    fs::create_dir_all(stale.join("tantivy")).unwrap();
    fs::write(stale.join("old.json"), "{}").unwrap();
    let staging = setup.root().join(format!("{sha}.staging-1-2"));
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("half.json"), "{").unwrap();

    let before = build_count();
    Book::open(setup.root(), &path, &sha).unwrap();
    assert_eq!(build_count() - before, 1);
    assert!(!staging.exists());
    assert!(!stale.join("old.json").exists());
    assert!(stale.join("manifest.json").is_file());
}

#[test]
fn failed_build_leaves_nothing() {
    let _serial = serial();
    let setup = Setup::new();
    let path = setup.books.path().join("corrupt.pdf");
    fs::write(&path, b"%PDF-1.4\nthis is not a pdf at all\n").unwrap();
    let sha = sha256_file(&path).unwrap();

    let before = build_count();
    assert!(Book::open(setup.root(), &path, &sha).is_err());
    assert_eq!(build_count() - before, 0);
    let names: Vec<String> = fs::read_dir(setup.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.contains(&sha), "{names:?}");
    assert!(
        names.iter().all(|name| !name.contains(".staging-")),
        "{names:?}"
    );
}

#[test]
fn concurrent_opens_build_once() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Concurrent");
    let start = Barrier::new(2);
    let before = build_count();
    let results: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    Book::open(setup.root(), &path, &sha)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(results.iter().all(|result| result.is_ok()));
    assert_eq!(build_count() - before, 1);
}

#[test]
fn lock_timeout_names_the_holder() {
    let root = TempDir::new().unwrap();
    let sha = "b".repeat(64);
    let held = File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(root.path().join(format!("{sha}.lock")))
        .unwrap();
    fs4::FileExt::try_lock(&held).unwrap();
    write!(&held, "{}", std::process::id()).unwrap();

    match with_lock(root.path(), &sha, Duration::from_millis(200), || Ok(())) {
        Err(Error::LockTimeout { sha256, pid, .. }) => {
            assert_eq!(sha256, sha);
            assert_eq!(pid, Some(std::process::id()));
        }
        Err(other) => panic!("expected LockTimeout, got {other}"),
        Ok(()) => panic!("the lock was held, so with_lock must not run the closure"),
    }

    drop(held);
    let value = with_lock(root.path(), &sha, Duration::from_millis(200), || Ok(7)).unwrap();
    assert_eq!(value, 7);
}

#[test]
fn replaced_file_is_reindexed() {
    let _serial = serial();
    let setup = Setup::new();
    let session = setup.session();
    let path = setup.books.path().join("book.epub");
    write_book(&path, "Replaced Alpha");
    let alpha = session.book(str_of(&path)).unwrap();
    assert_eq!(
        alpha.document.metadata.title.as_deref(),
        Some("Replaced Alpha")
    );

    write_book(&path, "Replaced Beta");
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    let beta = session.book(str_of(&path)).unwrap();
    assert_eq!(
        beta.document.metadata.title.as_deref(),
        Some("Replaced Beta")
    );
    assert_ne!(alpha.sha256, beta.sha256);
}

#[test]
fn resolves_tilde_relative_spaces_and_symlinks() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let cwd = tmp.path().join("work dir");
    fs::create_dir_all(home.join("my books")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let in_home = home.join("my books").join("a b.epub");
    let in_cwd = cwd.join("a b.epub");
    fs::write(&in_home, b"home").unwrap();
    fs::write(&in_cwd, b"cwd").unwrap();
    let canonical_home = fs::canonicalize(&in_home).unwrap();
    let canonical_cwd = fs::canonicalize(&in_cwd).unwrap();
    let resolve = |path: &str| resolve_path_in(path, Some(&home), &cwd);

    assert_eq!(resolve("~/my books/a b.epub").unwrap(), canonical_home);
    assert_eq!(resolve("./a b.epub").unwrap(), canonical_cwd);
    assert_eq!(resolve("a b.epub").unwrap(), canonical_cwd);
    assert_eq!(resolve("../work dir/./a b.epub").unwrap(), canonical_cwd);
    assert_eq!(resolve(str_of(&in_home)).unwrap(), canonical_home);
    assert_eq!(resolve_path(str_of(&in_home)).unwrap(), canonical_home);

    #[cfg(unix)]
    {
        let link = cwd.join("link.epub");
        std::os::unix::fs::symlink(&in_home, &link).unwrap();
        assert_eq!(resolve("link.epub").unwrap(), canonical_home);
    }

    assert!(matches!(
        resolve("~/missing.epub"),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        resolve("missing.epub"),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn missing_and_non_file_paths_are_reported() {
    let setup = Setup::new();
    let session = setup.session();
    let missing = setup.books.path().join("missing.epub");
    assert!(matches!(
        session.book(str_of(&missing)),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        session.book(str_of(setup.books.path())),
        Err(Error::NotAFile { .. })
    ));
}

#[test]
fn keeps_at_most_four_open_books() {
    let _serial = serial();
    let setup = Setup::new();
    let session = setup.session();
    assert_eq!(OPEN_BOOKS, 4);
    let paths: Vec<PathBuf> = (0..OPEN_BOOKS + 2)
        .map(|i| setup.book(&format!("Open {i}")).0)
        .collect();
    let mut opened = Vec::new();
    for path in &paths {
        opened.push(session.book(str_of(path)).unwrap());
        assert!(session.open_book_count() <= OPEN_BOOKS);
    }
    assert_eq!(session.open_book_count(), OPEN_BOOKS);

    let newest = paths.last().unwrap();
    let again = session.book(str_of(newest)).unwrap();
    assert!(std::sync::Arc::ptr_eq(&again, opened.last().unwrap()));
    let evicted = session.book(str_of(&paths[0])).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&evicted, &opened[0]));
    assert_eq!(evicted.sha256, opened[0].sha256);
    assert_eq!(session.open_book_count(), OPEN_BOOKS);
}
