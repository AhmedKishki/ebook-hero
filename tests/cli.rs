mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_cmd::assert::Assert;
use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::document::Document;
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::Session;
use ebook_hero::settings::TOP_K_DEFAULT;
use serde_json::Value;
use tempfile::TempDir;

const SAMPLE_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf");
const IMAGE_ONLY_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/imageonly.pdf");

/// The directories every command runs with: `TMPDIR`, which holds the index
/// cache, and the models directory. Both are private and empty, and no test
/// that uses them loads a model.
struct Env {
    tmp: TempDir,
    models: TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            tmp: private_tempdir(),
            models: private_tempdir(),
        }
    }

    fn command(&self) -> Command {
        let mut command = assert_cmd::cargo::cargo_bin_cmd!("ebook-hero");
        command
            .env("TMPDIR", self.tmp.path())
            .env("EBOOK_HERO_MODELS", self.models.path())
            .env_remove("EBOOK_HERO_LOG");
        command
    }

    fn run(&self, args: &[&str]) -> Assert {
        self.command().args(args).assert()
    }

    /// The stdout of a command that must succeed.
    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args).success().get_output().clone();
        String::from_utf8(output.stdout).unwrap()
    }

    fn cache_root(&self) -> PathBuf {
        self.tmp.path().join("ebook-hero")
    }
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// An EPUB titled "Test Book" with the given chapter bodies, each under its
/// own heading and table-of-contents entry, in `dir`.
fn epub(dir: &Path, paragraphs: &[&str]) -> String {
    let mut builder = EpubBuilder::new(EpubVersion::V3)
        .title("Test Book")
        .author("Ada Example");
    for (i, text) in paragraphs.iter().enumerate() {
        let href = format!("ch{i}.xhtml");
        builder = builder
            .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
            .toc(&format!("Part {i}"), &href, 1);
    }
    let path = dir.join("book.epub");
    builder.write(&path);
    path.to_str().unwrap().to_string()
}

#[test]
fn info_json_has_the_title() {
    let env = Env::new();

    let info = json(&env.stdout(&["info", SAMPLE_PDF, "--json"]));

    assert_eq!(info["title"], "Sample Book");
    assert_eq!(info["format"], "pdf");
    assert_eq!(info["page_count"], 5);
    assert_eq!(info["semantic_ready"], false);
}

#[test]
fn info_prints_one_line_per_field() {
    let env = Env::new();

    let out = env.stdout(&["info", SAMPLE_PDF]);

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "Title: Sample Book", "{out}");
    assert!(lines.contains(&"Authors: Ada Example"), "{out}");
    assert!(lines.contains(&"Format: PDF"), "{out}");
    assert!(lines.contains(&"Pages: 5"), "{out}");
    assert!(lines.iter().any(|l| l.starts_with("Passages: ")), "{out}");
    assert!(lines.contains(&"Semantic index: not built"), "{out}");
}

#[test]
fn info_counts_the_chapters_of_an_epub() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    let book = epub(books.path(), &["One.", "Two."]);

    let out = env.stdout(&["info", &book]);

    assert!(out.contains("Format: EPUB\n"), "{out}");
    assert!(out.contains("Chapters: 2\n"), "{out}");
    assert!(!out.contains("Pages:"), "{out}");
}

#[test]
fn outline_indents_two_spaces_per_level_below_one() {
    let env = Env::new();

    let out = env.stdout(&["outline", SAMPLE_PDF]);

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].starts_with("[0] Entropy — passage "), "{out}");
    assert!(
        lines[1].starts_with("  [1] Joint entropy — passage "),
        "{out}"
    );
    assert!(lines[2].starts_with("[2] Channels — passage "), "{out}");
    assert!(lines[0].contains(", p. 2 (1)"), "{out}");

    let entries = json(&env.stdout(&["outline", SAMPLE_PDF, "--json"]));
    assert_eq!(entries["entries"].as_array().unwrap().len(), 3);
}

#[test]
fn lexical_search_prints_hits_and_json() {
    let env = Env::new();

    let out = env.stdout(&["search", SAMPLE_PDF, "entropy", "--mode", "lexical"]);

    assert!(out.starts_with("1. passage "), "{out}");
    assert!(out.contains("«") && out.contains("»"), "{out}");
    assert!(out.contains("  (score "), "{out}");
    assert!(
        out.lines()
            .filter(|l| l.starts_with("    "))
            .all(|l| !l.starts_with("     ")),
        "text is indented four spaces: {out}"
    );

    let result = json(&env.stdout(&[
        "search", SAMPLE_PDF, "entropy", "--mode", "lexical", "--json",
    ]));
    assert!(result["hits"][0]["passage_id"].is_u64(), "{result}");
    assert_eq!(result["mode"], "lexical");
}

#[test]
fn top_k_limits_the_hits() {
    let env = Env::new();

    let result = json(&env.stdout(&[
        "search", SAMPLE_PDF, "entropy", "--mode", "lexical", "--top-k", "2", "--json",
    ]));

    assert_eq!(result["hits"].as_array().unwrap().len(), 2);
}

#[test]
fn json_equals_the_library_result() {
    let env = Env::new();
    let cli = json(&env.stdout(&[
        "search", SAMPLE_PDF, "entropy", "--mode", "lexical", "--json",
    ]));

    let cache = private_tempdir();
    let session = Session::with_dirs(cache.path().to_path_buf(), env.models.path().to_path_buf());
    let params = SearchParams {
        query: "entropy".to_string(),
        mode: Mode::Lexical,
        top_k: TOP_K_DEFAULT,
        rerank: true,
    };
    let result = session.search(SAMPLE_PDF, params, &|_, _| {}).unwrap();

    // Through text, as the CLI prints it: a `Value` made directly from an
    // `f32` score differs from one parsed from its shortest decimal form.
    let library = json(&serde_json::to_string_pretty(&result).unwrap());
    assert_eq!(cli, library);
}

#[test]
fn json_may_come_before_the_subcommand() {
    let env = Env::new();

    let before = json(&env.stdout(&["--json", "info", SAMPLE_PDF]));
    let after = json(&env.stdout(&["info", SAMPLE_PDF, "--json"]));

    assert_eq!(before, after);
}

#[test]
fn find_prints_the_totals_and_a_snippet_per_occurrence() {
    let env = Env::new();

    let out = env.stdout(&["find", SAMPLE_PDF, "joint entropy"]);

    let mut lines = out.lines();
    let first = lines.next().unwrap();
    let (occurrences, passages) = first
        .strip_suffix(" passages")
        .and_then(|rest| rest.split_once(" occurrences in "))
        .unwrap_or_else(|| panic!("{first}"));
    let occurrences: usize = occurrences.parse().unwrap();
    assert!(occurrences >= 2, "{out}");
    assert!(passages.parse::<usize>().unwrap() >= 1, "{out}");
    let snippets: Vec<&str> = lines.collect();
    assert_eq!(snippets.len(), occurrences, "{out}");
    for snippet in snippets {
        assert!(snippet.starts_with("passage "), "{snippet}");
        assert!(snippet.contains(": …"), "{snippet}");
        assert!(snippet.ends_with('…'), "{snippet}");
        assert!(
            snippet.to_lowercase().contains("«joint entropy»"),
            "{snippet}"
        );
        assert!(!snippet.contains('\n'));
    }
}

#[test]
fn find_json_and_cursor_continue_a_long_list() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    // Twenty-five chapters make twenty-five passages, and a page holds twenty.
    let chapters = vec!["The needle is here."; 25];
    let book = epub(books.path(), &chapters);

    let out = env.stdout(&["find", &book, "needle"]);
    assert!(out.starts_with("25 occurrences in 25 passages\n"), "{out}");
    assert!(out.ends_with("next: --cursor 20\n"), "{out}");

    let page = json(&env.stdout(&["find", &book, "needle", "--cursor", "20", "--json"]));
    assert_eq!(page["occurrences"].as_array().unwrap().len(), 5);
    assert!(page["next_cursor"].is_null());
    let out = env.stdout(&["find", &book, "needle", "--cursor", "20"]);
    assert!(!out.contains("next:"), "{out}");
}

#[test]
fn read_by_page_on_pdf() {
    let env = Env::new();

    let out = env.run(&["read", SAMPLE_PDF, "--page", "2"]);

    let output = out.success().get_output().clone();
    let markdown = String::from_utf8(output.stdout).unwrap();
    assert!(markdown.contains("<!-- passage "), "{markdown}");
    assert!(markdown.contains(" · p. 2 (1) -->"), "{markdown}");
    assert!(markdown.contains("Entropy measures"), "{markdown}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("next:"));

    let result = json(&env.stdout(&["read", SAMPLE_PDF, "--page", "2", "--json"]));
    assert_eq!(result["markdown"], markdown);
    assert!(result["next_cursor"].is_null());
}

#[test]
fn read_by_passage_and_outline() {
    let env = Env::new();

    let passage = env.stdout(&["read", SAMPLE_PDF, "0"]);
    assert!(passage.contains("<!-- passage 0 · "), "{passage}");
    assert!(!passage.contains("<!-- passage 1 · "), "{passage}");

    let widened = env.stdout(&["read", SAMPLE_PDF, "0", "--after", "1", "--before", "1"]);
    assert!(widened.contains("<!-- passage 1 · "), "{widened}");

    let entry = env.stdout(&["read", SAMPLE_PDF, "--outline", "2"]);
    assert!(entry.contains("## Channels"), "{entry}");
}

#[test]
fn read_needs_exactly_one_address() {
    let env = Env::new();

    for args in [
        &["read", SAMPLE_PDF][..],
        &["read", SAMPLE_PDF, "0", "--page", "2"],
        &["read", SAMPLE_PDF, "--page", "2", "--outline", "1"],
    ] {
        let assert = env.run(args).code(2);
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
        assert!(stderr.contains("Usage:"), "{stderr}");
        assert!(assert.get_output().stdout.is_empty());
    }
}

#[test]
fn read_prints_the_next_cursor_on_stderr() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    let paragraph = "word ".repeat(200);
    let book = {
        let body: String = (0..12).map(|i| format!("<p>{i} {paragraph}</p>")).collect();
        let path = books.path().join("long.epub");
        EpubBuilder::new(EpubVersion::V3)
            .chapter("ch0.xhtml", &format!("<h1>Long</h1>{body}"))
            .write(&path);
        path.to_str().unwrap().to_string()
    };

    let first = env.run(&["read", &book, "0", "--after", "10"]).success();
    let output = first.get_output();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    let cursor = stderr
        .trim_end()
        .strip_prefix("next: --cursor ")
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("next:"));

    let rest = env
        .run(&["read", &book, "0", "--after", "10", "--cursor", cursor])
        .success();
    let markdown = String::from_utf8_lossy(&rest.get_output().stdout).into_owned();
    assert!(
        markdown.contains(&format!("<!-- passage {cursor} · ")),
        "{markdown}"
    );
}

#[test]
fn verify_prints_the_verdict() {
    let env = Env::new();

    let exact = env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "Entropy measures how much surprise a source of messages carries",
    ]);
    assert!(
        exact.starts_with("exact: passage 1 · p. 2 (1)\n"),
        "{exact}"
    );

    let equivalent = env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "ENTROPY measures how much surprise, a source of messages carries",
    ]);
    assert!(
        equivalent.starts_with("equivalent: passage 1 · p. 2 (1)\n"),
        "{equivalent}"
    );

    let none = env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "Entropy measures how much surprise a source of messages delivers",
    ]);
    assert!(
        none.starts_with(
            "none; closest passage 1 · p. 2 (1) (9 of 10 words; differs at \"delivers\")\n"
        ),
        "{none}"
    );

    let result = json(&env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "Entropy measures how much surprise a source of messages carries",
        "--json",
    ]));
    assert_eq!(result["verdict"], "exact");
    assert_eq!(result["passage_id"], 1);
}

#[test]
fn verify_names_where_a_quote_was_found_elsewhere() {
    let env = Env::new();

    let out = env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "Entropy measures how much surprise a source of messages carries",
        "--passage-id",
        "3",
    ]);

    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("none"), "{out}");
    assert!(lines.contains(&"found in passage 1 · p. 2 (1)"), "{out}");
}

#[test]
fn extract_markdown_and_json() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    let book = epub(books.path(), &["Entropy is surprise."]);

    let markdown = env.stdout(&["extract", &book, "--format", "markdown"]);
    assert!(markdown.contains("# Test Book"), "{markdown}");
    assert!(markdown.contains("Entropy is surprise."), "{markdown}");
    assert_eq!(markdown, env.stdout(&["extract", &book]));

    let text = env.stdout(&["extract", &book, "--format", "json"]);
    let document: Document = serde_json::from_str(&text).unwrap();
    assert_eq!(document.metadata.title.as_deref(), Some("Test Book"));
    assert_eq!(document.source.file_name, "book.epub");
    assert_eq!(text, env.stdout(&["--json", "extract", &book]));
}

#[test]
fn cache_list_and_clear() {
    let env = Env::new();
    assert_eq!(env.stdout(&["cache", "list"]), "");
    assert_eq!(env.stdout(&["cache", "clear"]), "removed 0 indexes\n");

    env.stdout(&["info", SAMPLE_PDF]);
    let sha = {
        let mut names: Vec<String> = fs::read_dir(env.cache_root())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.len() == 64)
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        names.remove(0)
    };

    let listing = env.stdout(&["cache", "list"]);
    let line = listing.trim_end();
    assert_eq!(listing.lines().count(), 1, "{listing}");
    let rest = line.strip_prefix(&format!("{sha} ")).unwrap();
    let (size, title) = rest.split_once(" MB ").unwrap();
    assert!(size.parse::<f64>().is_ok(), "{line}");
    assert_eq!(title, "Sample Book");

    // Leftovers of a build are removed with the index; foreign files are not.
    fs::write(env.cache_root().join(format!("{sha}.lock")), "123").unwrap();
    fs::create_dir(env.cache_root().join(format!("{sha}.staging-1-2"))).unwrap();
    fs::write(env.cache_root().join("notes.txt"), "keep").unwrap();

    assert_eq!(env.stdout(&["cache", "clear"]), "removed 1 indexes\n");
    let left: Vec<String> = fs::read_dir(env.cache_root())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left, ["notes.txt"]);
    assert_eq!(env.stdout(&["cache", "list"]), "");
}

#[test]
fn missing_file_exits_1_with_cause() {
    let env = Env::new();

    let assert = env.run(&["info", "/no/such/book.pdf"]).code(1);

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("file not found"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn image_only_pdf_exits_1_with_ocr_message() {
    let env = Env::new();

    let assert = env.run(&["info", IMAGE_ONLY_PDF]).code(1);

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("no text layer; OCR is not supported"),
        "{stderr}"
    );
}

#[test]
fn bad_arguments_are_usage_errors() {
    let env = Env::new();

    env.run(&["search", SAMPLE_PDF, "entropy", "--mode", "fuzzy"])
        .code(2);
    env.run(&["bogus"]).code(2);
    env.run(&[]).code(2);
}

#[test]
fn library_errors_for_bad_arguments_exit_1() {
    let env = Env::new();

    let assert = env
        .run(&["search", SAMPLE_PDF, "entropy", "--top-k", "0"])
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.starts_with("error: "), "{stderr}");
}

#[test]
fn help_lists_every_subcommand() {
    let env = Env::new();

    let out = env.stdout(&["--help"]);

    for name in [
        "info", "outline", "search", "find", "read", "verify", "extract", "index", "cache",
    ] {
        assert!(
            out.lines().any(|l| l.trim_start().starts_with(name)),
            "{name}: {out}"
        );
    }
    assert!(out.contains("--json"), "{out}");
}

#[test]
#[ignore = "needs the embedding model"]
fn index_reports_progress_and_marks_the_book_ready() {
    let tmp = private_tempdir();
    let books = TempDir::new().unwrap();
    let book = epub(
        books.path(),
        &["Entropy is surprise.", "Channels carry bits."],
    );

    // The default models directory, so the model is downloaded once.
    let assert = assert_cmd::cargo::cargo_bin_cmd!("ebook-hero")
        .env("TMPDIR", tmp.path())
        .env_remove("EBOOK_HERO_MODELS")
        .args(["index", &book])
        .assert()
        .success();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("embedding 2/2"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Semantic index: ready"), "{stdout}");
}
