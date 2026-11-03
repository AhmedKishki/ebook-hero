//! One opened book: its document, passages and lexical index, read from the
//! cache or built into it first.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::document::Document;
use crate::embed::Models;
use crate::extract::extract;
use crate::index::cache::{
    self, DOCUMENT_FILE, EmbeddingStamp, MANIFEST_FILE, Manifest, PASSAGES_FILE, TANTIVY_DIR,
    VECTORS_FILE, expected_stage1, io_error,
};
use crate::index::lexical::LexicalIndex;
use crate::index::vectors::Vectors;
use crate::passages::{Passage, build_passages};
use crate::settings::{EMBEDDING_DIM, EMBEDDING_MODEL, LOCK_WAIT};
use crate::{Error, Result};

pub struct Book {
    pub path: PathBuf,
    pub sha256: String,
    pub dir: PathBuf,
    pub document: Document,
    pub passages: Vec<Passage>,
    pub lexical: LexicalIndex,
    /// Stage 2 of the index, loaded or built the first time it is needed.
    vectors: OnceLock<Vectors>,
}

impl Book {
    /// Opens the book at `path`, whose content hashes to `sha256`.
    ///
    /// `sha256` must be a SHA-256 hex digest and `cache_root` a private
    /// directory (see [`cache::ensure_private_dir`]): the cached index is
    /// loaded without being checked, so only this user may be able to write it.
    ///
    /// A cached stage-1 index whose manifest is current is loaded as it is.
    /// Otherwise the build lock for this book is taken, the cache is checked
    /// again (another process may have built it while this one waited), and
    /// only then is the index built. A cache that does not load is rebuilt
    /// like a stale one, and why it did not load is logged as a warning.
    pub fn open(cache_root: &Path, path: &Path, sha256: &str) -> Result<Book> {
        cache::require_sha256(sha256)?;
        cache::ensure_private_dir(cache_root)?;
        let dir = cache::stage_dir(cache_root, sha256);
        if let Ok(Some(book)) = load_current(&dir, path, sha256) {
            return Ok(book);
        }
        cache::with_lock(cache_root, sha256, LOCK_WAIT, || {
            match load_current(&dir, path, sha256) {
                Ok(Some(book)) => return Ok(book),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    path = %dir.display(),
                    %error,
                    "the cached index did not load; building it again"
                ),
            }
            cache::remove_stale(cache_root, sha256)?;
            build(cache_root, &dir, path, sha256)?;
            load(&dir, path, sha256)
        })
    }

    /// Whether the book's cached index is still on disk, judged by one look at
    /// its manifest. `cache clear` may have removed it since the book was
    /// opened.
    pub fn is_cached(&self) -> bool {
        self.dir.join(MANIFEST_FILE).is_file()
    }

    /// The passage embeddings, stage 2 of the index: loaded from the cache if
    /// they are there, else made with `models` and cached.
    ///
    /// Embeddings are loaded only if the manifest says the current model made
    /// them and the file holds exactly one row per passage. Otherwise, under
    /// the same lock as stage 1, every passage is embedded, `progress` is told
    /// how many are done, and the file is written through a temporary one.
    /// The manifest is stamped last, so an interrupted build is redone. A
    /// cache directory that was removed since the book was opened is an error
    /// that asks for another call, which opens the book again.
    pub fn vectors(&self, models: &Models, progress: &dyn Fn(usize, usize)) -> Result<&Vectors> {
        if let Some(vectors) = self.vectors.get() {
            return Ok(vectors);
        }
        let root = self
            .dir
            .parent()
            .ok_or_else(|| Error::Index(format!("{} is not inside a cache", self.dir.display())))?;
        let vectors = cache::with_lock(root, &self.sha256, LOCK_WAIT, || {
            if !self.dir.is_dir() {
                return Err(removed());
            }
            match self.stored_vectors() {
                Some(vectors) => Ok(vectors),
                None => self.embed(models, progress),
            }
        })?;
        Ok(self.vectors.get_or_init(|| vectors))
    }

    /// True when [`Book::vectors`] needs no model: the embeddings are in
    /// memory, or the cache holds a stamped file of the right size.
    pub fn vectors_ready(&self) -> bool {
        if self.vectors.get().is_some() {
            return true;
        }
        let size = fs::metadata(self.dir.join(VECTORS_FILE)).map(|m| m.len());
        self.stamped() && size.is_ok_and(|size| size == self.vectors_size())
    }

    /// True when the manifest says the current model made the cached vectors.
    fn stamped(&self) -> bool {
        cache::read_json::<Manifest>(&self.dir.join(MANIFEST_FILE))
            .is_ok_and(|manifest| manifest.embedding == Some(EmbeddingStamp::current()))
    }

    /// The size in bytes of a vector file with one row for each passage.
    fn vectors_size(&self) -> u64 {
        self.passages.len() as u64 * (EMBEDDING_DIM * size_of::<f32>()) as u64
    }

    fn stored_vectors(&self) -> Option<Vectors> {
        if !self.stamped() {
            return None;
        }
        Vectors::read(
            &self.dir.join(VECTORS_FILE),
            EMBEDDING_DIM,
            self.passages.len(),
        )
        .ok()
    }

    /// Embeds every passage and caches the result. Call it only while holding
    /// the lock for this book.
    fn embed(&self, models: &Models, progress: &dyn Fn(usize, usize)) -> Result<Vectors> {
        let texts: Vec<&str> = self.passages.iter().map(|p| p.text.as_str()).collect();
        let vectors = Vectors::from_rows(models.embed_passages(&texts, progress)?)?;
        if vectors.rows() > 0 && vectors.dim() != EMBEDDING_DIM {
            return Err(Error::Index(format!(
                "{EMBEDDING_MODEL} returned {} values per passage, not {EMBEDDING_DIM}",
                vectors.dim()
            )));
        }
        store_vectors(&self.dir, &vectors)?;
        Ok(vectors)
    }
}

/// Writes `vectors` into the cache directory `dir` through a temporary file
/// and stamps the manifest last. When the manifest no longer describes the
/// stage 1 this build makes, another version rebuilt the index while the
/// passages were embedded, and nothing is written; nor when `dir` is gone.
fn store_vectors(dir: &Path, vectors: &Vectors) -> Result<()> {
    if !dir.is_dir() {
        return Err(removed());
    }
    let path = dir.join(MANIFEST_FILE);
    let manifest: Manifest = cache::read_json(&path)?;
    if !manifest.is_current_stage1() {
        return Err(Error::Index(
            "the cached index changed while embedding; call again".to_string(),
        ));
    }
    let file = dir.join(VECTORS_FILE);
    let temp = dir.join(format!("{VECTORS_FILE}.tmp"));
    let written = vectors
        .write(&temp)
        .and_then(|()| fs::rename(&temp, &file).map_err(|e| io_error(&file, e)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    let stamped = Manifest {
        embedding: Some(EmbeddingStamp::current()),
        ..manifest
    };
    cache::write_json_atomic(&path, &stamped)
}

/// The error for a book whose cached index was deleted after it was opened.
fn removed() -> Error {
    Error::Index("the cached index for this book was removed; call again to rebuild it".to_string())
}

/// The book cached in `dir`, if its stage 1 is current. `Ok(None)` when
/// nothing is cached there or another version built it; an error when the
/// cache should load and does not.
fn load_current(dir: &Path, path: &Path, sha256: &str) -> Result<Option<Book>> {
    let manifest_path = dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest: Manifest = cache::read_json(&manifest_path)?;
    if !manifest.is_current_stage1() {
        return Ok(None);
    }
    load(dir, path, sha256).map(Some)
}

fn load(dir: &Path, path: &Path, sha256: &str) -> Result<Book> {
    Ok(Book {
        path: path.to_path_buf(),
        sha256: sha256.to_string(),
        dir: dir.to_path_buf(),
        document: cache::read_json(&dir.join(DOCUMENT_FILE))?,
        passages: cache::read_json(&dir.join(PASSAGES_FILE))?,
        lexical: LexicalIndex::open(&dir.join(TANTIVY_DIR))?,
        vectors: OnceLock::new(),
    })
}

/// Builds stage 1 in a staging directory and renames it to `dir`. The manifest
/// is written last, so a staging directory that has one is complete; nothing
/// is left behind when the build fails.
fn build(cache_root: &Path, dir: &Path, path: &Path, sha256: &str) -> Result<()> {
    let document = extract(path, sha256)?;
    let passages = build_passages(&document.blocks);
    let staging = cache::staging_dir(cache_root, sha256);
    fs::create_dir(&staging).map_err(|e| io_error(&staging, e))?;
    let built = write_stage(&staging, &document, &passages)
        .and_then(|()| fs::rename(&staging, dir).map_err(|e| io_error(dir, e)));
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
    fs::create_dir(&tantivy).map_err(|e| io_error(&tantivy, e))?;
    LexicalIndex::build(&tantivy, passages)?;
    cache::write_json_atomic(&staging.join(MANIFEST_FILE), &expected_stage1())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PDF_READER_VERSION;

    fn one_vector() -> Vectors {
        Vectors::from_rows(vec![vec![1.0; EMBEDDING_DIM]]).unwrap()
    }

    #[test]
    fn vectors_are_stored_and_stamped_on_the_index_they_were_made_from() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join(MANIFEST_FILE);
        cache::write_json_atomic(&manifest, &expected_stage1()).unwrap();

        store_vectors(dir.path(), &one_vector()).unwrap();

        let stamped: Manifest = cache::read_json(&manifest).unwrap();
        assert_eq!(stamped.embedding, Some(EmbeddingStamp::current()));
        assert!(stamped.is_current_stage1());
        assert!(dir.path().join(VECTORS_FILE).is_file());
    }

    #[test]
    fn an_index_rebuilt_by_another_version_while_embedding_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join(MANIFEST_FILE);
        let other = Manifest {
            pdf_reader: PDF_READER_VERSION + 1,
            ..expected_stage1()
        };
        cache::write_json_atomic(&manifest, &other).unwrap();

        let error = store_vectors(dir.path(), &one_vector()).unwrap_err();

        assert!(
            matches!(&error, Error::Index(message)
                if message == "the cached index changed while embedding; call again"),
            "{error}"
        );
        assert_eq!(cache::read_json::<Manifest>(&manifest).unwrap(), other);
        assert!(!dir.path().join(VECTORS_FILE).exists());
    }

    #[test]
    fn vectors_for_a_removed_index_ask_for_another_call() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");

        let error = store_vectors(&gone, &one_vector()).unwrap_err();

        assert!(
            error.to_string().contains("call again to rebuild it"),
            "{error}"
        );
        assert!(!gone.exists());
    }
}
