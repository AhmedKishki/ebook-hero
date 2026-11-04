//! The MCP server, started as a child process the way a client starts it and
//! spoken to over its stdio.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assert_cmd::Command;
use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::embed::Models;
use rmcp::model::{CallToolRequestParams, CallToolResult, ProgressNotificationParam};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

const SAMPLE_PDF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.pdf");
const ALICE_EPUB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/alice.epub");

/// How long a test waits for the server before it calls the server stuck.
const PATIENCE: Duration = Duration::from_secs(60);
/// The same for tests that load the models and embed a book.
const PATIENCE_WITH_MODELS: Duration = Duration::from_secs(600);

const INSTRUCTIONS: &str = "ebook-hero searches and reads one PDF or EPUB per call; pass the book's path to every tool. Use `search` for questions and `find` to list every occurrence of a phrase. Expand a hit with `read`. Check a quote with `verify_quote` before using it. `concept_graph` lists the book's concepts, which of them link and the communities they form. Ranges are Unicode code points.";

/// A client that keeps the progress notifications it receives.
#[derive(Clone, Default)]
struct Recorder {
    progress: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for Recorder {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress.lock().unwrap().push(params);
    }
}

/// `ebook-hero mcp` running as a child process, and a client connected to it.
/// Its index cache is in a private temporary directory.
struct Server {
    client: RunningService<RoleClient, Recorder>,
    progress: Arc<Mutex<Vec<ProgressNotificationParam>>>,
    stderr: JoinHandle<String>,
    patience: Duration,
    _tmp: TempDir,
    _models: Option<TempDir>,
}

impl Server {
    /// A server whose models directory is empty: it can search lexically but
    /// has no model to load.
    async fn start() -> Server {
        let models = private_tempdir();
        let mut server = Server::start_with_models(models.path(), PATIENCE).await;
        server._models = Some(models);
        server
    }

    async fn start_with_models(models: &Path, patience: Duration) -> Server {
        let tmp = private_tempdir();
        let command =
            tokio::process::Command::new(env!("CARGO_BIN_EXE_ebook-hero")).configure(|command| {
                command
                    .arg("mcp")
                    .env("EBOOK_HERO_INDEXES", tmp.path())
                    .env("EBOOK_HERO_MODELS", models)
                    .env("EBOOK_HERO_LOG", "debug");
            });
        // Standard error is read, or a server that logs a lot would fill the
        // pipe and stop, and the test run would print the log.
        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = stderr.expect("standard error is piped");
        let stderr = tokio::spawn(async move {
            let mut log = String::new();
            let _ = stderr.read_to_string(&mut log).await;
            log
        });
        let recorder = Recorder::default();
        let progress = Arc::clone(&recorder.progress);
        let client = tokio::time::timeout(patience, recorder.serve(transport))
            .await
            .expect("the server did not answer initialize")
            .expect("the server did not initialize");
        Server {
            client,
            progress,
            stderr,
            patience,
            _tmp: tmp,
            _models: None,
        }
    }

    /// Calls a tool. Only a protocol error or a stuck server fails this.
    async fn call(&self, tool: &'static str, arguments: Value) -> CallToolResult {
        let Value::Object(arguments) = arguments else {
            panic!("the arguments of {tool} must be a JSON object");
        };
        let request = CallToolRequestParams::new(tool).with_arguments(arguments);
        tokio::time::timeout(self.patience, self.client.call_tool(request))
            .await
            .unwrap_or_else(|_| panic!("{tool} did not answer"))
            .unwrap_or_else(|e| panic!("{tool} failed as a protocol error: {e}"))
    }

    /// Closes the connection, waits for the server to exit, and returns what
    /// it wrote to standard error.
    async fn finish(self) -> String {
        self.client.cancel().await.unwrap();
        self.stderr.await.unwrap()
    }
}

/// The one text content of a result that carries a single text.
fn text_of(result: &CallToolResult) -> &str {
    assert_eq!(result.content.len(), 1, "{result:?}");
    &result.content[0]
        .as_text()
        .unwrap_or_else(|| panic!("not text: {result:?}"))
        .text
}

fn is_error(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
}

#[tokio::test]
async fn lists_seven_tools_and_instructions() {
    let server = Server::start().await;

    let tools = server.client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "book_info",
            "concept_graph",
            "find",
            "outline",
            "read",
            "search",
            "verify_quote"
        ]
    );
    for tool in &tools {
        assert!(
            tool.description.as_deref().is_some_and(|d| d.len() > 20),
            "{} has no description",
            tool.name
        );
    }

    let info = server
        .client
        .peer_info()
        .expect("the server was initialized");
    let identity = info.server_info.as_ref().expect("the server names itself");
    assert_eq!(identity.name, "ebook-hero");
    assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools.is_some());
    let instructions = info.instructions.as_deref().expect("instructions");
    assert!(instructions.contains("verify_quote"), "{instructions}");
    assert_eq!(instructions, INSTRUCTIONS);
}

#[tokio::test]
async fn the_read_tool_says_which_page_it_reads() {
    let server = Server::start().await;

    let tools = server.client.list_all_tools().await.unwrap();

    let read = tools.iter().find(|tool| tool.name == "read").unwrap();
    let description = read.description.as_deref().unwrap();
    assert!(description.contains("physical page"), "{description}");
    assert!(description.contains("counted from 1"), "{description}");
    assert!(description.contains("page label"), "{description}");
    let page = read.input_schema["properties"]["page"]["description"]
        .as_str()
        .unwrap();
    assert!(page.contains("physical page"), "{page}");
    server.finish().await;
}

#[tokio::test]
async fn every_tool_returns_structured_json_and_text() {
    let server = Server::start().await;
    let quote = "A fair coin has one bit of entropy, because each toss is equally likely to land either way";
    let calls = [
        ("book_info", json!({ "path": SAMPLE_PDF })),
        ("outline", json!({ "path": SAMPLE_PDF })),
        (
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "lexical" }),
        ),
        ("find", json!({ "path": SAMPLE_PDF, "phrase": "entropy" })),
        ("read", json!({ "path": SAMPLE_PDF, "passage_id": 1 })),
        (
            "verify_quote",
            json!({ "path": SAMPLE_PDF, "quote": quote }),
        ),
        ("concept_graph", json!({ "path": ALICE_EPUB })),
    ];

    for (tool, arguments) in calls {
        let result = server.call(tool, arguments).await;

        assert!(!is_error(&result), "{tool}: {result:?}");
        let structured = result
            .structured_content
            .as_ref()
            .unwrap_or_else(|| panic!("{tool} has no structured content: {result:?}"));
        assert!(structured.is_object(), "{tool}: {structured}");
        let text = text_of(&result);
        let parsed: Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{tool} text is not JSON ({e}): {text}"));
        assert_eq!(&parsed, structured, "{tool}");
        assert!(!text.contains('\n'), "{tool} text is compact JSON: {text}");
    }

    let verdict = server
        .call(
            "verify_quote",
            json!({ "path": SAMPLE_PDF, "quote": quote }),
        )
        .await;
    assert_eq!(verdict.structured_content.unwrap()["verdict"], "exact");
}

#[tokio::test]
async fn structured_result_equals_cli_json() {
    let server = Server::start().await;
    let cli_env = private_tempdir();
    let cli_models = private_tempdir();
    let output = Command::new(env!("CARGO_BIN_EXE_ebook-hero"))
        .args([
            "search", SAMPLE_PDF, "entropy", "--mode", "lexical", "--json",
        ])
        .env("EBOOK_HERO_INDEXES", cli_env.path())
        .env("EBOOK_HERO_MODELS", cli_models.path())
        .env_remove("EBOOK_HERO_LOG")
        .assert()
        .success()
        .get_output()
        .clone();
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();

    let result = server
        .call(
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "lexical" }),
        )
        .await;

    assert!(!is_error(&result), "{result:?}");
    // The scores are `f32`; the CLI prints them as such, so a value made from
    // the number directly would differ in its last digits.
    assert_eq!(result.structured_content, Some(cli));
}

#[tokio::test]
async fn concept_graph_equals_cli_json() {
    let server = Server::start().await;
    let cli_env = private_tempdir();
    let cli_models = private_tempdir();
    let output = Command::new(env!("CARGO_BIN_EXE_ebook-hero"))
        .args(["graph", ALICE_EPUB, "--json"])
        .env("EBOOK_HERO_INDEXES", cli_env.path())
        .env("EBOOK_HERO_MODELS", cli_models.path())
        .env_remove("EBOOK_HERO_LOG")
        .assert()
        .success()
        .get_output()
        .clone();
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();

    let result = server
        .call("concept_graph", json!({ "path": ALICE_EPUB }))
        .await;

    assert!(!is_error(&result), "{result:?}");
    assert_eq!(result.structured_content, Some(cli));
}

#[tokio::test]
async fn errors_are_tool_errors() {
    let server = Server::start().await;

    let missing = server
        .call("book_info", json!({ "path": "/no/such/book.epub" }))
        .await;
    assert!(is_error(&missing), "{missing:?}");
    assert!(
        text_of(&missing).contains("file not found"),
        "{}",
        text_of(&missing)
    );
    assert_eq!(missing.structured_content, None);

    let too_many = server
        .call(
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "lexical", "top_k": 21 }),
        )
        .await;
    assert!(is_error(&too_many), "{too_many:?}");
    assert!(
        text_of(&too_many).contains("top_k must be between 1 and 20"),
        "{}",
        text_of(&too_many)
    );

    let again = server.call("outline", json!({ "path": SAMPLE_PDF })).await;
    assert!(!is_error(&again), "the server keeps serving after an error");
}

#[tokio::test]
async fn an_unavailable_model_is_a_tool_error_that_names_the_lexical_remedy() {
    // A models directory that is a regular file, so that no model can load.
    let models = tempfile::NamedTempFile::new().unwrap();
    let server = Server::start_with_models(models.path(), PATIENCE).await;

    let semantic = server
        .call(
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "semantic" }),
        )
        .await;

    assert!(is_error(&semantic), "{semantic:?}");
    assert!(
        text_of(&semantic).contains("mode=lexical"),
        "{}",
        text_of(&semantic)
    );
    let lexical = server
        .call(
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "lexical" }),
        )
        .await;
    assert!(
        !is_error(&lexical),
        "lexical search keeps working: {lexical:?}"
    );
}

#[tokio::test]
async fn an_argument_the_tool_does_not_have_is_a_tool_error() {
    let server = Server::start().await;

    // A model that spells `top_k` as `limit` would otherwise get five hits
    // and not know that its argument was dropped.
    let result = server
        .call(
            "search",
            json!({ "path": SAMPLE_PDF, "query": "entropy", "mode": "lexical", "limit": 2 }),
        )
        .await;

    assert!(is_error(&result), "{result:?}");
    let text = text_of(&result);
    assert!(text.contains("unknown field `limit`"), "{text}");
    assert!(
        text.contains("top_k"),
        "the message lists the arguments: {text}"
    );
}

#[tokio::test]
async fn read_needs_exactly_one_address() {
    let server = Server::start().await;
    let message = "give exactly one of passage_id, page, outline_id";

    for arguments in [
        json!({ "path": SAMPLE_PDF, "passage_id": 1, "page": 2 }),
        json!({ "path": SAMPLE_PDF, "page": 2, "outline_id": 0 }),
        json!({ "path": SAMPLE_PDF }),
    ] {
        let result = server.call("read", arguments.clone()).await;

        assert!(is_error(&result), "{arguments}: {result:?}");
        assert_eq!(text_of(&result), message, "{arguments}");
    }

    let by_page = server
        .call("read", json!({ "path": SAMPLE_PDF, "page": 2 }))
        .await;
    assert!(!is_error(&by_page), "{by_page:?}");
    let by_outline = server
        .call("read", json!({ "path": SAMPLE_PDF, "outline_id": 0 }))
        .await;
    assert!(!is_error(&by_outline), "{by_outline:?}");
}

#[tokio::test]
async fn read_adds_no_neighbours_by_default() {
    let server = Server::start().await;

    let alone = server
        .call("read", json!({ "path": SAMPLE_PDF, "passage_id": 2 }))
        .await;
    let widened = server
        .call(
            "read",
            json!({ "path": SAMPLE_PDF, "passage_id": 2, "before": 1, "after": 1 }),
        )
        .await;

    let ids = |result: &CallToolResult| -> Vec<u64> {
        result.structured_content.as_ref().unwrap()["passages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["passage_id"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(ids(&alone), [2]);
    assert_eq!(ids(&widened), [1, 2, 3]);
}

/// `mode` reaches a client as a plain string enum. A client that cannot
/// resolve `$ref` or `oneOf` could not offer the choice.
#[tokio::test]
async fn the_search_schema_offers_mode_as_a_plain_enum() {
    let server = Server::start().await;

    let tools = server.client.list_all_tools().await.unwrap();

    let search = tools.iter().find(|tool| tool.name == "search").unwrap();
    let schema = Value::Object((*search.input_schema).clone());
    let mode = &schema["properties"]["mode"];
    let values: Vec<&str> = mode["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("mode has no enum: {mode}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(values, ["hybrid", "lexical", "semantic"], "{mode}");
    for keyword in ["oneOf", "anyOf", "$ref"] {
        assert!(mode.get(keyword).is_none(), "mode uses {keyword}: {mode}");
    }
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(required, ["path", "query"], "{schema}");
}

#[tokio::test]
async fn logs_go_to_standard_error() {
    let server = Server::start().await;
    let info = server
        .call("book_info", json!({ "path": SAMPLE_PDF }))
        .await;
    assert!(!is_error(&info), "{info:?}");

    let log = server.finish().await;

    assert!(!log.is_empty(), "EBOOK_HERO_LOG=debug logs something");
}

// The tests below need the embedding and reranker models, in
// `$EBOOK_HERO_MODELS` or the user's cache. The first run downloads them.

const HARBOUR: [&str; 5] = [
    "The council voted to widen the harbour road and to levy a new toll on freight carts.",
    "There was a bottle labelled drink me.",
    "Tides rise and fall twice a day because of the gravitational pull of the moon.",
    "The recipe calls for two cups of flour, a pinch of salt and three eggs.",
    "Interest rates on long-term bonds moved little after the central bank met.",
];

/// An EPUB with one passage for each entry of `paragraphs`, in `dir`.
fn epub(dir: &Path, paragraphs: &[&str]) -> PathBuf {
    let mut builder = EpubBuilder::new(EpubVersion::V3).title("Harbour Book");
    for (i, text) in paragraphs.iter().enumerate() {
        let href = format!("ch{i}.xhtml");
        builder = builder
            .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
            .toc(&format!("Part {i}"), &href, 1);
    }
    let path = dir.join("harbour.epub");
    builder.write(&path);
    path
}

/// A hybrid search loads fastembed and ONNX Runtime inside the server. If
/// either wrote to standard output, the protocol would break and this call
/// would fail. The client always sends a progress token, so the embedding is
/// reported too.
#[tokio::test]
#[ignore = "needs the embedding and reranker models"]
async fn hybrid_search_works_over_stdio_and_reports_progress() {
    let books = TempDir::new().unwrap();
    let book = epub(books.path(), &HARBOUR);
    let server =
        Server::start_with_models(&Models::default_cache_dir(), PATIENCE_WITH_MODELS).await;

    let result = server
        .call(
            "search",
            json!({ "path": book, "query": "tides and the moon", "top_k": 3 }),
        )
        .await;

    assert!(!is_error(&result), "{result:?}");
    let found = result.structured_content.as_ref().unwrap();
    assert_eq!(found["mode"], "hybrid");
    let top = found["hits"][0]["text"].as_str().unwrap();
    assert!(top.contains("gravitational pull of the moon"), "{found}");

    // The client handles a notification after it has read it, so the last one
    // may still be on its way when the result is here.
    let mut seen = Vec::new();
    for _ in 0..50 {
        seen = server.progress.lock().unwrap().clone();
        if seen.last().is_some_and(|p| Some(p.progress) == p.total) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!seen.is_empty(), "no progress was reported");
    for update in &seen {
        assert_eq!(update.message.as_deref(), Some("embedding passages"));
        assert_eq!(update.total, Some(HARBOUR.len() as f64));
    }
    assert!(seen.windows(2).all(|w| w[0].progress < w[1].progress));
    assert_eq!(seen.last().unwrap().progress, HARBOUR.len() as f64);
    server.finish().await;
}

/// A request without a progress token gets no progress notification, and
/// every line the server writes is a JSON-RPC message. This client is written
/// by hand because a library client always sends a token.
#[test]
#[ignore = "needs the embedding and reranker models"]
fn without_a_progress_token_only_responses_are_written() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let tmp = private_tempdir();
    let books = TempDir::new().unwrap();
    let book = epub(books.path(), &HARBOUR);
    let mut child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_ebook-hero"))
            .arg("mcp")
            .env("EBOOK_HERO_INDEXES", tmp.path())
            .env("EBOOK_HERO_MODELS", Models::default_cache_dir())
            .env("EBOOK_HERO_LOG", "debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let stdout = child.0.stdout.take().unwrap();
    let (lines, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut send = |message: Value| {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    };
    // Every line up to the response with this id.
    let read_until = |id: u64| -> Vec<Value> {
        let mut messages = Vec::new();
        loop {
            let line = received
                .recv_timeout(PATIENCE_WITH_MODELS)
                .expect("the server went quiet");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carries a line that is not JSON ({e}): {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            let done = message["id"] == id;
            messages.push(message);
            if done {
                return messages;
            }
        }
    };

    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    }));
    let init = read_until(1);
    assert!(init.last().unwrap()["result"].is_object(), "{init:?}");
    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": { "path": book, "query": "tides and the moon", "top_k": 3 }
        }
    }));

    let messages = read_until(2);

    let response = messages.last().unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(
        messages.len(),
        1,
        "only the response is written: {messages:?}"
    );
}

/// A server spoken to by hand, one JSON-RPC line at a time, with its index in
/// a private directory and the models from `$EBOOK_HERO_MODELS` or the user's
/// cache.
struct StdioServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    next_id: u64,
    _cache: TempDir,
}

impl StdioServer {
    /// Starts the server with `env` added to its environment and initializes it.
    fn start(env: &[(&str, &str)]) -> StdioServer {
        let cache = private_tempdir();
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ebook-hero"));
        command
            .arg("mcp")
            .env("EBOOK_HERO_INDEXES", cache.path())
            .env("EBOOK_HERO_MODELS", Models::default_cache_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut server = StdioServer {
            child,
            stdin,
            lines,
            next_id: 0,
            _cache: cache,
        };
        let init = server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        );
        assert!(init.last().unwrap()["result"].is_object(), "{init:?}");
        server.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        server
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and returns every message up to its response.
    fn request(&mut self, method: &str, params: Value) -> Vec<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut messages = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(PATIENCE_WITH_MODELS)
                .expect("the server went quiet");
            let message: Value = serde_json::from_str(&line).unwrap();
            let done = message["id"] == id;
            messages.push(message);
            if done {
                return messages;
            }
        }
    }

    /// Calls a tool and asserts that it succeeded.
    fn call(&mut self, tool: &str, arguments: Value) {
        let messages = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        let response = messages.last().unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
    }

    /// The server's resident memory in MiB.
    #[cfg(target_os = "linux")]
    fn resident_mib(&self) -> u64 {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap();
        let line = status.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
        line.split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u64>()
            .unwrap()
            / 1024
    }
}

impl Drop for StdioServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const ALICE_QUERIES: [&str; 10] = [
    "the queen shouts off with her head",
    "why is a raven like a writing desk",
    "Alice falls down the rabbit hole and wonders whether she will fall right through the earth",
    "the caterpillar and the hookah",
    "who stole the tarts",
    "the Mock Turtle's story of his education in the school under the sea, with reeling, writhing and the different branches of arithmetic",
    "a croquet game played with flamingos and hedgehogs",
    "the Cheshire cat grins and vanishes",
    "drink me",
    "the trial, the jury and the evidence that Alice gives at the end of the book",
];

/// Reranking all thirty candidates in one padded batch made ONNX Runtime keep
/// close to 2 GB for as long as the server ran. Reranked in small batches,
/// the server stays well under a gibibyte.
#[test]
#[cfg(target_os = "linux")]
#[ignore = "needs the embedding and reranker models"]
fn the_server_holds_less_than_a_gibibyte_through_many_hybrid_searches() {
    let mut server = StdioServer::start(&[]);
    let mut most = 0;
    for query in ALICE_QUERIES {
        server.call("search", json!({ "path": ALICE_EPUB, "query": query }));
        most = most.max(server.resident_mib());
    }
    assert!(most < 1024, "the server held {most} MiB");
}

/// A server left idle for `EBOOK_HERO_MODEL_IDLE` seconds drops its models
/// and gives their memory back, and the next search loads them again.
#[test]
#[cfg(target_os = "linux")]
#[ignore = "needs the embedding and reranker models"]
fn an_idle_server_releases_its_models_and_loads_them_again() {
    let mut server = StdioServer::start(&[("EBOOK_HERO_MODEL_IDLE", "1")]);
    server.call(
        "search",
        json!({ "path": ALICE_EPUB, "query": ALICE_QUERIES[0] }),
    );
    let loaded = server.resident_mib();

    let mut idle = loaded;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_secs(1));
        idle = server.resident_mib();
        if idle < 200 {
            break;
        }
    }
    assert!(
        idle < 200,
        "the server held {idle} MiB after idling, {loaded} MiB loaded"
    );

    server.call(
        "search",
        json!({ "path": ALICE_EPUB, "query": ALICE_QUERIES[1] }),
    );
    assert!(server.resident_mib() > idle, "the models loaded again");
}
