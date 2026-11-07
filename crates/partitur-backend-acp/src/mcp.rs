//! The MCP tool server that orchestrator sessions call over streamable HTTP.
//!
//! Every orchestrator session has its own URL, `{base}/{session}`. The agent's MCP client
//! POSTs JSON-RPC 2.0 messages there and receives plain JSON answers; the server offers no
//! event stream, so a `GET` is refused with `405`. Two tools are served, `propose_children`
//! and `send_to_child`. A call is parsed here, parked in [`PendingTools`], reported to the
//! core as [`BackendEvent::ToolCalled`] and answered with the core's reply, or with an error
//! result when no reply arrives in time.

use std::io;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use partitur_core::{BackendEvent, OrchestratorTool, ProposedChild, RepositoryId, SessionId};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::runtime::{Events, emit};
use crate::tools::PendingTools;

/// The name the tool server reports to MCP clients and is registered under with sessions.
pub(crate) const SERVER_NAME: &str = "partitur";
/// How long a tool call waits for the core's answer before it fails.
pub(crate) const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The only JSON-RPC version the server speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The JSON-RPC error code for a method the server does not offer.
const METHOD_NOT_FOUND: i64 = -32601;
/// The message of [`METHOD_NOT_FOUND`].
const METHOD_NOT_FOUND_MESSAGE: &str = "method not found";

/// The tool that proposes child sessions.
const PROPOSE_CHILDREN: &str = "propose_children";
/// What [`PROPOSE_CHILDREN`] tells the agent about itself.
const PROPOSE_CHILDREN_DESCRIPTION: &str = "Proposes child sessions, one per entry, each in a \
    repository of the project. Partitur records the proposal and shows it to the user for \
    approval; end your turn after calling this. You are woken with the started children and \
    their session ids, or with the rejection.";
/// The tool that prompts a child session again.
const SEND_TO_CHILD: &str = "send_to_child";
/// What [`SEND_TO_CHILD`] tells the agent about itself.
const SEND_TO_CHILD_DESCRIPTION: &str = "Sends a follow-up prompt to a child session started \
    from one of your proposals. Queued while the child is mid-turn.";
/// The error result of a call the core did not answer within the answer timeout.
const ANSWER_TIMED_OUT: &str = "Partitur did not answer the tool call in time";

/// What every request handler shares.
#[derive(Debug, Clone)]
struct Server {
    events: Events,
    tools: PendingTools,
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

/// Builds the tool server: `POST /{session}` takes the JSON-RPC messages of one session,
/// `GET /{session}` is refused with `405` and every other path is `404`. A tool call waits
/// `answer_timeout` for the core's answer, which arrives through `tools`.
pub(crate) fn router(events: Events, tools: PendingTools, answer_timeout: Duration) -> Router {
    Router::new()
        .route("/{session}", post(handle))
        .with_state(Server {
            events,
            tools,
            answer_timeout,
        })
}

/// The URL under `base` that serves the tools of `session`.
pub(crate) fn session_url(base: &str, session: &SessionId) -> String {
    format!("{base}/{session}")
}

/// The server that serves `router` on `listener` until its task is dropped.
///
/// The future never resolves: axum's `serve` retries failed accepts itself and has no error
/// to report, so the caller only ever aborts it.
pub(crate) fn serve(
    listener: TcpListener,
    router: Router,
) -> impl Future<Output = io::Result<()>> + Send {
    axum::serve(listener, router).into_future()
}

/// Answers one JSON-RPC message posted for `session`.
async fn handle(
    State(server): State<Server>,
    Path(session): Path<String>,
    Json(message): Json<Value>,
) -> Response {
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
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = call_tool(session, name, arguments, &server).await;
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
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The tools the server offers, as `tools/list` describes them.
fn tools() -> Value {
    json!([
        {
            "name": PROPOSE_CHILDREN,
            "description": PROPOSE_CHILDREN_DESCRIPTION,
            "inputSchema": {
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
            },
        },
        {
            "name": SEND_TO_CHILD,
            "description": SEND_TO_CHILD_DESCRIPTION,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "prompt": { "type": "string" },
                },
                "required": ["session_id", "prompt"],
            },
        },
    ])
}

/// Runs the tool `name` for `session` and returns the result text and whether it is an
/// error. A call that parses is reported to the core and waits for its answer.
async fn call_tool(
    session: SessionId,
    name: &str,
    arguments: Value,
    server: &Server,
) -> (String, bool) {
    let tool = match parse_tool(name, arguments) {
        Ok(tool) => tool,
        Err(text) => return (text, true),
    };
    let (call, answer) = server.tools.open();
    emit(
        &server.events,
        BackendEvent::ToolCalled {
            call,
            session,
            tool,
        },
    );
    match tokio::time::timeout(server.answer_timeout, answer).await {
        Ok(Ok(Ok(text))) => (text, false),
        Ok(Ok(Err(text))) => (text, true),
        // A dropped answer sender can never deliver, which is a timeout that came early.
        Ok(Err(_)) | Err(_) => (ANSWER_TIMED_OUT.to_owned(), true),
    }
}

/// Parses a call of the tool `name`.
///
/// # Errors
///
/// Returns the error result text when `name` is no tool of this server or `arguments` do
/// not fit it.
fn parse_tool(name: &str, arguments: Value) -> Result<OrchestratorTool, String> {
    match name {
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
                    })
                })
                .collect::<Result<_, _>>()
                .map(OrchestratorTool::ProposeChildren)
        }
        SEND_TO_CHILD => {
            let SendToChildArguments { session_id, prompt } = parse_arguments(arguments)?;
            let child = session_id
                .parse::<SessionId>()
                .map_err(|_| invalid_session_id(&session_id))?;
            Ok(OrchestratorTool::SendToChild { child, prompt })
        }
        _ => Err(unknown_tool(name)),
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
        BackendEvent, OrchestratorTool, ProposedChild, RepositoryId, SessionId, ToolRequestId,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tower::ServiceExt;

    use super::*;

    /// A router under test, the inbox its events arrive in and the calls it parks.
    struct Harness {
        router: Router,
        inbox: UnboundedReceiver<BackendEvent>,
        tools: PendingTools,
        session: SessionId,
    }

    fn harness() -> Harness {
        let (events, inbox) = unbounded();
        let tools = PendingTools::default();
        Harness {
            router: router(events, tools.clone(), Duration::from_millis(200)),
            inbox,
            tools,
            session: SessionId::new(),
        }
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

    async fn post(harness: &Harness, body: &Value) -> Answer {
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

    /// Posts `body` on a task of its own, so the test can answer the call it parks.
    fn spawn_post(harness: &Harness, body: Value) -> tokio::task::JoinHandle<Answer> {
        let router = harness.router.clone();
        let path = format!("/{}", harness.session);
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
        let path = format!("/{}", harness.session);

        let answer = send(&harness.router, Method::GET, &path, "").await;

        assert_eq!(answer.status, StatusCode::METHOD_NOT_ALLOWED);
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
    async fn tools_list_names_both_tools_with_their_schemas() {
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
                    "description": "Proposes child sessions, one per entry, each in a repository of the project. Partitur records the proposal and shows it to the user for approval; end your turn after calling this. You are woken with the started children and their session ids, or with the rejection.",
                    "inputSchema": {"type":"object","properties":{"children":{"type":"array","minItems":1,"items":{"type":"object","properties":{"repository":{"type":"string","description":"The repository's directory name"},"prompt":{"type":"string"},"title":{"type":"string","description":"A short title for the child session"}},"required":["repository","prompt","title"]}}},"required":["children"]}
                },
                {
                    "name": "send_to_child",
                    "description": "Sends a follow-up prompt to a child session started from one of your proposals. Queued while the child is mid-turn.",
                    "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"},"prompt":{"type":"string"}},"required":["session_id","prompt"]}
                }
            ]"#,
        )
        .unwrap();
        assert_eq!(
            answer.json(),
            json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": tools}})
        );
    }

    #[tokio::test]
    async fn a_propose_children_call_reaches_the_core_and_returns_its_answer() {
        let mut harness = harness();
        let response = spawn_post(&harness, call("propose_children", &propose_api()));

        assert_eq!(
            harness.inbox.next().await,
            Some(BackendEvent::ToolCalled {
                call: ToolRequestId(0),
                session: harness.session.clone(),
                tool: OrchestratorTool::ProposeChildren(vec![ProposedChild {
                    repository: RepositoryId::try_from("api").unwrap(),
                    prompt: "Add the endpoint".into(),
                    title: "API endpoint".into(),
                }]),
            })
        );
        harness
            .tools
            .answer(
                ToolRequestId(0),
                Ok("proposal recorded, awaiting approval".into()),
            )
            .unwrap();

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

        assert!(matches!(
            harness.inbox.next().await,
            Some(BackendEvent::ToolCalled {
                call: ToolRequestId(0),
                ..
            })
        ));
        harness
            .tools
            .answer(ToolRequestId(0), Err("unknown repository".into()))
            .unwrap();

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

        assert_eq!(
            harness.inbox.next().await,
            Some(BackendEvent::ToolCalled {
                call: ToolRequestId(0),
                session: harness.session.clone(),
                tool: OrchestratorTool::SendToChild {
                    child,
                    prompt: "Also add tests".into(),
                },
            })
        );
        harness
            .tools
            .answer(ToolRequestId(0), Ok("queued".into()))
            .unwrap();

        assert_eq!(response.await.unwrap().json(), tool_result("queued", false));
    }

    #[tokio::test]
    async fn malformed_arguments_are_tool_errors_without_an_event() {
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
                call("rename_child", &json!({})),
                "unknown tool rename_child".to_owned(),
            ),
        ];

        for (body, text) in cases {
            let answer = post(&harness, &body).await;

            assert_eq!(answer.json(), tool_result(&text, true), "for {body}");
        }
        assert!(
            harness.inbox.try_recv().is_err(),
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
        assert!(matches!(
            harness.inbox.try_recv(),
            Ok(BackendEvent::ToolCalled { .. })
        ));
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

    #[tokio::test]
    async fn the_served_router_answers_a_ping_over_tcp() {
        let harness = harness();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, harness.router.clone()));

        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let request = format!(
            "POST /{} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            harness.session,
            body.len()
        );
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        server.abort();

        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK"), "response {response}");
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"jsonrpc": "2.0", "id": 1, "result": {}})
        );
    }

    #[test]
    fn session_url_joins_base_and_id() {
        let session = SessionId::new();

        assert_eq!(
            session_url("http://127.0.0.1:4711", &session),
            format!("http://127.0.0.1:4711/{session}")
        );
    }
}
