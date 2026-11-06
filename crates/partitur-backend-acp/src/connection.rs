//! One session's connection: an adapter process spoken to over ACP.
//!
//! [`run_session`] spawns the adapter, initializes it, creates or loads the session, switches
//! it to the `default` mode (the adapter starts sessions in `auto`, which would skip every
//! permission prompt), reports the session to the core and then executes the session's
//! commands until the runtime drops them or the adapter goes away.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    self, CancelNotification, ClientCapabilities, InitializeRequest, LoadSessionRequest,
    NewSessionRequest, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigOption,
    SessionConfigOptionValue, SessionModeState, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionModeRequest,
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
use crate::permissions::{Answer, PendingPermissions};
use crate::runtime::{AcpConfig, Events, LiveSessions, emit, lock};

/// The mode Partitur keeps every session in: the agent asks before acting.
const DEFAULT_MODE: &str = "default";
/// The environment variable that selects how the adapter reports reasoning.
const THINKING_DISPLAY_VAR: &str = "CLAUDE_CODE_THINKING_DISPLAY";

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
fn forward(gate: &Mutex<Gate>, events: &Events, session: &SessionId, update: SessionUpdate) {
    let mut gate = lock(gate);
    if gate.open {
        if let Some(update) = map_update(update) {
            emit(
                events,
                BackendEvent::Update {
                    session: session.clone(),
                    update,
                },
            );
        }
    } else if let SessionUpdate::ConfigOptionUpdate(changed) = update {
        gate.config = Some(changed.config_options);
    }
}

/// Reports the session to the core and opens the gate.
///
/// `opened` builds the created or loaded event from the session's config: the latest one
/// announced while the gate was closed, else `config` from the create or load response. The
/// session becomes live first, so the core can command it as soon as the event arrives.
fn open_gate(
    gate: &Mutex<Gate>,
    live: &LiveSessions,
    events: &Events,
    session: &SessionId,
    config: Vec<SessionConfigOption>,
    opened: impl FnOnce(Vec<ConfigOption>) -> BackendEvent,
) {
    let mut gate = lock(gate);
    let config = gate.config.take().unwrap_or(config);
    lock(live).insert(session.clone());
    emit(events, opened(map_config_options(&config)));
    gate.open = true;
}

/// Reports a permission request to the core and parks its responder until the answer
/// arrives as a [`SessionCommand::RespondPermission`].
fn ask_permission<R: Answer>(
    permissions: &Mutex<PendingPermissions<R>>,
    events: &Events,
    session: &SessionId,
    request: &RequestPermissionRequest,
    responder: R,
) {
    let fields = &request.tool_call.fields;
    let mut permissions = lock(permissions);
    let id = permissions.open(responder);
    emit(
        events,
        BackendEvent::Update {
            session: session.clone(),
            update: Update::PermissionRequested {
                id,
                tool_call_id: ToolCallId::new(request.tool_call.tool_call_id.to_string()),
                title: fields.title.clone().unwrap_or_default(),
                kind: fields.kind.map_or(ToolKind::Other, map_tool_kind),
                options: map_permission_options(&request.options),
            },
        },
    );
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
/// session itself failed. A connection the runtime closed needs no event.
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
        BackendEvent::Update {
            session,
            update: Update::Failed(format!("connection closed: {error}")),
        }
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

/// The state a session's handlers and command loop share.
struct Shared {
    session: SessionId,
    events: Events,
    gate: Arc<Mutex<Gate>>,
    live: LiveSessions,
    permissions: Permissions,
}

/// Runs one session from spawning its adapter until its connection ends, then reports how
/// it ended and forgets the session.
pub(crate) async fn run_session(
    config: Arc<AcpConfig>,
    start: SessionStart,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    events: Events,
    live: LiveSessions,
) {
    let SessionStart {
        request,
        session,
        open,
        cwd,
    } = start;
    let shared = Arc::new(Shared {
        session: session.clone(),
        events: events.clone(),
        gate: Arc::default(),
        live: Arc::clone(&live),
        permissions: Arc::default(),
    });
    let notifications = Arc::clone(&shared);
    let permission_requests = Arc::clone(&shared);
    let result = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let shared = &notifications;
                forward(
                    &shared.gate,
                    &shared.events,
                    &shared.session,
                    notification.update,
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let shared = &permission_requests;
                ask_permission(
                    &shared.permissions,
                    &shared.events,
                    &shared.session,
                    &request,
                    responder,
                );
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport(&config), async |cx: ConnectionTo<Agent>| {
            drive(cx, &shared, request, open, cwd, commands).await
        })
        .await;
    lock(&live).remove(&session);
    let opened = lock(&shared.gate).open;
    tracing::info!(%session, ?result, "ACP session ended");
    if let Some(event) = ending_event(request, session, opened, result) {
        emit(&events, event);
    }
}

/// The connection's main task: opens the session, reports it, then runs its commands.
async fn drive(
    cx: ConnectionTo<Agent>,
    shared: &Shared,
    request: RequestId,
    open: Open,
    cwd: PathBuf,
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
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            (response.session_id, response.modes, response.config_options)
        }
        Open::Load(id) => {
            let id = schema::SessionId::new(id.as_str().to_owned());
            let response = cx
                .send_request(LoadSessionRequest::new(id.clone(), cwd))
                .block_task()
                .await?;
            (id, response.modes, response.config_options)
        }
    };
    // Config announced during a load replay is history; only later changes are current.
    lock(&shared.gate).config = None;
    if offers_default_mode(modes.as_ref()) {
        cx.send_request(SetSessionModeRequest::new(id.clone(), DEFAULT_MODE))
            .block_task()
            .await?;
    }
    let session = shared.session.clone();
    let backend_session = BackendSessionId::new(id.to_string());
    open_gate(
        &shared.gate,
        &shared.live,
        &shared.events,
        &shared.session,
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
    let session = shared.session.clone();
    let events = shared.events.clone();
    match command {
        SessionCommand::Prompt(content) => {
            let turn = cx.send_request(PromptRequest::new(id.clone(), map_prompt(content)));
            cx.spawn(async move {
                let update = match turn.block_task().await {
                    Ok(response) => Update::TurnEnded(map_stop_reason(response.stop_reason)),
                    Err(error) => Update::Failed(error.to_string()),
                };
                emit(&events, BackendEvent::Update { session, update });
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
                emit(&events, BackendEvent::Update { session, update });
                Ok(())
            })
        }
        SessionCommand::RespondPermission { permission, option } => {
            if let Err(error) = lock(&shared.permissions).respond(permission, option) {
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
    use crate::runtime::ThinkingDisplay;

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
        let (events, mut inbox) = unbounded();
        let live = LiveSessions::default();
        let gate = Mutex::new(Gate::default());
        let session = SessionId::new();
        forward(&gate, &events, &session, text("replayed"));
        assert!(drain(&mut inbox).is_empty());

        open_gate(
            &gate,
            &live,
            &events,
            &session,
            Vec::new(),
            created(&session),
        );
        assert!(lock(&live).contains(&session));
        forward(&gate, &events, &session, text("live"));
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
        let (events, mut inbox) = unbounded();
        let gate = Mutex::new(Gate::default());
        let session = SessionId::new();
        forward(
            &gate,
            &events,
            &session,
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![model_option(
                "sonnet",
            )])),
        );
        open_gate(
            &gate,
            &LiveSessions::default(),
            &events,
            &session,
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
        let (events, mut inbox) = unbounded();
        let gate = Mutex::new(Gate::default());
        let session = SessionId::new();
        open_gate(
            &gate,
            &LiveSessions::default(),
            &events,
            &session,
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
    fn a_permission_request_is_reported_and_parked_until_answered() {
        let (events, mut inbox) = unbounded();
        let permissions = Mutex::new(PendingPermissions::default());
        let answers = Arc::default();
        let session = SessionId::new();
        let request = RequestPermissionRequest::new(
            "acp-session",
            ToolCallUpdate::new(
                "t-1",
                ToolCallUpdateFields::new()
                    .title("Edit main.rs".to_owned())
                    .kind(schema::ToolKind::Edit),
            ),
            vec![PermissionOption::new(
                "allow-once",
                "Yes",
                PermissionOptionKind::AllowOnce,
            )],
        );
        ask_permission(
            &permissions,
            &events,
            &session,
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
            &events,
            &session,
            &bare,
            Recorder(Arc::clone(&answers)),
        );

        // Ids count up from zero per session.
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
    fn a_connection_ending_after_the_report_fails_the_session() {
        let session = SessionId::new();
        let error = agent_client_protocol::Error::internal_error();
        let message = error.to_string();
        assert_eq!(
            ending_event(RequestId(4), session.clone(), true, Err(error)),
            Some(BackendEvent::Update {
                session: session.clone(),
                update: Update::Failed(format!("connection closed: {message}")),
            })
        );
        assert_eq!(
            ending_event(RequestId(4), session.clone(), true, Ok(Ending::AgentExited)),
            Some(BackendEvent::Update {
                session,
                update: Update::Failed("connection closed: the agent exited".into()),
            })
        );
    }

    #[test]
    fn the_adapter_process_gets_the_configured_command_and_environment() {
        let config = AcpConfig {
            command: PathBuf::from("/opt/agent"),
            args: vec!["--stdio".into()],
            env: vec![("AGENT_LOG".into(), "1".into())],
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
