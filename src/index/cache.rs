//! The on-disk cache of one book's index: where it lives, the manifest that
//! says whether it is current, and the lock that lets one process build it.
//!
//! ```text
//! <root>/<sha>/                    manifest.json, document.json, passages.json, tantivy/,
//!                                  and, once embedded, vectors.f32
//! <root>/<sha>.lock                held while building; holds the builder's pid; never deleted
//! <root>/<sha>.staging-<pid>-<ns>/ a build in progress, renamed to <sha>/ when done
//! ```
//!
//! [`list_indexes`] and [`clear`] are what `cache list` and `cache clear` do.
//!
//! The root is trusted: its indexes are loaded without checking them. On Unix
//! [`ensure_private_dir`] therefore insists it is a real directory that only
//! this user can write, and every `sha` that names a path inside it must be a
//! SHA-256 hex digest, so nothing outside the root is ever created or deleted.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs4::{FileExt, TryLockError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cancel::Cancel;
use crate::document::SCHEMA_VERSION;
use crate::settings::{
    ANALYZERS_VERSION, EMBEDDING_DIM, EMBEDDING_MODEL, EPUB_READER_VERSION, LOCK_POLL,
    PASSAGES_VERSION, PDF_READER_VERSION,
};
use crate::{Error, Result};

const SHA256_HEX_LEN: usize = 64;
/// How much of a rejected hash an error message repeats.
const ECHO_CHARS: usize = 80;

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const DOCUMENT_FILE: &str = "document.json";
pub(crate) const PASSAGES_FILE: &str = "passages.json";
pub(crate) const TANTIVY_DIR: &str = "tantivy";
pub(crate) const VECTORS_FILE: &str = "vectors.f32";
pub(crate) const GRAPH_FILE: &str = "graph.json";

const HASH_CHUNK: usize = 64 * 1024;

static BUILDS: AtomicUsize = AtomicUsize::new(0);

/// `$EBOOK_HERO_INDEXES` if it is set, else `ebook-hero/indexes` in the user's
/// cache directory, beside the models. A temporary directory would be emptied
/// at boot, and every book indexed and embedded again.
pub fn default_cache_root() -> PathBuf {
    cache_root_in(std::env::var_os("EBOOK_HERO_INDEXES"), dirs::cache_dir())
}

/// `from_env` if it is set, used as given, so its parent must exist. Else
/// `ebook-hero/indexes` in `user_cache`, or in the temporary directory when
/// there is no user cache, with the `ebook-hero` directory made if it is
/// missing, as the models directory is.
fn cache_root_in(from_env: Option<std::ffi::OsString>, user_cache: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = from_env.filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let app = user_cache
        .unwrap_or_else(std::env::temp_dir)
        .join("ebook-hero");
    // A directory that cannot be made shows up, with its cause, when the root
    // in it is made.
    let _ = fs::create_dir_all(&app);
    app.join("indexes")
}

/// True for exactly 64 lowercase hex digits, the form [`sha256_file`] returns.
pub(crate) fn is_sha256_hex(s: &str) -> bool {
    s.len() == SHA256_HEX_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Refuses a `sha` that could name anything but a book: the cache builds paths
/// from it and deletes some of them.
pub(crate) fn require_sha256(sha: &str) -> Result<()> {
    if is_sha256_hex(sha) {
        return Ok(());
    }
    let shown: String = sha.chars().take(ECHO_CHARS).collect();
    Err(Error::InvalidArgument(format!(
        "not a SHA-256 hex digest: {shown}"
    )))
}

/// Makes sure `root` is a directory that only this user can write to.
///
/// A missing root is created with mode 0700; its parent must exist. On Unix an
/// existing root must be a directory and not a symlink, owned by the current
/// user, and not writable by group or others, because anyone who can write it
/// can plant an index that this process would load.
pub fn ensure_private_dir(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        create_private(root)?;
        check_private(root)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(root).map_err(|e| io_error(root, e))
    }
}

#[cfg(unix)]
fn create_private(root: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    match fs::DirBuilder::new().mode(0o700).create(root) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(io_error(root, e)),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn check_private(root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(root).map_err(|e| io_error(root, e))?;
    // SAFETY: `geteuid` takes no arguments, cannot fail, and touches no memory.
    let user = unsafe { libc::geteuid() };
    let reason = if !metadata.file_type().is_dir() {
        "it is not a directory; a symbolic link is not followed".to_string()
    } else if metadata.uid() != user {
        format!(
            "it is owned by user {}, not by you (user {user})",
            metadata.uid()
        )
    } else if metadata.mode() & 0o022 != 0 {
        format!(
            "its mode {:o} lets group or others write to it",
            metadata.mode() & 0o777
        )
    } else {
        return Ok(());
    };
    Err(Error::Index(format!(
        "cache directory {} is not private ({reason}); set EBOOK_HERO_INDEXES to a private directory",
        root.display()
    )))
}

/// The SHA-256 of the file's bytes as lowercase hex, read in chunks.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| file_error(path, e))?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; HASH_CHUNK];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => hasher.update(&chunk[..read]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(file_error(path, e)),
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// What a cached index was built from. An index is reused only when every
/// stage-1 field equals [`expected_stage1`]; `embedding` belongs to stage 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The [`SCHEMA_VERSION`] of the cached document.
    pub schema_version: u32,
    /// The version of ebook-hero that built the index.
    pub tool_version: String,
    /// The [`PDF_READER_VERSION`] it was built with.
    pub pdf_reader: u32,
    /// The [`EPUB_READER_VERSION`] it was built with.
    pub epub_reader: u32,
    /// The [`PASSAGES_VERSION`] it was built with.
    pub passages: u32,
    /// The [`ANALYZERS_VERSION`] it was built with.
    pub analyzers: u32,
    /// Which model made the cached embeddings, once there are any.
    pub embedding: Option<EmbeddingStamp>,
}

/// The model behind a book's cached embeddings. They are used only when it
/// is the model this build embeds queries with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingStamp {
    /// The model's name, such as [`EMBEDDING_MODEL`].
    pub model: String,
    /// How many numbers each embedding has.
    pub dim: usize,
}

impl EmbeddingStamp {
    /// What vectors made by the model this build uses are stamped with.
    pub fn current() -> EmbeddingStamp {
        EmbeddingStamp {
            model: EMBEDDING_MODEL.to_string(),
            dim: EMBEDDING_DIM,
        }
    }
}

impl Manifest {
    /// True when everything stage 1 depends on matches this build.
    pub fn is_current_stage1(&self) -> bool {
        Manifest {
            embedding: None,
            ..self.clone()
        } == expected_stage1()
    }
}

/// The manifest a stage-1 index built by this version of ebook-hero has,
/// without embeddings.
pub fn expected_stage1() -> Manifest {
    Manifest {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        pdf_reader: PDF_READER_VERSION,
        epub_reader: EPUB_READER_VERSION,
        passages: PASSAGES_VERSION,
        analyzers: ANALYZERS_VERSION,
        embedding: None,
    }
}

/// Runs `f` while holding the exclusive build lock for `sha`.
///
/// Polls every [`LOCK_POLL`] for up to `wait`, then fails with
/// [`Error::LockTimeout`] naming the process that wrote its pid into the lock
/// file. The lock is released when `f` returns, or when the process dies.
pub fn with_lock<T>(
    root: &Path,
    sha: &str,
    wait: Duration,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_lock_cancellable(root, sha, wait, &Cancel::new(), f)
}

/// [`with_lock`], but the wait for the lock ends with [`Error::Cancelled`]
/// as soon as `cancel` is set, within [`LOCK_POLL`]. Once the lock is held,
/// `f` runs to its end whatever `cancel` says; `f` may check it itself.
pub fn with_lock_cancellable<T>(
    root: &Path,
    sha: &str,
    wait: Duration,
    cancel: &Cancel,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    require_sha256(sha)?;
    ensure_private_dir(root)?;
    let path = lock_path(root, sha);
    let mut lock = open_lock(&path)?;
    let started = Instant::now();
    loop {
        cancel.check()?;
        match FileExt::try_lock(&lock) {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {
                let waited = started.elapsed();
                if waited >= wait {
                    return Err(Error::LockTimeout {
                        sha256: sha.to_string(),
                        pid: holder_pid(&path),
                        waited_secs: waited.as_secs(),
                    });
                }
                thread::sleep(LOCK_POLL.min(wait - waited));
            }
            Err(TryLockError::Error(e)) => return Err(io_error(&path, e)),
        }
    }
    lock.set_len(0)
        .and_then(|()| lock.write_all(std::process::id().to_string().as_bytes()))
        .map_err(|e| io_error(&path, e))?;
    let result = f();
    // Clear the pid while the lock is still held, so a waiter never names a
    // process that has already let go.
    let _ = lock.set_len(0);
    result
}

/// Opens the lock file, creating it. It is written to, so a symlink there is
/// an error: following it would let whoever planted it truncate its target.
fn open_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|e| {
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            Error::Index(format!(
                "lock file {} is a symbolic link and is not followed; delete it",
                path.display()
            ))
        } else {
            io_error(path, e)
        }
    })
}

/// How many stage-1 builds this process has finished.
#[doc(hidden)]
pub fn build_count() -> usize {
    BUILDS.load(Ordering::SeqCst)
}

pub(crate) fn record_build() {
    BUILDS.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn stage_dir(root: &Path, sha: &str) -> PathBuf {
    root.join(sha)
}

fn lock_name(sha: &str) -> String {
    format!("{sha}.lock")
}

fn lock_path(root: &Path, sha: &str) -> PathBuf {
    root.join(lock_name(sha))
}

fn staging_prefix(sha: &str) -> String {
    format!("{sha}.staging-")
}

/// A fresh, unused path for a build of `sha`.
pub(crate) fn staging_dir(root: &Path, sha: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    root.join(format!(
        "{}{}-{nanos}",
        staging_prefix(sha),
        std::process::id()
    ))
}

/// Deletes the stage directory of `sha` and every staging directory left by a
/// build that died. Call it only while holding the lock for `sha`.
pub(crate) fn remove_stale(root: &Path, sha: &str) -> Result<()> {
    debug_assert!(is_sha256_hex(sha), "{sha:?} is not a SHA-256 hex digest");
    let prefix = staging_prefix(sha);
    for entry in fs::read_dir(root).map_err(|e| io_error(root, e))? {
        let entry = entry.map_err(|e| io_error(root, e))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove_all(&entry.path())?;
        }
    }
    remove_all(&stage_dir(root, sha))
}

/// Deletes `path`, whatever it is. A symbolic link is unlinked and never
/// followed, so nothing outside the cache root is touched. A missing path is
/// not an error.
fn remove_all(path: &Path) -> Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match removed {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(|e| io_error(path, e)),
    }
}

/// A book whose index is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedIndex {
    /// The SHA-256 of the book's file, which names its cache directory.
    pub sha256: String,
    /// The bytes in all the files of the index.
    pub size_bytes: u64,
    /// The title in the cached document, if it has one.
    pub title: Option<String>,
}

/// What [`clear`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearReport {
    /// The cached index directories that were deleted.
    pub removed: usize,
    /// The books left alone because another process holds their build lock.
    pub skipped_busy: usize,
}

/// The cached indexes under `root`, sorted by hash. A root that does not exist
/// holds none, and is not created.
pub fn list_indexes(root: &Path) -> Result<Vec<CachedIndex>> {
    if !root_exists(root)? {
        return Ok(Vec::new());
    }
    ensure_private_dir(root)?;
    let mut indexes = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| io_error(root, e))? {
        let entry = entry.map_err(|e| io_error(root, e))?;
        let Some(sha256) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // `file_type` does not follow a symbolic link, so a link is not an index.
        let file_type = entry.file_type().map_err(|e| io_error(&entry.path(), e))?;
        if !is_sha256_hex(&sha256) || !file_type.is_dir() {
            continue;
        }
        let dir = stage_dir(root, &sha256);
        indexes.push(CachedIndex {
            size_bytes: tree_size(&dir)?,
            title: cached_title(&dir),
            sha256,
        });
    }
    indexes.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    Ok(indexes)
}

/// Deletes every cached index under `root`, and the staging directories a dead
/// build left. Other files in the root stay.
///
/// A book is cleared under its build lock, taken without waiting. If another
/// process holds the lock, that book is skipped, because deleting the staging
/// directory of a build in progress would break it. Symbolic links are
/// unlinked, never followed. A root that does not exist holds nothing.
///
/// Lock files are never deleted, whether the book is cleared or not. A process
/// that opened `<sha>.lock` before it was unlinked would lock a file that no
/// path leads to, and a second process that created a new `<sha>.lock` could
/// lock that too, so two builds of one book could run at once. The files are
/// empty, so keeping them costs nothing. The one exception is a lock file that
/// is a symbolic link: nobody can hold a lock through it (`open_lock` refuses
/// to follow it), and it blocks the book, so it is unlinked.
pub fn clear(root: &Path) -> Result<ClearReport> {
    let mut report = ClearReport::default();
    if !root_exists(root)? {
        return Ok(report);
    }
    ensure_private_dir(root)?;
    let mut books = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(|e| io_error(root, e))? {
        let entry = entry.map_err(|e| io_error(root, e))?;
        if let Some(sha) = entry.file_name().to_str().and_then(book_of) {
            books.insert(sha.to_owned());
        }
    }
    for sha in books {
        // `with_lock` refuses a lock file that is a symbolic link.
        let lock = lock_path(root, &sha);
        if fs::symlink_metadata(&lock).is_ok_and(|m| m.file_type().is_symlink()) {
            remove_all(&lock)?;
        }
        let cleared = with_lock(root, &sha, Duration::ZERO, || {
            let had_index = fs::symlink_metadata(stage_dir(root, &sha)).is_ok_and(|m| m.is_dir());
            tracing::debug!(sha256 = %sha, "clearing cached index");
            remove_stale(root, &sha)?;
            Ok(had_index)
        });
        match cleared {
            Ok(had_index) => report.removed += usize::from(had_index),
            Err(Error::LockTimeout { .. }) => report.skipped_busy += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(report)
}

fn root_exists(root: &Path) -> Result<bool> {
    match fs::symlink_metadata(root) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(root, e)),
    }
}

/// The book a name in the cache root belongs to, for the names `clear`
/// deletes: its stage directory (named for the hash alone, see [`stage_dir`])
/// or one of its staging directories. A lock file is not one of them, and
/// neither is any other name.
fn book_of(name: &str) -> Option<&str> {
    let sha = name
        .get(..SHA256_HEX_LEN)
        .filter(|sha| is_sha256_hex(sha))?;
    let ours = name == sha || name.starts_with(&staging_prefix(sha));
    ours.then_some(sha)
}

/// The bytes in the files under `dir`; a symbolic link counts as itself.
fn tree_size(dir: &Path) -> Result<u64> {
    let mut bytes = 0;
    for entry in fs::read_dir(dir).map_err(|e| io_error(dir, e))? {
        let entry = entry.map_err(|e| io_error(dir, e))?;
        let metadata = entry.metadata().map_err(|e| io_error(&entry.path(), e))?;
        bytes += if metadata.is_dir() {
            tree_size(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(bytes)
}

/// Just the title of a cached document; the rest of the file is skipped.
#[derive(Deserialize)]
struct TitledDocument {
    metadata: TitledMetadata,
}

#[derive(Deserialize)]
struct TitledMetadata {
    title: Option<String>,
}

/// The title of the cached document in `dir`, or `None` when it has none or
/// the file cannot be read.
fn cached_title(dir: &Path) -> Option<String> {
    let file = File::open(dir.join(DOCUMENT_FILE)).ok()?;
    let document: TitledDocument = serde_json::from_reader(BufReader::new(file)).ok()?;
    document.metadata.title
}

/// Writes `value` as JSON through a temporary file and a rename, so a reader
/// sees the old file or the new one and never half of either.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::Index(format!("write {}: {e}", path.display())))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(|e| io_error(&temp, e))?;
    fs::rename(&temp, path).map_err(|e| io_error(path, e))
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|e| io_error(path, e))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Index(format!("read {}: {e}", path.display())))
}

/// An I/O error in the cache, as an index error that names the file or
/// directory it happened to.
pub(crate) fn io_error(path: &Path, error: io::Error) -> Error {
    Error::Index(format!("{}: {error}", path.display()))
}

/// Names the file in the error: a missing path is `NotFound`, anything else
/// `Unreadable`.
pub(crate) fn file_error(path: &Path, error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Error::NotFound {
            path: path.to_path_buf(),
        },
        _ => Error::Unreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        },
    }
}

fn holder_pid(lock: &Path) -> Option<u32> {
    fs::read_to_string(lock).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_cache_root_comes_from_the_environment_first_and_else_beside_the_models() {
        let user_cache = tempfile::tempdir().unwrap();
        let named = user_cache.path().join("missing").join("indexes");

        let root = cache_root_in(
            Some(named.clone().into_os_string()),
            Some(user_cache.path().into()),
        );
        assert_eq!(root, named);
        assert!(
            !named.parent().unwrap().exists(),
            "a named root is used as given"
        );

        for from_env in [None, Some(OsString::new())] {
            let root = cache_root_in(from_env, Some(user_cache.path().into()));
            assert_eq!(root, user_cache.path().join("ebook-hero").join("indexes"));
            assert!(
                root.parent().unwrap().is_dir(),
                "the ebook-hero directory is made"
            );
        }
    }

    #[test]
    fn sha256_of_known_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(matches!(
            sha256_file(&dir.path().join("missing")),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn only_sixty_four_lowercase_hex_digits_are_a_sha256() {
        assert!(is_sha256_hex(&"0123456789abcdef".repeat(4)));
        let bad = [
            String::new(),
            "..".to_string(),
            "abc".to_string(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            "g".repeat(64),
            format!("{}/", "a".repeat(63)),
            format!("../{}", "a".repeat(61)),
        ];
        assert!(bad.iter().all(|s| !is_sha256_hex(s)));
        let known = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(is_sha256_hex(known));
    }

    #[test]
    fn book_of_names_the_stage_and_staging_entries_of_a_book_but_not_its_lock() {
        let sha = "0123456789abcdef".repeat(4);
        for ours in [
            sha.clone(),
            format!("{sha}.staging-1-2"),
            format!("{sha}.staging-"),
        ] {
            assert_eq!(book_of(&ours), Some(sha.as_str()), "{ours}");
        }
        for foreign in [
            String::new(),
            "notes.txt".to_string(),
            sha[..63].to_string(),
            sha.to_uppercase(),
            format!("{sha}.lock"),
            format!("{sha}.locked"),
            format!("{sha}.lock.bak"),
            format!("{sha}.staging"),
            format!("{sha}x"),
            format!("{}é", &sha[..62]),
        ] {
            assert_eq!(book_of(&foreign), None, "{foreign}");
        }
    }

    #[test]
    fn every_stage_one_field_makes_a_manifest_stale() {
        let current = expected_stage1();
        assert!(current.is_current_stage1());
        let stamped = Manifest {
            embedding: Some(EmbeddingStamp {
                model: "m".to_string(),
                dim: 3,
            }),
            ..current.clone()
        };
        assert!(stamped.is_current_stage1());

        let stale = [
            Manifest {
                schema_version: current.schema_version + 1,
                ..current.clone()
            },
            Manifest {
                tool_version: "0.0.0".to_string(),
                ..current.clone()
            },
            Manifest {
                pdf_reader: current.pdf_reader + 1,
                ..current.clone()
            },
            Manifest {
                epub_reader: current.epub_reader + 1,
                ..current.clone()
            },
            Manifest {
                passages: current.passages + 1,
                ..current.clone()
            },
            Manifest {
                analyzers: current.analyzers + 1,
                ..current
            },
        ];
        assert!(stale.iter().all(|m| !m.is_current_stage1()));
    }

    /// Whether `error` is an index error whose message names `path`.
    fn names(error: &Error, path: &Path) -> bool {
        matches!(error, Error::Index(message) if message.contains(&*path.to_string_lossy()))
    }

    #[test]
    fn json_files_that_cannot_be_read_or_written_are_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        let manifest = gone.join(MANIFEST_FILE);

        let read = read_json::<Manifest>(&manifest).unwrap_err();
        let written = write_json_atomic(&manifest, &expected_stage1()).unwrap_err();

        assert!(names(&read, &manifest), "{read}");
        assert!(names(&written, &gone), "{written}");
    }

    #[test]
    fn a_listing_of_a_cache_that_cannot_be_read_names_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let sha = "0123456789abcdef".repeat(4);
        let index = root.path().join(&sha);
        fs::create_dir(&index).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&index, fs::Permissions::from_mode(0o000)).unwrap();
        }

        let listed = list_indexes(root.path());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&index, fs::Permissions::from_mode(0o700)).unwrap();
        }
        // Root may read any directory; the check only means something otherwise.
        if let Err(error) = listed {
            assert!(names(&error, &index), "{error}");
        }
    }

    #[test]
    fn staging_paths_are_named_for_the_book_and_the_process() {
        let root = Path::new("/cache");
        let first = staging_dir(root, "abc");
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("abc.staging-{}-", std::process::id())));
        assert_eq!(first.parent(), Some(root));
    }
}
