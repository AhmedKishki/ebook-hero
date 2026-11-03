//! The embedding and reranking models. Each loads on first use, from the
//! models directory (downloaded there if it is not), and then stays loaded.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use fastembed::{
    EmbeddingModel, RerankInitOptions, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
};

use crate::settings::{EMBED_BATCH, EMBEDDING_MODEL, QUERY_PREFIX, RERANKER_MODEL};
use crate::{Error, Result};

pub struct Models {
    cache_dir: PathBuf,
    embedder: OnceLock<Mutex<TextEmbedding>>,
    reranker: OnceLock<Mutex<TextRerank>>,
    /// Held while a model loads, so two threads never load the same one.
    loading: Mutex<()>,
}

impl Models {
    /// Models that will be loaded from, and downloaded into, `cache_dir`.
    /// Nothing is loaded until a model is first used.
    pub fn new(cache_dir: PathBuf) -> Models {
        Models {
            cache_dir,
            embedder: OnceLock::new(),
            reranker: OnceLock::new(),
            loading: Mutex::new(()),
        }
    }

    /// `$EBOOK_HERO_MODELS` if it is set, else `ebook-hero/models` in the
    /// user's cache directory.
    pub fn default_cache_dir() -> PathBuf {
        models_dir(std::env::var_os("EBOOK_HERO_MODELS"), dirs::cache_dir())
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
    /// expect on queries.
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{QUERY_PREFIX}{query}");
        let mut rows = self.embed(&[prefixed.as_str()])?;
        rows.pop()
            .ok_or_else(|| self.failed(EMBEDDING_MODEL, "it returned no embedding"))
    }

    /// How well each of `docs` answers `query`, in the order of `docs`. Scores
    /// are only comparable with each other.
    pub fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let model = self.reranker()?;
        // fastembed returns the documents best first, each with its input index.
        let ranked = locked(model)
            .rerank(query, docs, false, None)
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
        let model = self.embedder()?;
        let rows = locked(model)
            .embed(texts, Some(EMBED_BATCH))
            .map_err(|e| self.failed(EMBEDDING_MODEL, format!("inference failed: {e}")))?;
        if rows.len() == texts.len() {
            Ok(rows)
        } else {
            Err(self.failed(EMBEDDING_MODEL, "it returned a wrong number of embeddings"))
        }
    }

    fn embedder(&self) -> Result<&Mutex<TextEmbedding>> {
        self.load(&self.embedder, EMBEDDING_MODEL, |dir| {
            TextEmbedding::try_new(
                TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                    .with_cache_dir(dir.to_path_buf())
                    .with_show_download_progress(false),
            )
            .map_err(|e| e.to_string())
        })
    }

    fn reranker(&self) -> Result<&Mutex<TextRerank>> {
        self.load(&self.reranker, RERANKER_MODEL, |dir| {
            TextRerank::try_new(
                RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
                    .with_cache_dir(dir.to_path_buf())
                    .with_show_download_progress(false),
            )
            .map_err(|e| e.to_string())
        })
    }

    /// The model in `slot`, loaded by `load` if it is not there yet. A failed
    /// load is not remembered: the next call tries again, which succeeds once
    /// a missing connection is back.
    fn load<'a, T>(
        &self,
        slot: &'a OnceLock<Mutex<T>>,
        model: &str,
        load: impl FnOnce(&Path) -> std::result::Result<T, String>,
    ) -> Result<&'a Mutex<T>> {
        if let Some(loaded) = slot.get() {
            return Ok(loaded);
        }
        let _loading = locked(&self.loading);
        if let Some(loaded) = slot.get() {
            return Ok(loaded);
        }
        fs::create_dir_all(&self.cache_dir)
            .map_err(|e| self.failed(model, format!("cannot create the models directory: {e}")))?;
        let loaded = load(&self.cache_dir).map_err(|reason| self.failed(model, reason))?;
        Ok(slot.get_or_init(|| Mutex::new(loaded)))
    }

    fn failed(&self, model: &str, reason: impl Into<String>) -> Error {
        Error::ModelUnavailable {
            model: model.to_string(),
            cache: self.cache_dir.clone(),
            reason: reason.into(),
        }
    }
}

/// A model is not left half-changed by a thread that panicked while using it,
/// so a poisoned lock is still safe to take.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

    #[test]
    fn nothing_to_embed_or_rerank_loads_no_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let models = Models::new(file.path().to_path_buf());
        assert!(models.embed_passages(&[], &|_, _| {}).unwrap().is_empty());
        assert!(models.rerank("q", &[]).unwrap().is_empty());
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
