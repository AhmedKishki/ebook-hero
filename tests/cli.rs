mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use assert_cmd::assert::Assert;
use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::document::Document;
use ebook_hero::index::cache::{list_indexes, with_lock};
use ebook_hero::search::{Mode, SearchParams};
use ebook_hero::session::Session;
use ebook_hero::settings::TOP_K_DEFAULT;
use serde_json::Value;
use tempfile::TempDir;

const SAMPLE_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf");
const ALICE_EPUB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alice.epub");
const IMAGE_ONLY_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/imageonly.pdf");

/// The directories every command runs with: `EBOOK_HERO_INDEXES`, the index
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
            .env("EBOOK_HERO_INDEXES", self.cache_root())
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
fn info_counts_the_spine_items_of_an_epub() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    let book = epub(books.path(), &["One.", "Two."]);

    let out = env.stdout(&["info", &book]);

    assert!(out.contains("Format: EPUB\n"), "{out}");
    assert!(out.contains("Spine items: 2\n"), "{out}");
    assert!(!out.contains("Chapters:"), "{out}");
    assert!(!out.contains("Pages:"), "{out}");
    let info = json(&env.stdout(&["info", &book, "--json"]));
    assert_eq!(info["spine_count"], 2, "{info}");
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
fn find_names_a_single_occurrence_and_passage_in_the_singular() {
    let env = Env::new();

    let out = env.stdout(&["find", ALICE_EPUB, "pig and pepper"]);

    assert!(out.starts_with("1 occurrence in 1 passage\n"), "{out}");
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

    let first = env.run(&["find", &book, "needle"]).success();
    let output = first.get_output();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.starts_with("25 occurrences in 25 passages\n"), "{out}");
    assert_eq!(
        out.lines().count(),
        21,
        "the totals and 20 occurrences: {out}"
    );
    assert!(!out.contains("next:"), "{out}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "next: --cursor 20\n"
    );

    let page = json(&env.stdout(&["find", &book, "needle", "--json"]));
    assert_eq!(page["next_cursor"], 20, "{page}");
    let page = json(&env.stdout(&["find", &book, "needle", "--cursor", "20", "--json"]));
    assert_eq!(page["occurrences"].as_array().unwrap().len(), 5);
    assert!(page["next_cursor"].is_null());
    let last = env
        .run(&["find", &book, "needle", "--cursor", "20"])
        .success();
    let output = last.get_output();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("next:"));
    assert!(output.stderr.is_empty(), "{output:?}");
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

    let result = json(&env.stdout(&[
        "verify",
        SAMPLE_PDF,
        "Entropy measures how much surprise a source of messages carries",
        "--json",
    ]));
    assert_eq!(result["verdict"], "exact");
    assert_eq!(result["passage_id"], 1);
}

/// The stdout and stderr of a command that must exit with `code`.
fn output_with_code(env: &Env, args: &[&str], code: i32) -> (String, String) {
    let output = env.run(args).code(code).get_output().clone();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn verify_exits_with_3_when_the_quote_is_not_in_the_book() {
    let env = Env::new();
    let quote = "Entropy measures how much surprise a source of messages delivers";

    let (none, stderr) = output_with_code(&env, &["verify", SAMPLE_PDF, quote], 3);

    assert_eq!(
        none,
        "none; closest passage 1 · p. 2 (1) (9 of 10 words; differs at \"delivers\")\n"
    );
    assert!(stderr.is_empty(), "{stderr}");
    let (result, _) = output_with_code(&env, &["verify", SAMPLE_PDF, quote, "--json"], 3);
    assert_eq!(json(&result)["verdict"], "none", "{result}");
}

#[test]
fn verify_names_where_a_quote_was_found_elsewhere() {
    let env = Env::new();

    let (out, _) = output_with_code(
        &env,
        &[
            "verify",
            SAMPLE_PDF,
            "Entropy measures how much surprise a source of messages carries",
            "--passage-id",
            "3",
        ],
        3,
    );

    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("none"), "{out}");
    assert!(lines.contains(&"found in passage 1 · p. 2 (1)"), "{out}");
}

#[test]
fn verify_help_gives_the_exit_status_of_each_verdict() {
    let env = Env::new();

    let out = env.stdout(&["verify", "--help"]);

    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("0 after exact or equivalent"), "{out}");
    assert!(flat.contains("3 after none"), "{out}");
    assert!(flat.contains("error, which exits with 1"), "{out}");
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

/// The hashes of the indexes in the CLI's cache, sorted.
fn cached_shas(env: &Env) -> Vec<String> {
    list_indexes(&env.cache_root())
        .unwrap()
        .into_iter()
        .map(|index| index.sha256)
        .collect()
}

#[test]
fn cache_list_prints_a_line_per_index() {
    let env = Env::new();
    assert_eq!(env.stdout(&["cache", "list"]), "");
    assert!(
        !env.cache_root().exists(),
        "listing must not create the cache"
    );

    env.stdout(&["info", SAMPLE_PDF]);
    let real = cached_shas(&env).remove(0);
    // Indexes whose titles are missing, or span lines.
    let (untitled, spread) = ("1".repeat(64), "2".repeat(64));
    for (sha, document) in [
        (&untitled, r#"{"metadata":{"title":null}}"#),
        (&spread, r#"{"metadata":{"title":"Two\nLines   Here"}}"#),
    ] {
        let dir = env.cache_root().join(sha);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("document.json"), document).unwrap();
    }

    let listing = env.stdout(&["cache", "list"]);

    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 3, "{listing}");
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted, "sorted by hash");
    assert!(lines.contains(&format!("{untitled} 0.0 MB (untitled)").as_str()));
    assert!(lines.contains(&format!("{spread} 0.0 MB Two Lines Here").as_str()));
    let line = lines.iter().find(|l| l.starts_with(&real)).unwrap();
    let (size, title) = line[real.len() + 1..].split_once(" MB ").unwrap();
    assert!(size.parse::<f64>().is_ok(), "{line}");
    assert_eq!(title, "Sample Book");
}

#[test]
fn cache_list_json_gives_the_hash_size_and_title_of_each_index() {
    let env = Env::new();
    assert_eq!(
        json(&env.stdout(&["cache", "list", "--json"])),
        serde_json::json!({ "indexes": [] })
    );

    env.stdout(&["info", SAMPLE_PDF]);
    let untitled = "1".repeat(64);
    let dir = env.cache_root().join(&untitled);
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("document.json"), r#"{"metadata":{"title":null}}"#).unwrap();

    let listing = json(&env.stdout(&["cache", "list", "--json"]));

    let indexes: Vec<Value> = list_indexes(&env.cache_root())
        .unwrap()
        .into_iter()
        .map(|index| {
            serde_json::json!({
                "sha256": index.sha256,
                "size_bytes": index.size_bytes,
                "title": index.title,
            })
        })
        .collect();
    assert_eq!(listing, serde_json::json!({ "indexes": indexes }));
    let titles: Vec<&Value> = indexes.iter().map(|index| &index["title"]).collect();
    assert_eq!(titles, [&Value::Null, &Value::from("Sample Book")]);
}

#[test]
fn cache_clear_prints_how_many_indexes_it_removed() {
    let env = Env::new();
    assert_eq!(env.stdout(&["cache", "clear"]), "removed 0 indexes\n");
    assert!(
        !env.cache_root().exists(),
        "clearing must not create the cache"
    );

    env.stdout(&["info", SAMPLE_PDF]);
    assert_eq!(cached_shas(&env).len(), 1);

    assert_eq!(env.stdout(&["cache", "clear"]), "removed 1 index\n");
    assert_eq!(env.stdout(&["cache", "list"]), "");

    env.stdout(&["info", SAMPLE_PDF]);
    env.stdout(&["info", ALICE_EPUB]);
    assert_eq!(env.stdout(&["cache", "clear"]), "removed 2 indexes\n");
}

#[test]
fn cache_clear_json_gives_how_many_indexes_it_removed() {
    let env = Env::new();
    assert_eq!(
        json(&env.stdout(&["cache", "clear", "--json"])),
        serde_json::json!({ "removed": 0, "skipped_in_use": 0 })
    );

    env.stdout(&["info", SAMPLE_PDF]);

    assert_eq!(
        json(&env.stdout(&["cache", "clear", "--json"])),
        serde_json::json!({ "removed": 1, "skipped_in_use": 0 })
    );
    assert!(cached_shas(&env).is_empty());
}

#[test]
fn cache_clear_reports_a_book_that_is_in_use() {
    let env = Env::new();
    env.stdout(&["info", SAMPLE_PDF]);
    let sha = cached_shas(&env).remove(0);

    // This process holds the build lock while the CLI, another process, clears.
    let (text, json_text) = with_lock(&env.cache_root(), &sha, Duration::ZERO, || {
        Ok((
            env.stdout(&["cache", "clear"]),
            env.stdout(&["cache", "clear", "--json"]),
        ))
    })
    .unwrap();

    assert_eq!(text, "removed 0 indexes (1 skipped: in use)\n");
    assert_eq!(
        json(&json_text),
        serde_json::json!({ "removed": 0, "skipped_in_use": 1 })
    );
    assert_eq!(cached_shas(&env), [sha]);
    assert_eq!(env.stdout(&["cache", "clear"]), "removed 1 index\n");
}

#[test]
fn mcp_refuses_json_before_it_starts() {
    let env = Env::new();
    let initialize = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    });

    // A server that started would answer the request on stdout.
    let assert = env
        .command()
        .args(["mcp", "--json"])
        .write_stdin(format!("{initialize}\n"))
        .assert()
        .code(1);

    let output = assert.get_output();
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error: --json "), "{stderr}");
    assert!(stderr.contains("without --json"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[test]
fn dependency_warnings_are_not_shown_by_default() {
    let env = Env::new();

    // Building the index of this PDF makes pdf_oxide log two warnings.
    let output = env
        .run(&["info", SAMPLE_PDF])
        .success()
        .get_output()
        .clone();

    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ebook_hero_log_selects_the_log_lines() {
    let env = Env::new();
    env.stdout(&["info", SAMPLE_PDF]);

    let assert = env
        .command()
        .env("EBOOK_HERO_LOG", "ebook_hero=debug")
        .args(["cache", "clear"])
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("clearing cached index"), "{stderr}");
}

#[test]
fn a_cache_that_does_not_load_is_rebuilt_with_one_warning_naming_the_cause() {
    let env = Env::new();
    let books = TempDir::new().unwrap();
    let path = epub(books.path(), &["Words to index."]);
    env.stdout(&["info", &path]);
    let cached = list_indexes(&env.cache_root()).unwrap();
    let document = env
        .cache_root()
        .join(&cached[0].sha256)
        .join("document.json");
    fs::write(&document, b"not json").unwrap();

    let assert = env.run(&["info", &path]).success();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    let warnings: Vec<&str> = stderr.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(
        warnings[0].contains(&*document.to_string_lossy()),
        "{stderr}"
    );
    assert!(
        String::from_utf8_lossy(&fs::read(&document).unwrap()).starts_with('{'),
        "the document was written again"
    );
}

#[test]
fn a_log_filter_that_does_not_parse_is_reported_and_ignored() {
    let env = Env::new();

    let assert = env
        .command()
        .env("EBOOK_HERO_LOG", "ebook_hero=loud")
        .args(["info", SAMPLE_PDF])
        .assert()
        .success();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("warning: EBOOK_HERO_LOG ignored: "),
        "{stderr}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Title: Sample Book"));
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
fn read_help_says_which_page_it_reads() {
    let env = Env::new();

    let out = env.stdout(&["read", "--help"]);

    assert!(out.contains("physical page"), "{out}");
    assert!(out.contains("counted from 1"), "{out}");
    assert!(out.contains("page label"), "{out}");
}

#[test]
fn graph_prints_the_summary_line_then_communities() {
    let env = Env::new();
    let out = env.stdout(&["graph", ALICE_EPUB]);
    let first = out.lines().next().unwrap();
    let words: Vec<&str> = first.split(' ').collect();
    assert_eq!(
        [
            words[1], words[2], words[4], words[5], words[7], words[8], words[10]
        ],
        [
            "concepts",
            "in",
            "communities",
            "·",
            "passages",
            "·",
            "topics"
        ],
        "{first}"
    );
    assert!(out.lines().any(|l| l.starts_with("Community 1: ")), "{out}");
}

#[test]
fn graph_refuses_a_book_shorter_than_twenty_passages() {
    let env = Env::new();
    let stderr = env
        .run(&["graph", SAMPLE_PDF])
        .code(1)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.contains("a concept graph needs at least 20"),
        "{stderr}"
    );
}

#[test]
fn help_names_the_mcp_tool_each_json_output_matches() {
    let env = Env::new();

    let out = env.stdout(&["--help"]);

    let json = out
        .lines()
        .skip_while(|l| !l.contains("--json"))
        .take_while(|l| !l.trim_start().starts_with("-h, --help"))
        .collect::<Vec<_>>()
        .join(" ");
    for name in [
        "info",
        "verify",
        "graph",
        "book_info",
        "verify_quote",
        "concept_graph",
    ] {
        assert!(json.contains(name), "{name}: {json}");
    }
    assert!(!json.contains("same name"), "{json}");
    assert!(json.contains("cache list"), "{json}");
    assert!(!json.contains("ignore"), "{json}");
}

#[test]
fn help_lists_every_subcommand() {
    let env = Env::new();

    let out = env.stdout(&["--help"]);

    for name in [
        "info", "outline", "search", "find", "read", "verify", "graph", "extract", "index", "cache",
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
        .env("EBOOK_HERO_INDEXES", tmp.path())
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
