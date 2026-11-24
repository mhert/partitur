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
    ModuleEvent, OrchestratorTool, ProposedChild, Question, QuestionOption, RepositoryId,
    SessionId, SessionTool, SuperpowersV7Event,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;

/// A router under test, the calls it reports and the session it is posted for.
struct Harness {
    router: Router,
    calls: UnboundedReceiver<ToolCall>,
    session: SessionId,
    html: HtmlOffer,
}

fn harness_with(answer_timeout: Duration) -> Harness {
    let (sender, calls) = unbounded();
    let html = HtmlOffer::default();
    Harness {
        router: router(sender, answer_timeout, html.clone()),
        calls,
        session: SessionId::new(),
        html,
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

/// Posts `body` to the URL of a child session.
async fn post_child(harness: &Harness, body: &Value) -> Answer {
    let path = format!("/{}/child", harness.session);
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
async fn the_child_url_answers_initialize() {
    let harness = harness();

    let answer = post_child(
        &harness,
        &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}),
    )
    .await;

    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.json()["result"]["serverInfo"]["name"], "partitur");
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
                "description": "sends a follow-up prompt to a child; it is queued while the child is mid-turn unless a gate or question is open. At a gate the child waits at, decision \"approve\" approves it, with the prompt after the approval when given, and \"changes\" asks for the changes the prompt names; a child that asks a question is answered by a prompt without a decision.",
                "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"},"prompt":{"type":"string"},"decision":{"type":"string","enum":["approve","changes"]}},"required":["session_id"]}
            },
            {
                "name": "list_children",
                "description": "lists your children with their title, repository, session id and status and, for a child that has them, its workflow step and the gate or question it waits at.",
                "inputSchema": {"type":"object","properties":{}}
            },
            {
                "name": "cancel_child",
                "description": "stops a child's current turn; its result then reaches you as a child result marked cancelled.",
                "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}
            },
            {
                "name": "get_child_result",
                "description": "returns a child's last final message and its status and, when it has them, its workflow step and the gate or question it waits at.",
                "inputSchema": {"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}
            }
        ]"#,
    )
    .unwrap();
    let listed = answer.json()["result"]["tools"].clone();
    assert_eq!(
        listed.as_array().unwrap()[..catalog::orchestration().count()],
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
    let tools = tools.as_array().unwrap();
    let catalogue = catalogue();
    assert_eq!(tools.len(), catalogue.len());
    for (tool, (name, description)) in tools.iter().zip(catalogue) {
        assert_eq!(tool["name"], name);
        assert_eq!(tool["description"], description);
        assert!(!tool["inputSchema"].is_null(), "{name}");
    }
    let workflow = &tools[catalog::orchestration().count()..tools.len() - 5];
    for (tool, listed) in workflow.iter().zip(workflow::tools(true)) {
        assert_eq!(tool["inputSchema"], (listed.schema)(), "{}", listed.name);
    }
}

/// The names and descriptions of both catalogues, orchestration tools first.
fn catalogue() -> Vec<(&'static str, &'static str)> {
    catalog::orchestration()
        .map(|spec| (spec.name, spec.description))
        .chain(
            workflow::tools(true)
                .into_iter()
                .map(|tool| (tool.name, tool.description)),
        )
        .chain(
            catalog::offered(ToolRole::Orchestrator)
                .filter(|spec| !spec.is_orchestration() && spec.name != "show_html")
                .map(|spec| (spec.name, spec.description_for(ToolRole::Orchestrator))),
        )
        .collect()
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

fn names(catalogue: impl IntoIterator<Item = (&'static str, &'static str)>) -> Vec<String> {
    catalogue
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// The names of the workflow tools a session is offered.
fn workflow_names(orchestrator: bool) -> Vec<String> {
    workflow::tools(orchestrator)
        .into_iter()
        .map(|tool| tool.name.to_owned())
        .collect()
}

/// The names of the peer tools a session is offered; `propose_session` only a plain one.
fn peer_names(plain: bool) -> Vec<String> {
    ["propose_session", "list_sessions", "read_session"]
        .into_iter()
        .filter(|name| plain || *name != "propose_session")
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn a_plain_session_lists_the_peer_tools() {
    let harness = harness();

    let answer = post_plain(&harness, &list()).await;

    for spec in ["propose_session", "list_sessions", "read_session"]
        .map(|name| catalog::find(name).unwrap())
    {
        let tool = listed(&answer, spec.name);
        assert_eq!(tool["description"], spec.description, "{}", spec.name);
        assert!(!tool["inputSchema"].is_null(), "{}", spec.name);
    }
    let propose = listed(&answer, "propose_session");
    assert_eq!(
        propose["inputSchema"],
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "A short title for the new session"},
                "prompt": {"type": "string"},
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
        })
    );
    assert_eq!(
        listed(&answer, "list_sessions")["inputSchema"],
        json!({"type": "object", "properties": {}})
    );
    assert_eq!(
        listed(&answer, "read_session")["inputSchema"],
        json!({
            "type": "object",
            "properties": {
                "session_id": {"type": "string"},
                "page": {"type": "integer", "minimum": 1},
            },
            "required": ["session_id"],
        })
    );
    let names = listed_names(&answer);
    let position = |name: &str| names.iter().position(|listed| listed == name).unwrap();
    assert!(position("bmad_v6_12_finish_workflow") < position("propose_session"));
    assert!(position("read_session") < position("rename_session"));
}

#[tokio::test]
async fn children_and_orchestrators_list_only_list_and_read() {
    let mut harness = harness();
    let body = call("propose_session", &json!({"prompt": "Go"}));

    for answer in [
        post_child(&harness, &list()).await,
        post(&harness, &list()).await,
    ] {
        assert!(listed(&answer, "propose_session").is_null());
        assert!(!listed(&answer, "list_sessions").is_null());
        assert!(!listed(&answer, "read_session").is_null());
    }
    for answer in [
        post_child(&harness, &body).await,
        post(&harness, &body).await,
    ] {
        assert_eq!(
            answer.json(),
            tool_result("unknown tool propose_session", true)
        );
    }
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn propose_session_parses_its_arguments() {
    let mut harness = harness();
    let response = spawn_plain_post(&harness, call("propose_session", &json!({"prompt": "Go"})));
    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        PartiturTool::from(PeerTool::ProposeSession {
            repository: None,
            title: String::new(),
            prompt: "Go".into(),
            base: None,
        })
    );
    reported.reply.send(Ok("proposed".into()));
    assert_eq!(
        response.await.unwrap().json(),
        tool_result("proposed", false)
    );

    let response = spawn_plain_post(
        &harness,
        call(
            "propose_session",
            &json!({"title": "T", "prompt": "Go", "repository": "api", "base": "main"}),
        ),
    );
    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        PartiturTool::from(PeerTool::ProposeSession {
            repository: Some(RepositoryId::try_from("api").unwrap()),
            title: "T".into(),
            prompt: "Go".into(),
            base: Some("main".into()),
        })
    );
    reported.reply.send(Ok("proposed".into()));
    response.await.unwrap();

    let answer = post_plain(
        &harness,
        &call(
            "propose_session",
            &json!({"prompt": "Go", "repository": "a/b"}),
        ),
    )
    .await;
    assert_eq!(
        answer.json(),
        tool_result("repository \"a/b\" must be a directory name", true)
    );
    assert!(harness.calls.try_recv().is_err());
}

#[tokio::test]
async fn list_sessions_parses_on_every_url() {
    let mut harness = harness();
    for body in [
        spawn_plain_post(&harness, call("list_sessions", &json!({}))),
        spawn_post(&harness, call("list_sessions", &json!({}))),
        spawn_child_post(&harness, call("list_sessions", &json!({}))),
    ] {
        let reported = next_call(&mut harness).await.unwrap();
        assert_eq!(reported.tool, PartiturTool::from(PeerTool::ListSessions));
        reported.reply.send(Ok("listed".into()));
        assert_eq!(body.await.unwrap().json(), tool_result("listed", false));
    }
}

#[tokio::test]
async fn read_session_parses_session_and_page() {
    let mut harness = harness();
    let target = SessionId::new();
    let response = spawn_child_post(
        &harness,
        call(
            "read_session",
            &json!({"session_id": target.to_string(), "page": 2}),
        ),
    );
    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        PartiturTool::from(PeerTool::ReadSession {
            session: target.clone(),
            page: Some(2),
        })
    );
    reported.reply.send(Ok("read".into()));
    response.await.unwrap();

    let response = spawn_post(
        &harness,
        call("read_session", &json!({"session_id": target.to_string()})),
    );
    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        PartiturTool::from(PeerTool::ReadSession {
            session: target.clone(),
            page: None,
        })
    );
    reported.reply.send(Ok("read".into()));
    response.await.unwrap();

    let bad = post_plain(&harness, &call("read_session", &json!({"session_id": "x"}))).await;
    assert_eq!(
        bad.json(),
        tool_result("session_id \"x\" is not a session id", true)
    );
    let zero = post_plain(
        &harness,
        &call(
            "read_session",
            &json!({"session_id": target.to_string(), "page": 0}),
        ),
    )
    .await;
    assert_eq!(zero.json(), tool_result("page must be at least 1", true));
    assert!(harness.calls.try_recv().is_err());
}

#[tokio::test]
async fn a_plain_session_lists_the_bmad_workflow_tools() {
    let harness = harness();

    let answer = post_plain(
        &harness,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;

    let listed = listed_names(&answer);
    let mut expected = workflow_names(false);
    expected.extend(peer_names(true));
    expected.push("rename_session".to_owned());
    expected.push("show_svg".to_owned());
    expected.push("ask_user".to_owned());
    assert_eq!(listed, expected);
    for name in [
        "superpowers_v7_start_session",
        "bmad_v6_12_start_workflow",
        "bmad_v6_12_request_document_approval",
        "bmad_v6_12_finish_workflow",
    ] {
        assert!(listed.iter().any(|listed| listed == name), "{name}");
    }
}

#[tokio::test]
async fn an_orchestrator_lists_both_catalogues() {
    let harness = harness();

    let answer = post(
        &harness,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;

    let mut expected = names(catalog::orchestration().map(|spec| (spec.name, spec.description)));
    expected.extend(workflow_names(true));
    expected.extend(peer_names(false));
    expected.push("rename_session".to_owned());
    expected.push("show_svg".to_owned());
    expected.push("ask_user".to_owned());
    assert_eq!(listed_names(&answer), expected);
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

#[tokio::test]
async fn run_level_tools_are_unknown_on_a_plain_url() {
    let mut harness = harness();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});

    let plain = listed_names(&post_plain(&harness, &list).await);
    let orchestrator = listed_names(&post(&harness, &list).await);
    let answer = post_plain(
        &harness,
        &call(
            "bmad_v6_12_start_run",
            &json!({"topic": "Invoices", "path": "epic"}),
        ),
    )
    .await;

    assert!(!plain.iter().any(|name| name == "bmad_v6_12_start_run"));
    assert!(
        orchestrator
            .iter()
            .any(|name| name == "bmad_v6_12_start_run")
    );
    assert_eq!(
        answer.json(),
        tool_result("unknown tool bmad_v6_12_start_run", true)
    );
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn a_workflow_call_parses_into_its_event() {
    let mut harness = harness();
    let response = spawn_plain_post(
        &harness,
        call("superpowers_v7_start_task", &json!({"n": 2, "note": "why"})),
    );

    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(reported.session, harness.session);
    assert_eq!(
        reported.tool,
        PartiturTool::from(ModuleEvent::from(SuperpowersV7Event::StartTask {
            n: 2,
            note: Some("why".into()),
        }))
    );
    reported.reply.send(Ok("recorded".into()));
    assert_eq!(
        response.await.unwrap().json(),
        tool_result("recorded", false)
    );
}

#[tokio::test]
async fn an_unversioned_superpowers_name_is_an_unknown_tool() {
    let mut harness = harness();
    let body = call("superpowers_start_brainstorming", &json!({}));

    for answer in [
        post_plain(&harness, &body).await,
        post(&harness, &body).await,
    ] {
        assert_eq!(
            answer.json(),
            tool_result("unknown tool superpowers_start_brainstorming", true)
        );
    }
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn superpowers_calls_work_on_an_orchestrator_url_too() {
    let mut harness = harness();
    let response = spawn_post(
        &harness,
        call("superpowers_v7_start_brainstorming", &json!({})),
    );

    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        PartiturTool::from(ModuleEvent::from(SuperpowersV7Event::StartBrainstorming))
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
        (
            "superpowers_v7_start_execution",
            json!({"mode": "batch"}),
            "unknown variant `batch`, expected `subagent` or `inline`",
        ),
        (
            "superpowers_v7_request_design_approval",
            json!({}),
            "missing field `path`",
        ),
    ];

    for (name, arguments, text) in cases {
        let answer = post_plain(&harness, &call(name, &arguments)).await;

        assert_eq!(answer.json(), tool_result(text, true), "for {name}");
    }
    assert!(
        harness.calls.try_recv().is_err(),
        "a malformed call reached the core"
    );
}

#[tokio::test]
async fn both_urls_list_ask_user_last_with_its_schema() {
    let harness = harness();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});

    for answer in [
        post_plain(&harness, &request).await,
        post(&harness, &request).await,
    ] {
        let tools = answer.json()["result"]["tools"].clone();
        let ask = tools.as_array().unwrap().last().unwrap().clone();
        assert_eq!(ask["name"], "ask_user");
        assert_eq!(
            ask["description"],
            "asks the user to choose between 2 to 6 named options; the user picks one or answers in their own words. End your turn after calling it: the answer arrives as your next prompt."
        );
        let schema = &ask["inputSchema"];
        assert_eq!(schema["required"], json!(["question", "options"]));
        assert_eq!(schema["properties"]["question"]["type"], "string");
        let options = &schema["properties"]["options"];
        assert_eq!(options["type"], "array");
        assert_eq!(options["minItems"], 2);
        assert_eq!(options["maxItems"], 6);
        assert_eq!(options["items"]["required"], json!(["label"]));
        assert_eq!(options["items"]["properties"]["label"]["type"], "string");
        assert_eq!(
            options["items"]["properties"]["description"]["type"],
            "string"
        );
    }
}

#[tokio::test]
async fn an_ask_user_call_reaches_the_core_as_a_question() {
    let mut harness = harness();
    let response = spawn_plain_post(
        &harness,
        call(
            "ask_user",
            &json!({"question": "How?", "options": [
                {"label": "A"},
                {"label": "B", "description": "b"},
            ]}),
        ),
    );

    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(reported.session, harness.session);
    assert_eq!(
        reported.tool,
        PartiturTool::AskUser(Question {
            text: "How?".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: None,
                },
                QuestionOption {
                    label: "B".into(),
                    description: Some("b".into()),
                },
            ],
        })
    );
    reported.reply.send(Ok("asked".into()));
    assert_eq!(response.await.unwrap().json(), tool_result("asked", false));
}

#[tokio::test]
async fn malformed_ask_user_arguments_are_tool_errors_without_a_call() {
    let mut harness = harness();

    let answer = post_plain(
        &harness,
        &call("ask_user", &json!({"question": "How?", "options": "A"})),
    )
    .await;

    let result = &answer.json()["result"];
    assert_eq!(result["isError"], true);
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

fn spawn_child_post(harness: &Harness, body: Value) -> tokio::task::JoinHandle<Answer> {
    let router = harness.router.clone();
    let path = format!("/{}/child", harness.session);
    tokio::spawn(async move { send(&router, Method::POST, &path, &body.to_string()).await })
}

fn list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

/// The listed tool `name`, or `Null` when it is not listed.
fn listed(answer: &Answer, name: &str) -> Value {
    answer.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

fn title_only() -> Value {
    json!({"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]})
}

#[tokio::test]
async fn a_plain_session_lists_rename_without_session_id() {
    let harness = harness();

    let answer = post_plain(&harness, &list()).await;

    let rename = listed(&answer, "rename_session");
    assert_eq!(rename["inputSchema"], title_only());
    assert_eq!(
        rename["description"],
        "renames this session. The title replaces any earlier one, also one the user typed."
    );
    assert!(listed(&answer, "propose_title").is_null());
}

#[tokio::test]
async fn an_orchestrator_lists_rename_with_session_id() {
    let harness = harness();

    let answer = post(&harness, &list()).await;

    let rename = listed(&answer, "rename_session");
    assert_eq!(
        rename["inputSchema"],
        json!({
            "type": "object",
            "properties": {"title": {"type": "string"}, "session_id": {"type": "string"}},
            "required": ["title"],
        })
    );
    assert_eq!(
        rename["description"],
        "renames this session, or the child session_id. The title replaces any earlier one, also one the user typed: list_children marks such a title \"set by you\", so weigh it against a child's proposal before you replace it."
    );
    assert!(listed(&answer, "propose_title").is_null());
}

#[tokio::test]
async fn a_child_lists_propose_title_and_no_rename() {
    let harness = harness();

    let answer = post_child(&harness, &list()).await;

    let propose = listed(&answer, "propose_title");
    assert_eq!(propose["inputSchema"], title_only());
    assert_eq!(
        propose["description"],
        "proposes a new title for this session to your orchestrator, which decides. Call it when your work drifted from your title."
    );
    let mut expected = workflow_names(false);
    expected.extend(peer_names(false));
    expected.push("propose_title".to_owned());
    expected.push("show_svg".to_owned());
    expected.push("ask_user".to_owned());
    assert_eq!(listed_names(&answer), expected);
}

#[tokio::test]
async fn rename_parses_into_its_tool_call() {
    let mut harness = harness();
    let child = SessionId::new();

    let plain = spawn_plain_post(&harness, call("rename_session", &json!({"title": "Auth"})));
    let renamed = next_call(&mut harness).await.unwrap();
    assert_eq!(
        renamed.tool,
        PartiturTool::from(SessionTool::Rename {
            title: "Auth".into(),
            session: None,
        })
    );
    renamed.reply.send(Ok("renamed".into()));
    assert_eq!(plain.await.unwrap().json(), tool_result("renamed", false));

    let orchestrator = spawn_post(
        &harness,
        call(
            "rename_session",
            &json!({"title": "Auth", "session_id": child.to_string()}),
        ),
    );
    let renamed = next_call(&mut harness).await.unwrap();
    assert_eq!(
        renamed.tool,
        PartiturTool::from(SessionTool::Rename {
            title: "Auth".into(),
            session: Some(child),
        })
    );
    renamed.reply.send(Ok("renamed".into()));
    assert_eq!(
        orchestrator.await.unwrap().json(),
        tool_result("renamed", false)
    );
}

#[tokio::test]
async fn propose_title_parses_on_the_child_url() {
    let mut harness = harness();

    let response = spawn_child_post(&harness, call("propose_title", &json!({"title": "Auth"})));

    let proposed = next_call(&mut harness).await.unwrap();
    assert_eq!(proposed.session, harness.session);
    assert_eq!(
        proposed.tool,
        PartiturTool::from(SessionTool::ProposeTitle {
            title: "Auth".into()
        })
    );
    proposed.reply.send(Ok("sent to the orchestrator".into()));
    assert_eq!(
        response.await.unwrap().json(),
        tool_result("sent to the orchestrator", false)
    );
}

#[tokio::test]
async fn rename_with_session_id_on_a_plain_url_is_refused() {
    let mut harness = harness();
    let body = call(
        "rename_session",
        &json!({"title": "Auth", "session_id": SessionId::new().to_string()}),
    );

    let answer = post_plain(&harness, &body).await;

    assert_eq!(
        answer.json(),
        tool_result("session_id is only for an orchestrator", true)
    );
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn a_malformed_title_tool_call_is_an_error_without_a_call() {
    let mut harness = harness();

    let rename = post_plain(&harness, &call("rename_session", &json!({}))).await;
    let session = post(
        &harness,
        &call(
            "rename_session",
            &json!({"title": "A", "session_id": "nope"}),
        ),
    )
    .await;
    let propose = post_child(&harness, &call("propose_title", &json!({"title": 3}))).await;

    assert_eq!(rename.json()["result"]["isError"], true);
    assert_eq!(
        session.json(),
        tool_result("session_id \"nope\" is not a session id", true)
    );
    assert_eq!(propose.json()["result"]["isError"], true);
    assert!(harness.calls.try_recv().is_err(), "a call reached the core");
}

#[tokio::test]
async fn propose_title_is_unknown_off_the_child_url() {
    let mut harness = harness();
    let body = call("propose_title", &json!({"title": "Auth"}));

    let plain = post_plain(&harness, &body).await;
    let orchestrator = post(&harness, &body).await;

    for answer in [plain, orchestrator] {
        assert_eq!(
            answer.json(),
            tool_result("unknown tool propose_title", true)
        );
    }
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn rename_is_unknown_on_the_child_url() {
    let mut harness = harness();

    let answer = post_child(&harness, &call("rename_session", &json!({"title": "Auth"}))).await;

    assert_eq!(
        answer.json(),
        tool_result("unknown tool rename_session", true)
    );
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[tokio::test]
async fn orchestration_tools_are_unknown_on_the_child_url() {
    let mut harness = harness();

    let answer = post_child(&harness, &call("list_children", &json!({}))).await;

    assert_eq!(
        answer.json(),
        tool_result("unknown tool list_children", true)
    );
    assert!(
        harness.calls.try_recv().is_err(),
        "the call reached the core"
    );
}

#[test]
fn a_tool_outside_the_catalogue_has_no_schema() {
    assert_eq!(input_schema("rename_child", ToolRole::Plain), Value::Null);
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
            base: None,
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
            decision: None,
        }
        .into()
    );
    reported.reply.send(Ok("queued".into()));

    assert_eq!(response.await.unwrap().json(), tool_result("queued", false));
}

#[tokio::test]
async fn a_send_to_child_call_carries_its_decision_without_a_prompt() {
    let mut harness = harness();
    let child = SessionId::new();
    let response = spawn_post(
        &harness,
        call(
            "send_to_child",
            &json!({"session_id": child.to_string(), "decision": "approve"}),
        ),
    );

    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: String::new(),
            decision: Some(GateDecision::Approve),
        }
        .into()
    );
    reported.reply.send(Ok("sent".into()));
    assert_eq!(response.await.unwrap().json(), tool_result("sent", false));

    let response = spawn_post(
        &harness,
        call(
            "send_to_child",
            &json!({"session_id": child.to_string(), "prompt": "Fix x", "decision": "changes"}),
        ),
    );

    let reported = next_call(&mut harness).await.unwrap();
    assert_eq!(
        reported.tool,
        OrchestratorTool::SendToChild {
            child,
            prompt: "Fix x".into(),
            decision: Some(GateDecision::Changes),
        }
        .into()
    );
    reported.reply.send(Ok("sent".into()));
    assert_eq!(response.await.unwrap().json(), tool_result("sent", false));
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
                &json!({"session_id": SessionId::new().to_string(), "decision": "maybe"}),
            ),
            "unknown variant `maybe`, expected `approve` or `changes`".to_owned(),
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
        html: _,
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

#[tokio::test]
async fn every_catalog_tool_is_served_for_its_roles() {
    let harness = harness();
    harness.html.enable();
    let roles = [
        (ToolRole::Plain, post_plain(&harness, &list()).await),
        (ToolRole::Orchestrator, post(&harness, &list()).await),
        (ToolRole::Child, post_child(&harness, &list()).await),
    ];

    for (role, answer) in roles {
        let orchestrator = role == ToolRole::Orchestrator;
        let mut expected: Vec<String> = partitur_core::tools::offered(role)
            .map(|spec| spec.name.to_owned())
            .collect();
        expected.extend(workflow_names(orchestrator));
        let mut served = listed_names(&answer);
        expected.sort();
        served.sort();
        assert_eq!(served, expected, "{role:?}");
        for spec in partitur_core::tools::offered(role) {
            let tool = listed(&answer, spec.name);
            assert_eq!(
                tool["description"],
                spec.description_for(role),
                "{role:?} {}",
                spec.name
            );
            assert!(!tool["inputSchema"].is_null(), "{role:?} {}", spec.name);
            assert_ne!(
                parse_tool(spec.name, json!({}), role, true).err(),
                Some(unknown_tool(spec.name)),
                "{role:?} {}",
                spec.name
            );
        }
    }
}

/// `tools/list` for every role, as the server lists it without a widget engine and with
/// one: names, descriptions, schemas and order.
#[test]
fn tools_list_matches_its_snapshot_for_every_role() {
    for (role, without, with) in [
        (
            ToolRole::Plain,
            include_str!("../../tests/fixtures/tools-plain.json"),
            include_str!("../../tests/fixtures/tools-plain-html.json"),
        ),
        (
            ToolRole::Orchestrator,
            include_str!("../../tests/fixtures/tools-orchestrator.json"),
            include_str!("../../tests/fixtures/tools-orchestrator-html.json"),
        ),
        (
            ToolRole::Child,
            include_str!("../../tests/fixtures/tools-child.json"),
            include_str!("../../tests/fixtures/tools-child-html.json"),
        ),
    ] {
        let without: Value = serde_json::from_str(without).unwrap();
        assert_eq!(tools(role, false), without, "{role:?}");
        let with: Value = serde_json::from_str(with).unwrap();
        assert_eq!(tools(role, true), with, "{role:?} with HTML");
    }
}

fn svg_schema(argument: &str) -> Value {
    json!({
        "type": "object",
        "properties": { argument: { "type": "string" } },
        "required": [argument],
    })
}

#[tokio::test]
async fn show_html_is_listed_only_once_offered() {
    let harness = harness();
    let urls = [
        post_plain(&harness, &list()).await,
        post(&harness, &list()).await,
        post_child(&harness, &list()).await,
    ];
    for answer in &urls {
        let before = listed_names(answer);
        assert!(before.iter().any(|name| name == "show_svg"));
        assert!(!before.iter().any(|name| name == "show_html"));
    }

    let offer = HtmlOffer::default();
    assert!(!offer.enabled());
    offer.clone().enable();
    assert!(offer.enabled());
    harness.html.clone().enable();
    let urls = [
        post_plain(&harness, &list()).await,
        post(&harness, &list()).await,
        post_child(&harness, &list()).await,
    ];
    for answer in &urls {
        let after = listed_names(answer);
        let ask = after.iter().position(|name| name == "ask_user").unwrap();
        assert_eq!(after[ask - 2..=ask], ["show_svg", "show_html", "ask_user"]);
        assert_eq!(listed(answer, "show_svg")["inputSchema"], svg_schema("svg"));
        assert_eq!(
            listed(answer, "show_html")["inputSchema"],
            svg_schema("html")
        );
    }
}

#[tokio::test]
async fn show_svg_is_sent_on_as_a_drawing() {
    let mut harness = harness();
    let spawned = spawn_plain_post(&harness, call("show_svg", &json!({"svg": "<svg/>"})));

    let sent = next_call(&mut harness).await.unwrap();
    assert_eq!(
        sent.tool,
        PartiturTool::Show(Drawing {
            kind: DrawingKind::Svg,
            markup: "<svg/>".into(),
        })
    );
    sent.reply.send(Ok("shown to the user".to_owned()));
    let answer = spawned.await.unwrap();
    assert_eq!(answer.json(), tool_result("shown to the user", false));
}

#[tokio::test]
async fn show_html_is_sent_on_as_a_drawing_once_offered() {
    let mut harness = harness();
    harness.html.enable();
    let spawned = spawn_plain_post(&harness, call("show_html", &json!({"html": "<p>hi</p>"})));

    let sent = next_call(&mut harness).await.unwrap();
    assert_eq!(
        sent.tool,
        PartiturTool::Show(Drawing {
            kind: DrawingKind::Html,
            markup: "<p>hi</p>".into(),
        })
    );
    sent.reply.send(Ok("shown to the user".to_owned()));
    spawned.await.unwrap();
}

#[tokio::test]
async fn show_html_without_an_engine_is_refused() {
    let mut harness = harness();

    let answer = post_plain(&harness, &call("show_html", &json!({"html": "<p/>"}))).await;

    assert_eq!(
        answer.json(),
        tool_result("HTML cannot be shown here", true)
    );
    assert!(next_call(&mut harness).await.is_none());
}

#[tokio::test]
async fn malformed_drawing_arguments_are_tool_errors_without_a_call() {
    let mut harness = harness();
    harness.html.enable();

    for (name, arguments) in [
        ("show_svg", json!({})),
        ("show_svg", json!({"svg": 3})),
        ("show_html", json!({"svg": "<svg/>"})),
    ] {
        let answer = post_plain(&harness, &call(name, &arguments)).await;
        assert_eq!(answer.json()["result"]["isError"], true, "{name}");
    }
    assert!(next_call(&mut harness).await.is_none());
}
