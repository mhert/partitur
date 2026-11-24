//! The MCP tool server that every session calls over streamable HTTP.
//!
//! Every session has its own URL, `{base}/{session}`; an orchestrator session uses
//! `{base}/{session}/orchestrator` and a child session `{base}/{session}/child`. The agent's
//! MCP client POSTs JSON-RPC 2.0 messages there and receives plain JSON answers; the server
//! offers no event stream, so a `GET` is refused with `405`. Every URL lists and serves the
//! workflow reporting tools of [`workflow::tools`], whose calls [`workflow::parse`] reads, and
//! the tools of core's tool catalog offered to its role (`tools::offered`): the peer tools,
//! `list_sessions` and `read_session` on every URL and `propose_session` on the plain URL
//! alone, then a title tool, `rename_session` on the plain and the orchestrator URL, with a
//! `session_id` only an orchestrator may pass, and `propose_title` on the child URL, and the
//! `show_svg` and `show_html` drawing tools and the `ask_user` tool last. `show_html` is
//! listed and served only once the server's [`HtmlOffer`] is enabled, that is, while a widget
//! engine runs; before that a call of it is an error result, `HTML cannot be shown here`. Only the orchestrator URL also lists and serves the orchestration
//! tools, before the workflow tools, and the run-level workflow tools. A call of a tool the
//! URL does not list is an unknown tool. A call is parsed, sent on as a [`ToolCall`] and
//! answered through its reply, or with an error result when no answer arrives in time.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures::channel::mpsc::UnboundedSender;
use partitur_core::tools as catalog;
use partitur_core::workflow;
use partitur_core::{
    Drawing, DrawingKind, GateDecision, OrchestratorTool, PartiturTool, PeerTool, ProposedChild,
    Question, QuestionOption, RepositoryId, SessionId, SessionTool, TOOL_SERVER, TitleSource,
    ToolCall, ToolRole, ToolSpec,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::HtmlOffer;
use crate::reply::OneshotReply;

/// The only JSON-RPC version the server speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The JSON-RPC error code for a method the server does not offer.
const METHOD_NOT_FOUND: i64 = -32601;
/// The message of [`METHOD_NOT_FOUND`].
const METHOD_NOT_FOUND_MESSAGE: &str = "method not found";

/// The error result of a `read_session` call for page 0.
const PAGE_AT_LEAST_ONE: &str = "page must be at least 1";
/// The error result of a `rename_session` call with a `session_id` from a session that is no
/// orchestrator.
const SESSION_ID_FOR_ORCHESTRATORS: &str = "session_id is only for an orchestrator";
/// The error result of a call that was not answered within the answer timeout.
const ANSWER_TIMED_OUT: &str = "Partitur did not answer the tool call in time";

/// The error result of a `show_html` call while no widget engine runs.
const HTML_UNAVAILABLE: &str = "HTML cannot be shown here";

/// What every request handler shares.
#[derive(Debug, Clone)]
struct Server {
    calls: UnboundedSender<ToolCall>,
    answer_timeout: Duration,
    html: HtmlOffer,
}

/// The arguments of `propose_children`, before the repositories are checked.
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

/// The arguments of `send_to_child`, before the session id is parsed.
#[derive(Debug, Deserialize)]
struct SendToChildArguments {
    session_id: String,
    /// The prompt; may be left out with an approval.
    #[serde(default)]
    prompt: String,
    /// The decision on the child's open gate, `approve` or `changes`.
    #[serde(default)]
    decision: Option<GateDecision>,
}

/// The arguments of `cancel_child` and `get_child_result`, before the session id is
/// parsed.
#[derive(Debug, Deserialize)]
struct SessionArguments {
    session_id: String,
}

/// The arguments of `propose_session`, before the repository is checked.
#[derive(Debug, Deserialize)]
struct ProposeSessionArguments {
    /// The title; left out, the core derives one from the prompt.
    #[serde(default)]
    title: String,
    prompt: String,
    repository: Option<String>,
    base: Option<String>,
}

/// The arguments of `read_session`, before the session id is parsed.
#[derive(Debug, Deserialize)]
struct ReadSessionArguments {
    session_id: String,
    page: Option<usize>,
}

/// The arguments of `show_svg`; the core checks the markup.
#[derive(Deserialize)]
struct SvgArguments {
    svg: String,
}

/// The arguments of `show_html`; the core checks the markup.
#[derive(Deserialize)]
struct HtmlArguments {
    html: String,
}

/// The arguments of `ask_user`; the core checks the question and the options.
#[derive(Debug, Deserialize)]
struct AskUserArguments {
    question: String,
    options: Vec<QuestionOption>,
}

/// The arguments of `rename_session`, before the session id is parsed.
#[derive(Debug, Deserialize)]
struct RenameArguments {
    title: String,
    session_id: Option<String>,
}

/// The arguments of `propose_title`.
#[derive(Debug, Deserialize)]
struct TitleArguments {
    title: String,
}

/// Builds the tool server: `POST /{session}` takes the JSON-RPC messages of one plain
/// session, `POST /{session}/orchestrator` those of an orchestrator and
/// `POST /{session}/child` those of a child, `GET` on any of them is
/// refused with `405` and every other path is `404`. A tool call is sent on `calls` and
/// waits `answer_timeout` for its answer. `show_html` is listed and served only once `html` is
/// enabled.
pub(crate) fn router(
    calls: UnboundedSender<ToolCall>,
    answer_timeout: Duration,
    html: HtmlOffer,
) -> Router {
    Router::new()
        .route("/{session}", post(handle_plain))
        .route("/{session}/orchestrator", post(handle_orchestrator))
        .route("/{session}/child", post(handle_child))
        .with_state(Server {
            calls,
            answer_timeout,
            html,
        })
}

/// Answers one JSON-RPC message posted for the plain session `session`.
async fn handle_plain(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
    handle(&server, &session, &message, ToolRole::Plain).await
}

/// Answers one JSON-RPC message posted for the orchestrator session `session`.
async fn handle_orchestrator(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
    handle(&server, &session, &message, ToolRole::Orchestrator).await
}

/// Answers one JSON-RPC message posted for the child session `session`.
async fn handle_child(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
    handle(&server, &session, &message, ToolRole::Child).await
}

/// Answers one JSON-RPC message posted for `session`, which is offered the tools of `role`.
async fn handle(server: &Server, session: &str, message: &Value, role: ToolRole) -> Response {
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
        "tools/list" => Ok(json!({ "tools": tools(role, server.html.enabled()) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = call_tool(session, name, arguments, role, server).await;
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

/// The tools the server offers a session in `role`, as `tools/list` describes them: the
/// orchestration tools of [`catalog::offered`] for an orchestrator, then the workflow
/// tools, then the other tools of [`catalog::offered`] for the role in catalog order, without
/// `show_html` unless `html` is set.
fn tools(role: ToolRole, html: bool) -> Value {
    let orchestrator = role == ToolRole::Orchestrator;
    let listed = |spec: &ToolSpec| {
        json!({
            "name": spec.name,
            "description": spec.description_for(role),
            "inputSchema": input_schema(spec.name, role),
        })
    };
    let (orchestration, others): (Vec<_>, Vec<_>) = catalog::offered(role)
        .filter(|spec| html || spec.name != DrawingKind::Html.tool())
        .partition(|spec| spec.is_orchestration());
    let reporting = workflow::tools(orchestrator).into_iter().map(|tool| {
        json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": (tool.schema)(),
        })
    });
    orchestration
        .into_iter()
        .map(listed)
        .chain(reporting)
        .chain(others.into_iter().map(listed))
        .collect()
}

/// The JSON schema of the arguments of the tool that shows a drawing of `kind`: its markup,
/// under the tool's argument name.
fn drawing_schema(kind: DrawingKind) -> Value {
    json!({
        "type": "object",
        "properties": { kind.argument(): { "type": "string" } },
        "required": [kind.argument()],
    })
}

/// The JSON schema of the arguments of `ask_user`.
fn ask_user_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "question": { "type": "string" },
            "options": {
                "type": "array",
                "minItems": 2,
                "maxItems": 6,
                "items": {
                    "type": "object",
                    "properties": {
                        "label": {
                            "type": "string",
                            "description": "The choice, which is also the answer when picked",
                        },
                        "description": {
                            "type": "string",
                            "description": "A longer explanation of the choice",
                        },
                    },
                    "required": ["label"],
                },
            },
        },
        "required": ["question", "options"],
    })
}

/// The JSON schema of the arguments of the catalogued tool `name` as a session in `role` is
/// offered it: `rename_session` takes a `session_id` from an orchestrator alone.
fn input_schema(name: &str, role: ToolRole) -> Value {
    let title = json!({ "type": "string" });
    match name {
        "propose_children" => json!({
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
        "send_to_child" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string" },
                "prompt": { "type": "string" },
                "decision": { "type": "string", "enum": ["approve", "changes"] },
            },
            "required": ["session_id"],
        }),
        "list_children" | "list_sessions" => json!({ "type": "object", "properties": {} }),
        "propose_session" => json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "A short title for the new session" },
                "prompt": { "type": "string" },
                "repository": {
                    "type": "string",
                    "description": "The repository's directory name; defaults to yours",
                },
                "base": {
                    "type": "string",
                    "description": "A local branch a new branch starts from",
                },
            },
            "required": ["prompt"],
        }),
        "read_session" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string" },
                "page": { "type": "integer", "minimum": 1 },
            },
            "required": ["session_id"],
        }),
        "cancel_child" | "get_child_result" => json!({
            "type": "object",
            "properties": { "session_id": { "type": "string" } },
            "required": ["session_id"],
        }),
        "rename_session" if role == ToolRole::Orchestrator => json!({
            "type": "object",
            "properties": { "title": title, "session_id": { "type": "string" } },
            "required": ["title"],
        }),
        "rename_session" | "propose_title" => json!({
            "type": "object",
            "properties": { "title": title },
            "required": ["title"],
        }),
        "show_svg" => drawing_schema(DrawingKind::Svg),
        "show_html" => drawing_schema(DrawingKind::Html),
        "ask_user" => ask_user_schema(),
        _ => Value::Null,
    }
}

/// Runs the tool `name` for `session` and returns the result text and whether it is an
/// error. A call that parses is sent on and waits for its answer.
async fn call_tool(
    session: SessionId,
    name: &str,
    arguments: Value,
    role: ToolRole,
    server: &Server,
) -> (String, bool) {
    let tool = match parse_tool(name, arguments, role, server.html.enabled()) {
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
/// Returns the error result text when `name` is a tool of [`catalog::CATALOG`] not offered to
/// a session in `role`, or no such tool and no workflow tool offered to it, or `arguments` do
/// not fit it, or `name` is `show_html` while `html` is not set.
fn parse_tool(
    name: &str,
    arguments: Value,
    role: ToolRole,
    html: bool,
) -> Result<PartiturTool, String> {
    let orchestrator = role == ToolRole::Orchestrator;
    if catalog::find(name).is_some_and(|spec| !spec.is_offered_to(role)) {
        return Err(unknown_tool(name));
    }
    if name == DrawingKind::Html.tool() && !html {
        return Err(HTML_UNAVAILABLE.to_owned());
    }
    let tool = match name {
        "propose_children" => {
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
                        base: None,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
                .map(OrchestratorTool::ProposeChildren)?
        }
        "send_to_child" => {
            let SendToChildArguments {
                session_id,
                prompt,
                decision,
            } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::SendToChild {
                child,
                prompt,
                decision,
            }
        }
        "list_children" => OrchestratorTool::ListChildren,
        "cancel_child" => {
            let SessionArguments { session_id } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::CancelChild { child }
        }
        "get_child_result" => {
            let SessionArguments { session_id } = parse_arguments(arguments)?;
            let child = parse_session_id(&session_id)?;
            OrchestratorTool::GetChildResult { child }
        }
        "propose_session" | "list_sessions" | "read_session" => {
            return parse_peer_tool(name, arguments).map(Into::into);
        }
        "rename_session" => {
            let RenameArguments { title, session_id } = parse_arguments(arguments)?;
            let session = match session_id {
                Some(_) if !orchestrator => return Err(SESSION_ID_FOR_ORCHESTRATORS.to_owned()),
                Some(id) => Some(parse_session_id(&id)?),
                None => None,
            };
            return Ok(SessionTool::Rename { title, session }.into());
        }
        "propose_title" => {
            let TitleArguments { title } = parse_arguments(arguments)?;
            return Ok(SessionTool::ProposeTitle { title }.into());
        }
        "show_svg" => {
            let SvgArguments { svg } = parse_arguments(arguments)?;
            return Ok(PartiturTool::Show(Drawing {
                kind: DrawingKind::Svg,
                markup: svg.into(),
            }));
        }
        "show_html" => {
            let HtmlArguments { html } = parse_arguments(arguments)?;
            return Ok(PartiturTool::Show(Drawing {
                kind: DrawingKind::Html,
                markup: html.into(),
            }));
        }
        "ask_user" => {
            let AskUserArguments { question, options } = parse_arguments(arguments)?;
            return Ok(PartiturTool::AskUser(Question {
                text: question,
                options,
            }));
        }
        _ => {
            return workflow::parse(name, arguments, orchestrator)
                .unwrap_or_else(|| Err(unknown_tool(name)))
                .map(PartiturTool::Workflow);
        }
    };
    Ok(tool.into())
}

/// Parses a call of the peer tool `name`.
///
/// # Errors
///
/// Returns the error result text when `arguments` do not fit the tool.
fn parse_peer_tool(name: &str, arguments: Value) -> Result<PeerTool, String> {
    match name {
        "propose_session" => {
            let ProposeSessionArguments {
                title,
                prompt,
                repository,
                base,
            } = parse_arguments(arguments)?;
            let repository = repository
                .map(|value| {
                    RepositoryId::try_from(value.as_str()).map_err(|_| invalid_repository(&value))
                })
                .transpose()?;
            Ok(PeerTool::ProposeSession {
                repository,
                title,
                prompt,
                base,
            })
        }
        "read_session" => {
            let ReadSessionArguments { session_id, page } = parse_arguments(arguments)?;
            let session = parse_session_id(&session_id)?;
            if page == Some(0) {
                return Err(PAGE_AT_LEAST_ONE.to_owned());
            }
            Ok(PeerTool::ReadSession { session, page })
        }
        _ => Ok(PeerTool::ListSessions),
    }
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
mod tests;
