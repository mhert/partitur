//! One session's connection: an adapter process spoken to over ACP.
//!
//! [`run_session`] spawns the adapter, initializes it, creates or loads the session (an
//! orchestrator session with Partitur's MCP tool server registered), switches it to the
//! `default` mode (the adapter starts sessions in `auto`, which would skip every
//! permission prompt; an agent without that mode gets a notice saying so), reports the
//! session to the core and then executes the session's commands until the runtime drops them
//! or the adapter goes away. Everything it reports goes through its [`Reporter`], which stops
//! a connection the runtime replaced from reaching the core.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    self, CancelNotification, ClientCapabilities, InitializeRequest, LoadSessionRequest, McpServer,
    McpServerHttp, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigOption, SessionConfigOptionValue, SessionModeState, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client};
use partitur_core::{
    BackendEvent, BackendSessionId, ConfigId, ConfigOption, ConfigValueId, PermissionId,
    PermissionOptionId, PromptContent, RequestId, SessionId, ToolCallId, ToolKind, Update,
};
use tokio::sync::mpsc;

use crate::map::{
    map_config_options, map_permission_options, map_prompt, map_stop_reason, map_tool_kind,
    map_update,
};
use crate::mcp::SERVER_NAME;
use crate::permissions::{Answer, PendingPermissions, PermissionIds};
use crate::runtime::{AcpConfig, Reporter, lock};

/// The mode Partitur keeps every session in: the agent asks before acting.
const DEFAULT_MODE: &str = "default";
/// The environment variable that selects how the adapter reports reasoning.
const THINKING_DISPLAY_VAR: &str = "CLAUDE_CODE_THINKING_DISPLAY";
/// What the user is told when the agent offers no [`DEFAULT_MODE`] to switch to.
const NO_DEFAULT_MODE_NOTICE: &str =
    "this agent offers no default (ask) mode; permissions follow the agent's own policy";

/// Whether a session is created fresh or resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Open {
    /// Creates a new session.
    New,
    /// Resumes the session the backend knows by this id.
    Load(BackendSessionId),
}

/// Everything needed to open one session.
#[derive(Debug)]
pub(crate) struct SessionStart {
    /// The request the created or loaded event answers.
    pub(crate) request: RequestId,
    /// The Partitur session.
    pub(crate) session: SessionId,
    /// Whether to create or load.
    pub(crate) open: Open,
    /// The directory the agent works in.
    pub(crate) cwd: PathBuf,
    /// Whether the session orchestrates child sessions and gets Partitur's tools.
    pub(crate) orchestrator: bool,
}

/// A command for an open session.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    /// Starts a turn with this prompt.
    Prompt(Vec<PromptContent>),
    /// Cancels the running turn.
    Cancel,
    /// Changes a configuration option.
    SetConfig {
        /// The option to change.
        option: ConfigId,
        /// Its new value.
        value: ConfigValueId,
    },
    /// Answers a permission request; `None` cancels it.
    RespondPermission {
        /// The request being answered.
        permission: PermissionId,
        /// The chosen option.
        option: Option<PermissionOptionId>,
    },
}

impl Answer for Responder<RequestPermissionResponse> {
    fn answer(
        self,
        option: Option<PermissionOptionId>,
    ) -> Result<(), agent_client_protocol::Error> {
        let outcome = match option {
            Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.as_str().to_owned(),
            )),
            None => RequestPermissionOutcome::Cancelled,
        };
        self.respond(RequestPermissionResponse::new(outcome))
    }
}

/// The parked permission requests of one session.
type Permissions = Arc<Mutex<PendingPermissions<Responder<RequestPermissionResponse>>>>;

/// Holds back the session's notifications until the core knows the session.
///
/// While a session loads, the adapter replays its history as notifications; the core renders
/// that history from its own transcript, so they are dropped. Config changes that arrive
/// before the session is reported are kept instead, so the reported config is current.
#[derive(Debug, Default)]
struct Gate {
    /// Whether the session was reported and notifications are forwarded.
    open: bool,
    /// The latest config announced while the gate was closed.
    config: Option<Vec<SessionConfigOption>>,
}

/// Forwards one notification to the core, or holds it back while the gate is closed.
///
/// The gate stays locked while the event is sent, so updates keep the agent's order.
fn forward(gate: &Mutex<Gate>, reporter: &Reporter, update: SessionUpdate) {
    let mut gate = lock(gate);
    if gate.open {
        if let Some(update) = map_update(update) {
            reporter.update(update);
        }
    } else if let SessionUpdate::ConfigOptionUpdate(changed) = update {
        gate.config = Some(changed.config_options);
    }
}

/// Reports the session to the core and opens the gate.
///
/// `opened` builds the created or loaded event from the session's config: the latest one
/// announced while the gate was closed, else `config` from the create or load response. The
/// session becomes live first, so the core can command it as soon as the event arrives. A
/// replaced connection reports nothing and keeps its gate closed.
fn open_gate(
    gate: &Mutex<Gate>,
    reporter: &Reporter,
    config: Vec<SessionConfigOption>,
    opened: impl FnOnce(Vec<ConfigOption>) -> BackendEvent,
) {
    let mut gate = lock(gate);
    let config = gate.config.take().unwrap_or(config);
    gate.open = reporter.go_live(opened(map_config_options(&config)));
}

/// Reports a permission request to the core and parks its responder until the answer
/// arrives as a [`SessionCommand::RespondPermission`].
fn ask_permission<R: Answer>(
    permissions: &Mutex<PendingPermissions<R>>,
    reporter: &Reporter,
    request: &RequestPermissionRequest,
    responder: R,
) {
    let fields = &request.tool_call.fields;
    let mut permissions = lock(permissions);
    let id = permissions.open(responder);
    reporter.update(Update::PermissionRequested {
        id,
        tool_call_id: ToolCallId::new(request.tool_call.tool_call_id.to_string()),
        title: fields.title.clone().unwrap_or_default(),
        kind: fields.kind.map_or(ToolKind::Other, map_tool_kind),
        options: map_permission_options(&request.options),
        name: fields.name.clone(),
    });
}

/// Whether `modes` offers the mode Partitur wants.
fn offers_default_mode(modes: Option<&SessionModeState>) -> bool {
    modes.is_some_and(|modes| {
        modes
            .available_modes
            .iter()
            .any(|mode| &*mode.id.0 == DEFAULT_MODE)
    })
}

/// How a session's command loop ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The runtime dropped the session's commands: shutdown, or a newer connection.
    Closed,
    /// The adapter closed its output.
    AgentExited,
}

/// The event that reports how a connection ended, if the core needs one.
///
/// Before the session was reported, the create or load request failed; afterwards the
/// session closed and must be re-opened. A connection the runtime closed needs no event; nor
/// does a replaced one, whose [`Reporter`] never sends this.
fn ending_event(
    request: RequestId,
    session: SessionId,
    opened: bool,
    result: Result<Ending, agent_client_protocol::Error>,
) -> Option<BackendEvent> {
    let error = match result {
        Ok(Ending::Closed) => return None,
        Ok(Ending::AgentExited) => "the agent exited".to_owned(),
        Err(error) => error.to_string(),
    };
    Some(if opened {
        BackendEvent::SessionClosed { session, error }
    } else {
        BackendEvent::RequestFailed {
            request,
            session,
            error,
        }
    })
}

/// The adapter process for one session, with its child's stderr and the wire logged at
/// `debug`.
fn transport(config: &AcpConfig) -> AcpAgent {
    let agent_config = AcpAgentConfig::new(&config.command)
        .args(&config.args)
        .envs(config.env.iter().cloned())
        .env(THINKING_DISPLAY_VAR, config.thinking_display.env_value());
    AcpAgent::new(agent_config)
        .with_debug(|line, direction| tracing::debug!(?direction, "acp: {line}"))
}

/// The MCP servers a session is opened with: Partitur's tool server at `tools_url`, if any.
fn mcp_servers(tools_url: Option<String>) -> Vec<McpServer> {
    tools_url
        .map(|url| McpServer::Http(McpServerHttp::new(SERVER_NAME, url)))
        .into_iter()
        .collect()
}

/// The request creating a session in `cwd`, with the tool server at `tools_url`, if any.
fn new_session_request(cwd: PathBuf, tools_url: Option<String>) -> NewSessionRequest {
    NewSessionRequest::new(cwd).mcp_servers(mcp_servers(tools_url))
}

/// The request loading the session `id` in `cwd`, with the tool server at `tools_url`, if
/// any: an agent knows a session's MCP servers only from the request that opens it.
fn load_session_request(
    id: schema::SessionId,
    cwd: PathBuf,
    tools_url: Option<String>,
) -> LoadSessionRequest {
    LoadSessionRequest::new(id, cwd).mcp_servers(mcp_servers(tools_url))
}

/// The state a session's handlers and command loop share.
struct Shared {
    reporter: Reporter,
    gate: Arc<Mutex<Gate>>,
    permissions: Permissions,
}

/// Runs one session from spawning its adapter until its connection ends, then reports how
/// it ended and forgets the session, all through `reporter`, the session's current
/// connection when the runtime opened this one. Its permission requests take their ids from
/// `permission_ids`. With `tools_url`, the session is opened with the tool server there.
pub(crate) async fn run_session(
    config: Arc<AcpConfig>,
    start: SessionStart,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    reporter: Reporter,
    permission_ids: PermissionIds,
    tools_url: Option<String>,
) {
    let SessionStart {
        request, open, cwd, ..
    } = start;
    let shared = Arc::new(Shared {
        reporter,
        gate: Arc::default(),
        permissions: Arc::new(Mutex::new(PendingPermissions::new(permission_ids))),
    });
    let notifications = Arc::clone(&shared);
    let permission_requests = Arc::clone(&shared);
    let result = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let shared = &notifications;
                forward(&shared.gate, &shared.reporter, notification.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let shared = &permission_requests;
                ask_permission(&shared.permissions, &shared.reporter, &request, responder);
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport(&config), async |cx: ConnectionTo<Agent>| {
            drive(cx, &shared, request, open, cwd, tools_url, commands).await
        })
        .await;
    let reporter = &shared.reporter;
    let session = reporter.session();
    tracing::info!(%session, ?result, "ACP session ended");
    reporter.retire(|live| ending_event(request, session.clone(), live, result));
}

/// The connection's main task: opens the session, with the tool server at `tools_url` if
/// any, reports it, then runs its commands.
async fn drive(
    cx: ConnectionTo<Agent>,
    shared: &Shared,
    request: RequestId,
    open: Open,
    cwd: PathBuf,
    tools_url: Option<String>,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
) -> Result<Ending, agent_client_protocol::Error> {
    cx.send_request(
        InitializeRequest::new(ProtocolVersion::V1).client_capabilities(ClientCapabilities::new()),
    )
    .block_task()
    .await?;
    let (id, modes, config) = match &open {
        Open::New => {
            let response = cx
                .send_request(new_session_request(cwd, tools_url))
                .block_task()
                .await?;
            (response.session_id, response.modes, response.config_options)
        }
        Open::Load(id) => {
            let id = schema::SessionId::new(id.as_str().to_owned());
            let response = cx
                .send_request(load_session_request(id.clone(), cwd, tools_url))
                .block_task()
                .await?;
            (id, response.modes, response.config_options)
        }
    };
    // Config announced during a load replay is history; only later changes are current.
    lock(&shared.gate).config = None;
    let asks = offers_default_mode(modes.as_ref());
    if asks {
        cx.send_request(SetSessionModeRequest::new(id.clone(), DEFAULT_MODE))
            .block_task()
            .await?;
    }
    let session = shared.reporter.session().clone();
    let backend_session = BackendSessionId::new(id.to_string());
    open_gate(
        &shared.gate,
        &shared.reporter,
        config.unwrap_or_default(),
        |config| match open {
            Open::New => BackendEvent::SessionCreated {
                request,
                session,
                backend_session,
                config,
            },
            Open::Load(_) => BackendEvent::SessionLoaded {
                request,
                session,
                config,
            },
        },
    );
    if !asks {
        shared
            .reporter
            .update(Update::Notice(NO_DEFAULT_MODE_NOTICE.to_owned()));
    }
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = cx.incoming_closed() => return Ok(Ending::AgentExited),
        };
        let Some(command) = command else {
            return Ok(Ending::Closed);
        };
        execute(&cx, shared, &id, command)?;
    }
}

/// Executes one command without waiting for the agent, so a cancel or a permission answer
/// is never stuck behind a running turn.
fn execute(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    id: &schema::SessionId,
    command: SessionCommand,
) -> Result<(), agent_client_protocol::Error> {
    let reporter = shared.reporter.clone();
    match command {
        SessionCommand::Prompt(content) => {
            let turn = cx.send_request(PromptRequest::new(id.clone(), map_prompt(content)));
            cx.spawn(async move {
                let update = match turn.block_task().await {
                    Ok(response) => Update::TurnEnded(map_stop_reason(response.stop_reason)),
                    Err(error) => Update::Failed(error.to_string()),
                };
                reporter.update(update);
                Ok(())
            })
        }
        SessionCommand::Cancel => cx.send_notification(CancelNotification::new(id.clone())),
        SessionCommand::SetConfig { option, value } => {
            let change = cx.send_request(SetSessionConfigOptionRequest::new(
                id.clone(),
                option.as_str().to_owned(),
                SessionConfigOptionValue::value_id(value.as_str().to_owned()),
            ));
            cx.spawn(async move {
                let update = match change.block_task().await {
                    Ok(response) => {
                        Update::ConfigOptions(map_config_options(&response.config_options))
                    }
                    Err(error) => Update::Notice(format!("could not change {option}: {error}")),
                };
                reporter.update(update);
                Ok(())
            })
        }
        SessionCommand::RespondPermission { permission, option } => {
            if let Err(error) = lock(&shared.permissions).respond(permission, option) {
                let session = reporter.session();
                tracing::warn!(%session, %error, "dropping a permission answer");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use agent_client_protocol::v1::schema::{
        ConfigOptionUpdate, ContentBlock, ContentChunk, PermissionOption, PermissionOptionKind,
        SessionConfigSelectOption, SessionMode, TextContent, ToolCallUpdate, ToolCallUpdateFields,
    };
    use futures::channel::mpsc::{UnboundedReceiver, unbounded};
    use partitur_core::PermissionKind;

    use super::*;
    use crate::runtime::{Connections, ThinkingDisplay};

    /// Collects the answers given to parked requests.
    struct Recorder(Arc<Mutex<Vec<Option<PermissionOptionId>>>>);

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

    fn text(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text.to_owned(),
        ))))
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
        let gate = Mutex::new(Gate::default());
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
    fn config_announced_before_the_report_replaces_the_response_config() {
        let (_connections, reporter, mut inbox) = claimed();
        let session = reporter.session().clone();
        let gate = Mutex::new(Gate::default());
        forward(
            &gate,
            &reporter,
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![model_option(
                "sonnet",
            )])),
        );
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
        let gate = Mutex::new(Gate::default());
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
        let gate = Mutex::new(Gate::default());
        open_gate(&gate, &old, Vec::new(), created(&session));
        assert!(!lock(&gate).open);
        assert!(!connections.is_live(&session));
        forward(&gate, &old, text("late"));
        assert!(drain(&mut inbox).is_empty());
        assert!(drain(&mut newer_inbox).is_empty());
    }

    #[test]
    fn a_permission_request_is_reported_and_parked_until_answered() {
        let (_connections, reporter, mut inbox) = claimed();
        let session = reporter.session().clone();
        let permissions = Mutex::new(PendingPermissions::new(PermissionIds::default()));
        let answers = Arc::default();
        let request = RequestPermissionRequest::new(
            "acp-session",
            ToolCallUpdate::new(
                "t-1",
                ToolCallUpdateFields::new()
                    .title("Edit main.rs".to_owned())
                    .kind(schema::ToolKind::Edit)
                    .name("mcp__partitur__propose_children".to_owned()),
            ),
            vec![PermissionOption::new(
                "allow-once",
                "Yes",
                PermissionOptionKind::AllowOnce,
            )],
        );
        ask_permission(
            &permissions,
            &reporter,
            &request,
            Recorder(Arc::clone(&answers)),
        );
        let bare = RequestPermissionRequest::new(
            "acp-session",
            ToolCallUpdate::new("t-2", ToolCallUpdateFields::new()),
            Vec::new(),
        );
        ask_permission(
            &permissions,
            &reporter,
            &bare,
            Recorder(Arc::clone(&answers)),
        );

        // A fresh counter hands out ids from zero.
        let ids = [PermissionId(0), PermissionId(1)];
        let received = drain(&mut inbox);
        assert_eq!(received.len(), 2);
        assert_eq!(
            received[0],
            BackendEvent::Update {
                session: session.clone(),
                update: Update::PermissionRequested {
                    id: ids[0],
                    tool_call_id: ToolCallId::new("t-1"),
                    title: "Edit main.rs".into(),
                    kind: ToolKind::Edit,
                    options: vec![partitur_core::PermissionOption {
                        id: PermissionOptionId::new("allow-once"),
                        name: "Yes".into(),
                        kind: PermissionKind::AllowOnce,
                    }],
                    name: Some("mcp__partitur__propose_children".into()),
                },
            }
        );
        assert_eq!(
            received[1],
            BackendEvent::Update {
                session,
                update: Update::PermissionRequested {
                    id: ids[1],
                    tool_call_id: ToolCallId::new("t-2"),
                    title: String::new(),
                    kind: ToolKind::Other,
                    options: Vec::new(),
                    name: None,
                },
            }
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
    fn only_a_session_offering_the_default_mode_is_switched() {
        let modes = |ids: &[&'static str]| {
            SessionModeState::new(
                "auto",
                ids.iter()
                    .map(|id| SessionMode::new(*id, id.to_uppercase()))
                    .collect(),
            )
        };
        assert!(offers_default_mode(Some(&modes(&["auto", "default"]))));
        assert!(!offers_default_mode(Some(&modes(&["auto", "plan"]))));
        assert!(!offers_default_mode(None));
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
    fn only_an_orchestrator_session_is_opened_with_the_tool_server() {
        let cwd = PathBuf::from("/work");
        let url = "http://127.0.0.1:4711/session".to_owned();
        let tool_server = [McpServer::Http(McpServerHttp::new("partitur", url.clone()))];
        let id = schema::SessionId::new("b-1");

        assert!(
            new_session_request(cwd.clone(), None)
                .mcp_servers
                .is_empty()
        );
        assert_eq!(
            new_session_request(cwd.clone(), Some(url.clone())).mcp_servers,
            tool_server
        );
        let load = load_session_request(id.clone(), cwd.clone(), None);
        assert!(load.mcp_servers.is_empty());
        let load = load_session_request(id.clone(), cwd.clone(), Some(url));
        assert_eq!(load.mcp_servers, tool_server);
        assert_eq!((load.session_id, load.cwd), (id, cwd));
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
    }
}
