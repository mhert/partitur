//! The MCP tool server that every session calls over streamable HTTP.
//!
//! Every session has its own URL, `{base}/{session}`, and an orchestrator session uses
//! `{base}/{session}/orchestrator`. The agent's MCP client POSTs JSON-RPC 2.0 messages there
//! and receives plain JSON answers; the server offers no event stream, so a `GET` is refused
//! with `405`. Both URLs serve the workflow reporting tools of [`SUPERPOWERS_TOOLS`]; only
//! the orchestrator URL also lists and serves the tools of [`TOOLS`], and a call of one of
//! them on the plain URL is an unknown tool. A call is parsed here, sent on as a
//! [`ToolCall`] and answered through its reply, or with an error result when no answer
//! arrives in time.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures::channel::mpsc::UnboundedSender;
use partitur_core::{
    ExecutionMode, OrchestratorTool, Outcome, PartiturTool, PlannedTask, ProposedChild,
    RepositoryId, SUPERPOWERS_TOOLS, SessionId, SuperpowersEvent, TOOL_SERVER, TOOLS, TitleSource,
    ToolCall,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::reply::OneshotReply;

/// The only JSON-RPC version the server speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The JSON-RPC error code for a method the server does not offer.
const METHOD_NOT_FOUND: i64 = -32601;
/// The message of [`METHOD_NOT_FOUND`].
const METHOD_NOT_FOUND_MESSAGE: &str = "method not found";

/// The tool that proposes child sessions.
const PROPOSE_CHILDREN: &str = "propose_children";
/// The tool that prompts a child session again.
const SEND_TO_CHILD: &str = "send_to_child";
/// The tool that lists the orchestrator's children.
const LIST_CHILDREN: &str = "list_children";
/// The tool that stops a child's current turn.
const CANCEL_CHILD: &str = "cancel_child";
/// The tool that returns a child's last final message.
const GET_CHILD_RESULT: &str = "get_child_result";
/// The error result of a call that was not answered within the answer timeout.
const ANSWER_TIMED_OUT: &str = "Partitur did not answer the tool call in time";

/// What every request handler shares.
#[derive(Debug, Clone)]
struct Server {
    calls: UnboundedSender<ToolCall>,
    answer_timeout: Duration,
}

/// The arguments of [`PROPOSE_CHILDREN`], before the repositories are checked.
#[derive(Debug, Deserialize)]
struct ProposeChildrenArguments {
    children: Vec<ChildArguments>,
}

/// One proposed child, before its repository is checked.
#[derive(Debug, Deserialize)]
struct ChildArguments {
    repository: String,
    prompt: String,
    title: String,
}

/// The arguments of [`SEND_TO_CHILD`], before the session id is parsed.
#[derive(Debug, Deserialize)]
struct SendToChildArguments {
    session_id: String,
    prompt: String,
}

/// The arguments of [`CANCEL_CHILD`] and [`GET_CHILD_RESULT`], before the session id is
/// parsed.
#[derive(Debug, Deserialize)]
struct SessionArguments {
    session_id: String,
}

/// The arguments of the superpowers tools that report a topic.
#[derive(Debug, Deserialize)]
struct TopicArguments {
    topic: String,
}

/// The arguments of the superpowers tool that requests the design's approval.
#[derive(Debug, Deserialize)]
struct PathArguments {
    path: PathBuf,
}

/// The arguments of the superpowers tool that requests the plan's approval.
#[derive(Debug, Deserialize)]
struct PlanArguments {
    path: PathBuf,
    tasks: Vec<PlannedTask>,
    checkpoints: Vec<u32>,
}

/// The arguments of the superpowers tool that starts the execution.
#[derive(Debug, Deserialize)]
struct ModeArguments {
    mode: ExecutionMode,
}

/// The arguments of the superpowers tools that report a step of one task.
#[derive(Debug, Deserialize)]
struct TaskArguments {
    n: u32,
    note: Option<String>,
}

/// The arguments of the superpowers tool that reports a checkpoint.
#[derive(Debug, Deserialize)]
struct CheckpointArguments {
    after_task: u32,
}

/// The arguments of the superpowers tool that finishes the session.
#[derive(Debug, Deserialize)]
struct OutcomeArguments {
    outcome: Outcome,
}

/// Builds the tool server: `POST /{session}` takes the JSON-RPC messages of one plain
/// session, `POST /{session}/orchestrator` those of an orchestrator, `GET` on either is
/// refused with `405` and every other path is `404`. A tool call is sent on `calls` and
/// waits `answer_timeout` for its answer.
pub(crate) fn router(calls: UnboundedSender<ToolCall>, answer_timeout: Duration) -> Router {
    Router::new()
        .route("/{session}", post(handle_plain))
        .route("/{session}/orchestrator", post(handle_orchestrator))
        .with_state(Server {
            calls,
            answer_timeout,
        })
}

/// Answers one JSON-RPC message posted for the plain session `session`.
async fn handle_plain(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
    handle(&server, &session, &message, false).await
}

/// Answers one JSON-RPC message posted for the orchestrator session `session`.
async fn handle_orchestrator(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
    handle(&server, &session, &message, true).await
}

/// Answers one JSON-RPC message posted for `session`, which offers the orchestration tools
/// when `orchestrator` is set.
async fn handle(server: &Server, session: &str, message: &Value, orchestrator: bool) -> Response {
    let Ok(session) = session.parse::<SessionId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if message.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(id) = message.get("id") else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").unwrap_or(&Value::Null);
    let outcome = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(orchestrator) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = call_tool(session, name, arguments, orchestrator, server).await;
            Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "isError": is_error,
            }))
        }
        _ => Err(json!({ "code": METHOD_NOT_FOUND, "message": METHOD_NOT_FOUND_MESSAGE })),
    };
    let reply = match outcome {
        Ok(result) => json!({ "jsonrpc": JSON_RPC_VERSION, "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": JSON_RPC_VERSION, "id": id, "error": error }),
    };
    Json(reply).into_response()
}

/// The `initialize` result: the client's protocol version back, and the tools capability.
fn initialize(params: &Value) -> Value {
    json!({
        "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(Value::Null),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": TOOL_SERVER, "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The tools the server offers a session, as `tools/list` describes them: the orchestration
/// tools of an orchestrator, then the superpowers tools.
fn tools(orchestrator: bool) -> Value {
    let orchestration: &[_] = if orchestrator { TOOLS } else { &[] };
    orchestration
        .iter()
        .chain(SUPERPOWERS_TOOLS)
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "inputSchema": input_schema(spec.name),
            })
        })
        .collect()
}

/// The JSON schema of the arguments of the tool `name`.
fn input_schema(name: &str) -> Value {
    match name {
        PROPOSE_CHILDREN => json!({
            "type": "object",
            "properties": {
                "children": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "repository": {
                                "type": "string",
                                "description": "The repository's directory name",
                            },
                            "prompt": { "type": "string" },
                            "title": {
                                "type": "string",
                                "description": "A short title for the child session",
                            },
                        },
                        "required": ["repository", "prompt", "title"],
                    },
                },
            },
            "required": ["children"],
        }),
        SEND_TO_CHILD => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string" },
                "prompt": { "type": "string" },
            },
            "required": ["session_id", "prompt"],
        }),
        LIST_CHILDREN => json!({ "type": "object", "properties": {} }),
        CANCEL_CHILD | GET_CHILD_RESULT => json!({
            "type": "object",
            "properties": { "session_id": { "type": "string" } },
            "required": ["session_id"],
        }),
        _ => superpowers_schema(name),
    }
}

/// The JSON schema of the arguments of the superpowers tool `name`.
fn superpowers_schema(name: &str) -> Value {
    let number =
        |description: &str| json!({ "type": "integer", "minimum": 1, "description": description });
    let task_step = json!({
        "type": "object",
        "properties": {
            "n": number("The task's number in the plan"),
            "note": { "type": "string", "description": "A short remark" },
        },
        "required": ["n"],
    });
    match name {
        "superpowers_start_session" => json!({
            "type": "object",
            "properties": { "topic": { "type": "string" } },
            "required": ["topic"],
        }),
        "superpowers_start_brainstorming"
        | "superpowers_start_writing_plan"
        | "superpowers_start_finishing_branch" => json!({ "type": "object", "properties": {} }),
        "superpowers_request_design_approval" => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The design document, relative to your working folder",
                },
            },
            "required": ["path"],
        }),
        "superpowers_request_plan_approval" => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The plan document, relative to your working folder",
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "n": number("The task's number in the plan"),
                            "title": { "type": "string" },
                            "files": { "type": "array", "items": { "type": "string" } },
                        },
                        "required": ["n", "title", "files"],
                    },
                },
                "checkpoints": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "The task numbers after which you stop for the user",
                },
            },
            "required": ["path", "tasks", "checkpoints"],
        }),
        "superpowers_start_execution" => json!({
            "type": "object",
            "properties": { "mode": { "type": "string", "enum": ["subagent", "inline"] } },
            "required": ["mode"],
        }),
        "superpowers_start_task"
        | "superpowers_write_failing_test"
        | "superpowers_make_test_pass"
        | "superpowers_start_refactoring"
        | "superpowers_finish_task" => task_step,
        "superpowers_reach_checkpoint" => json!({
            "type": "object",
            "properties": { "after_task": number("The task the checkpoint follows") },
            "required": ["after_task"],
        }),
        "superpowers_finish_session" => json!({
            "type": "object",
            "properties": {
                "outcome": {
                    "type": "string",
                    "enum": ["merged", "pr_opened", "kept", "discarded"],
                },
            },
            "required": ["outcome"],
        }),
        _ => Value::Null,
    }
}

/// Runs the tool `name` for `session` and returns the result text and whether it is an
/// error. A call that parses is sent on and waits for its answer.
async fn call_tool(
    session: SessionId,
    name: &str,
    arguments: Value,
    orchestrator: bool,
    server: &Server,
) -> (String, bool) {
    let tool = match parse_tool(name, arguments, orchestrator) {
        Ok(tool) => tool,
        Err(text) => return (text, true),
    };
    let (reply, answer) = OneshotReply::new();
    let call = ToolCall {
        session,
        tool,
        reply: Arc::new(reply),
    };
    if server.calls.unbounded_send(call).is_err() {
        // The call is dropped with its reply, which ends the wait below at once.
        tracing::warn!("a tool call arrived with nobody to answer it");
    }
    match tokio::time::timeout(server.answer_timeout, answer).await {
        Ok(Ok(Ok(text))) => (text, false),
        Ok(Ok(Err(text))) => (text, true),
        // A dropped reply can never deliver, which is a timeout that came early.
        Ok(Err(_)) | Err(_) => (ANSWER_TIMED_OUT.to_owned(), true),
    }
}

/// Parses a call of the tool `name`.
///
/// # Errors
///
/// Returns the error result text when `name` is no tool offered to the session, which is no
/// orchestration tool unless `orchestrator` is set, or `arguments` do not fit it.
fn parse_tool(name: &str, arguments: Value, orchestrator: bool) -> Result<PartiturTool, String> {
    if !orchestrator && TOOLS.iter().any(|spec| spec.name == name) {
        return Err(unknown_tool(name));
    }
    let tool = match name {
        PROPOSE_CHILDREN => {
            let ProposeChildrenArguments { children } = parse_arguments(arguments)?;
            children
                .into_iter()
                .map(|child| {
                    let repository = RepositoryId::try_from(child.repository.as_str())
                        .map_err(|_| invalid_repository(&child.repository))?;
                    Ok(ProposedChild {
                        repository,
                        prompt: child.prompt,
                        title: child.title,
                        title_source: TitleSource::Given,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
                .map(OrchestratorTool::ProposeChildren)?
        }
        SEND_TO_CHILD => {
            let SendToChildArguments { session_id, prompt } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::SendToChild { child, prompt }
        }
        LIST_CHILDREN => OrchestratorTool::ListChildren,
        CANCEL_CHILD => {
            let SessionArguments { session_id } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::CancelChild { child }
        }
        GET_CHILD_RESULT => {
            let SessionArguments { session_id } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::GetChildResult { child }
        }
        _ => return parse_superpowers_tool(name, arguments).map(PartiturTool::Superpowers),
    };
    Ok(tool.into())
}

/// Parses a call of the superpowers tool `name`.
///
/// # Errors
///
/// Returns the error result text when `name` is no superpowers tool or `arguments` do not
/// fit it.
fn parse_superpowers_tool(name: &str, arguments: Value) -> Result<SuperpowersEvent, String> {
    let event = match name {
        "superpowers_start_session" => {
            let TopicArguments { topic } = parse_arguments(arguments)?;
            SuperpowersEvent::StartSession { topic }
        }
        "superpowers_start_brainstorming" => SuperpowersEvent::StartBrainstorming,
        "superpowers_request_design_approval" => {
            let PathArguments { path } = parse_arguments(arguments)?;
            SuperpowersEvent::RequestDesignApproval { path }
        }
        "superpowers_start_writing_plan" => SuperpowersEvent::StartWritingPlan,
        "superpowers_request_plan_approval" => {
            let PlanArguments {
                path,
                tasks,
                checkpoints,
            } = parse_arguments(arguments)?;
            SuperpowersEvent::RequestPlanApproval {
                path,
                tasks,
                checkpoints,
            }
        }
        "superpowers_start_execution" => {
            let ModeArguments { mode } = parse_arguments(arguments)?;
            SuperpowersEvent::StartExecution { mode }
        }
        "superpowers_start_task" => {
            let TaskArguments { n, note } = parse_arguments(arguments)?;
            SuperpowersEvent::StartTask { n, note }
        }
        "superpowers_write_failing_test" => {
            let TaskArguments { n, note } = parse_arguments(arguments)?;
            SuperpowersEvent::WriteFailingTest { n, note }
        }
        "superpowers_make_test_pass" => {
            let TaskArguments { n, note } = parse_arguments(arguments)?;
            SuperpowersEvent::MakeTestPass { n, note }
        }
        "superpowers_start_refactoring" => {
            let TaskArguments { n, note } = parse_arguments(arguments)?;
            SuperpowersEvent::StartRefactoring { n, note }
        }
        "superpowers_finish_task" => {
            let TaskArguments { n, note } = parse_arguments(arguments)?;
            SuperpowersEvent::FinishTask { n, note }
        }
        "superpowers_reach_checkpoint" => {
            let CheckpointArguments { after_task } = parse_arguments(arguments)?;
            SuperpowersEvent::ReachCheckpoint { after_task }
        }
        "superpowers_start_finishing_branch" => SuperpowersEvent::StartFinishingBranch,
        "superpowers_finish_session" => {
            let OutcomeArguments { outcome } = parse_arguments(arguments)?;
            SuperpowersEvent::FinishSession { outcome }
        }
        _ => return Err(unknown_tool(name)),
    };
    Ok(event)
}

/// Reads a tool's `arguments` into `T`.
///
/// # Errors
///
/// Returns serde's message when an argument is missing or of the wrong type.
fn parse_arguments<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| error.to_string())
}

/// The error result for a proposed repository that is no directory name.
fn invalid_repository(value: &str) -> String {
    format!("repository \"{value}\" must be a directory name")
}

/// The session id `value` names, or the error result for a `session_id` that is no session id.
fn parse_session_id(value: &str) -> Result<SessionId, String> {
    value.parse().map_err(|_| invalid_session_id(value))
}

/// The error result for a `session_id` that is no session id.
fn invalid_session_id(value: &str) -> String {
    format!("session_id \"{value}\" is not a session id")
}

/// The error result for a tool this server does not offer.
fn unknown_tool(name: &str) -> String {
    format!("unknown tool {name}")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use futures::StreamExt;
    use futures::channel::mpsc::{UnboundedReceiver, unbounded};
    use http_body_util::BodyExt;
    use partitur_core::{
        ExecutionMode, OrchestratorTool, Outcome, PlannedTask, ProposedChild, RepositoryId,
        SessionId, SuperpowersEvent, ToolSpec,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    /// A router under test, the calls it reports and the session it is posted for.
    struct Harness {
        router: Router,
        calls: UnboundedReceiver<ToolCall>,
        session: SessionId,
    }

    fn harness_with(answer_timeout: Duration) -> Harness {
        let (sender, calls) = unbounded();
        Harness {
            router: router(sender, answer_timeout),
            calls,
            session: SessionId::new(),
        }
    }

    fn harness() -> Harness {
        harness_with(Duration::from_millis(200))
    }

    /// The status, the content type and the body of one response.
    struct Answer {
        status: StatusCode,
        content_type: Option<String>,
        body: Vec<u8>,
    }

    impl Answer {
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    async fn send(router: &Router, method: Method, path: &str, body: &str) -> Answer {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Answer {
            status,
            content_type,
            body: body.to_vec(),
        }
    }

    /// Posts `body` to the URL of an orchestrator session.
    async fn post(harness: &Harness, body: &Value) -> Answer {
        let path = format!("/{}/orchestrator", harness.session);
        send(&harness.router, Method::POST, &path, &body.to_string()).await
    }

    /// Posts `body` to the URL of a plain session.
    async fn post_plain(harness: &Harness, body: &Value) -> Answer {
        let path = format!("/{}", harness.session);
        send(&harness.router, Method::POST, &path, &body.to_string()).await
    }

    fn call(name: &str, arguments: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
    }

    fn tool_result(text: &str, is_error: bool) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {"content": [{"type": "text", "text": text}], "isError": is_error},
        })
    }

    fn propose_api() -> Value {
        json!({"children": [
            {"repository": "api", "prompt": "Add the endpoint", "title": "API endpoint"}
        ]})
    }

    /// The next call the router reported, or `None` when none arrives within a second, so a
    /// call that never reaches the core fails the test instead of hanging it.
    async fn next_call(harness: &mut Harness) -> Option<ToolCall> {
        tokio::time::timeout(Duration::from_secs(1), harness.calls.next())
            .await
            .ok()
            .flatten()
    }

    /// Posts `body` to an orchestrator URL on a task of its own, so the test can answer the
    /// call it reports.
    fn spawn_post(harness: &Harness, body: Value) -> tokio::task::JoinHandle<Answer> {
        let router = harness.router.clone();
        let path = format!("/{}/orchestrator", harness.session);
        tokio::spawn(async move { send(&router, Method::POST, &path, &body.to_string()).await })
    }

    #[tokio::test]
    async fn initialize_echoes_the_protocol_version_and_offers_tools() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "claude-code", "version": "2"},
                },
            }),
        )
        .await;

        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            answer.json(),
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "partitur", "version": env!("CARGO_PKG_VERSION")},
                },
            })
        );
    }

    #[tokio::test]
    async fn initialized_is_accepted_without_a_body() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;

        assert_eq!(answer.status, StatusCode::ACCEPTED);
        assert!(answer.body.is_empty(), "body {:?}", answer.body);
    }

    #[tokio::test]
    async fn a_get_is_refused_with_405() {
        let harness = harness();
        for path in [
            format!("/{}", harness.session),
            format!("/{}/orchestrator", harness.session),
        ] {
            let answer = send(&harness.router, Method::GET, &path, "").await;

            assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED, "for {path}");
        }
    }

    #[tokio::test]
    async fn ping_answers_an_empty_result() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({"jsonrpc": "2.0", "id": "p", "method": "ping"}),
        )
        .await;

        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(
            answer.json(),
            json!({"jsonrpc": "2.0", "id": "p", "result": {}})
        );
    }

    #[tokio::test]
    async fn tools_list_names_the_orchestration_tools_with_their_schemas() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;

        assert_eq!(answer.status, StatusCode::OK);
        let tools: Value = serde_json::from_str(
            r#"[
                {
                    "name": "propose_children",
                    "description": "proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.",
                    "inputSchema": {"type":"object","properties":{"children":{"type":"array","minItems":1,"items":{"type":"object","properties":{"repository":{"type":"string","description":"The repository's directory name"},"prompt":{"type":"string"},"title":{"type":"string","description":"A short title for the child session"}},"required":["repository","prompt","title"]}}},"required":["children"]}
                },
                {
                    "name": "send_to_child",
                    "description": "sends a follow-up prompt to a child; it is queued while the child is mid-turn.",
                    "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"},"prompt":{"type":"string"}},"required":["session_id","prompt"]}
                },
                {
                    "name": "list_children",
                    "description": "lists your children with their title, repository, session id and status.",
                    "inputSchema": {"type":"object","properties":{}}
                },
                {
                    "name": "cancel_child",
                    "description": "stops a child's current turn; its result then reaches you as a child result marked cancelled.",
                    "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}
                },
                {
                    "name": "get_child_result",
                    "description": "returns a child's last final message and its status.",
                    "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}
                }
            ]"#,
        )
        .unwrap();
        let listed = answer.json()["result"]["tools"].clone();
        assert_eq!(
            listed.as_array().unwrap()[..TOOLS.len()],
            tools.as_array().unwrap()[..]
        );
    }

    #[tokio::test]
    async fn tools_list_describes_every_catalogued_tool() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;

        let tools = answer.json()["result"]["tools"].clone();
        for (tool, spec) in tools.as_array().unwrap().iter().zip(catalogue()) {
            assert_eq!(tool["name"], spec.name);
            assert_eq!(tool["description"], spec.description);
            assert!(!tool["inputSchema"].is_null(), "{}", spec.name);
        }
    }

    /// Both catalogues, orchestration tools first.
    fn catalogue() -> Vec<&'static ToolSpec> {
        TOOLS.iter().chain(SUPERPOWERS_TOOLS).collect()
    }

    /// The names `tools/list` offered in `answer`.
    fn listed_names(answer: &Answer) -> Vec<String> {
        answer.json()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }

    fn names(specs: impl IntoIterator<Item = &'static ToolSpec>) -> Vec<String> {
        specs.into_iter().map(|spec| spec.name.to_owned()).collect()
    }

    #[tokio::test]
    async fn a_plain_session_lists_only_the_superpowers_tools() {
        let harness = harness();

        let answer = post_plain(
            &harness,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;

        assert_eq!(listed_names(&answer), names(SUPERPOWERS_TOOLS));
    }

    #[tokio::test]
    async fn an_orchestrator_lists_both_catalogues() {
        let harness = harness();

        let answer = post(
            &harness,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;

        assert_eq!(listed_names(&answer), names(catalogue()));
    }

    #[tokio::test]
    async fn orchestration_tools_are_unknown_on_a_plain_url() {
        let mut harness = harness();

        let answer = post_plain(&harness, &call("list_children", &json!({}))).await;

        assert_eq!(
            answer.json(),
            tool_result("unknown tool list_children", true)
        );
        assert!(
            harness.calls.try_recv().is_err(),
            "the call reached the core"
        );
    }

    /// A minimal valid call of each superpowers tool that reports a workflow stage, with the
    /// event it must parse into.
    fn workflow_cases() -> Vec<(&'static str, Value, SuperpowersEvent)> {
        use SuperpowersEvent as E;
        vec![
            (
                "superpowers_start_session",
                json!({"topic": "Billing"}),
                E::StartSession {
                    topic: "Billing".into(),
                },
            ),
            (
                "superpowers_start_brainstorming",
                json!({}),
                E::StartBrainstorming,
            ),
            (
                "superpowers_request_design_approval",
                json!({"path": "docs/design.md"}),
                E::RequestDesignApproval {
                    path: "docs/design.md".into(),
                },
            ),
            (
                "superpowers_start_writing_plan",
                json!({}),
                E::StartWritingPlan,
            ),
            (
                "superpowers_request_plan_approval",
                json!({
                    "path": "plans/p.md",
                    "tasks": [{"n": 1, "title": "A", "files": ["a.rs"]}],
                    "checkpoints": [3],
                }),
                E::RequestPlanApproval {
                    path: "plans/p.md".into(),
                    tasks: vec![PlannedTask {
                        n: 1,
                        title: "A".into(),
                        files: vec!["a.rs".into()],
                    }],
                    checkpoints: vec![3],
                },
            ),
            (
                "superpowers_start_execution",
                json!({"mode": "subagent"}),
                E::StartExecution {
                    mode: ExecutionMode::Subagent,
                },
            ),
            (
                "superpowers_start_execution",
                json!({"mode": "inline"}),
                E::StartExecution {
                    mode: ExecutionMode::Inline,
                },
            ),
        ]
    }

    /// A minimal valid call of each superpowers tool that reports a task's progress, with the
    /// event it must parse into, and the checkpoint and end of the session.
    fn task_cases() -> Vec<(&'static str, Value, SuperpowersEvent)> {
        use SuperpowersEvent as E;
        let n = json!({"n": 2});
        let noted = json!({"n": 2, "note": "why"});
        vec![
            (
                "superpowers_start_task",
                n.clone(),
                E::StartTask { n: 2, note: None },
            ),
            (
                "superpowers_write_failing_test",
                noted.clone(),
                E::WriteFailingTest {
                    n: 2,
                    note: Some("why".into()),
                },
            ),
            (
                "superpowers_make_test_pass",
                n.clone(),
                E::MakeTestPass { n: 2, note: None },
            ),
            (
                "superpowers_start_refactoring",
                noted.clone(),
                E::StartRefactoring {
                    n: 2,
                    note: Some("why".into()),
                },
            ),
            (
                "superpowers_finish_task",
                noted,
                E::FinishTask {
                    n: 2,
                    note: Some("why".into()),
                },
            ),
            (
                "superpowers_reach_checkpoint",
                json!({"after_task": 3}),
                E::ReachCheckpoint { after_task: 3 },
            ),
            (
                "superpowers_start_finishing_branch",
                json!({}),
                E::StartFinishingBranch,
            ),
            (
                "superpowers_finish_session",
                json!({"outcome": "pr_opened"}),
                E::FinishSession {
                    outcome: Outcome::PrOpened,
                },
            ),
        ]
    }

    #[tokio::test]
    async fn superpowers_calls_parse_into_their_events() {
        let mut harness = harness();
        let cases: Vec<_> = workflow_cases().into_iter().chain(task_cases()).collect();
        let covered: std::collections::BTreeSet<_> = cases.iter().map(|case| case.0).collect();
        assert_eq!(covered.len(), SUPERPOWERS_TOOLS.len());

        for (name, arguments, event) in cases {
            assert_eq!(event.tool_name(), name);
            let response = spawn_plain_post(&harness, call(name, &arguments));

            let reported = next_call(&mut harness).await.unwrap();
            assert_eq!(reported.session, harness.session);
            assert_eq!(
                reported.tool,
                PartiturTool::Superpowers(event),
                "for {name}"
            );
            reported.reply.send(Ok("recorded".into()));
            assert_eq!(
                response.await.unwrap().json(),
                tool_result("recorded", false)
            );
        }
    }

    #[tokio::test]
    async fn superpowers_calls_work_on_an_orchestrator_url_too() {
        let mut harness = harness();
        let response = spawn_post(
            &harness,
            call("superpowers_start_brainstorming", &json!({})),
        );

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(
            reported.tool,
            PartiturTool::Superpowers(SuperpowersEvent::StartBrainstorming)
        );
        reported.reply.send(Ok("recorded".into()));
        assert_eq!(
            response.await.unwrap().json(),
            tool_result("recorded", false)
        );
    }

    #[tokio::test]
    async fn malformed_superpowers_arguments_are_tool_errors_without_a_call() {
        let mut harness = harness();
        let cases = [
            ("superpowers_start_execution", json!({"mode": "batch"})),
            ("superpowers_request_design_approval", json!({})),
            ("superpowers_start_task", json!({"n": -1})),
            ("superpowers_finish_session", json!({"outcome": "lost"})),
        ];

        for (name, arguments) in cases {
            let answer = post_plain(&harness, &call(name, &arguments)).await;

            assert_eq!(answer.json()["result"]["isError"], true, "for {name}");
        }
        assert!(
            harness.calls.try_recv().is_err(),
            "a malformed call reached the core"
        );
    }

    /// Posts `body` to a plain URL on a task of its own.
    fn spawn_plain_post(harness: &Harness, body: Value) -> tokio::task::JoinHandle<Answer> {
        let router = harness.router.clone();
        let path = format!("/{}", harness.session);
        tokio::spawn(async move { send(&router, Method::POST, &path, &body.to_string()).await })
    }

    #[test]
    fn a_tool_outside_the_catalogue_has_no_schema() {
        assert_eq!(input_schema("rename_child"), Value::Null);
    }

    #[tokio::test]
    async fn a_propose_children_call_reaches_the_core_and_returns_its_answer() {
        let mut harness = harness();
        let response = spawn_post(&harness, call("propose_children", &propose_api()));

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.session, harness.session);
        assert_eq!(
            reported.tool,
            OrchestratorTool::ProposeChildren(vec![ProposedChild {
                repository: RepositoryId::try_from("api").unwrap(),
                prompt: "Add the endpoint".into(),
                title: "API endpoint".into(),
                title_source: TitleSource::Given,
            }])
            .into()
        );
        assert!(reported.reply.is_waiting());
        reported
            .reply
            .send(Ok("proposal recorded, awaiting approval".into()));

        let answer = response.await.unwrap();
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(
            answer.json(),
            tool_result("proposal recorded, awaiting approval", false)
        );
    }

    #[tokio::test]
    async fn an_error_answer_is_an_error_result() {
        let mut harness = harness();
        let response = spawn_post(&harness, call("propose_children", &propose_api()));

        let reported = next_call(&mut harness).await.unwrap();
        reported.reply.send(Err("unknown repository".into()));

        let answer = response.await.unwrap();
        assert_eq!(answer.json(), tool_result("unknown repository", true));
    }

    #[tokio::test]
    async fn a_send_to_child_call_carries_the_child_id() {
        let mut harness = harness();
        let child = SessionId::new();
        let response = spawn_post(
            &harness,
            call(
                "send_to_child",
                &json!({"session_id": child.to_string(), "prompt": "Also add tests"}),
            ),
        );

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.session, harness.session);
        assert_eq!(
            reported.tool,
            OrchestratorTool::SendToChild {
                child,
                prompt: "Also add tests".into(),
            }
            .into()
        );
        reported.reply.send(Ok("queued".into()));

        assert_eq!(response.await.unwrap().json(), tool_result("queued", false));
    }

    #[tokio::test]
    async fn list_children_parses_without_arguments() {
        let mut harness = harness();
        let response = spawn_post(&harness, call("list_children", &json!({})));

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.session, harness.session);
        assert_eq!(reported.tool, OrchestratorTool::ListChildren.into());
        reported.reply.send(Ok("no children yet".into()));

        assert_eq!(
            response.await.unwrap().json(),
            tool_result("no children yet", false)
        );
    }

    #[tokio::test]
    async fn cancel_child_parses_the_session_id() {
        let mut harness = harness();
        let child = SessionId::new();
        let response = spawn_post(
            &harness,
            call("cancel_child", &json!({"session_id": child.to_string()})),
        );

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.session, harness.session);
        assert_eq!(
            reported.tool,
            OrchestratorTool::CancelChild { child }.into()
        );
        reported.reply.send(Ok("cancel requested".into()));

        assert_eq!(
            response.await.unwrap().json(),
            tool_result("cancel requested", false)
        );
    }

    #[tokio::test]
    async fn get_child_result_parses_the_session_id() {
        let mut harness = harness();
        let child = SessionId::new();
        let response = spawn_post(
            &harness,
            call(
                "get_child_result",
                &json!({"session_id": child.to_string()}),
            ),
        );

        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.session, harness.session);
        assert_eq!(
            reported.tool,
            OrchestratorTool::GetChildResult { child }.into()
        );
        reported.reply.send(Ok("the result".into()));

        assert_eq!(
            response.await.unwrap().json(),
            tool_result("the result", false)
        );
    }

    #[tokio::test]
    async fn malformed_arguments_are_tool_errors_without_a_call() {
        let mut harness = harness();
        let cases = [
            (
                call(
                    "propose_children",
                    &json!({"children": [
                        {"repository": "../api", "prompt": "Add the endpoint", "title": "API endpoint"}
                    ]}),
                ),
                r#"repository "../api" must be a directory name"#.to_owned(),
            ),
            (
                call(
                    "send_to_child",
                    &json!({"session_id": "child-1", "prompt": "Also add tests"}),
                ),
                r#"session_id "child-1" is not a session id"#.to_owned(),
            ),
            (
                call(
                    "send_to_child",
                    &json!({"session_id": SessionId::new().to_string()}),
                ),
                "missing field `prompt`".to_owned(),
            ),
            (
                call("cancel_child", &json!({"session_id": "child-1"})),
                r#"session_id "child-1" is not a session id"#.to_owned(),
            ),
            (
                call("get_child_result", &json!({"session_id": "child-2"})),
                r#"session_id "child-2" is not a session id"#.to_owned(),
            ),
            (
                call("rename_child", &json!({})),
                "unknown tool rename_child".to_owned(),
            ),
        ];

        for (body, text) in cases {
            let answer = post(&harness, &body).await;

            assert_eq!(answer.json(), tool_result(&text, true), "for {body}");
        }
        assert!(
            harness.calls.try_recv().is_err(),
            "a malformed call reached the core"
        );
    }

    #[tokio::test]
    async fn an_unanswered_call_times_out_as_an_error() {
        let mut harness = harness();

        let answer = post(&harness, &call("propose_children", &propose_api())).await;

        assert_eq!(
            answer.json(),
            tool_result("Partitur did not answer the tool call in time", true)
        );
        assert!(harness.calls.try_recv().is_ok());
    }

    #[tokio::test]
    async fn a_reply_after_the_timeout_reports_not_waiting() {
        let mut harness = harness_with(Duration::from_millis(10));

        let answer = post(&harness, &call("propose_children", &propose_api())).await;

        assert_eq!(
            answer.json(),
            tool_result("Partitur did not answer the tool call in time", true)
        );
        let reported = harness.calls.try_recv().unwrap();
        assert!(!reported.reply.is_waiting());
        reported.reply.send(Ok("too late".into()));
    }

    #[tokio::test]
    async fn a_call_nobody_receives_fails_at_once() {
        let harness = harness_with(Duration::from_secs(30));
        let Harness {
            router,
            calls,
            session,
        } = harness;
        drop(calls);
        let body = call("propose_children", &propose_api()).to_string();

        let answer = tokio::time::timeout(
            Duration::from_secs(1),
            send(
                &router,
                Method::POST,
                &format!("/{session}/orchestrator"),
                &body,
            ),
        )
        .await
        .expect("the call waited for a reply nobody can send");

        assert_eq!(
            answer.json(),
            tool_result("Partitur did not answer the tool call in time", true)
        );
    }

    #[tokio::test]
    async fn unknown_methods_sessions_and_bodies_are_rejected() {
        let harness = harness();
        let path = format!("/{}", harness.session);

        let unknown = post(
            &harness,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::OK);
        assert_eq!(
            unknown.json(),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "error": {"code": -32601, "message": "method not found"},
            })
        );

        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let no_session = send(&harness.router, Method::POST, "/not-a-uuid", &ping).await;
        assert_eq!(no_session.status, StatusCode::NOT_FOUND);
        let no_orchestrator_session = send(
            &harness.router,
            Method::POST,
            "/not-a-uuid/orchestrator",
            &ping,
        )
        .await;
        assert_eq!(no_orchestrator_session.status, StatusCode::NOT_FOUND);
        let other_suffix = send(
            &harness.router,
            Method::POST,
            &format!("{path}/other"),
            &ping,
        )
        .await;
        assert_eq!(other_suffix.status, StatusCode::NOT_FOUND);

        let not_json = send(&harness.router, Method::POST, &path, "{").await;
        assert_eq!(not_json.status, StatusCode::BAD_REQUEST);

        let not_json_rpc = send(
            &harness.router,
            Method::POST,
            &path,
            &json!({"id": 1, "method": "ping"}).to_string(),
        )
        .await;
        assert_eq!(not_json_rpc.status, StatusCode::BAD_REQUEST);

        let no_method = send(
            &harness.router,
            Method::POST,
            &path,
            &json!({"jsonrpc": "2.0", "id": 1}).to_string(),
        )
        .await;
        assert_eq!(no_method.status, StatusCode::BAD_REQUEST);
    }
}
