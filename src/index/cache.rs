//! The on-disk cache of one book's index: where it lives, the manifest that
//! says whether it is current, and the lock that lets one process build it.
//!
//! ```text
//! <root>/<sha>/                    manifest.json, document.json, passages.json, tantivy/
//! <root>/<sha>.lock                held while building; holds the builder's pid
//! <root>/<sha>.staging-<pid>-<ns>/ a build in progress, renamed to <sha>/ when done
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs4::{FileExt, TryLockError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::document::SCHEMA_VERSION;
use crate::settings::{
    ANALYZERS_VERSION, EPUB_READER_VERSION, LOCK_POLL, PASSAGES_VERSION, PDF_READER_VERSION,
};
use crate::{Error, Result};

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const DOCUMENT_FILE: &str = "document.json";
pub(crate) const PASSAGES_FILE: &str = "passages.json";
pub(crate) const TANTIVY_DIR: &str = "tantivy";

const HASH_CHUNK: usize = 64 * 1024;

static BUILDS: AtomicUsize = AtomicUsize::new(0);

pub fn default_cache_root() -> PathBuf {
    std::env::temp_dir().join("ebook-hero")
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
    pub schema_version: u32,
    pub tool_version: String,
    pub pdf_reader: u32,
    pub epub_reader: u32,
    pub passages: u32,
    pub analyzers: u32,
    pub embedding: Option<EmbeddingStamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingStamp {
    pub model: String,
    pub dim: usize,
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
    fs::create_dir_all(root)?;
    let path = lock_path(root, sha);
    let mut lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let started = Instant::now();
    loop {
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
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    lock.set_len(0)?;
    lock.write_all(std::process::id().to_string().as_bytes())?;
    let result = f();
    // Clear the pid while the lock is still held, so a waiter never names a
    // process that has already let go.
    let _ = lock.set_len(0);
    result
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

fn lock_path(root: &Path, sha: &str) -> PathBuf {
    root.join(format!("{sha}.lock"))
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
    let prefix = staging_prefix(sha);
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove_all(&entry.path())?;
        }
    }
    remove_all(&stage_dir(root, sha))
}

fn remove_all(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(Error::from),
    }
}

/// Writes `value` as JSON through a temporary file and a rename, so a reader
/// sees the old file or the new one and never half of either.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::Index(format!("write {}: {e}", path.display())))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)?;
    Ok(())
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Index(format!("read {}: {e}", path.display())))
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

    #[test]
    fn staging_paths_are_named_for_the_book_and_the_process() {
        let root = Path::new("/cache");
        let first = staging_dir(root, "abc");
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("abc.staging-{}-", std::process::id())));
        assert_eq!(first.parent(), Some(root));
    }
}
