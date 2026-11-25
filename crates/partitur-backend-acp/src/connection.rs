//! One session's connection: an adapter process spoken to over ACP v2, or over v1 for an
//! agent that speaks only that.
//!
//! [`run_session`] spawns the adapter and initializes it with ACP v2. An agent that answers
//! with protocol version 1, such as GitHub Copilot CLI, is handed to the v1 client of
//! [`crate::connection_v1`] on the same process, which translates its traffic into the v2
//! values the code here reads. Either client then runs the one session loop of
//! [`crate::protocol`] over its protocol version; the rest of this description is the v2
//! client.
//!
//! The v2 client creates or resumes the session (with Partitur's MCP tool server registered
//! and the `_meta` its profile opens sessions with, [`AgentProfile::session_meta`], which for
//! Claude's adapter appends the tool instructions to the system prompt), keeps it asking
//! before it acts (it switches the `mode` option to `default`, as the adapter starts sessions
//! in `auto`, which would skip every permission prompt; [`asking_settings`] says what an agent
//! without that mode is switched to, and an agent offering nothing of the kind gets a notice
//! saying so), reports the session to the core and then executes the session's commands until
//! the runtime drops them or the adapter goes away. Everything it reports goes through its
//! [`Reporter`], which stops a connection the runtime replaced from reaching the core.
//!
//! A turn ends when the agent reports its foreground work idle, not when the prompt is
//! answered: the agent takes a prompt in at once, and it may also start work of its own, for
//! example once a background command finishes. A cancel cancels the turn and every subagent
//! the agent lists as running.
//!
//! What differs for one agent is its profile's, whichever protocol it speaks: both clients take
//! the agent's [`AgentProfile`] from its `initialize` answer ([`crate::agent::for_agent`]) and
//! call its hooks. Every update is read as the profile adapts it
//! ([`AgentProfile::adapt_update`]), so Claude's tool calls named only in its `_meta` are named,
//! and only a notice the profile marks as an MCP server's status
//! ([`AgentProfile::reports_server_status`]) asks for a new widget tool list. For GitHub Copilot
//! CLI ([`crate::agent::copilot`]), on the v2 path a Copilot's session events report its
//! state, its subagents' updates are dropped, its prompts are held while one is unanswered (by
//! the session loop), and its orchestrator is started again without its editing and shell
//! tools, as on the v1 path. Titles naming Partitur's tools concern how Copilot speaks v1 and
//! are read there.
//!
//! An adapter whose `initialize` response carries `_meta.partitur.widgets`, Partitur's patched
//! one, serves MCP App widgets. Only then does the connection ask it for the tools that declare
//! a widget, with the extension request `_partitur/widget_tools`, once the session is open and
//! again whenever the adapter reports an MCP server's status, and when the first call of an MCP
//! tool without a known widget arrives while no list is under way: its server may have
//! connected late, and a list that names the tool gives that call its widget after all. A
//! tool still unknown then asks for no list again. A tool call of such a tool whose
//! widget was not read yet makes the connection read it with `_partitur/read_widget`, one read
//! per widget however many calls wait for it, and report the widget, or why it could not be
//! read, as a change of every waiting call. Neither request is waited for: prompts, commands
//! and updates flow on while the adapter answers. Each answer is taken in before the next
//! message from the adapter is, so a tool call reported after it sees the tools it listed.

use std::collections::HashMap;
use std::future::ready;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::schema::{
    self, CancelSessionNotification, CancelledPermissionOutcome, ContentBlock, Implementation,
    InitializeRequest, InitializeResponse, McpServer, NewSessionRequest, PromptRequest, ReplayFrom,
    ReplayFromStart, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    RequestPermissionSubject, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigOption,
    SessionConfigOptionValue, SessionUpdate, SetSessionConfigOptionRequest,
    UpdateSessionNotification,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ErrorCode, UntypedMessage,
};
use partitur_core::{
    AgentUpdate, BackendEvent, BackendSessionId, ConfigId, ConfigOption, ConfigValueId,
    PermissionId, PermissionOptionId, PromptContent, RequestId, SessionFailure, SessionId,
    StopReason, TOOL_SERVER, ToolCallId, ToolKind, ToolRole,
};
use tokio::sync::mpsc;

use crate::agent::copilot::{SessionEventNotification, with_exclusion};
use crate::agent::generic::Generic;
use crate::agent::{
    AgentProfile, ProcessStart, dropped, for_agent, initialize_meta, process_env, restart_without,
};
use crate::connection_v1::V1Client;
use crate::map::{
    SessionState, map_config_options, map_permission_options, map_tool_kind, map_update,
    widget_tools,
};
use crate::permissions::{Answer, PendingPermissions, PermissionIds};
use crate::protocol::{OpenSession, Opened, Protocol, drive};
use crate::runtime::{AcpConfig, Reporter, lock};

/// The config option holding the session's mode.
const MODE_OPTION: &str = "mode";
/// The mode Partitur keeps every session in: the agent asks before acting.
const DEFAULT_MODE: &str = "default";
/// The asking mode of an agent without [`DEFAULT_MODE`], such as GitHub Copilot CLI: ACP's
/// session mode `agent`.
const AGENT_MODE: &str = "https://agentclientprotocol.com/protocol/session-modes#agent";
/// GitHub Copilot CLI's config option that, on, runs every tool without asking.
const ALLOW_ALL_OPTION: &str = "allow_all";
/// The value of [`ALLOW_ALL_OPTION`] that keeps the agent asking.
const ALLOW_ALL_OFF: &str = "off";
/// What the user is told when Partitur's instructions folder could not be written.
pub(crate) const UNINSTRUCTED_NOTICE: &str = "Partitur's instructions could not be written; \
     workflows and questions will not work on agents that read them from a folder";
/// What the user is told when the agent offers no asking setting to switch to.
pub(crate) const NO_DEFAULT_MODE_NOTICE: &str =
    "this agent offers no default (ask) mode; permissions follow the agent's own policy";
/// The patched adapter's request listing the tools that declare a widget.
const WIDGET_TOOLS: &str = "_partitur/widget_tools";
/// The patched adapter's request reading a widget's `ui://` resource.
const READ_WIDGET: &str = "_partitur/read_widget";

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
    /// The tools the session is offered.
    pub(crate) role: ToolRole,
}

/// A command for an open session.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    /// Starts a turn with this prompt.
    Prompt(Vec<PromptContent>),
    /// Cancels the running turn and every subagent the agent lists as running.
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

/// Holds back the session's notifications until the core knows the session.
///
/// Only the session's own notifications pass: the adapter also streams the sessions of its
/// subagents on the same connection, and before the create or resume response names the
/// session, nothing is its. While a session resumes, the adapter replays its history and its
/// current state as notifications; the core renders that history from its own transcript, so
/// they are dropped, and the session's state starts with no work open. Config changes that
/// arrive before the session is reported are kept instead, so the reported config is current.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// The session's id at the agent, once the create or resume response named it.
    session: Option<schema::SessionId>,
    /// Whether the session was reported and notifications are forwarded.
    open: bool,
    /// The latest config announced while the gate was closed.
    config: Option<Vec<SessionConfigOption>>,
    /// What the forwarded updates have established.
    state: SessionState,
    /// Whether the adapter serves widgets.
    widgets: bool,
}

impl Gate {
    /// Records that a prompt was sent, or held for the agent: the core sees a turn open from
    /// now on.
    pub(crate) fn prompted(&mut self) {
        self.state.prompted();
    }
}

/// What the connection still has to ask the adapter after forwarding a notification.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowUp {
    /// Whether to list the tools that declare a widget again.
    list_tools: bool,
    /// The widgets to read, server and URI.
    reads: Vec<(String, String)>,
}

/// Forwards one notification to the core, read as `profile` adapts it, or holds it back while
/// the gate is closed. A notification for another session is dropped.
///
/// A state update reports the number of items its background list names, when that changed,
/// before what the state itself means: the `idle` that ends a turn while background work
/// still runs never shows the session done without it.
///
/// The gate stays locked while the event is sent, so updates keep the agent's order.
///
/// Returns what the connection has to ask the adapter next: the widgets the forwarded update
/// made the session read, and, from an adapter that serves widgets, whether to list the tools
/// that declare one again: after it reported an MCP server's status, as `profile` reads it, or
/// a call of an MCP tool without a known widget that the session wants a new list for.
pub(crate) fn forward(
    gate: &Mutex<Gate>,
    reporter: &Reporter,
    profile: &dyn AgentProfile,
    notification: UpdateSessionNotification,
) -> FollowUp {
    let mut gate = lock(gate);
    let mut follow_up = FollowUp::default();
    if gate.session.as_ref() != Some(&notification.session_id) {
        return follow_up;
    }
    let update = notification.update;
    if gate.open {
        if let SessionUpdate::StateUpdate(state) = &update
            && let Some(count) = gate.state.background(state)
        {
            reporter.update(count);
        }
        let status = profile.reports_server_status(&update);
        if let Some(update) = map_update(profile.adapt_update(update), &mut gate.state) {
            reporter.update(update);
        }
        let wanted = gate.state.take_list_wanted();
        follow_up.list_tools = gate.widgets && (status || wanted);
        follow_up.reads = gate.state.take_reads();
    } else if let SessionUpdate::ConfigOptionUpdate(changed) = update {
        gate.config = Some(changed.config_options);
    }
    follow_up
}

/// Whether the adapter that answered `initialize` with `response` serves widgets: it says so
/// in `_meta.partitur.widgets`.
fn serves_widgets(response: &InitializeResponse) -> bool {
    let widgets = response.meta.as_ref().and_then(|meta| {
        let widgets = meta.get("partitur")?.get("widgets")?;
        Some(widgets)
    });
    widgets.is_some_and(|widgets| !widgets.is_null())
}

/// The tools that declare a widget, from the adapter's `reply` to `_partitur/widget_tools`; a
/// failed request lists none, and is logged.
fn listed_tools(
    reply: Result<serde_json::Value, agent_client_protocol::Error>,
) -> HashMap<String, (String, String)> {
    reply.map_or_else(
        |error| {
            tracing::warn!(%error, "the adapter did not list its widget tools");
            HashMap::new()
        },
        |reply| widget_tools(&reply),
    )
}

/// Asks the adapter for the tools of the session `id` that declare a widget, and makes them the
/// session's once it answers: the calls that waited for the list and whose tool it names are
/// reported with their widget, and the widgets they need are read.
///
/// # Errors
///
/// Returns the error of a request that cannot be sent.
fn list_widget_tools(
    cx: &ConnectionTo<Agent>,
    shared: &Arc<Shared>,
    id: &schema::SessionId,
) -> Result<(), agent_client_protocol::Error> {
    let request = UntypedMessage::new(WIDGET_TOOLS, serde_json::json!({ "sessionId": id }))?;
    lock(&shared.gate).state.listing();
    let (cx, shared, id) = (cx.clone(), Arc::clone(shared), id.clone());
    cx.clone()
        .send_request(request)
        .on_receiving_result(move |reply| {
            let reads = {
                let mut gate = lock(&shared.gate);
                for update in gate.state.set_widget_tools(listed_tools(reply)) {
                    shared.reporter.update(update);
                }
                gate.state.take_reads()
            };
            let sent = reads
                .into_iter()
                .try_for_each(|(server, uri)| read_widget(&cx, &shared, &id, server, uri));
            ready(sent)
        })
}

/// Reads the widget `uri` of `server` for the session `id` and reports it, or why it could not
/// be read, as a change of every call waiting for it.
///
/// # Errors
///
/// Returns the error of a request that cannot be sent.
fn read_widget(
    cx: &ConnectionTo<Agent>,
    shared: &Shared,
    id: &schema::SessionId,
    server: String,
    uri: String,
) -> Result<(), agent_client_protocol::Error> {
    let params = serde_json::json!({ "sessionId": id, "server": server, "uri": uri });
    let request = UntypedMessage::new(READ_WIDGET, params)?;
    let (gate, reporter) = (Arc::clone(&shared.gate), shared.reporter.clone());
    cx.send_request(request).on_receiving_result(move |reply| {
        let reply = reply.map_err(|error| error.to_string());
        let mut gate = lock(&gate);
        for update in gate.state.widget_read(&server, &uri, reply) {
            reporter.update(update);
        }
        ready(Ok(()))
    })
}

/// Sends the requests `follow_up` asks for, for the session `id`.
///
/// # Errors
///
/// Returns the error of a request that cannot be sent.
fn follow(
    cx: &ConnectionTo<Agent>,
    shared: &Arc<Shared>,
    id: &schema::SessionId,
    follow_up: FollowUp,
) -> Result<(), agent_client_protocol::Error> {
    if follow_up.list_tools {
        list_widget_tools(cx, shared, id)?;
    }
    follow_up
        .reads
        .into_iter()
        .try_for_each(|(server, uri)| read_widget(cx, shared, id, server, uri))
}

/// Lets the notifications of the session the agent knows as `id` reach the gate, dropping
/// whatever config they announced so far: config announced during a resume replay is
/// history, and only later changes are current.
pub(crate) fn admit(gate: &Mutex<Gate>, id: schema::SessionId) {
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
pub(crate) fn open_gate(
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
pub(crate) fn ask_permission<R: Answer>(
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
    reporter.update(AgentUpdate::PermissionRequested {
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

/// The option and value pairs that keep a session with `config` asking before it acts, in
/// the order they are set: [`MODE_OPTION`] to [`DEFAULT_MODE`] when it offers that, else to
/// [`AGENT_MODE`] when it offers that, then [`ALLOW_ALL_OPTION`] to [`ALLOW_ALL_OFF`] when it
/// offers that. None when the agent offers none of them. Every agent is kept asking alike,
/// whatever its profile.
pub(crate) fn asking_settings(config: &[SessionConfigOption]) -> Vec<(&'static str, &'static str)> {
    let options = map_config_options(config);
    let offers = |id: &str, value: &str| {
        options.iter().any(|option| {
            option.id.as_str() == id
                && option
                    .choices
                    .iter()
                    .any(|choice| choice.id.as_str() == value)
        })
    };
    let mode = [DEFAULT_MODE, AGENT_MODE]
        .into_iter()
        .find(|mode| offers(MODE_OPTION, mode))
        .map(|mode| (MODE_OPTION, mode));
    let allow_all =
        offers(ALLOW_ALL_OPTION, ALLOW_ALL_OFF).then_some((ALLOW_ALL_OPTION, ALLOW_ALL_OFF));
    mode.into_iter().chain(allow_all).collect()
}

/// Tells the user, when `settings` is empty, that the agent offers nothing that keeps it
/// asking.
pub(crate) fn notice_unless_asking(reporter: &Reporter, settings: &[(&str, &str)]) {
    if settings.is_empty() {
        reporter.update(AgentUpdate::Notice(NO_DEFAULT_MODE_NOTICE.to_owned()));
    }
}

/// Tells the user, when the session's instructions folder could not be written
/// (`uninstructed`), that agents reading their instructions from a folder miss Partitur's.
pub(crate) fn notice_if_uninstructed(reporter: &Reporter, uninstructed: bool) {
    if uninstructed {
        reporter.update(AgentUpdate::Notice(UNINSTRUCTED_NOTICE.to_owned()));
    }
}

/// How a turn ends whose prompt the agent refused: cancelled when the prompt was cancelled
/// before the agent took it in, else failed as [`failure_of`] says.
fn refusal(error: &agent_client_protocol::Error) -> Result<StopReason, SessionFailure> {
    if error.code == ErrorCode::REQUEST_CANCELLED {
        Ok(StopReason::Cancelled)
    } else {
        Err(failure_of(error))
    }
}

/// The failure an error of the agent's means, carrying the error's message: a login the agent
/// asks for when it has [`ErrorCode::AUTH_REQUIRED`], else a failed request.
fn failure_of(error: &agent_client_protocol::Error) -> SessionFailure {
    if error.code == ErrorCode::AUTH_REQUIRED {
        SessionFailure::AuthRequired(error.to_string())
    } else {
        SessionFailure::RequestFailed(error.to_string())
    }
}

/// Settles a prompt the agent refused with `error` through the session's state, which
/// reports the turn's end only if no `idle` has ended or will end it.
///
/// The gate stays locked while the event is sent, so the end keeps its place among the
/// forwarded updates.
pub(crate) fn settle_refusal(
    gate: &Mutex<Gate>,
    reporter: &Reporter,
    error: &agent_client_protocol::Error,
) {
    let mut gate = lock(gate);
    if let Some(update) = gate.state.refused(refusal(error)) {
        reporter.update(update);
    }
}

/// How a session's command loop ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// The runtime dropped the session's commands: shutdown, or a newer connection.
    Closed,
    /// The adapter closed its output.
    AgentExited,
    /// The client opened no session, as the agent must be started again with these tools
    /// excluded ([`crate::agent::restart_without`]).
    Restart(&'static [&'static str]),
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
        Ok(Ending::Closed | Ending::Restart(_)) => return None,
        Ok(Ending::AgentExited) => SessionFailure::AgentExited,
        Err(error) => failure_of(&error),
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

/// The adapter process for one session in `role`, with its child's stderr and the wire logged
/// at `debug`. Its environment is decided before the agent is known, as every profile needs it
/// ([`process_env`]), with Partitur's own environment as the inherited one.
fn transport(config: &AcpConfig, role: ToolRole) -> AcpAgent {
    let inherited = |name: &str| std::env::var(name).ok();
    let env = process_env(&ProcessStart {
        config,
        role,
        inherited: &inherited,
    });
    let agent_config = AcpAgentConfig::new(&config.command)
        .args(&config.args)
        .envs(env);
    AcpAgent::new(agent_config)
        .with_debug(|line, direction| tracing::debug!(?direction, "acp: {line}"))
}

/// `config` with its arguments excluding `tools` ([`with_exclusion`]).
fn excluding_tools(config: &AcpConfig, tools: &[&str]) -> AcpConfig {
    let mut config = config.clone();
    config.args = with_exclusion(&config.args, tools);
    config
}

/// The request creating a session in `cwd`, with `servers` and `meta`, the profile's
/// [`AgentProfile::session_meta`].
fn new_session_request(
    cwd: PathBuf,
    servers: Vec<McpServer>,
    meta: Option<schema::Meta>,
) -> NewSessionRequest {
    NewSessionRequest::new(cwd).mcp_servers(servers).meta(meta)
}

/// The request resuming the session `id` in `cwd` with its history replayed from the start,
/// with `servers`: an agent knows a session's MCP servers only from the request that opens it.
/// It carries `meta`, the profile's [`AgentProfile::session_meta`].
fn resume_session_request(
    id: schema::SessionId,
    cwd: PathBuf,
    servers: Vec<McpServer>,
    meta: Option<schema::Meta>,
) -> ResumeSessionRequest {
    ResumeSessionRequest::new(id, cwd)
        .mcp_servers(servers)
        .meta(meta)
        .replay_from(ReplayFrom::Start(ReplayFromStart::new()))
}

/// The state a session's handlers and command loop share, its permission requests answered
/// through responders of the type `R`.
pub(crate) struct Shared<R = Responder<RequestPermissionResponse>> {
    /// Reports to the core.
    pub(crate) reporter: Reporter,
    /// Holds the session's notifications back until it is reported.
    pub(crate) gate: Arc<Mutex<Gate>>,
    /// The parked permission requests.
    pub(crate) permissions: Arc<Mutex<PendingPermissions<R>>>,
    /// The agent's profile, once its `initialize` answer named it; [`Generic`] before.
    pub(crate) profile: Mutex<&'static dyn AgentProfile>,
}

impl<R: Answer> Shared<R> {
    /// The state of a connection reporting through `reporter`, behind `gate`, its permission
    /// requests taking their ids from `ids`.
    pub(crate) fn new(reporter: Reporter, gate: Arc<Mutex<Gate>>, ids: PermissionIds) -> Self {
        Self {
            reporter,
            gate,
            permissions: Arc::new(Mutex::new(PendingPermissions::new(ids))),
            profile: Mutex::new(&Generic),
        }
    }
}

/// What opening one session needs, shared by the clients the connector may start for it.
///
/// The connector starts the v2 client first and, when the agent negotiates v1, the v1 client
/// in its place, so the session's commands wait here: the client whose `initialize` the agent
/// answered takes them, and leaves how its command loop ended.
pub(crate) struct Opening {
    /// The request the created or loaded event answers.
    pub(crate) request: RequestId,
    /// Whether to create or load.
    pub(crate) open: Open,
    /// The directory the agent works in.
    pub(crate) cwd: PathBuf,
    /// The URL of the session's tools.
    pub(crate) tools_url: String,
    /// The tools the session is offered.
    pub(crate) role: ToolRole,
    /// Whether the session's instructions folder could not be written, which the user is told
    /// once the session is reported.
    pub(crate) uninstructed: bool,
    /// The session's commands, until a client takes them.
    commands: Mutex<Option<mpsc::UnboundedReceiver<SessionCommand>>>,
    /// How the command loop ended, once it did.
    ending: Mutex<Option<Ending>>,
}

impl Opening {
    /// Takes the session's commands.
    ///
    /// # Errors
    ///
    /// Returns an internal error when a client took them already.
    pub(crate) fn commands(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<SessionCommand>, agent_client_protocol::Error> {
        lock(&self.commands)
            .take()
            .ok_or_else(agent_client_protocol::Error::internal_error)
    }

    /// Records how the command loop ended.
    pub(crate) fn end(&self, ending: Ending) {
        *lock(&self.ending) = Some(ending);
    }

    /// How the command loop ended; a connection that ended before its loop did ended with the
    /// agent.
    fn ending(&self) -> Ending {
        lock(&self.ending).take().unwrap_or(Ending::AgentExited)
    }

    /// The opening of a plain session as `open` says, in `/work` with the tools at `tools_url`,
    /// whose commands the test hands to the loop itself.
    #[cfg(test)]
    pub(crate) fn for_test(open: Open, tools_url: &str, uninstructed: bool) -> Self {
        Self {
            request: RequestId(7),
            open,
            cwd: PathBuf::from("/work"),
            tools_url: tools_url.to_owned(),
            role: ToolRole::Plain,
            uninstructed,
            commands: Mutex::new(None),
            ending: Mutex::new(None),
        }
    }
}

/// Builds the created or loaded event of the session `id`, opened as `open` for `request`,
/// from its config.
pub(crate) fn opened(
    request: RequestId,
    reporter: &Reporter,
    id: &schema::SessionId,
    open: Open,
) -> impl FnOnce(Vec<ConfigOption>) -> BackendEvent {
    let session = reporter.session().clone();
    let backend_session = BackendSessionId::new(id.to_string());
    move |config| match open {
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
    }
}

/// What a change of `option` to `value` reports: the options the agent answered with, or why
/// it refused.
pub(crate) fn config_change(
    option: ConfigId,
    value: ConfigValueId,
    answer: Result<Vec<SessionConfigOption>, agent_client_protocol::Error>,
) -> AgentUpdate {
    match answer {
        Ok(options) => AgentUpdate::ConfigOptions(map_config_options(&options)),
        Err(error) => AgentUpdate::ConfigRejected {
            option,
            value,
            error: error.to_string(),
        },
    }
}

/// Answers the parked permission request `permission` with `option`; an answer to a request
/// that is not open is dropped and logged.
pub(crate) fn respond_permission<R: Answer>(
    shared: &Shared<R>,
    permission: PermissionId,
    option: Option<PermissionOptionId>,
) {
    if let Err(error) = lock(&shared.permissions).respond(permission, option) {
        let session = shared.reporter.session();
        tracing::warn!(%session, %error, "dropping a permission answer");
    }
}

/// The client the connector runs for an agent that speaks ACP v2.
struct V2Client {
    /// The state its handlers and command loop share.
    shared: Arc<Shared>,
    /// What opening the session needs.
    opening: Arc<Opening>,
    /// The arguments the agent process was started with.
    args: Arc<[String]>,
}

impl ConnectTo<Agent> for V2Client {
    async fn connect_to(
        self,
        agent: impl ConnectTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Self {
            shared,
            opening,
            args,
        } = self;
        let notifications = Arc::clone(&shared);
        let permission_requests = Arc::clone(&shared);
        let events = Arc::clone(&shared);
        Client
            .v2()
            .on_receive_notification(
                async move |notification: UpdateSessionNotification, cx: ConnectionTo<Agent>| {
                    let shared = &notifications;
                    let profile = *lock(&shared.profile);
                    if dropped(profile, &notification.update) {
                        return Ok(());
                    }
                    let id = notification.session_id.clone();
                    let follow_up = forward(&shared.gate, &shared.reporter, profile, notification);
                    follow(&cx, shared, &id, follow_up)
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_notification(
                async move |event: SessionEventNotification, _cx| {
                    let profile = *lock(&events.profile);
                    if let Some(state) = profile.session_event(&event.0) {
                        forward(&events.gate, &events.reporter, profile, state);
                    }
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
            .connect_with(agent, async |cx: ConnectionTo<Agent>| {
                let initialized = cx.send_request(initialize_request()).block_task().await?;
                let profile = for_agent(Some(initialized.info.name.as_str()));
                tracing::debug!(profile = profile.label(), "agent profile");
                *lock(&shared.profile) = profile;
                if let Some(tools) = restart_without(profile, opening.role, &args) {
                    opening.end(Ending::Restart(tools));
                    return Ok(());
                }
                lock(&shared.gate).widgets = serves_widgets(&initialized);
                let commands = opening.commands()?;
                let v2 = V2 {
                    cx,
                    shared: Arc::clone(&shared),
                };
                let ending = drive(v2, &shared, &opening, commands, profile).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v2 `initialize`. It carries the opt-in decided before the agent is known
/// ([`initialize_meta`]): the connector reuses the connection for the v1 client only when the
/// v1 `initialize` is this one, and that one carries it too. Claude's adapter ignores the
/// `_meta`.
pub(crate) fn initialize_request() -> InitializeRequest {
    InitializeRequest::new(
        ProtocolVersion::V2,
        Implementation::new("partitur", env!("CARGO_PKG_VERSION")),
    )
    .capabilities(schema::ClientCapabilities::new().meta(initialize_meta()))
}

/// Runs one session from spawning its adapter until its connection ends, then reports how
/// it ended and forgets the session, all through `reporter`, the session's current
/// connection when the runtime opened this one. Its permission requests take their ids from
/// `permission_ids`. The session is opened with the tool server at `tools_url`, and when
/// `uninstructed` the user is told its instructions folder could not be written.
///
/// The connection starts with ACP v2; an agent that answers with v1 continues on the same
/// process with the v1 client. When either client ends the connection with
/// [`Ending::Restart`], the adapter is started once more with its arguments excluding the
/// tools it names ([`excluding_tools`]), and the session is opened on that process.
pub(crate) async fn run_session(
    mut config: Arc<AcpConfig>,
    start: SessionStart,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    reporter: Reporter,
    permission_ids: PermissionIds,
    tools_url: String,
    uninstructed: bool,
) {
    let SessionStart {
        request,
        open,
        cwd,
        role,
        ..
    } = start;
    let gate: Arc<Mutex<Gate>> = Arc::default();
    let v2 = Arc::new(Shared::new(
        reporter.clone(),
        Arc::clone(&gate),
        Arc::clone(&permission_ids),
    ));
    let v1 = Arc::new(Shared::new(reporter.clone(), gate, permission_ids));
    let opening = Arc::new(Opening {
        request,
        open,
        cwd,
        tools_url,
        role,
        uninstructed,
        commands: Mutex::new(Some(commands)),
        ending: Mutex::new(None),
    });
    let session = reporter.session();
    let result = loop {
        let (v2, v1) = (Arc::clone(&v2), Arc::clone(&v1));
        let (v2_opening, v1_opening) = (Arc::clone(&opening), Arc::clone(&opening));
        let args: Arc<[String]> = config.args.clone().into();
        let v2_args = Arc::clone(&args);
        let spawned = Arc::clone(&config);
        let connected = Client
            .protocol_connector()
            .with_v2(move || V2Client {
                shared: Arc::clone(&v2),
                opening: Arc::clone(&v2_opening),
                args: Arc::clone(&v2_args),
            })
            .with_v1(move || V1Client {
                shared: Arc::clone(&v1),
                opening: Arc::clone(&v1_opening),
                args: Arc::clone(&args),
            })
            .connect_to(move || transport(&spawned, role))
            .await;
        // The discarded process's exit, however it went, is no ending of the session.
        let ending = opening.ending();
        let Ending::Restart(tools) = ending else {
            break connected.map(|()| ending);
        };
        tracing::info!(%session, "restarting the orchestrator's agent without editing tools");
        config = Arc::new(excluding_tools(&config, tools));
    };
    tracing::info!(%session, ?result, "ACP session ended");
    reporter.retire(|live| ending_event(request, session.clone(), live, result));
}

/// The v2 requests of an open session, over the connection `cx`.
#[derive(Clone)]
struct V2 {
    /// The connection to the adapter.
    cx: ConnectionTo<Agent>,
    /// The state its handlers and command loop share.
    shared: Arc<Shared>,
}

impl Protocol for V2 {
    async fn open(&self, request: OpenSession) -> Result<Opened, agent_client_protocol::Error> {
        let OpenSession {
            open,
            cwd,
            mcp_servers,
        } = request;
        Ok(match open {
            Open::New => {
                let meta = lock(&self.shared.profile).session_meta();
                let request = new_session_request(cwd, mcp_servers, meta);
                let response = self.cx.send_request(request).block_task().await?;
                Opened {
                    id: response.session_id,
                    config: response.config_options,
                }
            }
            Open::Load(id) => {
                let id = schema::SessionId::new(id.as_str().to_owned());
                let meta = lock(&self.shared.profile).session_meta();
                let request = resume_session_request(id.clone(), cwd, mcp_servers, meta);
                let response = self.cx.send_request(request).block_task().await?;
                Opened {
                    id,
                    config: response.config_options,
                }
            }
        })
    }

    /// Asks an adapter that serves widgets for the tools that declare one.
    fn admitted(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error> {
        if lock(&self.shared.gate).widgets {
            list_widget_tools(&self.cx, &self.shared, id)?;
        }
        Ok(())
    }

    fn set_config(
        &self,
        id: &schema::SessionId,
        option: &str,
        value: &str,
    ) -> impl Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
    + Send
    + 'static {
        let change = self.cx.send_request(SetSessionConfigOptionRequest::new(
            id.clone(),
            option.to_owned(),
            SessionConfigOptionValue::id(value.to_owned()),
        ));
        async move {
            change
                .block_task()
                .await
                .map(|response| response.config_options)
        }
    }

    /// The adapter takes a prompt in at once and reports the turn's end in the session's
    /// state, so its answer reports nothing but a refusal. A prompt sent while the agent
    /// already works on a turn of its own, one whose start had not reached the core yet, is
    /// held by the agent behind that work; the `idle` ending that work then ends the turn the
    /// core opened for the prompt, and the prompt's own work reads as a turn the agent started.
    fn prompt(
        &self,
        id: &schema::SessionId,
        prompt: Vec<ContentBlock>,
        answered: impl FnOnce() -> Result<(), agent_client_protocol::Error> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error> {
        lock(&self.shared.gate).prompted();
        let turn = self.cx.send_request(PromptRequest::new(id.clone(), prompt));
        let (gate, reporter) = (Arc::clone(&self.shared.gate), self.shared.reporter.clone());
        self.cx.spawn(async move {
            if let Err(error) = turn.block_task().await {
                settle_refusal(&gate, &reporter, &error);
            }
            answered()
        })
    }

    /// Cancels the session and every subagent the agent lists as running.
    fn cancel(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error> {
        let subagents: Vec<schema::SessionId> =
            lock(&self.shared.gate).state.subagents().cloned().collect();
        std::iter::once(id.clone())
            .chain(subagents)
            .try_for_each(|session| {
                self.cx
                    .send_notification(CancelSessionNotification::new(session))
            })
    }

    fn spawn(
        &self,
        task: impl Future<Output = Result<(), agent_client_protocol::Error>> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error> {
        self.cx.spawn(task)
    }

    fn closed(&self) -> impl Future<Output = ()> + Send {
        self.cx.incoming_closed()
    }
}

#[cfg(test)]
mod tests;
