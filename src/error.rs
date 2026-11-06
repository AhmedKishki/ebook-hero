//! The one error type of the library. Each message names its cause and,
//! where there is one, a remedy; the command line prints it after `error: `
//! and the MCP server returns it as a tool result with `isError` set.

use std::path::PathBuf;

use thiserror::Error;

/// The result of every fallible operation in the library.
pub type Result<T> = std::result::Result<T, Error>;

/// Why an operation failed.
#[derive(Debug, Error)]
pub enum Error {
    /// The path, or a directory on the way to it, does not exist.
    #[error("{path}: file not found")]
    NotFound {
        /// The path as it was resolved.
        path: PathBuf,
    },
    /// The path exists but is a directory or another kind of file that is not
    /// a regular file.
    #[error("{path}: not a regular file")]
    NotAFile {
        /// The path as it was resolved.
        path: PathBuf,
    },
    /// The file is neither a PDF, an EPUB nor UTF-8 text, judged from its
    /// content and not its name.
    #[error("{path}: unsupported format; expected a PDF, an EPUB or a UTF-8 text file")]
    Unsupported {
        /// The path as it was resolved.
        path: PathBuf,
    },
    /// The file could not be opened or parsed, such as a PDF that needs a
    /// password, an EPUB with no readable text or a Markdown file over one of
    /// its caps.
    #[error("{path}: could not be read: {reason}")]
    Unreadable {
        /// The path as it was resolved.
        path: PathBuf,
        /// What went wrong, from the operating system or the reader.
        reason: String,
    },
    /// The PDF has no text on any page, as a scan without OCR has none.
    #[error("{path}: no text layer; OCR is not supported")]
    NoTextLayer {
        /// The path as it was resolved.
        path: PathBuf,
    },
    /// The embedding or reranking model could not be downloaded, loaded or
    /// run. Lexical search needs neither model.
    #[error(
        "model {model} is unavailable ({reason}); models are cached in {cache}; use mode=lexical to search without it"
    )]
    ModelUnavailable {
        /// The model's name, such as `BAAI/bge-small-en-v1.5`.
        model: String,
        /// The models directory it was looked for in.
        cache: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// The call was cancelled before it finished. Nothing was left half
    /// written; call again to retry.
    #[error("cancelled before it finished; call again to retry")]
    Cancelled,
    /// Another process held the book's build lock for longer than
    /// [`LOCK_WAIT`](crate::settings::LOCK_WAIT).
    #[error("index build for {sha256} is held by {}; waited {waited_secs}s", holder(.pid))]
    LockTimeout {
        /// The SHA-256 of the book whose lock was busy.
        sha256: String,
        /// The process that holds the lock, if it wrote its id into the lock
        /// file.
        pid: Option<u32>,
        /// How long this process waited, in whole seconds.
        waited_secs: u64,
    },
    /// An argument is out of range or names something the book does not
    /// have; the message says which and what is allowed.
    #[error("{0}")]
    InvalidArgument(String),
    /// The cache could not be used or written: a root that is not private, a
    /// file that cannot be written, or an index that does not match its book.
    #[error("index error: {0}")]
    Index(String),
    /// Any other input or output error.
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
            "books/a.pdf: unsupported format; expected a PDF, an EPUB or a UTF-8 text file"
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
