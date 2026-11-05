mod common;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::cancel::Cancel;
use ebook_hero::index::book::Book;
use ebook_hero::index::cache::{
    CachedIndex, ClearReport, Manifest, build_count, clear, expected_stage1, list_indexes,
    sha256_file, with_lock, with_lock_cancellable,
};
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::{Session, resolve_path, resolve_path_in};
use ebook_hero::settings::OPEN_BOOKS;
use tempfile::TempDir;

/// `build_count` is process-wide and the tests run in parallel, so every test
/// that builds a book holds this lock and sees only its own builds.
static BUILDS: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    BUILDS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Creates `path` (whose parent exists) readable by its owner only.
#[cfg(unix)]
fn private_dir(path: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
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
            root: private_tempdir(),
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
    let root = private_tempdir();
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

/// Holds the build lock of `sha` in `root` until the returned file is dropped.
fn hold_lock(root: &Path, sha: &str) -> File {
    let held = File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(root.join(format!("{sha}.lock")))
        .unwrap();
    fs4::FileExt::try_lock(&held).unwrap();
    held
}

/// Calls `cancel` after `after` on another thread.
fn cancel_later(cancel: &Cancel, after: Duration) -> thread::JoinHandle<()> {
    let cancel = cancel.clone();
    thread::spawn(move || {
        thread::sleep(after);
        cancel.cancel();
    })
}

#[test]
fn a_cancelled_wait_for_the_lock_ends_at_once() {
    let root = private_tempdir();
    let sha = "e".repeat(64);
    let _held = hold_lock(root.path(), &sha);
    let cancel = Cancel::new();
    let canceller = cancel_later(&cancel, Duration::from_millis(300));

    let started = std::time::Instant::now();
    let result = with_lock_cancellable(root.path(), &sha, Duration::from_secs(60), &cancel, || {
        Ok(())
    });

    canceller.join().unwrap();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait went on for {:?}",
        started.elapsed()
    );
}

#[test]
fn a_flag_set_after_the_lock_is_taken_does_not_stop_the_work() {
    let root = private_tempdir();
    let sha = "f".repeat(64);
    let cancel = Cancel::new();

    let value = with_lock_cancellable(root.path(), &sha, Duration::ZERO, &cancel, || {
        cancel.cancel();
        Ok(7)
    });

    assert_eq!(value.unwrap(), 7);
}

#[test]
fn a_cancellable_session_stops_waiting_for_a_busy_index() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Cancelled While Waiting");
    let held = hold_lock(setup.root(), &sha);
    let cancel = Cancel::new();
    let session = setup.session().cancellable(cancel.clone());
    let canceller = cancel_later(&cancel, Duration::from_millis(300));

    let result = session.info(str_of(&path));

    canceller.join().unwrap();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    // Nothing was left behind: once the lock is free, the book opens.
    drop(held);
    assert!(setup.session().info(str_of(&path)).is_ok());
}

#[test]
fn a_session_that_is_not_cancelled_is_not_affected_by_another_flag() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, _) = setup.book("Not Cancelled");
    let session = setup.session();
    let cancelled = session.cancellable(Cancel::new());
    let other = Cancel::new();
    other.cancel();

    assert!(cancelled.info(str_of(&path)).is_ok());
    assert!(session.info(str_of(&path)).is_ok());
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

#[test]
fn a_book_whose_cache_was_cleared_is_built_again() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Cleared Book");
    let session = setup.session();
    let first = session.book(str_of(&path)).unwrap();
    assert_eq!(clear(setup.root()).unwrap().removed, 1);
    let before = build_count();

    let again = session.book(str_of(&path)).unwrap();

    assert_eq!(build_count() - before, 1);
    assert!(!Arc::ptr_eq(&first, &again));
    assert!(setup.root().join(&sha).join("manifest.json").is_file());
    assert_eq!(session.open_book_count(), 1);
    let params = SearchParams {
        query: "nothing else".to_string(),
        mode: Mode::Lexical,
        top_k: 5,
        rerank: false,
    };
    let result = session.search(str_of(&path), params, &|_, _| {}).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(build_count() - before, 1);
}

#[cfg(unix)]
#[test]
fn rejects_non_hex_sha() {
    // The root sits inside `outer`, so even a hash that climbs out of the
    // root can only reach files this test planted.
    let outer = TempDir::new().unwrap();
    let root = outer.path().join("cache");
    private_dir(&root);
    let in_root = root.join("keep.txt");
    let in_outer = outer.path().join("keep.txt");
    fs::write(&in_root, b"keep").unwrap();
    fs::write(&in_outer, b"keep").unwrap();
    let books = TempDir::new().unwrap();
    let path = books.path().join("book.epub");
    write_book(&path, "Rejects Non Hex");

    let upper = "A".repeat(64);
    for sha in ["", "..", "abc", "../escape", upper.as_str()] {
        assert!(
            matches!(
                Book::open(&root, &path, sha),
                Err(Error::InvalidArgument(_))
            ),
            "Book::open accepted {sha:?}"
        );
        assert!(
            matches!(
                with_lock(&root, sha, Duration::from_millis(50), || Ok(())),
                Err(Error::InvalidArgument(_))
            ),
            "with_lock accepted {sha:?}"
        );
        assert!(
            in_root.is_file() && in_outer.is_file(),
            "{sha:?} deleted a file"
        );
    }
    let echoed = match with_lock(&root, &"x".repeat(500), Duration::ZERO, || Ok(())) {
        Err(Error::InvalidArgument(message)) => message,
        other => panic!("expected InvalidArgument, got {:?}", other.err()),
    };
    assert!(
        echoed.starts_with("not a SHA-256 hex digest: x"),
        "{echoed}"
    );
    assert!(
        echoed.len() < 150,
        "the echoed value is not truncated: {echoed}"
    );
}

#[cfg(unix)]
#[test]
fn group_writable_root_is_rejected() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Group Writable");
    Book::open(setup.root(), &path, &sha).unwrap();

    set_mode(setup.root(), 0o777);
    for result in [
        Book::open(setup.root(), &path, &sha).map(|_| ()),
        with_lock(setup.root(), &sha, Duration::from_millis(50), || Ok(())),
        setup.session().book(str_of(&path)).map(|_| ()),
    ] {
        let message = result.unwrap_err().to_string();
        assert!(message.contains("not private"), "{message}");
        assert!(message.contains("EBOOK_HERO_INDEXES"), "{message}");
    }
    set_mode(setup.root(), 0o770);
    assert!(Book::open(setup.root(), &path, &sha).is_err());
    set_mode(setup.root(), 0o707);
    assert!(Book::open(setup.root(), &path, &sha).is_err());
}

#[cfg(unix)]
#[test]
fn a_root_others_can_only_read_is_accepted() {
    let _serial = serial();
    let setup = Setup::new();
    let (path, sha) = setup.book("Readable");
    for mode in [0o755, 0o750, 0o700] {
        set_mode(setup.root(), mode);
        Book::open(setup.root(), &path, &sha).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn symlinked_root_is_rejected() {
    let _serial = serial();
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    let link = tmp.path().join("link");
    private_dir(&real);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let books = TempDir::new().unwrap();
    let path = books.path().join("book.epub");
    write_book(&path, "Symlinked Root");
    let sha = sha256_file(&path).unwrap();

    let message = Book::open(&link, &path, &sha).err().unwrap().to_string();
    assert!(message.contains("not private"), "{message}");
    assert!(with_lock(&link, &sha, Duration::from_millis(50), || Ok(())).is_err());
    assert!(fs::read_dir(&real).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn lock_symlink_is_not_followed() {
    let root = private_tempdir();
    let elsewhere = TempDir::new().unwrap();
    let target = elsewhere.path().join("precious.txt");
    fs::write(&target, b"keep").unwrap();
    let sha = "c".repeat(64);
    std::os::unix::fs::symlink(&target, root.path().join(format!("{sha}.lock"))).unwrap();

    let result = with_lock(root.path(), &sha, Duration::from_millis(50), || Ok(()));
    let message = result.unwrap_err().to_string();
    assert!(message.contains("symbolic link"), "{message}");
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn fresh_root_is_created_private() {
    let tmp = TempDir::new().unwrap();
    let fresh = tmp.path().join("cache");
    let _session = Session::with_cache_root(fresh.clone());
    assert_eq!(mode_of(&fresh), 0o700);

    let by_lock = tmp.path().join("by-lock");
    with_lock(&by_lock, &"d".repeat(64), Duration::from_millis(50), || {
        Ok(())
    })
    .unwrap();
    assert_eq!(mode_of(&by_lock), 0o700);

    // Only the root itself is created: its parent must already exist.
    let orphan = tmp.path().join("missing").join("cache");
    let _session = Session::with_cache_root(orphan.clone());
    assert!(!tmp.path().join("missing").exists());
    assert!(with_lock(&orphan, &"d".repeat(64), Duration::ZERO, || Ok(())).is_err());
}

/// The bytes in the files under `dir`.
fn tree_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                tree_bytes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

/// The names in `root`, sorted.
fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn shas(indexes: &[CachedIndex]) -> Vec<&str> {
    indexes.iter().map(|index| index.sha256.as_str()).collect()
}

impl Setup {
    /// Writes `<title>.epub` and caches its index; returns its hash.
    fn cached(&self, title: &str) -> String {
        let (path, sha) = self.book(title);
        Book::open(self.root(), &path, &sha).unwrap();
        sha
    }
}

#[test]
fn list_indexes_reports_each_cached_book_sorted_by_hash() {
    let _serial = serial();
    let setup = Setup::new();
    let mut books = [
        ("Alpha Book", setup.cached("Alpha Book")),
        ("Beta Book", setup.cached("Beta Book")),
    ];
    books.sort_by(|a, b| a.1.cmp(&b.1));
    // None of these is an index.
    fs::write(setup.root().join("notes.txt"), "x").unwrap();
    fs::create_dir(setup.root().join("not-a-hash")).unwrap();
    fs::create_dir(setup.root().join("E".repeat(64))).unwrap();
    fs::write(setup.root().join("f".repeat(64)), "a file").unwrap();
    fs::create_dir(setup.root().join(format!("{}.staging-1-2", books[0].1))).unwrap();

    let listed = list_indexes(setup.root()).unwrap();

    assert_eq!(shas(&listed), [books[0].1.as_str(), books[1].1.as_str()]);
    for (index, (title, sha)) in listed.iter().zip(&books) {
        assert_eq!(index.title.as_deref(), Some(*title));
        assert_eq!(index.size_bytes, tree_bytes(&setup.root().join(sha)));
        assert!(index.size_bytes > 0);
    }
}

#[test]
fn an_index_without_a_readable_title_is_listed_untitled() {
    let root = private_tempdir();
    let [broken, empty, untitled, missing] = ["1", "2", "3", "4"].map(|n| n.repeat(64));
    for (sha, document) in [
        (&broken, Some("{not json")),
        (&empty, Some("")),
        (
            &untitled,
            Some(r#"{"metadata":{"title":null},"blocks":[1,2,3]}"#),
        ),
        (&missing, None),
    ] {
        let dir = root.path().join(sha);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("other.bin"), [0u8; 10]).unwrap();
        if let Some(document) = document {
            fs::write(dir.join("document.json"), document).unwrap();
        }
    }

    let listed = list_indexes(root.path()).unwrap();

    assert_eq!(listed.len(), 4);
    assert!(
        listed.iter().all(|index| index.title.is_none()),
        "{listed:?}"
    );
    assert!(listed.iter().all(|index| index.size_bytes >= 10));
}

#[test]
fn a_missing_root_holds_nothing_and_is_not_created() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("cache");

    assert!(list_indexes(&root).unwrap().is_empty());
    assert_eq!(clear(&root).unwrap(), ClearReport::default());
    assert!(!root.exists());
}

#[test]
fn clear_removes_indexes_and_staging_and_keeps_lock_files_and_other_files() {
    let _serial = serial();
    let setup = Setup::new();
    let (a, b) = (setup.cached("Clear A"), setup.cached("Clear B"));
    let (orphan, lone) = ("9".repeat(64), "8".repeat(64));
    // Leftovers of a dead build whose index was never finished.
    for dir in [format!("{a}.staging-1-2"), format!("{orphan}.staging-3-4")] {
        fs::create_dir(setup.root().join(&dir)).unwrap();
        fs::write(setup.root().join(&dir).join("half.json"), "{").unwrap();
    }
    // A lock file with nothing else of its book.
    fs::write(setup.root().join(format!("{lone}.lock")), "123").unwrap();
    // Not ours: names that only look like cache entries.
    let foreign = [
        "notes.txt".to_string(),
        "keep-me".to_string(),
        "E".repeat(64),
        format!("{a}.other"),
        format!("{a}.locked"),
    ];
    fs::write(setup.root().join(&foreign[0]), "x").unwrap();
    for name in &foreign[1..] {
        fs::create_dir(setup.root().join(name)).unwrap();
    }

    let report = clear(setup.root()).unwrap();

    assert_eq!(
        report,
        ClearReport {
            removed: 2,
            skipped_busy: 0
        }
    );
    // Every lock file is still there: unlinking one lets two processes lock
    // "the same" book through different files.
    let locks = [&a, &b, &orphan, &lone].map(|sha| format!("{sha}.lock"));
    let mut expected: Vec<String> = foreign.iter().cloned().chain(locks.clone()).collect();
    expected.sort();
    assert_eq!(names(setup.root()), expected);
    assert_eq!(fs::read(setup.root().join(&locks[3])).unwrap(), b"123");
    assert!(list_indexes(setup.root()).unwrap().is_empty());
    assert_eq!(clear(setup.root()).unwrap(), ClearReport::default());
    assert_eq!(names(setup.root()), expected);
}

#[test]
fn clear_skips_a_book_whose_lock_is_held() {
    let _serial = serial();
    let setup = Setup::new();
    let (busy, idle) = (setup.cached("Busy Book"), setup.cached("Idle Book"));
    let building = setup.root().join(format!("{busy}.staging-1-2"));
    fs::create_dir(&building).unwrap();
    fs::write(building.join("half.json"), "{").unwrap();

    let report = with_lock(setup.root(), &busy, Duration::from_secs(5), || {
        Ok(clear(setup.root()))
    })
    .unwrap()
    .unwrap();

    assert_eq!(
        report,
        ClearReport {
            removed: 1,
            skipped_busy: 1
        }
    );
    // The busy book is whole, with the build that is under way.
    assert_eq!(shas(&list_indexes(setup.root()).unwrap()), [busy.as_str()]);
    assert!(setup.root().join(&busy).join("manifest.json").is_file());
    assert!(building.join("half.json").is_file());
    assert!(!setup.root().join(&idle).exists());

    // Once the lock is let go, the book is cleared with its staging directory.
    let report = clear(setup.root()).unwrap();
    assert_eq!(
        report,
        ClearReport {
            removed: 1,
            skipped_busy: 0
        }
    );
    // Only the lock files are left, the busy book's among them.
    let mut locks = [format!("{busy}.lock"), format!("{idle}.lock")];
    locks.sort();
    assert_eq!(names(setup.root()), locks);
}

#[cfg(unix)]
#[test]
fn clear_does_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let root = private_tempdir();
    let outside = TempDir::new().unwrap();
    let [linked_index, linked_staging, linked_lock] = ["a", "b", "c"].map(|n| n.repeat(64));
    let precious = |name: &str| {
        let dir = outside.path().join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("keep.txt"), b"keep").unwrap();
        dir
    };
    let index_target = precious("index");
    let staging_target = precious("staging");
    let lock_target = outside.path().join("lock.txt");
    fs::write(&lock_target, b"keep").unwrap();
    symlink(&index_target, root.path().join(&linked_index)).unwrap();
    symlink(
        &staging_target,
        root.path().join(format!("{linked_staging}.staging-1-2")),
    )
    .unwrap();
    // A book whose lock file is a link, which no process can lock through.
    fs::create_dir(root.path().join(&linked_lock)).unwrap();
    symlink(
        &lock_target,
        root.path().join(format!("{linked_lock}.lock")),
    )
    .unwrap();

    // A link is not an index.
    assert!(list_indexes(root.path()).unwrap().len() == 1);
    let report = clear(root.path()).unwrap();

    // Only the real index directory counts.
    assert_eq!(
        report,
        ClearReport {
            removed: 1,
            skipped_busy: 0
        }
    );
    // The links are gone. Each book has a lock file now, a real one and kept,
    // the last book's in place of the link. Every target is as it was.
    let locks = [&linked_index, &linked_staging, &linked_lock].map(|sha| format!("{sha}.lock"));
    assert_eq!(names(root.path()), locks);
    for lock in &locks {
        let metadata = fs::symlink_metadata(root.path().join(lock)).unwrap();
        assert!(metadata.is_file(), "{lock}");
    }
    assert_eq!(fs::read(index_target.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(fs::read(staging_target.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(fs::read(&lock_target).unwrap(), b"keep");
}
