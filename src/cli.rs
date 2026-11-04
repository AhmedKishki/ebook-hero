//! The `ebook-hero` command line: parses the arguments, calls a [`Session`],
//! and prints the result as text for a person or, with `--json`, as the
//! structure the matching MCP tool returns.

use std::cell::Cell;
use std::fmt::Write as _;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::document::markdown::document_to_markdown;
use crate::document::{Format, Warning};
use crate::extract::extract;
use crate::graph::{Concept, GraphResult};
use crate::index::cache::{
    CachedIndex, ClearReport, clear, default_cache_root, list_indexes, sha256_file,
};
use crate::passages::{Passage, PassageLocator};
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
    /// Print the result as JSON. For info, outline, search, find, read, verify and graph, it is what the MCP tools book_info, outline, search, find, read, verify_quote and concept_graph return
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
        /// Read the passages on this PDF page: the physical page, counted from 1 (the printed page label is shown in the locator)
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
    /// List the book's concepts, how they link and the communities they form
    Graph {
        /// The PDF or EPUB
        path: String,
        /// With --json, list every passage of each concept and link, not only the first ten
        #[arg(long)]
        full: bool,
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
    /// Serve the tools to an MCP client over standard input and output
    Mcp,
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

/// The environment variable that sets which log lines are shown.
const LOG_VARIABLE: &str = "EBOOK_HERO_LOG";
/// Errors from anywhere, warnings from this program: a dependency's warning is
/// not something the person running it can act on.
const DEFAULT_LOG_FILTER: &str = "error,ebook_hero=warn";

fn init_logging() {
    let filter = EnvFilter::try_from_env(LOG_VARIABLE).unwrap_or_else(|error| {
        // A variable that is not set is the normal case; one that is set and
        // does not parse is a mistake the person should hear about.
        if std::env::var_os(LOG_VARIABLE).is_some() {
            eprintln!("warning: {LOG_VARIABLE} ignored: {error}");
        }
        EnvFilter::new(DEFAULT_LOG_FILTER)
    });
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
        Command::Mcp => return serve_mcp().map(Output::from),
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
        Command::Graph { path, full } => {
            let graph = session.graph(&path, full)?;
            printed(json, &graph, graph_text)
        }
        Command::Index { path } => {
            let info = session.index(&path, &report)?;
            printed(json, &info, info_text)
        }
        Command::Cache { .. } | Command::Extract { .. } | Command::Mcp => {
            unreachable!("handled above")
        }
    }
}

/// Runs the MCP server until the client disconnects. It prints nothing: its
/// standard output belongs to the protocol.
fn serve_mcp() -> Result<String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(crate::mcp::serve())
        .map_err(io::Error::other)?;
    Ok(String::new())
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

/// The summary line, then each community: its concepts by passage count, and
/// one line per concept with its passage count, first passage and linked
/// concepts by NPMI, highest first.
fn graph_text(graph: &GraphResult) -> String {
    let mut out = format!(
        "{} in {} · {} · {}\n",
        counted(graph.concepts.len(), "concept"),
        counted(graph.communities.len(), "community"),
        counted(graph.passages as usize, "passage"),
        counted(graph.topics as usize, "topic"),
    );
    if graph.concepts.is_empty() {
        out.push_str(
            "\nno phrase occurs in at least 3 passages and in at most 30% of them, so the book has no concepts\n",
        );
        return out;
    }
    let mut partners: Vec<Vec<(f32, u32)>> = vec![Vec::new(); graph.concepts.len()];
    for link in &graph.links {
        partners[link.a as usize].push((link.npmi, link.b));
        partners[link.b as usize].push((link.npmi, link.a));
    }
    for list in &mut partners {
        list.sort_by(|(x, i), (y, j)| y.total_cmp(x).then(i.cmp(j)));
    }
    for community in &graph.communities {
        let mut members: Vec<&Concept> = community
            .concepts
            .iter()
            .map(|&id| &graph.concepts[id as usize])
            .collect();
        members.sort_by(|x, y| y.passage_count.cmp(&x.passage_count).then(x.id.cmp(&y.id)));
        let mut names: Vec<&str> = members.iter().take(5).map(|c| c.label.as_str()).collect();
        if members.len() > 5 {
            names.push("…");
        }
        let _ = writeln!(
            out,
            "\nCommunity {}: {}",
            community.id + 1,
            names.join(", ")
        );
        let width = members
            .iter()
            .map(|c| c.label.chars().count())
            .max()
            .unwrap_or(0);
        let digits = members
            .iter()
            .map(|c| c.passage_count.to_string().len())
            .max()
            .unwrap_or(1);
        let first_digits = members
            .iter()
            .map(|c| c.passages[0].to_string().len())
            .max()
            .unwrap_or(1);
        for concept in members {
            let links: Vec<String> = partners[concept.id as usize]
                .iter()
                .map(|(npmi, other)| format!("{} {npmi:.2}", graph.concepts[*other as usize].label))
                .collect();
            let links = if links.is_empty() {
                "no links".to_string()
            } else {
                links.join(", ")
            };
            let pad = width - concept.label.chars().count();
            let _ = writeln!(
                out,
                "  {}{}  {:>digits$} passages, first {:<first_digits$}   {links}",
                concept.label,
                " ".repeat(pad),
                concept.passage_count,
                concept.passages[0],
            );
        }
    }
    out
}

/// `n` and `noun`, with an s unless `n` is 1 (and "communities" for many).
fn counted(n: usize, noun: &str) -> String {
    match (n, noun) {
        (1, _) => format!("1 {noun}"),
        (_, "community") => format!("{n} communities"),
        _ => format!("{n} {noun}s"),
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
        let _ = writeln!(
            out,
            "{}. passage {}{SEPARATOR}{}  (score {:.3})",
            i + 1,
            hit.passage_id,
            hit_place(&hit.section, &hit.locator),
            hit.score
        );
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

/// Where a search hit is: its locator, then its section path. An EPUB locator
/// that already names the nearest heading takes the rest of the path in front
/// of it instead, so the heading is printed once.
fn hit_place(section: &[String], locator: &PassageLocator) -> String {
    let place = locator_text(locator);
    if let (
        PassageLocator::Epub {
            under_heading: Some(heading),
            ..
        },
        Some((last, parents)),
    ) = (locator, section.split_last())
        && *last == heading.text
    {
        return parents
            .iter()
            .map(|parent| format!("{parent} › "))
            .chain([place])
            .collect();
    }
    if section.is_empty() {
        place
    } else {
        format!("{place}{SEPARATOR}{}", section.join(" › "))
    }
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
    match action {
        CacheCommand::List => Ok(indexes_text(&list_indexes(&root)?)),
        CacheCommand::Clear => Ok(clear_text(&clear(&root)?)),
    }
}

fn indexes_text(indexes: &[CachedIndex]) -> String {
    let mut out = String::new();
    for index in indexes {
        // A title is one line, whatever whitespace the book put in it.
        let title = index
            .title
            .as_deref()
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "(untitled)".to_string());
        let megabytes = index.size_bytes as f64 / 1_000_000.0;
        let _ = writeln!(out, "{} {megabytes:.1} MB {title}", index.sha256);
    }
    out
}

fn clear_text(report: &ClearReport) -> String {
    let mut out = format!("removed {} indexes", report.removed);
    if report.skipped_busy > 0 {
        let _ = write!(out, " ({} skipped: in use)", report.skipped_busy);
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_graph_says_why_it_has_no_concepts() {
        let graph = GraphResult {
            passages: 25,
            topics: 5,
            concepts: Vec::new(),
            links: Vec::new(),
            communities: Vec::new(),
        };
        let text = graph_text(&graph);
        assert!(
            text.contains("no phrase occurs in at least 3 passages and in at most 30% of them"),
            "{text}"
        );
    }

    use super::*;
    use crate::passages::HeadingPlace;

    fn section(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn epub(under_heading: Option<&str>) -> PassageLocator {
        PassageLocator::Epub {
            href: "ch8.xhtml".to_string(),
            para_start: 11,
            para_end: 12,
            print_page: None,
            under_heading: under_heading.map(|text| HeadingPlace {
                text: text.to_string(),
                para_start: 10,
                para_end: 11,
            }),
        }
    }

    #[test]
    fn an_epub_hit_names_its_heading_once() {
        let place = hit_place(
            &section(&["Alice", "CHAPTER VIII"]),
            &epub(Some("CHAPTER VIII")),
        );
        assert_eq!(place, "Alice › CHAPTER VIII ¶10–11");
    }

    #[test]
    fn a_hit_without_a_heading_place_adds_its_section_path() {
        let pdf = PassageLocator::Pdf {
            page_start: 12,
            page_end: 12,
            label_start: Some("xii".to_string()),
            label_end: None,
        };
        assert_eq!(
            hit_place(&section(&["Part 1", "Rules"]), &pdf),
            "p. 12 (xii) · Part 1 › Rules"
        );
        assert_eq!(hit_place(&[], &epub(None)), "ch8.xhtml ¶11–12");
    }
}
