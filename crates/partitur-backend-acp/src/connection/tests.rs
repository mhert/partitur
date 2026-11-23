#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use agent_client_protocol::v2::schema::{
    BackgroundSubagent, BackgroundWork, CommandPermissionSubject, ConfigOptionUpdate, ContentBlock,
    ContentChunk, IdleStateUpdate, PermissionOption, PermissionOptionKind, RunningStateUpdate,
    SessionConfigSelectOption, StateUpdate, StopReason as SchemaStopReason, TextContent,
    ToolCallUpdate,
};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use partitur_core::PermissionKind;

use super::*;
use crate::runtime::{Connections, ThinkingDisplay};

/// The answers given to parked requests.
type Answers = Arc<Mutex<Vec<Option<PermissionOptionId>>>>;

/// Collects the answers given to parked requests.
struct Recorder(Answers);

impl Answer for Recorder {
    fn answer(
        self,
        option: Option<PermissionOptionId>,
    ) -> Result<(), agent_client_protocol::Error> {
        self.0.lock().unwrap().push(option);
        Ok(())
    }
}

/// A fresh registry, the reporter of a new connection of a new session in it and the
/// events that reporter sends.
fn claimed() -> (Connections, Reporter, UnboundedReceiver<BackendEvent>) {
    let (events, inbox) = unbounded();
    let connections = Connections::default();
    let reporter = connections.claim(SessionId::new(), events);
    (connections, reporter, inbox)
}

fn drain(inbox: &mut UnboundedReceiver<BackendEvent>) -> Vec<BackendEvent> {
    std::iter::from_fn(|| inbox.try_recv().ok()).collect()
}

/// The agent's id of the session the tests' gates are for.
const OWN: &str = "b-1";

/// A gate for the session [`OWN`].
fn own_gate() -> Mutex<Gate> {
    let gate = Mutex::new(Gate::default());
    admit(&gate, schema::SessionId::new(OWN));
    gate
}

/// `update` as a notification for the session [`OWN`].
fn own(update: SessionUpdate) -> UpdateSessionNotification {
    UpdateSessionNotification::new(OWN, update)
}

fn text(text: &str) -> UpdateSessionNotification {
    own(SessionUpdate::AgentMessageChunk(ContentChunk::new(
        ContentBlock::Text(TextContent::new(text.to_owned())),
        "m-1",
    )))
}

fn running() -> SessionUpdate {
    SessionUpdate::StateUpdate(StateUpdate::Running(RunningStateUpdate::new()))
}

fn ended() -> SessionUpdate {
    SessionUpdate::StateUpdate(StateUpdate::Idle(
        IdleStateUpdate::new().stop_reason(SchemaStopReason::EndTurn),
    ))
}

fn config_update(option: SessionConfigOption) -> SessionUpdate {
    SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![option]))
}

fn model_option(current: &str) -> SessionConfigOption {
    SessionConfigOption::select(
        "model",
        "Model",
        current.to_owned(),
        vec![
            SessionConfigSelectOption::new("opus", "Opus"),
            SessionConfigSelectOption::new("sonnet", "Sonnet"),
        ],
    )
}

fn created(session: &SessionId) -> impl FnOnce(Vec<ConfigOption>) -> BackendEvent {
    let session = session.clone();
    move |config| BackendEvent::SessionCreated {
        request: RequestId(1),
        session,
        backend_session: BackendSessionId::new("b-1"),
        config,
    }
}

#[test]
fn notifications_are_held_back_until_the_session_is_reported() {
    let (connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    forward(&gate, &reporter, text("replayed"));
    assert!(drain(&mut inbox).is_empty());

    open_gate(&gate, &reporter, Vec::new(), created(&session));
    assert!(connections.is_live(&session));
    forward(&gate, &reporter, text("live"));
    let received = drain(&mut inbox);
    assert!(matches!(&received[0], BackendEvent::SessionCreated { .. }));
    assert_eq!(
        received[1..],
        [BackendEvent::Update {
            session,
            update: Update::AgentText("live".into())
        }]
    );
}

#[test]
fn notifications_for_another_session_are_dropped() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let foreign = |update| UpdateSessionNotification::new("sub-1", update);
    let gate = own_gate();
    forward(
        &gate,
        &reporter,
        foreign(config_update(model_option("sonnet"))),
    );
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    for update in [running(), ended()] {
        forward(&gate, &reporter, foreign(update));
    }
    assert_eq!(drain(&mut inbox), [created(&session)(Vec::new())]);

    // Before the agent named the session, nothing is its.
    let unnamed = Mutex::new(Gate::default());
    forward(&unnamed, &reporter, text("early"));
    assert!(lock(&unnamed).config.is_none());
    open_gate(&unnamed, &reporter, Vec::new(), created(&session));
    drain(&mut inbox);
    forward(&unnamed, &reporter, text("live"));
    assert!(drain(&mut inbox).is_empty());
}

#[test]
fn the_session_state_starts_when_the_gate_opens() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    // A replayed or snapshot `running` opens no work.
    forward(&gate, &reporter, own(running()));
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    forward(&gate, &reporter, own(ended()));
    forward(&gate, &reporter, own(running()));
    forward(&gate, &reporter, own(ended()));
    let received = drain(&mut inbox);
    assert_eq!(
        received[1..],
        [
            BackendEvent::Update {
                session: session.clone(),
                update: Update::TurnStarted
            },
            BackendEvent::Update {
                session,
                update: Update::TurnEnded(StopReason::EndTurn)
            },
        ]
    );
}

#[test]
fn a_turn_ending_while_a_subagent_runs_reports_the_subagent_first() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    let listing = |state: IdleStateUpdate| {
        let background = vec![BackgroundWork::Subagent(BackgroundSubagent::new("sub-1"))];
        own(SessionUpdate::StateUpdate(StateUpdate::Idle(
            state.background(background),
        )))
    };
    // A list replayed before the session is reported is history.
    forward(&gate, &reporter, listing(IdleStateUpdate::new()));
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    forward(&gate, &reporter, own(running()));
    let ended = IdleStateUpdate::new().stop_reason(SchemaStopReason::EndTurn);
    forward(&gate, &reporter, listing(ended));
    let update = |update| BackendEvent::Update {
        session: session.clone(),
        update,
    };
    assert_eq!(
        drain(&mut inbox)[1..],
        [
            update(Update::TurnStarted),
            update(Update::RunningBackground(1)),
            update(Update::TurnEnded(StopReason::EndTurn)),
        ]
    );
}

/// A notice carrying `meta` as its `_meta`.
fn notice(meta: Option<serde_json::Value>) -> SessionUpdate {
    let notice = schema::Notice::new(schema::NoticeSeverity::Info, "MCP server x connected");
    SessionUpdate::Notice(notice.meta(meta.and_then(|meta| meta.as_object().cloned())))
}

#[test]
fn only_a_server_status_notice_reports_a_server_status() {
    let status = serde_json::json!({"claudeCode": {"kind": "mcpServerStatus"}});
    assert!(reports_server_status(&notice(Some(status))));
    let other = serde_json::json!({"claudeCode": {"kind": "other"}});
    assert!(!reports_server_status(&notice(Some(other))));
    assert!(!reports_server_status(&notice(Some(serde_json::json!({})))));
    assert!(!reports_server_status(&notice(None)));
    assert!(!reports_server_status(&running()));
}

#[test]
fn only_an_adapter_saying_so_serves_widgets() {
    let response = |meta: Option<serde_json::Value>| {
        InitializeResponse::new(ProtocolVersion::V2, Implementation::new("a", "1"))
            .meta(meta.and_then(|meta| meta.as_object().cloned()))
    };
    let widgets = |value| response(Some(serde_json::json!({"partitur": {"widgets": value}})));
    assert!(serves_widgets(&widgets(serde_json::json!(1))));
    assert!(!serves_widgets(&widgets(serde_json::Value::Null)));
    assert!(!serves_widgets(&response(Some(
        serde_json::json!({"partitur": {}})
    ))));
    assert!(!serves_widgets(&response(Some(serde_json::json!({})))));
    assert!(!serves_widgets(&response(None)));
}

#[test]
fn a_failed_tool_list_lists_no_tools() {
    assert!(listed_tools(Err(agent_client_protocol::Error::method_not_found())).is_empty());
    let reply = serde_json::json!({"tools": [
        {"server": "a", "tool": "t", "resourceUri": "ui://a"},
    ]});
    assert_eq!(listed_tools(Ok(reply)).len(), 1);
}

#[test]
fn a_server_status_asks_for_the_tools_only_from_an_adapter_serving_widgets() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    let status = || {
        own(notice(Some(
            serde_json::json!({"claudeCode": {"kind": "mcpServerStatus"}}),
        )))
    };
    // Before the session is reported, nothing is followed up.
    lock(&gate).widgets = true;
    assert_eq!(forward(&gate, &reporter, status()), FollowUp::default());
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    assert_eq!(
        forward(&gate, &reporter, status()),
        FollowUp {
            list_tools: true,
            reads: Vec::new(),
        }
    );
    assert_eq!(forward(&gate, &reporter, text("hi")), FollowUp::default());
    lock(&gate).widgets = false;
    assert_eq!(forward(&gate, &reporter, status()), FollowUp::default());
    // A foreign session's status is not the session's.
    lock(&gate).widgets = true;
    let foreign = UpdateSessionNotification::new(
        "sub-1",
        notice(Some(
            serde_json::json!({"claudeCode": {"kind": "mcpServerStatus"}}),
        )),
    );
    assert_eq!(forward(&gate, &reporter, foreign), FollowUp::default());
    assert_eq!(drain(&mut inbox).len(), 4);
}

#[test]
fn a_missed_mcp_tool_asks_for_the_tools_only_from_an_adapter_serving_widgets() {
    let (_connections, reporter, _inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    let call = |id: &str| {
        let call = ToolCallUpdate::new(id).name("mcp__atlassian__getJiraIssue");
        own(SessionUpdate::ToolCallUpdate(call))
    };
    assert_eq!(forward(&gate, &reporter, call("w1")), FollowUp::default());
    let gate = own_gate();
    lock(&gate).widgets = true;
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    assert_eq!(
        forward(&gate, &reporter, call("w1")),
        FollowUp {
            list_tools: true,
            reads: Vec::new(),
        }
    );
}

#[test]
fn a_widget_tool_call_asks_for_its_widget() {
    let (_connections, reporter, _inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    let tools = serde_json::json!({"tools": [
        {"server": "atlassian", "tool": "getJiraIssue", "resourceUri": "ui://jira/issue"},
    ]});
    lock(&gate).state.set_widget_tools(widget_tools(&tools));
    let call = ToolCallUpdate::new("w1").name("mcp__atlassian__getJiraIssue");
    assert_eq!(
        forward(&gate, &reporter, own(SessionUpdate::ToolCallUpdate(call))),
        FollowUp {
            list_tools: false,
            reads: vec![("atlassian".to_owned(), "ui://jira/issue".to_owned())],
        }
    );
}

#[test]
fn admitting_the_session_drops_the_config_announced_before() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    forward(&gate, &reporter, own(config_update(model_option("sonnet"))));
    admit(&gate, schema::SessionId::new(OWN));
    open_gate(
        &gate,
        &reporter,
        vec![model_option("opus")],
        created(&session),
    );
    assert_eq!(
        drain(&mut inbox),
        [created(&session)(map_config_options(&[model_option(
            "opus"
        )]))]
    );
}

#[test]
fn config_announced_before_the_report_replaces_the_response_config() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    forward(&gate, &reporter, own(config_update(model_option("sonnet"))));
    open_gate(
        &gate,
        &reporter,
        vec![model_option("opus")],
        created(&session),
    );
    assert_eq!(
        drain(&mut inbox),
        [created(&session)(map_config_options(&[model_option(
            "sonnet"
        )]))]
    );
}

#[test]
fn without_announced_config_the_response_config_is_reported() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    open_gate(
        &gate,
        &reporter,
        vec![model_option("opus")],
        created(&session),
    );
    assert_eq!(
        drain(&mut inbox),
        [created(&session)(map_config_options(&[model_option(
            "opus"
        )]))]
    );
    assert!(lock(&gate).open);
}

#[test]
fn a_replaced_connection_neither_reports_its_session_nor_forwards_updates() {
    let (connections, old, mut inbox) = claimed();
    let session = old.session().clone();
    let (events, mut newer_inbox) = unbounded();
    let _new = connections.claim(session.clone(), events);
    let gate = own_gate();
    open_gate(&gate, &old, Vec::new(), created(&session));
    assert!(!lock(&gate).open);
    assert!(!connections.is_live(&session));
    forward(&gate, &old, text("late"));
    assert!(drain(&mut inbox).is_empty());
    assert!(drain(&mut newer_inbox).is_empty());
}

/// Reports `requests` through a fresh reporter and returns the events it sent, with the
/// parked permissions and the answers their responders gave.
fn ask_all(
    requests: &[RequestPermissionRequest],
) -> (
    SessionId,
    Vec<BackendEvent>,
    Mutex<PendingPermissions<Recorder>>,
    Answers,
) {
    let (_connections, reporter, mut inbox) = claimed();
    let permissions = Mutex::new(PendingPermissions::new(PermissionIds::default()));
    let answers = Arc::default();
    for request in requests {
        ask_permission(
            &permissions,
            &reporter,
            request,
            Recorder(Arc::clone(&answers)),
        );
    }
    (
        reporter.session().clone(),
        drain(&mut inbox),
        permissions,
        answers,
    )
}

#[test]
fn a_permission_request_is_reported_and_parked_until_answered() {
    let request = RequestPermissionRequest::new(
        "acp-session",
        "Edit main.rs",
        vec![PermissionOption::new(
            "allow-once",
            "Yes",
            PermissionOptionKind::AllowOnce,
        )],
    )
    .subject(RequestPermissionSubject::from(
        ToolCallUpdate::new("t-1")
            .title("Editing")
            .kind(schema::ToolKind::Edit)
            .name("mcp__partitur__propose_children"),
    ));
    let bare = RequestPermissionRequest::new("acp-session", "Bare", Vec::new())
        .subject(RequestPermissionSubject::from(ToolCallUpdate::new("t-2")));
    let (session, received, permissions, answers) = ask_all(&[request, bare]);

    // A fresh counter hands out ids from zero.
    let ids = [PermissionId(0), PermissionId(1)];
    assert_eq!(
        received,
        [
            BackendEvent::Update {
                session: session.clone(),
                update: Update::PermissionRequested {
                    id: ids[0],
                    tool_call_id: ToolCallId::new("t-1"),
                    // The request's title, not the call's.
                    title: "Edit main.rs".into(),
                    kind: ToolKind::Edit,
                    options: vec![partitur_core::PermissionOption {
                        id: PermissionOptionId::new("allow-once"),
                        name: "Yes".into(),
                        kind: PermissionKind::AllowOnce,
                    }],
                    name: Some("mcp__partitur__propose_children".into()),
                    locations: Vec::new(),
                    partitur_tool: true,
                },
            },
            BackendEvent::Update {
                session,
                update: Update::PermissionRequested {
                    id: ids[1],
                    tool_call_id: ToolCallId::new("t-2"),
                    title: "Bare".into(),
                    kind: ToolKind::Other,
                    options: Vec::new(),
                    name: None,
                    locations: Vec::new(),
                    partitur_tool: false,
                },
            },
        ]
    );

    lock(&permissions)
        .respond(ids[1], Some(PermissionOptionId::new("allow-once")))
        .unwrap();
    assert_eq!(
        *answers.lock().unwrap(),
        [Some(PermissionOptionId::new("allow-once"))]
    );
}

#[test]
fn a_permission_request_reports_the_calls_locations() {
    let read = RequestPermissionRequest::new("acp-session", "Read", Vec::new()).subject(
        RequestPermissionSubject::from(ToolCallUpdate::new("t-1").locations(vec![
            schema::ToolCallLocation::new("/wt/a.rs"),
            schema::ToolCallLocation::new("/wt/b.rs").line(3),
        ])),
    );
    let bare = RequestPermissionRequest::new("acp-session", "Bare", Vec::new())
        .subject(RequestPermissionSubject::from(ToolCallUpdate::new("t-2")));
    let command = RequestPermissionRequest::new("acp-session", "Run", Vec::new()).subject(
        RequestPermissionSubject::from(CommandPermissionSubject::new("ls", "/work")),
    );
    let (session, received, _permissions, _answers) = ask_all(&[read, bare, command]);

    let asked = |id: u64, call: &str, title: &str, locations: Vec<PathBuf>| BackendEvent::Update {
        session: session.clone(),
        update: Update::PermissionRequested {
            id: PermissionId(id),
            tool_call_id: ToolCallId::new(call),
            title: title.into(),
            kind: ToolKind::Other,
            options: Vec::new(),
            name: None,
            locations,
            partitur_tool: false,
        },
    };
    assert_eq!(
        received,
        [
            asked(
                0,
                "t-1",
                "Read",
                vec![PathBuf::from("/wt/a.rs"), PathBuf::from("/wt/b.rs")]
            ),
            asked(1, "t-2", "Bare", Vec::new()),
            asked(2, "", "Run", Vec::new()),
        ]
    );
}

#[test]
fn only_partitur_server_tools_are_flagged() {
    let flagged = is_partitur_tool;
    assert!(flagged(Some("mcp__partitur__propose_children")));
    assert!(!flagged(Some("mcp__partitur_extra__x")));
    assert!(!flagged(Some("mcp__other__x")));
    assert!(!flagged(Some("Bash")));
    assert!(!flagged(None));
}

#[test]
fn a_permission_request_about_no_tool_call_names_the_command_call_if_any() {
    let command = |call: Option<&str>| {
        let subject = CommandPermissionSubject::new("rm -rf target", "/work")
            .tool_call_id(call.map(schema::ToolCallId::new));
        RequestPermissionRequest::new("acp-session", "Run rm", Vec::new())
            .subject(RequestPermissionSubject::from(subject))
    };
    let unnamed = RequestPermissionRequest::new("acp-session", "Something", Vec::new());
    let (session, received, _permissions, _answers) =
        ask_all(&[command(Some("t-3")), command(None), unnamed]);
    let requested = |id, call: &str, title: &str| BackendEvent::Update {
        session: session.clone(),
        update: Update::PermissionRequested {
            id: PermissionId(id),
            tool_call_id: ToolCallId::new(call),
            title: title.into(),
            kind: ToolKind::Other,
            options: Vec::new(),
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        },
    };
    assert_eq!(
        received,
        [
            requested(0, "t-3", "Run rm"),
            requested(1, "", "Run rm"),
            requested(2, "", "Something"),
        ]
    );
}

#[test]
fn only_the_asking_settings_a_session_offers_are_set() {
    let option = |id: &str, values: &[&str]| {
        SessionConfigOption::select(
            id.to_owned(),
            id.to_uppercase(),
            "auto",
            values
                .iter()
                .map(|value| SessionConfigSelectOption::new(*value, value.to_uppercase()))
                .collect::<Vec<_>>(),
        )
    };
    let agent = "https://agentclientprotocol.com/protocol/session-modes#agent";
    let autopilot = "https://agentclientprotocol.com/protocol/session-modes#autopilot";
    // Claude's adapter.
    assert_eq!(
        asking_settings(&[model_option("opus"), option("mode", &["auto", "default"])]),
        [("mode", "default")]
    );
    // GitHub Copilot CLI.
    assert_eq!(
        asking_settings(&[
            option("mode", &[autopilot, agent]),
            option("allow_all", &["on", "off"]),
            model_option("gpt-5"),
        ]),
        [("mode", agent), ("allow_all", "off")]
    );
    // `default` wins over `#agent`, and each setting needs its own option and value.
    assert_eq!(
        asking_settings(&[option("mode", &[agent, "default"])]),
        [("mode", "default")]
    );
    assert_eq!(
        asking_settings(&[option("allow_all", &["on", "off"])]),
        [("allow_all", "off")]
    );
    assert!(asking_settings(&[option("mode", &["auto", "plan"])]).is_empty());
    assert!(asking_settings(&[option("agent", &["default"])]).is_empty());
    assert!(asking_settings(&[option("allow_all", &["on"])]).is_empty());
    assert!(asking_settings(&[option("off", &["off"])]).is_empty());
    assert!(asking_settings(&[]).is_empty());
}

#[test]
fn a_refused_prompt_ends_the_turn_cancelled_only_when_it_was_cancelled() {
    assert_eq!(
        refusal(&agent_client_protocol::Error::request_cancelled()),
        Ok(StopReason::Cancelled)
    );
    let error = agent_client_protocol::Error::auth_required();
    assert_eq!(refusal(&error), Err(error.to_string()));
}

#[test]
fn a_refusal_is_settled_through_the_session_state() {
    let (_connections, reporter, mut inbox) = claimed();
    let session = reporter.session().clone();
    let gate = own_gate();
    open_gate(&gate, &reporter, Vec::new(), created(&session));
    drain(&mut inbox);
    let cancelled = agent_client_protocol::Error::request_cancelled();
    // No prompt opened a turn: there is nothing to end.
    settle_refusal(&gate, &reporter, &cancelled);
    lock(&gate).state.prompted();
    settle_refusal(&gate, &reporter, &cancelled);
    settle_refusal(&gate, &reporter, &cancelled);
    assert_eq!(
        drain(&mut inbox),
        [BackendEvent::Update {
            session,
            update: Update::TurnEnded(StopReason::Cancelled)
        }]
    );
}

#[test]
fn a_closed_connection_needs_no_event() {
    let session = SessionId::new();
    assert_eq!(
        ending_event(RequestId(1), session.clone(), true, Ok(Ending::Closed)),
        None
    );
    assert_eq!(
        ending_event(RequestId(1), session, false, Ok(Ending::Closed)),
        None
    );
}

#[test]
fn a_connection_ending_before_the_report_fails_the_request() {
    let session = SessionId::new();
    let error = agent_client_protocol::Error::internal_error();
    let message = error.to_string();
    assert_eq!(
        ending_event(RequestId(4), session.clone(), false, Err(error)),
        Some(BackendEvent::RequestFailed {
            request: RequestId(4),
            session: session.clone(),
            error: message,
        })
    );
    assert_eq!(
        ending_event(
            RequestId(5),
            session.clone(),
            false,
            Ok(Ending::AgentExited)
        ),
        Some(BackendEvent::RequestFailed {
            request: RequestId(5),
            session,
            error: "the agent exited".into(),
        })
    );
}

#[test]
fn a_connection_ending_after_the_report_closes_the_session() {
    let session = SessionId::new();
    let error = agent_client_protocol::Error::internal_error();
    let message = error.to_string();
    assert_eq!(
        ending_event(RequestId(4), session.clone(), true, Err(error)),
        Some(BackendEvent::SessionClosed {
            session: session.clone(),
            error: message,
        })
    );
    assert_eq!(
        ending_event(RequestId(4), session.clone(), true, Ok(Ending::AgentExited)),
        Some(BackendEvent::SessionClosed {
            session,
            error: "the agent exited".into(),
        })
    );
}

#[test]
fn every_session_is_opened_with_the_tool_server() {
    let cwd = PathBuf::from("/work");
    let url = "http://127.0.0.1:4711/session".to_owned();
    let tool_server = [McpServer::Http(McpServerHttp::new("partitur", url.clone()))];
    let id = schema::SessionId::new("b-1");

    assert_eq!(
        new_session_request(cwd.clone(), url.clone()).mcp_servers,
        tool_server
    );
    let resume = resume_session_request(id.clone(), cwd.clone(), url);
    assert_eq!(resume.mcp_servers, tool_server);
    assert_eq!(
        resume.replay_from,
        Some(ReplayFrom::Start(ReplayFromStart::new()))
    );
    assert_eq!(
        (resume.session_id, resume.cwd),
        (id, schema::AbsolutePath::from(cwd))
    );
}

#[test]
fn both_opening_requests_append_the_tool_instructions() {
    let cwd = PathBuf::from("/work");
    let expected: schema::Meta = serde_json::from_value(serde_json::json!({
        "systemPrompt": {"append": partitur_core::tool_instructions()}
    }))
    .unwrap();
    let new = new_session_request(cwd.clone(), "http://x".into());
    let resume = resume_session_request(schema::SessionId::new("b-1"), cwd, "http://x".into());
    assert_eq!(new.meta, Some(expected.clone()));
    assert_eq!(resume.meta, Some(expected));
    assert!(new.meta.is_some_and(|m| m["systemPrompt"].is_object()));
}

#[test]
fn the_adapter_process_gets_the_configured_command_and_environment() {
    let config = AcpConfig {
        command: PathBuf::from("/opt/agent"),
        args: vec!["--stdio".into()],
        env: vec![
            ("AGENT_LOG".into(), "1".into()),
            // A configured variable of the same name loses to the thinking setting.
            (THINKING_DISPLAY_VAR.into(), "summarized".into()),
        ],
        thinking_display: ThinkingDisplay::Omitted,
        instructions_dir: None,
    };
    let agent = transport(&config);
    let spawned = agent.config();
    assert_eq!(spawned.command(), PathBuf::from("/opt/agent"));
    assert_eq!(spawned.arguments(), ["--stdio"]);
    let env = spawned.environment();
    assert_eq!(env.get("AGENT_LOG").map(String::as_str), Some("1"));
    assert_eq!(
        env.get(THINKING_DISPLAY_VAR).map(String::as_str),
        Some("omitted")
    );
    // Without an instructions folder the variable is left to the inherited environment.
    assert_eq!(env.get(INSTRUCTIONS_DIRS_VAR), None);
}

#[test]
fn instructions_dirs_append_to_a_configured_value() {
    let dir = Path::new("/p");
    assert_eq!(instructions_dirs(Some("/a,/b"), dir), "/a,/b,/p");
    assert_eq!(instructions_dirs(None, dir), "/p");
    assert_eq!(instructions_dirs(Some(""), dir), "/p");
}

#[test]
fn the_adapter_process_names_the_instructions_folder() {
    let mut config = AcpConfig {
        env: vec![
            (INSTRUCTIONS_DIRS_VAR.into(), "/a,/b".into()),
            ("AGENT_LOG".into(), "1".into()),
        ],
        instructions_dir: Some(PathBuf::from("/p")),
        ..AcpConfig::default()
    };
    let agent = transport(&config);
    let env = agent.config().environment();
    assert_eq!(
        env.get(INSTRUCTIONS_DIRS_VAR).map(String::as_str),
        Some("/a,/b,/p")
    );
    // Without a configured value, the one Partitur inherited is kept in front.
    config.env.remove(0);
    let inherited = std::env::var(INSTRUCTIONS_DIRS_VAR).ok();
    let agent = transport(&config);
    assert_eq!(
        agent.config().environment().get(INSTRUCTIONS_DIRS_VAR),
        Some(&instructions_dirs(inherited.as_deref(), Path::new("/p")))
    );
}

#[test]
fn only_an_uninstructed_session_is_told_so() {
    let (_connections, reporter, mut inbox) = claimed();
    notice_if_uninstructed(&reporter, false);
    assert_eq!(drain(&mut inbox), []);
    notice_if_uninstructed(&reporter, true);
    assert_eq!(
        drain(&mut inbox),
        [BackendEvent::Update {
            session: reporter.session().clone(),
            update: Update::Notice(
                "Partitur's instructions could not be written; workflows and questions \
                 will not work on agents that read them from a folder"
                    .to_owned()
            ),
        }]
    );
}
