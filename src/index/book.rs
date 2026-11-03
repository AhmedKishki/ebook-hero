//! One opened book: its document, passages and lexical index, read from the
//! cache or built into it first.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::document::Document;
use crate::extract::extract;
use crate::index::cache::{
    self, DOCUMENT_FILE, MANIFEST_FILE, Manifest, PASSAGES_FILE, TANTIVY_DIR, expected_stage1,
};
use crate::index::lexical::LexicalIndex;
use crate::passages::{Passage, build_passages};
use crate::settings::LOCK_WAIT;

pub struct Book {
    pub path: PathBuf,
    pub sha256: String,
    pub dir: PathBuf,
    pub document: Document,
    pub passages: Vec<Passage>,
    pub lexical: LexicalIndex,
}

impl Book {
    /// Opens the book at `path`, whose content hashes to `sha256`.
    ///
    /// A cached stage-1 index whose manifest is current is loaded as it is.
    /// Otherwise the build lock for this book is taken, the cache is checked
    /// again (another process may have built it while this one waited), and
    /// only then is the index built. A cache that does not load is rebuilt
    /// like a stale one.
    pub fn open(cache_root: &Path, path: &Path, sha256: &str) -> Result<Book> {
        let dir = cache::stage_dir(cache_root, sha256);
        if let Some(book) = load_current(&dir, path, sha256) {
            return Ok(book);
        }
        cache::with_lock(cache_root, sha256, LOCK_WAIT, || {
            if let Some(book) = load_current(&dir, path, sha256) {
                return Ok(book);
            }
            cache::remove_stale(cache_root, sha256)?;
            build(cache_root, &dir, path, sha256)?;
            load(&dir, path, sha256)
        })
    }
}

fn load_current(dir: &Path, path: &Path, sha256: &str) -> Option<Book> {
    let manifest: Manifest = cache::read_json(&dir.join(MANIFEST_FILE)).ok()?;
    if !manifest.is_current_stage1() {
        return None;
    }
    load(dir, path, sha256).ok()
}

fn load(dir: &Path, path: &Path, sha256: &str) -> Result<Book> {
    Ok(Book {
        path: path.to_path_buf(),
        sha256: sha256.to_string(),
        dir: dir.to_path_buf(),
        document: cache::read_json(&dir.join(DOCUMENT_FILE))?,
        passages: cache::read_json(&dir.join(PASSAGES_FILE))?,
        lexical: LexicalIndex::open(&dir.join(TANTIVY_DIR))?,
    })
}

/// Builds stage 1 in a staging directory and renames it to `dir`. The manifest
/// is written last, so a staging directory that has one is complete; nothing
/// is left behind when the build fails.
fn build(cache_root: &Path, dir: &Path, path: &Path, sha256: &str) -> Result<()> {
    let document = extract(path, sha256)?;
    let passages = build_passages(&document.blocks);
    let staging = cache::staging_dir(cache_root, sha256);
    fs::create_dir(&staging)?;
    let built = write_stage(&staging, &document, &passages)
        .and_then(|()| fs::rename(&staging, dir).map_err(Into::into));
    if built.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    built?;
    cache::record_build();
    Ok(())
}

fn write_stage(staging: &Path, document: &Document, passages: &[Passage]) -> Result<()> {
    cache::write_json_atomic(&staging.join(DOCUMENT_FILE), document)?;
    cache::write_json_atomic(&staging.join(PASSAGES_FILE), &passages)?;
    let tantivy = staging.join(TANTIVY_DIR);
    fs::create_dir(&tantivy)?;
    LexicalIndex::build(&tantivy, passages)?;
    cache::write_json_atomic(&staging.join(MANIFEST_FILE), &expected_stage1())
}
