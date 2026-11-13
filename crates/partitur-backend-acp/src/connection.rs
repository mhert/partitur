//! One session's connection: an adapter process spoken to over ACP v2.
//!
//! [`run_session`] spawns the adapter, initializes it, creates or resumes the session (with
//! Partitur's MCP tool server registered), switches its `mode`
//! option to `default` (the adapter starts sessions in `auto`, which would skip every
//! permission prompt; an agent without that mode gets a notice saying so), reports the
//! session to the core and then executes the session's commands until the runtime drops them
//! or the adapter goes away. Everything it reports goes through its [`Reporter`], which stops
//! a connection the runtime replaced from reaching the core.
//!
//! A turn ends when the agent reports its foreground work idle, not when the prompt is
//! answered: the agent takes a prompt in at once, and it may also start work of its own, for
//! example once a background command finishes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::schema::{
    self, CancelSessionNotification, CancelledPermissionOutcome, Implementation, InitializeRequest,
    McpServer, McpServerHttp, NewSessionRequest, PromptRequest, ReplayFrom, ReplayFromStart,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    RequestPermissionSubject, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigOption,
    SessionConfigOptionValue, SessionUpdate, SetSessionConfigOptionRequest,
    UpdateSessionNotification,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ErrorCode};
use partitur_core::{
    BackendEvent, BackendSessionId, ConfigId, ConfigOption, ConfigValueId, PermissionId,
    PermissionOptionId, PromptContent, RequestId, SessionId, StopReason, TOOL_SERVER, ToolCallId,
    ToolKind, Update,
};
use tokio::sync::mpsc;

use crate::map::{
    SessionState, map_config_options, map_permission_options, map_prompt, map_tool_kind, map_update,
};
use crate::permissions::{Answer, PendingPermissions, PermissionIds};
use crate::runtime::{AcpConfig, Reporter, lock};

/// The config option holding the session's mode.
const MODE_OPTION: &str = "mode";
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
            None => RequestPermissionOutcome::Cancelled(CancelledPermissionOutcome::new()),
        };
        self.respond(RequestPermissionResponse::new(outcome))
    }
}

/// The parked permission requests of one session.
type Permissions = Arc<Mutex<PendingPermissions<Responder<RequestPermissionResponse>>>>;

/// Holds back the session's notifications until the core knows the session.
///
/// Only the session's own notifications pass: the adapter also streams the sessions of its
/// subagents on the same connection, and before the create or resume response names the
/// session, nothing is its. While a session resumes, the adapter replays its history and its
/// current state as notifications; the core renders that history from its own transcript, so
/// they are dropped, and the session's state starts with no work open. Config changes that
/// arrive before the session is reported are kept instead, so the reported config is current.
#[derive(Debug, Default)]
struct Gate {
    /// The session's id at the agent, once the create or resume response named it.
    session: Option<schema::SessionId>,
    /// Whether the session was reported and notifications are forwarded.
    open: bool,
    /// The latest config announced while the gate was closed.
    config: Option<Vec<SessionConfigOption>>,
    /// What the forwarded updates have established.
    state: SessionState,
}

/// Forwards one notification to the core, or holds it back while the gate is closed. A
/// notification for another session is dropped.
///
/// The gate stays locked while the event is sent, so updates keep the agent's order.
fn forward(gate: &Mutex<Gate>, reporter: &Reporter, notification: UpdateSessionNotification) {
    let mut gate = lock(gate);
    if gate.session.as_ref() != Some(&notification.session_id) {
        return;
    }
    let update = notification.update;
    if gate.open {
        if let Some(update) = map_update(update, &mut gate.state) {
            reporter.update(update);
        }
    } else if let SessionUpdate::ConfigOptionUpdate(changed) = update {
        gate.config = Some(changed.config_options);
    }
}

/// Lets the notifications of the session the agent knows as `id` reach the gate, dropping
/// whatever config they announced so far: config announced during a resume replay is
/// history, and only later changes are current.
fn admit(gate: &Mutex<Gate>, id: schema::SessionId) {
    let mut gate = lock(gate);
    gate.session = Some(id);
    gate.config = None;
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
///
/// The request's title is shown. A tool call the request is about gives the call's id, kind
/// tool name and paths; a command gives the id of the call running it, if any, and kind
/// [`ToolKind::Other`], as does a request about anything else, which names no call.
fn ask_permission<R: Answer>(
    permissions: &Mutex<PendingPermissions<R>>,
    reporter: &Reporter,
    request: &RequestPermissionRequest,
    responder: R,
) {
    let (tool_call_id, kind, name, locations) = match &request.subject {
        Some(RequestPermissionSubject::ToolCall(subject)) => {
            let call = &subject.tool_call;
            (
                call.tool_call_id.to_string(),
                call.kind.value().map_or(ToolKind::Other, map_tool_kind),
                call.name.value().cloned(),
                call.locations
                    .value()
                    .map(|locations| locations.iter().map(|at| at.path.0.clone()).collect())
                    .unwrap_or_default(),
            )
        }
        Some(RequestPermissionSubject::Command(command)) => (
            command
                .tool_call_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            ToolKind::Other,
            None,
            Vec::new(),
        ),
        _ => (String::new(), ToolKind::Other, None, Vec::new()),
    };
    let partitur_tool = is_partitur_tool(name.as_deref());
    let mut permissions = lock(permissions);
    let id = permissions.open(responder);
    reporter.update(Update::PermissionRequested {
        id,
        tool_call_id: ToolCallId::new(tool_call_id),
        title: request.title.clone(),
        kind,
        options: map_permission_options(&request.options),
        name,
        locations,
        partitur_tool,
    });
}

/// Whether `name`, a tool's name as the agent reports it, is one of the tools Partitur's own
/// MCP server offers: the agent prefixes those with `mcp__`, the server's name and `__`.
fn is_partitur_tool(name: Option<&str>) -> bool {
    let prefix = format!("mcp__{TOOL_SERVER}__");
    name.is_some_and(|name| name.starts_with(&prefix))
}

/// Whether `config` has a select option [`MODE_OPTION`] offering the mode Partitur wants.
fn offers_default_mode(config: &[SessionConfigOption]) -> bool {
    map_config_options(config).iter().any(|option| {
        option.id.as_str() == MODE_OPTION
            && option
                .choices
                .iter()
                .any(|choice| choice.id.as_str() == DEFAULT_MODE)
    })
}

/// How a turn ends whose prompt the agent refused: cancelled when the prompt was cancelled
/// before the agent took it in, else failed with the error's message.
fn refusal(error: &agent_client_protocol::Error) -> Result<StopReason, String> {
    if error.code == ErrorCode::REQUEST_CANCELLED {
        Ok(StopReason::Cancelled)
    } else {
        Err(error.to_string())
    }
}

/// Settles a prompt the agent refused with `error` through the session's state, which
/// reports the turn's end only if no `idle` has ended or will end it.
///
/// The gate stays locked while the event is sent, so the end keeps its place among the
/// forwarded updates.
fn settle_refusal(gate: &Mutex<Gate>, reporter: &Reporter, error: &agent_client_protocol::Error) {
    let mut gate = lock(gate);
    if let Some(update) = gate.state.refused(refusal(error)) {
        reporter.update(update);
    }
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

/// The MCP servers a session is opened with: Partitur's tool server at `tools_url`.
fn mcp_servers(tools_url: String) -> Vec<McpServer> {
    vec![McpServer::Http(McpServerHttp::new(TOOL_SERVER, tools_url))]
}

/// The request creating a session in `cwd`, with the tool server at `tools_url`.
fn new_session_request(cwd: PathBuf, tools_url: String) -> NewSessionRequest {
    NewSessionRequest::new(cwd).mcp_servers(mcp_servers(tools_url))
}

/// The request resuming the session `id` in `cwd` with its history replayed from the start,
/// with the tool server at `tools_url`: an agent knows a session's MCP servers only
/// from the request that opens it.
fn resume_session_request(
    id: schema::SessionId,
    cwd: PathBuf,
    tools_url: String,
) -> ResumeSessionRequest {
    ResumeSessionRequest::new(id, cwd)
        .mcp_servers(mcp_servers(tools_url))
        .replay_from(ReplayFrom::Start(ReplayFromStart::new()))
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
/// `permission_ids`. The session is opened with the tool server at `tools_url`.
pub(crate) async fn run_session(
    config: Arc<AcpConfig>,
    start: SessionStart,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    reporter: Reporter,
    permission_ids: PermissionIds,
    tools_url: String,
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
        .v2()
        .on_receive_notification(
            async move |notification: UpdateSessionNotification, _cx| {
                let shared = &notifications;
                forward(&shared.gate, &shared.reporter, notification);
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

/// The connection's main task: opens the session, with the tool server at `tools_url`
/// any, switches it to the default mode, reports it, then runs its commands.
async fn drive(
    cx: ConnectionTo<Agent>,
    shared: &Shared,
    request: RequestId,
    open: Open,
    cwd: PathBuf,
    tools_url: String,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
) -> Result<Ending, agent_client_protocol::Error> {
    cx.send_request(InitializeRequest::new(
        ProtocolVersion::V2,
        Implementation::new("partitur", env!("CARGO_PKG_VERSION")),
    ))
    .block_task()
    .await?;
    let (id, config) = match &open {
        Open::New => {
            let response = cx
                .send_request(new_session_request(cwd, tools_url))
                .block_task()
                .await?;
            (response.session_id, response.config_options)
        }
        Open::Load(id) => {
            let id = schema::SessionId::new(id.as_str().to_owned());
            let response = cx
                .send_request(resume_session_request(id.clone(), cwd, tools_url))
                .block_task()
                .await?;
            (id, response.config_options)
        }
    };
    // Notifications that arrive between the response and this point are dropped as not yet
    // the session's; the adapter sends none there that the reported session would miss, as
    // the mode switch below answers with the whole config.
    admit(&shared.gate, id.clone());
    let asks = offers_default_mode(&config);
    let config = if asks {
        let switch = SetSessionConfigOptionRequest::new(id.clone(), MODE_OPTION, DEFAULT_MODE);
        cx.send_request(switch).block_task().await?.config_options
    } else {
        config
    };
    let session = shared.reporter.session().clone();
    let backend_session = BackendSessionId::new(id.to_string());
    open_gate(
        &shared.gate,
        &shared.reporter,
        config,
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
///
/// A prompt the agent takes in reports nothing: the session's state reports the turn's end.
/// A prompt sent while the agent already works on a turn of its own, one whose start had not
/// reached the core yet, is held by the agent behind that work; the `idle` ending that work
/// then ends the turn the core opened for the prompt, and the prompt's own work reads as a
/// turn the agent started.
fn execute(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    id: &schema::SessionId,
    command: SessionCommand,
) -> Result<(), agent_client_protocol::Error> {
    let reporter = shared.reporter.clone();
    match command {
        SessionCommand::Prompt(content) => {
            lock(&shared.gate).state.prompted();
            let turn = cx.send_request(PromptRequest::new(id.clone(), map_prompt(content)));
            let gate = Arc::clone(&shared.gate);
            cx.spawn(async move {
                if let Err(error) = turn.block_task().await {
                    settle_refusal(&gate, &reporter, &error);
                }
                Ok(())
            })
        }
        SessionCommand::Cancel => cx.send_notification(CancelSessionNotification::new(id.clone())),
        SessionCommand::SetConfig { option, value } => {
            let change = cx.send_request(SetSessionConfigOptionRequest::new(
                id.clone(),
                option.as_str().to_owned(),
                SessionConfigOptionValue::id(value.as_str().to_owned()),
            ));
            cx.spawn(async move {
                let update = match change.block_task().await {
                    Ok(response) => {
                        Update::ConfigOptions(map_config_options(&response.config_options))
                    }
                    Err(error) => Update::ConfigRejected {
                        option,
                        value,
                        error: error.to_string(),
                    },
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

    use agent_client_protocol::v2::schema::{
        CommandPermissionSubject, ConfigOptionUpdate, ContentBlock, ContentChunk, IdleStateUpdate,
        PermissionOption, PermissionOptionKind, RunningStateUpdate, SessionConfigSelectOption,
        StateUpdate, StopReason as SchemaStopReason, TextContent, ToolCallUpdate,
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

        let asked =
            |id: u64, call: &str, title: &str, locations: Vec<PathBuf>| BackendEvent::Update {
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
    fn only_a_session_offering_the_default_mode_is_switched() {
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
        assert!(offers_default_mode(&[
            model_option("opus"),
            option("mode", &["auto", "default"])
        ]));
        assert!(!offers_default_mode(&[option("mode", &["auto", "plan"])]));
        assert!(!offers_default_mode(&[option("agent", &["default"])]));
        assert!(!offers_default_mode(&[]));
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
