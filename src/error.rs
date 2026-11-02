use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{path}: file not found")]
    NotFound { path: PathBuf },
    #[error("{path}: not a regular file")]
    NotAFile { path: PathBuf },
    #[error("{path}: unsupported format; expected a PDF or an EPUB")]
    Unsupported { path: PathBuf },
    #[error("{path}: could not be read: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("{path}: no text layer; OCR is not supported")]
    NoTextLayer { path: PathBuf },
    #[error(
        "model {model} is unavailable ({reason}); models are cached in {cache}; use mode=lexical to search without it"
    )]
    ModelUnavailable {
        model: String,
        cache: PathBuf,
        reason: String,
    },
    #[error("index build for {sha256} is held by {}; waited {waited_secs}s", holder(.pid))]
    LockTimeout {
        sha256: String,
        pid: Option<u32>,
        waited_secs: u64,
    },
    #[error("{0}")]
    InvalidArgument(String),
    #[error("index error: {0}")]
    Index(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn holder(pid: &Option<u32>) -> String {
    match pid {
        Some(pid) => format!("process {pid}"),
        None => "another process".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("books/a.pdf")
    }

    #[test]
    fn file_errors_name_the_path_and_the_cause() {
        let path = path();
        assert_eq!(
            Error::NotFound { path: path.clone() }.to_string(),
            "books/a.pdf: file not found"
        );
        assert_eq!(
            Error::NotAFile { path: path.clone() }.to_string(),
            "books/a.pdf: not a regular file"
        );
        assert_eq!(
            Error::Unsupported { path: path.clone() }.to_string(),
            "books/a.pdf: unsupported format; expected a PDF or an EPUB"
        );
        assert_eq!(
            Error::Unreadable {
                path: path.clone(),
                reason: "password required".to_string()
            }
            .to_string(),
            "books/a.pdf: could not be read: password required"
        );
        assert_eq!(
            Error::NoTextLayer { path }.to_string(),
            "books/a.pdf: no text layer; OCR is not supported"
        );
    }

    #[test]
    fn model_unavailable_names_the_cache_and_the_lexical_remedy() {
        let error = Error::ModelUnavailable {
            model: "BAAI/bge-small-en-v1.5".to_string(),
            cache: PathBuf::from("/cache/models"),
            reason: "download failed".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "model BAAI/bge-small-en-v1.5 is unavailable (download failed); models are cached in /cache/models; use mode=lexical to search without it"
        );
    }

    #[test]
    fn lock_timeout_names_the_holder() {
        let held = Error::LockTimeout {
            sha256: "abc".to_string(),
            pid: Some(42),
            waited_secs: 900,
        };
        assert_eq!(
            held.to_string(),
            "index build for abc is held by process 42; waited 900s"
        );
        let unknown = Error::LockTimeout {
            sha256: "abc".to_string(),
            pid: None,
            waited_secs: 900,
        };
        assert_eq!(
            unknown.to_string(),
            "index build for abc is held by another process; waited 900s"
        );
    }

    #[test]
    fn io_errors_convert_with_from() {
        let error: Error = std::io::Error::other("disk full").into();
        assert_eq!(error.to_string(), "I/O error: disk full");
    }
}
