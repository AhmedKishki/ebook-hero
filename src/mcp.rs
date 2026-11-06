//! The MCP server: serves the seven tools over standard input and output until
//! the client disconnects. Like the command line it parses arguments and
//! formats results, and calls a [`Session`] for everything else.
//!
//! Standard output carries only protocol messages. Logs go to standard error
//! (see `cli`), and nothing here prints.

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock,
    CreateTaskResult, GetTaskParams, GetTaskResult, Implementation, ProgressNotificationParam,
    RequestMetaObject, ServerCapabilities, ServerConfig, UpdateTaskParams,
};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskContext, TaskExit, TaskManager, TaskOptions};
use rmcp::transport::stdio;
use rmcp::{
    ErrorData, Peer, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;
use crate::cancel::Cancel;
use crate::read::{ReadAt, ReadParams};
use crate::search::{Mode, SearchParams};
use crate::session::Session;
use crate::settings::{MODEL_IDLE, TASK_TTL, TOP_K_DEFAULT};

/// What the server tells a client about itself when it connects.
const INSTRUCTIONS: &str = "ebook-hero searches and reads one PDF, EPUB or Markdown file per call; pass the book's path to every tool. Use `search` for questions and `find` to list every occurrence of a phrase. Expand a hit with `read`. Check a quote with `verify_quote` before using it. `concept_graph` lists the book's concepts, which of them link and the communities they form. Ranges are Unicode code points.";

/// The progress message while the passages of a book are embedded.
const EMBEDDING_MESSAGE: &str = "embedding passages";

/// The error text when `read` is not given one place to read.
const ONE_ADDRESS: &str = "give exactly one of passage_id, page, outline_id";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
    /// List every passage of each concept and link (default false: the first ten, with `passage_count` saying how many there are).
    full: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
    /// Words, "quoted phrases" or a question.
    query: String,
    /// `hybrid` (the default) fuses lexical and semantic results; `lexical` matches stemmed words and exact phrases only; `semantic` ranks by embedding similarity only.
    mode: Option<Mode>,
    /// How many hits to return, from 1 to 20. The default is 5.
    top_k: Option<usize>,
    /// Rerank the candidates with a reranking model. The default is true; lexical mode ignores it.
    rerank: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
    /// The phrase, matched as whole words in any case, without stemming.
    phrase: String,
    /// Where to continue: the `next_cursor` of the previous page. Leave it out for the first page.
    cursor: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
    /// Read this passage.
    passage_id: Option<u32>,
    /// Read the passages on this PDF page: the physical page, counted from 1. The printed page label is shown in the locator.
    page: Option<u32>,
    /// Read the passages of this outline entry, by the `id` the `outline` tool gives.
    outline_id: Option<u32>,
    /// Passages to add before the range, at most 10. The default is 0.
    before: Option<u32>,
    /// Passages to add after the range, at most 10. The default is 0.
    after: Option<u32>,
    /// The passage to continue at: the `next_cursor` of the previous call, which is repeated with the other arguments unchanged.
    cursor: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VerifyArgs {
    /// The PDF, EPUB or Markdown file: an absolute path, a path relative to the server's working directory, or one starting with `~`.
    path: String,
    /// The text to look for.
    quote: String,
    /// Look in this passage first, as a citation says. Leave it out to search the whole book.
    passage_id: Option<u32>,
}

#[derive(Clone)]
struct EbookHero {
    session: Arc<Session>,
    tool_router: ToolRouter<EbookHero>,
    /// The tasks of every client that asked for one; clones share them.
    tasks: TaskManager,
    /// Set on the copy that runs one task, so a long call can report what it
    /// is doing in the task's status message.
    task: Option<TaskContext>,
}

#[tool_router]
impl EbookHero {
    #[tool(
        description = "Summarize the book: title, authors, language, format, page or chapter count, passage count, whether the semantic index is built, and warnings about parts that could not be read."
    )]
    async fn book_info(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        self.run(move |session| session.info(&args.path)).await
    }

    #[tool(
        description = "List the table of contents: for each entry its `id`, title, level, and the first passage of its text with the passage's locator. Pass an entry's `id` as `outline_id` to `read`."
    )]
    async fn outline(&self, Parameters(args): Parameters<PathArgs>) -> CallToolResult {
        self.run(move |session| session.outline(&args.path)).await
    }

    #[tool(
        description = "Find the passages that best answer a query. Returns up to `top_k` hits (default 5, at most 20), best first, each with the full passage text, its section, locator, `matches` (ranges of the query words in the text), `score`, and the `lexical_rank` and `semantic_rank` it came from. `mode` is `hybrid` (default), `lexical` or `semantic`; `rerank` (default true) reorders hybrid and semantic hits with a reranking model. `score` orders hits within one call only, so do not compare scores across calls: after reranking it is a reranker logit, otherwise BM25 (lexical), cosine similarity (semantic) or a fused score (hybrid). The first hybrid or semantic search of a book embeds all its passages, which can take minutes. Ranges are Unicode code points."
    )]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
    ) -> CallToolResult {
        let params = SearchParams {
            query: args.query,
            mode: args.mode.unwrap_or_default(),
            top_k: args.top_k.unwrap_or(TOP_K_DEFAULT),
            rerank: args.rerank.unwrap_or(true),
        };
        // Embedding can take minutes. A client that sent a progress token is
        // told how far it is; one that did not is told nothing, so no sender
        // is kept and the channel closes at once.
        let token = meta.get_progress_token();
        let (updates, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let updates = token.is_some().then_some(updates);
        let session = Arc::clone(&self.session);
        let task = self.task.clone();
        let work = tokio::task::spawn_blocking(move || {
            session.search(&args.path, params, &|done, total| {
                if let Some(task) = &task {
                    task.set_status_message(format!("{EMBEDDING_MESSAGE} {done} of {total}"));
                }
                if let Some(updates) = &updates {
                    // The receiver is gone only when this request was dropped.
                    let _ = updates.send((done, total));
                }
            })
        });
        if let Some(token) = token {
            // The channel closes when the search ends and drops its sender.
            while let Some((done, total)) = arrivals.recv().await {
                let update = ProgressNotificationParam::new(token.clone(), done as f64)
                    .with_total(total as f64)
                    .with_message(EMBEDDING_MESSAGE);
                if let Err(error) = peer.notify_progress(update).await {
                    tracing::debug!("progress was not delivered: {error}");
                }
            }
        }
        finished(work.await)
    }

    #[tool(
        description = "List every passage that contains an exact phrase (whole words, any case, no stemming), in reading order, with the total number of occurrences and passages in the book. A page holds 20 passages, each with the ranges of its occurrences; when `next_cursor` is set, pass it as `cursor` for the next page. Ranges are Unicode code points."
    )]
    async fn find(&self, Parameters(args): Parameters<FindArgs>) -> CallToolResult {
        self.run(move |session| session.find(&args.path, &args.phrase, args.cursor))
            .await
    }

    #[tool(
        description = "Read passages as Markdown, with their locators. Give exactly one of `passage_id`, `page` or `outline_id`. `page` is the physical page of a PDF, counted from 1; the printed page label is shown in the locator. `before` and `after` add up to 10 passages on each side (default 0). Returns at most about 8000 characters per call; when more is left, `next_cursor` is the passage to continue at: pass it as `cursor` and repeat the other arguments unchanged."
    )]
    async fn read(&self, Parameters(args): Parameters<ReadArgs>) -> CallToolResult {
        let at = match read_at(&args) {
            Ok(at) => at,
            Err(error) => return failure(&error),
        };
        let params = ReadParams {
            at,
            before: args.before.unwrap_or(0),
            after: args.after.unwrap_or(0),
            cursor: args.cursor,
        };
        self.run(move |session| session.read(&args.path, params))
            .await
    }

    #[tool(
        description = "Check whether a quote occurs in the book. `verdict` is `exact` (case-sensitive, from a word start to a word end), `equivalent` (only case, punctuation or spacing differ) or `none`. With `exact` or `equivalent`, `passage_id`, `locator` and `matched` (a range of that passage's text) say where it starts. With `none`, `found_elsewhere` names where the quote actually is when `passage_id` was given and the quote is in another passage; otherwise `closest` names the passage that shares the longest run of words. With `passage_id`, that passage is checked first, as a citation names it. A quote may run on into the next passage. Ranges are Unicode code points."
    )]
    async fn verify_quote(&self, Parameters(args): Parameters<VerifyArgs>) -> CallToolResult {
        self.run(move |session| session.verify(&args.path, &args.quote, args.passage_id))
            .await
    }

    #[tool(
        description = "Build or load the book's concept graph: up to 60 concepts (phrases that mark its topics); links between concepts that share passages more often than chance, with their NPMI; and communities of linked concepts. A concept's passages are those where it stands as a phrase of its own, so `find` can list more mentions; a link's are those both concepts stand in. Each gives `passage_count` and the first ten passage ids, or all of them with `full`; pass an id as `passage_id` to `read`. Built from the text alone, in seconds, on the first call; later calls read the cache."
    )]
    async fn concept_graph(&self, Parameters(args): Parameters<GraphArgs>) -> CallToolResult {
        let full = args.full.unwrap_or(false);
        self.run(move |session| session.graph(&args.path, full))
            .await
    }
}

impl EbookHero {
    fn new(session: Session) -> EbookHero {
        EbookHero {
            session: Arc::new(session),
            tool_router: EbookHero::tool_router(),
            tasks: TaskManager::new(),
            task: None,
        }
    }

    /// A copy of the server whose calls end with [`Error::Cancelled`] once
    /// `cancel` is set. It shares the books, the models and the tasks.
    fn cancellable(&self, cancel: &Cancel, task: Option<TaskContext>) -> EbookHero {
        EbookHero {
            session: Arc::new(self.session.cancellable(cancel.clone())),
            tool_router: self.tool_router.clone(),
            tasks: self.tasks.clone(),
            task,
        }
    }

    /// Runs the tool as a task and returns its handle at once. The task ends
    /// `cancelled` when the client cancels it and the call stops, `completed`
    /// when the call finishes, whatever the client asked, and `failed` when
    /// it runs past `TASK_TTL`.
    fn start_task(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> CreateTaskResult {
        let server = self.clone();
        let task = self
            .tasks
            .spawn(TaskOptions::new().with_ttl_ms(TASK_TTL), |task| {
                Box::pin(async move {
                    let cancel = Cancel::new();
                    // A task that expires is dropped; its call must stop as well.
                    let _stop_on_drop = StopOnDrop(cancel.clone());
                    let handler = server.cancellable(&cancel, Some(task.clone()));
                    let call = handler
                        .tool_router
                        .call(ToolCallContext::new(&handler, request, context));
                    tokio::pin!(call);
                    let response = tokio::select! {
                        response = &mut call => response,
                        () = task.cancelled() => {
                            cancel.cancel();
                            call.await
                        }
                    };
                    match response {
                        Ok(CallToolResponse::Complete(result)) if was_cancelled(&result) => {
                            Err(TaskExit::Cancelled)
                        }
                        Ok(CallToolResponse::Complete(result)) => Ok(result),
                        Ok(_) => Err(TaskExit::Error(ErrorData::internal_error(
                            "a tool asked for input, which this server never does",
                            None,
                        ))),
                        Err(error) => Err(TaskExit::Error(error)),
                    }
                })
            });
        CreateTaskResult::new(task)
    }

    /// Runs `work` on a thread meant for blocking. A call can wait minutes for
    /// an index to build, and that must not hold up the other requests.
    async fn run<T, F>(&self, work: F) -> CallToolResult
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&Session) -> crate::Result<T> + Send + 'static,
    {
        let session = Arc::clone(&self.session);
        finished(tokio::task::spawn_blocking(move || work(&session)).await)
    }
}

/// Sets its flag when dropped, so a call whose task was dropped stops too.
struct StopOnDrop(Cancel);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Whether `result` is the failure of a call that stopped because it was
/// cancelled.
fn was_cancelled(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
        && result.content.len() == 1
        && result.content[0]
            .as_text()
            .is_some_and(|text| text.text == Error::Cancelled.to_string())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EbookHero {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks()
                .build(),
        )
        .with_server_info(Implementation::new("ebook-hero", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    /// A client that declared the tasks extension gets a task handle for every
    /// tool and polls it; any other client waits for the result, as before.
    /// Either way the call stops, with a tool error, when the client cancels
    /// the request.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.tool_router.has_route(&request.name) {
            return Err(ErrorData::invalid_params("tool not found", None));
        }
        let wants_task = context
            .client_capabilities()
            .is_some_and(|capabilities| capabilities.supports_tasks());
        if wants_task {
            return Ok(CallToolResponse::Task(self.start_task(request, context)));
        }
        let cancel = Cancel::new();
        let stop = {
            let (token, cancel) = (context.ct.clone(), cancel.clone());
            tokio::spawn(async move {
                token.cancelled().await;
                cancel.cancel();
            })
        };
        let handler = self.cancellable(&cancel, None);
        let response = handler
            .tool_router
            .call(ToolCallContext::new(&handler, request, context))
            .await;
        stop.abort();
        response
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        self.tasks
            .get_task(&request.task_id)
            .map(GetTaskResult::new)
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks
            .update_task(&request.task_id, request.input_responses)
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks.cancel_task(&request.task_id)
    }
}

/// The place `read` is asked to read: exactly one address must be given.
fn read_at(args: &ReadArgs) -> crate::Result<ReadAt> {
    match (args.passage_id, args.page, args.outline_id) {
        (Some(id), None, None) => Ok(ReadAt::Passage(id)),
        (None, Some(page), None) => Ok(ReadAt::Page(page)),
        (None, None, Some(id)) => Ok(ReadAt::Outline(id)),
        _ => Err(Error::InvalidArgument(ONE_ADDRESS.to_string())),
    }
}

/// The result of a tool that ran on a blocking thread. A thread that panicked
/// is reported to the client like any other failure.
fn finished<T: Serialize>(
    outcome: Result<crate::Result<T>, tokio::task::JoinError>,
) -> CallToolResult {
    match outcome {
        Ok(Ok(output)) => success(&output),
        Ok(Err(error)) => failure(&error),
        Err(error) => failure(&format!("the tool stopped unexpectedly: {error}")),
    }
}

/// A successful result: the output as structured content, and the same JSON,
/// compact, as the one text.
///
/// The structured value is read back from the text rather than made from the
/// output directly. A `Value` made from an `f32` score holds its widened
/// `f64`, which prints with more digits than the text, and `--json` of the
/// command line prints the shorter form.
fn success<T: Serialize>(output: &T) -> CallToolResult {
    let written = serde_json::to_string(output).and_then(|text| {
        let structured: Value = serde_json::from_str(&text)?;
        Ok((text, structured))
    });
    match written {
        Ok((text, structured)) => {
            let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
            result.structured_content = Some(structured);
            result
        }
        Err(error) => failure(&format!("the result could not be written as JSON: {error}")),
    }
}

/// A failed tool call. The client sees it as a tool result with `isError` set,
/// which a model can read and act on, not as a protocol error.
fn failure(message: &dyn std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.to_string())])
}

/// How long the server keeps a model it has not used: `from_env`, the value of
/// `$EBOOK_HERO_MODEL_IDLE`, in whole seconds, or `MODEL_IDLE` when it is not
/// set.
fn model_idle(from_env: Option<std::ffi::OsString>) -> crate::Result<Duration> {
    let Some(value) = from_env.filter(|v| !v.is_empty()) else {
        return Ok(MODEL_IDLE);
    };
    let text = value.to_string_lossy();
    match text.parse::<u64>() {
        Ok(seconds) if seconds > 0 => Ok(Duration::from_secs(seconds)),
        _ => Err(Error::InvalidArgument(format!(
            "EBOOK_HERO_MODEL_IDLE is {text:?}; set it to a whole number of seconds, at least 1, \
             or unset it to keep a model {} seconds after its last use",
            MODEL_IDLE.as_secs()
        ))),
    }
}

/// Drops the models each time they have been unused for `idle`, checking once
/// per `idle`, so a model goes between one and two idle periods after its
/// last use.
async fn release_idle_models(session: Arc<Session>, idle: Duration) {
    let mut checks = tokio::time::interval(idle);
    checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        checks.tick().await;
        let session = Arc::clone(&session);
        let released =
            tokio::task::spawn_blocking(move || session.models().release_idle(idle)).await;
        if matches!(released, Ok(true)) {
            tracing::debug!(idle_seconds = idle.as_secs(), "released the idle models");
        }
    }
}

/// Serves the tools over standard input and output until the client
/// disconnects.
pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let idle = model_idle(std::env::var_os("EBOOK_HERO_MODEL_IDLE"))?;
    // The session makes its cache directory, which is file system work.
    let session = tokio::task::spawn_blocking(Session::new).await?;
    let server = EbookHero::new(session);
    let tasks = server.tasks.clone();
    let releaser = tokio::spawn(release_idle_models(Arc::clone(&server.session), idle));
    let served = server.serve(stdio()).await?.waiting().await;
    releaser.abort();
    tasks.shutdown();
    served?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{FIND_PAGE, READ_MAX_CHARS, READ_NEIGHBOURS_MAX, TOP_K_MAX};
    use std::ffi::OsString;

    #[test]
    fn the_idle_time_comes_from_the_environment_in_whole_seconds() {
        assert_eq!(model_idle(None).unwrap(), MODEL_IDLE);
        assert_eq!(model_idle(Some(OsString::new())).unwrap(), MODEL_IDLE);
        assert_eq!(
            model_idle(Some(OsString::from("90"))).unwrap(),
            Duration::from_secs(90)
        );
        for bad in ["0", "-5", "1.5", "five", " 9"] {
            let error = model_idle(Some(OsString::from(bad)))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("EBOOK_HERO_MODEL_IDLE")
                    && error.contains("whole number of seconds"),
                "{bad}: {error}"
            );
        }
    }

    fn description(tool: &str) -> String {
        EbookHero::tool_router()
            .list_all()
            .into_iter()
            .find(|candidate| candidate.name == tool)
            .unwrap_or_else(|| panic!("no tool {tool}"))
            .description
            .unwrap_or_else(|| panic!("{tool} has no description"))
            .to_string()
    }

    /// The descriptions spell out numbers that `settings` owns.
    #[test]
    fn descriptions_state_the_limits_in_settings() {
        let search = description("search");
        assert!(
            search.contains(&format!("default {TOP_K_DEFAULT}")),
            "{search}"
        );
        assert!(search.contains(&format!("at most {TOP_K_MAX}")), "{search}");
        assert!(
            description("find").contains(&format!("holds {FIND_PAGE} passages")),
            "{}",
            description("find")
        );
        let read = description("read");
        assert!(read.contains(&READ_MAX_CHARS.to_string()), "{read}");
        assert!(
            read.contains(&format!("up to {READ_NEIGHBOURS_MAX} passages")),
            "{read}"
        );
    }

    #[test]
    fn descriptions_say_what_scores_and_ranges_mean() {
        let search = description("search");
        assert!(search.contains("within one call only"), "{search}");
        assert!(search.contains("reranker logit"), "{search}");
        for tool in ["search", "find", "verify_quote"] {
            let text = description(tool);
            assert!(text.contains("Unicode code points"), "{tool}: {text}");
        }
        let verify = description("verify_quote");
        for word in ["exact", "equivalent", "none", "found_elsewhere"] {
            assert!(verify.contains(word), "{verify}");
        }
    }

    #[test]
    fn the_instructions_name_every_tool_a_model_must_pick() {
        for tool in ["search", "find", "read", "verify_quote", "concept_graph"] {
            assert!(INSTRUCTIONS.contains(&format!("`{tool}`")), "{tool}");
        }
    }
}
