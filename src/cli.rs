//! The `ebook-hero` command line: parses the arguments, calls a [`Session`],
//! and prints the result as text for a person or, with `--json`, as the
//! structure the matching MCP tool returns.

use std::cell::Cell;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::document::markdown::document_to_markdown;
use crate::document::{Format, Warning};
use crate::extract::extract;
use crate::index::cache::{
    DOCUMENT_FILE, default_cache_root, ensure_private_dir, is_sha256_hex, sha256_file,
};
use crate::passages::Passage;
use crate::read::{BookInfo, OutlineResult, ReadAt, ReadParams, ReadResult, locator_text};
use crate::search::find::FindResult;
use crate::search::{Mode, SearchParams, SearchResult};
use crate::session::{Session, resolve_path};
use crate::settings::TOP_K_DEFAULT;
use crate::verify::{Verdict, VerifyResult};
use crate::{Error, Result};

/// Characters of context `find` shows on each side of a match.
const SNIPPET_CONTEXT: usize = 40;
/// Between the parts of a passage's place in the book: `passage 3 · p. 2 · Intro`.
const SEPARATOR: &str = " · ";

#[derive(Parser)]
#[command(
    name = "ebook-hero",
    version,
    about = "Search and read one PDF or EPUB"
)]
struct Cli {
    /// Print the result as JSON, exactly as the MCP tool of the same name returns it
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Summarize the book: title, authors, size, and how far its index is built
    Info {
        /// The PDF or EPUB
        path: String,
    },
    /// List the book's table of contents with the passage each entry starts at
    Outline {
        /// The PDF or EPUB
        path: String,
    },
    /// Find the passages that answer a query
    Search {
        /// The PDF or EPUB
        path: String,
        /// Words, "quoted phrases" or a question
        query: String,
        /// hybrid, lexical or semantic
        #[arg(long, default_value = "hybrid")]
        mode: Mode,
        /// How many passages to return
        #[arg(long, default_value_t = TOP_K_DEFAULT)]
        top_k: usize,
        /// Skip the reranking model
        #[arg(long)]
        no_rerank: bool,
    },
    /// List every passage that contains an exact phrase
    Find {
        /// The PDF or EPUB
        path: String,
        /// The phrase, matched as whole words in any case
        phrase: String,
        /// Where to continue, from the `next: --cursor` of the previous page
        #[arg(long)]
        cursor: Option<usize>,
    },
    /// Print passages as Markdown
    #[command(group(
        ArgGroup::new("address")
            .required(true)
            .multiple(false)
            .args(["passage_id", "page", "outline"])
    ))]
    Read {
        /// The PDF or EPUB
        path: String,
        /// Read this passage
        passage_id: Option<u32>,
        /// Read the passages on this PDF page
        #[arg(long)]
        page: Option<u32>,
        /// Read the passages of this outline entry
        #[arg(long)]
        outline: Option<u32>,
        /// Passages to add before the range, at most 10
        #[arg(long, default_value_t = 0)]
        before: u32,
        /// Passages to add after the range, at most 10
        #[arg(long, default_value_t = 0)]
        after: u32,
        /// The passage to continue at, from the `next: --cursor` of the previous page
        #[arg(long)]
        cursor: Option<u32>,
    },
    /// Check whether a quote occurs in the book, and where
    Verify {
        /// The PDF or EPUB
        path: String,
        /// The text to look for
        quote: String,
        /// Look in this passage first
        #[arg(long)]
        passage_id: Option<u32>,
    },
    /// Print the whole book as Markdown or as JSON
    Extract {
        /// The PDF or EPUB
        path: String,
        #[arg(long, value_enum, default_value_t = ExtractFormat::Markdown)]
        format: ExtractFormat,
    },
    /// Build the whole index of the book, including the embeddings
    Index {
        /// The PDF or EPUB
        path: String,
    },
    /// Manage the cached indexes
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExtractFormat {
    Markdown,
    Json,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// List the cached indexes with their size and book title
    List,
    /// Delete every cached index
    Clear,
}

/// What a command prints: `stdout` is its result, `stderr` a hint about it.
struct Output {
    stdout: String,
    stderr: String,
}

impl From<String> for Output {
    fn from(stdout: String) -> Output {
        Output {
            stdout,
            stderr: String::new(),
        }
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    let progress = Progress::new();
    let result = run(cli, &progress);
    progress.finish();
    match result.and_then(|output| emit(&output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    let filter =
        EnvFilter::try_from_env("EBOOK_HERO_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    // A second subscriber cannot be installed; the first one stays.
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_env_filter(filter)
        .try_init();
}

/// Writes the output. A reader that closes the pipe early, such as `head`, is
/// not an error.
fn emit(output: &Output) -> Result<()> {
    fn write(mut stream: impl io::Write, text: &str) -> Result<()> {
        match stream
            .write_all(text.as_bytes())
            .and_then(|()| stream.flush())
        {
            Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
            _ => Ok(()),
        }
    }
    write(io::stdout().lock(), &output.stdout)?;
    write(io::stderr().lock(), &output.stderr)
}

fn run(cli: Cli, progress: &Progress) -> Result<Output> {
    let json = cli.json;
    let session = match cli.command {
        Command::Cache { action } => return cache(&action).map(Output::from),
        Command::Extract { path, format } => {
            let json = json || format == ExtractFormat::Json;
            return extract_book(&path, json).map(Output::from);
        }
        _ => Session::new(),
    };
    let report = |done, total| progress.report(done, total);
    match cli.command {
        Command::Info { path } => {
            let info = session.info(&path)?;
            printed(json, &info, info_text)
        }
        Command::Outline { path } => {
            let outline = session.outline(&path)?;
            printed(json, &outline, outline_text)
        }
        Command::Search {
            path,
            query,
            mode,
            top_k,
            no_rerank,
        } => {
            let params = SearchParams {
                query,
                mode,
                top_k,
                rerank: !no_rerank,
            };
            let result = session.search(&path, params, &report)?;
            printed(json, &result, search_text)
        }
        Command::Find {
            path,
            phrase,
            cursor,
        } => {
            let result = session.find(&path, &phrase, cursor)?;
            if json {
                return to_json(&result).map(Output::from);
            }
            let book = session.book(&path)?;
            find_text(&result, &book.passages).map(Output::from)
        }
        Command::Read {
            path,
            passage_id,
            page,
            outline,
            before,
            after,
            cursor,
        } => {
            let at = match (passage_id, page, outline) {
                (Some(id), None, None) => ReadAt::Passage(id),
                (None, Some(page), None) => ReadAt::Page(page),
                (None, None, Some(id)) => ReadAt::Outline(id),
                _ => unreachable!("clap requires exactly one address"),
            };
            let params = ReadParams {
                at,
                before,
                after,
                cursor,
            };
            let result = session.read(&path, params)?;
            if json {
                return to_json(&result).map(Output::from);
            }
            Ok(read_output(result))
        }
        Command::Verify {
            path,
            quote,
            passage_id,
        } => {
            let result = session.verify(&path, &quote, passage_id)?;
            printed(json, &result, verify_text)
        }
        Command::Index { path } => {
            let info = session.index(&path, &report)?;
            printed(json, &info, info_text)
        }
        Command::Cache { .. } | Command::Extract { .. } => unreachable!("handled above"),
    }
}

/// `value` as JSON, or as the text `human` makes of it.
fn printed<T: Serialize>(json: bool, value: &T, human: fn(&T) -> String) -> Result<Output> {
    let text = if json { to_json(value)? } else { human(value) };
    Ok(text.into())
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value).map_err(io::Error::from)?;
    text.push('\n');
    Ok(text)
}

/// Embedding progress on stderr: one line per update, or on a terminal one line
/// that is overwritten.
struct Progress {
    terminal: bool,
    /// Whether a terminal line was started and not yet ended.
    open: Cell<bool>,
}

impl Progress {
    fn new() -> Progress {
        Progress {
            terminal: io::stderr().is_terminal(),
            open: Cell::new(false),
        }
    }

    fn report(&self, done: usize, total: usize) {
        if self.terminal {
            eprint!("\rembedding {done}/{total}");
            self.open.set(true);
        } else {
            eprintln!("embedding {done}/{total}");
        }
    }

    /// Ends the line being overwritten, so that what follows starts on its own.
    fn finish(&self) {
        if self.open.replace(false) {
            eprintln!();
        }
    }
}

fn extract_book(path: &str, json: bool) -> Result<String> {
    let path = resolve_path(path)?;
    if !path.is_file() {
        return Err(Error::NotAFile { path });
    }
    let document = extract(&path, &sha256_file(&path)?)?;
    if json {
        to_json(&document)
    } else {
        Ok(document_to_markdown(&document))
    }
}

fn read_output(result: ReadResult) -> Output {
    Output {
        stderr: result
            .next_cursor
            .map(|cursor| format!("next: --cursor {cursor}\n"))
            .unwrap_or_default(),
        stdout: result.markdown,
    }
}

fn info_text(info: &BookInfo) -> String {
    let authors = if info.authors.is_empty() {
        "unknown".to_string()
    } else {
        info.authors.join("; ")
    };
    let format = match info.format {
        Format::Pdf => "PDF",
        Format::Epub => "EPUB",
    };
    let mut out = String::new();
    let mut field = |name: &str, value: &dyn std::fmt::Display| {
        let _ = writeln!(out, "{name}: {value}");
    };
    field("Title", &info.title.as_deref().unwrap_or("(untitled)"));
    field("Authors", &authors);
    field("Language", &info.language.as_deref().unwrap_or("unknown"));
    field("Format", &format);
    match (info.page_count, info.spine_count) {
        (Some(pages), _) => field("Pages", &pages),
        (None, Some(chapters)) => field("Chapters", &chapters),
        (None, None) => {}
    }
    field("Passages", &info.passage_count);
    field(
        "Semantic index",
        &if info.semantic_ready {
            "ready"
        } else {
            "not built"
        },
    );
    for warning in &info.warnings {
        let _ = writeln!(out, "Warning: {}", warning_text(warning));
    }
    out
}

fn warning_text(warning: &Warning) -> String {
    match warning {
        Warning::PagesWithoutText { pages } => {
            let pages: Vec<String> = pages.iter().map(u32::to_string).collect();
            format!("no text on pages {}", pages.join(", "))
        }
        Warning::UnreadableContent { href, reason } => format!("could not read {href}: {reason}"),
        Warning::UnreadablePart { part, reason } => format!("could not read {part}: {reason}"),
    }
}

fn outline_text(outline: &OutlineResult) -> String {
    let mut out = String::new();
    for entry in &outline.entries {
        let indent = "  ".repeat(usize::from(entry.level.saturating_sub(1)));
        let place = match (entry.passage_id, &entry.locator) {
            (Some(id), Some(locator)) => format!("passage {id}, {}", locator_text(locator)),
            _ => "no text".to_string(),
        };
        let _ = writeln!(out, "{indent}[{}] {} — {place}", entry.id, entry.title);
    }
    out
}

fn search_text(result: &SearchResult) -> String {
    if result.hits.is_empty() {
        return "no results\n".to_string();
    }
    let mut out = String::new();
    for (i, hit) in result.hits.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{}. passage {}{SEPARATOR}{}",
            i + 1,
            hit.passage_id,
            locator_text(&hit.locator)
        );
        if !hit.section.is_empty() {
            let _ = write!(out, "{SEPARATOR}{}", hit.section.join(" › "));
        }
        let _ = writeln!(out, "  (score {:.3})", hit.score);
        for line in highlighted(&hit.text, &hit.matches).lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    out
}

/// `text` with the code-point ranges `matches` wrapped in `«` and `»`.
fn highlighted(text: &str, matches: &[[usize; 2]]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut ranges: Vec<[usize; 2]> = matches
        .iter()
        .map(|&[start, end]| [start.min(chars.len()), end.min(chars.len())])
        .filter(|[start, end]| start < end)
        .collect();
    ranges.sort_unstable();
    let mut out = String::with_capacity(text.len() + 2 * ranges.len());
    let mut at = 0;
    for [start, end] in ranges {
        if end <= at {
            continue;
        }
        let start = start.max(at);
        out.extend(&chars[at..start]);
        out.push('«');
        out.extend(&chars[start..end]);
        out.push('»');
        at = end;
    }
    out.extend(&chars[at..]);
    out
}

fn find_text(result: &FindResult, passages: &[Passage]) -> Result<String> {
    let mut out = format!(
        "{} occurrences in {} passages\n",
        result.total_occurrences, result.total_passages
    );
    for hit in &result.occurrences {
        let passage = passages.get(hit.passage_id as usize).ok_or_else(|| {
            Error::Index(format!(
                "passage {} is in the index but not in the book; the cache is inconsistent",
                hit.passage_id
            ))
        })?;
        let chars: Vec<char> = passage.text.chars().collect();
        let flat = |range: &[char]| -> String {
            let mut text = String::with_capacity(range.len());
            for &c in range {
                if !c.is_whitespace() {
                    text.push(c);
                } else if !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            text
        };
        for &[start, end] in &hit.matches {
            let end = end.min(chars.len());
            let start = start.min(end);
            let _ = writeln!(
                out,
                "passage {}{SEPARATOR}{}: …{}«{}»{}…",
                hit.passage_id,
                locator_text(&hit.locator),
                flat(&chars[start.saturating_sub(SNIPPET_CONTEXT)..start]),
                flat(&chars[start..end]),
                flat(&chars[end..(end + SNIPPET_CONTEXT).min(chars.len())]),
            );
        }
    }
    if let Some(cursor) = result.next_cursor {
        let _ = writeln!(out, "next: --cursor {cursor}");
    }
    Ok(out)
}

fn verdict_word(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Exact => "exact",
        Verdict::Equivalent => "equivalent",
        Verdict::None => "none",
    }
}

fn verify_text(result: &VerifyResult) -> String {
    let mut out = verdict_word(result.verdict).to_string();
    if result.verdict == Verdict::None {
        match &result.closest {
            Some(closest) => {
                let _ = write!(
                    out,
                    "; closest passage {}{SEPARATOR}{} ({} of {} words",
                    closest.passage_id,
                    locator_text(&closest.locator),
                    closest.matched_words,
                    closest.quote_words
                );
                if let Some(word) = &closest.first_difference {
                    let _ = write!(out, "; differs at \"{word}\"");
                }
                out.push(')');
            }
            None if result.found_elsewhere.is_none() => {
                out.push_str("; no passage shares a word with the quote");
            }
            None => {}
        }
    } else if let (Some(id), Some(locator)) = (result.passage_id, &result.locator) {
        let _ = write!(out, ": passage {id}{SEPARATOR}{}", locator_text(locator));
    }
    out.push('\n');
    if let Some(elsewhere) = &result.found_elsewhere {
        let _ = writeln!(
            out,
            "found in passage {}{SEPARATOR}{}",
            elsewhere.passage_id,
            locator_text(&elsewhere.locator)
        );
    }
    out
}

fn cache(action: &CacheCommand) -> Result<String> {
    let root = default_cache_root();
    ensure_private_dir(&root)?;
    match action {
        CacheCommand::List => cache_list(&root),
        CacheCommand::Clear => cache_clear(&root),
    }
}

/// The part of a cache entry's name that tells what it is.
enum CacheEntry {
    /// `<sha>`, a directory.
    Index,
    /// `<sha>.lock`, or `<sha>.staging-<pid>-<ns>`, a build in progress.
    Leftover,
}

/// What the cache root's entry called `name` is, or `None` for a file that
/// this program did not put there.
fn classify(name: &str, is_dir: bool) -> Option<CacheEntry> {
    if is_sha256_hex(name) {
        return is_dir.then_some(CacheEntry::Index);
    }
    let (sha, rest) = name.split_once('.')?;
    (is_sha256_hex(sha) && (rest == "lock" || rest.starts_with("staging-")))
        .then_some(CacheEntry::Leftover)
}

fn cache_list(root: &Path) -> Result<String> {
    let mut books = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(sha) = name.to_str() else { continue };
        if let Some(CacheEntry::Index) = classify(sha, entry.file_type()?.is_dir()) {
            books.push((sha.to_string(), entry.path()));
        }
    }
    books.sort();
    let mut out = String::new();
    for (sha, dir) in books {
        let megabytes = tree_size(&dir)? as f64 / 1_000_000.0;
        let _ = writeln!(out, "{sha} {megabytes:.1} MB {}", cached_title(&dir));
    }
    Ok(out)
}

/// The bytes in all the files under `dir`.
fn tree_size(dir: &Path) -> Result<u64> {
    let mut bytes = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        bytes += if metadata.is_dir() {
            tree_size(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(bytes)
}

/// The title the cached document says, on one line, or `(untitled)`.
fn cached_title(dir: &Path) -> String {
    let title = fs::read(dir.join(DOCUMENT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|document| document["metadata"]["title"].as_str().map(str::to_string))
        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|title| !title.is_empty());
    title.unwrap_or_else(|| "(untitled)".to_string())
}

fn cache_clear(root: &Path) -> Result<String> {
    let mut removed = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let is_dir = entry.file_type()?.is_dir();
        let Some(kind) = name.to_str().and_then(|name| classify(name, is_dir)) else {
            continue;
        };
        tracing::debug!(path = %entry.path().display(), "removing cache entry");
        let gone = if is_dir {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        match gone {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        if matches!(kind, CacheEntry::Index) {
            removed += 1;
        }
    }
    Ok(format!("removed {removed} indexes\n"))
}
