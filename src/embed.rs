//! The embedding and reranking models. Each loads on first use, from the
//! models directory (downloaded there if it is not), and stays loaded until
//! [`Models::release_idle`] finds it unused for a while.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use fastembed::{
    EmbeddingModel, RerankInitOptions, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
};

use crate::settings::{EMBED_BATCH, EMBEDDING_MODEL, QUERY_PREFIX, RERANK_BATCH, RERANKER_MODEL};
use crate::{Error, Result};

pub struct Models {
    cache_dir: PathBuf,
    embedder: Slot<TextEmbedding>,
    reranker: Slot<TextRerank>,
}

impl Models {
    /// Models that will be loaded from, and downloaded into, `cache_dir`.
    /// Nothing is loaded until a model is first used.
    pub fn new(cache_dir: PathBuf) -> Models {
        Models {
            cache_dir,
            embedder: Slot::new(),
            reranker: Slot::new(),
        }
    }

    /// `$EBOOK_HERO_MODELS` if it is set, else `ebook-hero/models` in the
    /// user's cache directory.
    pub fn default_cache_dir() -> PathBuf {
        models_dir(std::env::var_os("EBOOK_HERO_MODELS"), dirs::cache_dir())
    }

    /// Drops each model that has not been used for `idle`, so that a server
    /// gives their memory back between bursts of searches; the next search
    /// loads them again. A model in use is kept. Returns whether any model was
    /// dropped.
    pub fn release_idle(&self, idle: Duration) -> bool {
        let now = Instant::now();
        let embedder = self.embedder.release_if_idle(now, idle);
        let reranker = self.reranker.release_if_idle(now, idle);
        if embedder || reranker {
            return_freed_memory();
        }
        embedder || reranker
    }

    /// Embeds passage texts, `EMBED_BATCH` at a time, and calls
    /// `progress(done, total)` after each batch. Rows are unit length, in the
    /// order of `texts`.
    pub fn embed_passages(
        &self,
        texts: &[&str],
        progress: &dyn Fn(usize, usize),
    ) -> Result<Vec<Vec<f32>>> {
        let total = texts.len();
        let mut rows = Vec::with_capacity(total);
        for batch in texts.chunks(EMBED_BATCH) {
            rows.extend(self.embed(batch)?);
            progress(rows.len(), total);
        }
        Ok(rows)
    }

    /// Embeds a search query, behind the prefix the model was trained to
    /// expect on queries. The row is unit length, like the passage rows it is
    /// compared with.
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{QUERY_PREFIX}{query}");
        let mut rows = self.embed(&[prefixed.as_str()])?;
        rows.pop()
            .map(unit_length)
            .ok_or_else(|| self.failed(EMBEDDING_MODEL, "it returned no embedding"))
    }

    /// How well each of `docs` answers `query`, in the order of `docs`. Scores
    /// are only comparable with each other.
    pub fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        // fastembed returns the documents best first, each with its input index.
        let ranked = self
            .reranker
            .with(
                || self.load(RERANKER_MODEL, load_reranker),
                |model| model.rerank(query, docs, false, Some(RERANK_BATCH)),
            )?
            .map_err(|e| self.failed(RERANKER_MODEL, format!("inference failed: {e}")))?;
        let mut scores = vec![None; docs.len()];
        for result in ranked {
            if let Some(slot) = scores.get_mut(result.index) {
                *slot = Some(result.score);
            }
        }
        scores
            .into_iter()
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| self.failed(RERANKER_MODEL, "it did not score every passage"))
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let rows = self
            .embedder
            .with(
                || self.load(EMBEDDING_MODEL, load_embedder),
                |model| model.embed(texts, Some(EMBED_BATCH)),
            )?
            .map_err(|e| self.failed(EMBEDDING_MODEL, format!("inference failed: {e}")))?;
        if rows.len() == texts.len() {
            Ok(rows)
        } else {
            Err(self.failed(EMBEDDING_MODEL, "it returned a wrong number of embeddings"))
        }
    }

    /// Loads `model` with `load` from the models directory, which is made if
    /// it is missing.
    fn load<T>(
        &self,
        model: &str,
        load: impl FnOnce(&Path) -> std::result::Result<T, String>,
    ) -> Result<T> {
        fs::create_dir_all(&self.cache_dir)
            .map_err(|e| self.failed(model, format!("cannot create the models directory: {e}")))?;
        load(&self.cache_dir).map_err(|reason| self.failed(model, reason))
    }

    fn failed(&self, model: &str, reason: impl Into<String>) -> Error {
        Error::ModelUnavailable {
            model: model.to_string(),
            cache: self.cache_dir.clone(),
            reason: reason.into(),
        }
    }
}

fn load_embedder(dir: &Path) -> std::result::Result<TextEmbedding, String> {
    TextEmbedding::try_new(
        TextInitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(dir.to_path_buf())
            .with_show_download_progress(false),
    )
    .map_err(|e| e.to_string())
}

fn load_reranker(dir: &Path) -> std::result::Result<TextRerank, String> {
    TextRerank::try_new(
        RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
            .with_cache_dir(dir.to_path_buf())
            .with_show_download_progress(false),
    )
    .map_err(|e| e.to_string())
}

/// One model: loaded on first use and kept until it has been idle for as long
/// as [`Slot::release_if_idle`] is told.
struct Slot<T> {
    held: Mutex<Option<Held<T>>>,
}

struct Held<T> {
    model: T,
    /// When its last use ended.
    last_used: Instant,
}

impl<T> Slot<T> {
    fn new() -> Slot<T> {
        Slot {
            held: Mutex::new(None),
        }
    }

    /// Runs `work` on the model, loaded first by `load` if it is not there.
    /// The lock is held while the model loads, so two threads never load it
    /// at once. A failed load is not remembered: the next call tries again,
    /// which succeeds once a missing connection is back.
    fn with<R>(
        &self,
        load: impl FnOnce() -> Result<T>,
        work: impl FnOnce(&mut T) -> R,
    ) -> Result<R> {
        let mut held = locked(&self.held);
        let held = match held.as_mut() {
            Some(held) => held,
            None => held.insert(Held {
                model: load()?,
                last_used: Instant::now(),
            }),
        };
        let result = work(&mut held.model);
        held.last_used = Instant::now();
        Ok(result)
    }

    /// Drops the model if its last use ended at least `idle` before `now`. A
    /// model that a thread is using, or loading, is kept. Returns whether it
    /// was dropped.
    fn release_if_idle(&self, now: Instant, idle: Duration) -> bool {
        let mut held = match self.held.try_lock() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        let idle_long_enough = held
            .as_ref()
            .is_some_and(|h| now.saturating_duration_since(h.last_used) >= idle);
        if idle_long_enough {
            *held = None;
        }
        idle_long_enough
    }
}

/// Hands the memory the models freed back to the system. glibc keeps freed
/// memory for later allocations unless asked, and a server that kept it would
/// gain nothing from dropping them.
fn return_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` takes a size, touches only the allocator's own
    // bookkeeping and is safe to call at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// A model is not left half-changed by a thread that panicked while using it,
/// so a poisoned lock is still safe to take.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `row` scaled to unit length; a row of zeros stays zeros.
fn unit_length(mut row: Vec<f32>) -> Vec<f32> {
    let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut row {
            *x /= norm;
        }
    }
    row
}

fn models_dir(from_env: Option<OsString>, user_cache: Option<PathBuf>) -> PathBuf {
    match from_env.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => user_cache
            .unwrap_or_else(std::env::temp_dir)
            .join("ebook-hero")
            .join("models"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn unavailable_model_names_model_and_remedy() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let models = Models::new(file.path().to_path_buf());
        let error = models.embed_query("x").unwrap_err();
        match &error {
            Error::ModelUnavailable { model, cache, .. } => {
                assert_eq!(model, "BAAI/bge-small-en-v1.5");
                assert_eq!(cache, file.path());
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
        assert!(error.to_string().contains("mode=lexical"), "{error}");
        // The failure is not remembered: the next call tries again.
        assert!(matches!(
            models.embed_query("x"),
            Err(Error::ModelUnavailable { .. })
        ));
    }

    #[test]
    fn the_reranker_reports_its_own_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let models = Models::new(file.path().to_path_buf());
        match models.rerank("q", &["a document"]).unwrap_err() {
            Error::ModelUnavailable { model, .. } => {
                assert_eq!(model, "jinaai/jina-reranker-v1-turbo-en");
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
    }

    /// A slot whose model is the number of the load that made it.
    fn counting_slot() -> (Slot<usize>, std::cell::Cell<usize>) {
        (Slot::new(), std::cell::Cell::new(0))
    }

    fn load_counted(loads: &std::cell::Cell<usize>) -> Result<usize> {
        loads.set(loads.get() + 1);
        Ok(loads.get())
    }

    #[test]
    fn a_slot_loads_its_model_once_and_drops_it_only_when_idle() {
        let (slot, loads) = counting_slot();
        assert_eq!(slot.with(|| load_counted(&loads), |m| *m).unwrap(), 1);
        assert_eq!(slot.with(|| load_counted(&loads), |m| *m).unwrap(), 1);

        let hour = Duration::from_secs(3600);
        let now = Instant::now();
        assert!(!slot.release_if_idle(now, hour), "used just now");
        assert_eq!(slot.with(|| load_counted(&loads), |m| *m).unwrap(), 1);

        assert!(slot.release_if_idle(now + 2 * hour, hour));
        assert!(
            !slot.release_if_idle(now + 2 * hour, hour),
            "nothing left to drop"
        );
        assert_eq!(slot.with(|| load_counted(&loads), |m| *m).unwrap(), 2);
    }

    #[test]
    fn a_slot_whose_load_failed_tries_again() {
        let slot: Slot<usize> = Slot::new();
        let failed = slot.with(|| Err(Error::InvalidArgument("offline".into())), |m| *m);
        assert!(failed.is_err());
        assert_eq!(slot.with(|| Ok(7), |m| *m).unwrap(), 7);
    }

    #[test]
    fn a_model_in_use_is_not_released() {
        let (slot, loads) = counting_slot();
        let later = Instant::now() + Duration::from_secs(3600);
        let released = slot
            .with(
                || load_counted(&loads),
                |_| slot.release_if_idle(later, Duration::ZERO),
            )
            .unwrap();
        assert!(!released);
        assert_eq!(slot.with(|| load_counted(&loads), |m| *m).unwrap(), 1);
    }

    #[test]
    fn nothing_to_embed_or_rerank_loads_no_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let models = Models::new(file.path().to_path_buf());
        assert!(models.embed_passages(&[], &|_, _| {}).unwrap().is_empty());
        assert!(models.rerank("q", &[]).unwrap().is_empty());
    }

    #[test]
    fn unit_length_scales_to_norm_one_and_keeps_zeros() {
        let row = unit_length(vec![3.0, 4.0]);
        assert!(
            (row[0] - 0.6).abs() < 1e-6 && (row[1] - 0.8).abs() < 1e-6,
            "{row:?}"
        );
        let norm = unit_length(vec![1.0, 2.0, 2.0, 4.0, 0.5])
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "{norm}");
        assert_eq!(unit_length(vec![0.0, 0.0]), [0.0, 0.0]);
        assert!(unit_length(Vec::new()).is_empty());
    }

    #[test]
    fn the_models_directory_comes_from_the_environment_first() {
        let from_env = Some(OsString::from("/models"));
        let platform = Some(PathBuf::from("/home/u/.cache"));
        assert_eq!(
            models_dir(from_env, platform.clone()),
            PathBuf::from("/models")
        );
        assert_eq!(
            models_dir(Some(OsString::new()), platform.clone()),
            PathBuf::from("/home/u/.cache/ebook-hero/models")
        );
        assert_eq!(
            models_dir(None, platform),
            PathBuf::from("/home/u/.cache/ebook-hero/models")
        );
        assert_eq!(
            models_dir(None, None),
            std::env::temp_dir().join("ebook-hero").join("models")
        );
    }
}
