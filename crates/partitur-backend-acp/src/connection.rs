//! One session's connection: an adapter process spoken to over ACP v2, or over v1 for an
//! agent that speaks only that.
//!
//! [`run_session`] spawns the adapter and initializes it with ACP v2. An agent that answers
//! with protocol version 1, such as GitHub Copilot CLI, is handed to the v1 client of
//! [`crate::connection_v1`] on the same process, which translates its traffic into the v2
//! values the code here reads; the rest of this description is the v2 path.
//!
//! The v2 client creates or resumes the session (with Partitur's MCP tool server registered
//! and the tool instructions appended to the system prompt), keeps it asking before it acts
//! (it switches the `mode` option to `default`, as the adapter starts sessions in `auto`,
//! which would skip every permission prompt; [`asking_settings`] says what an agent without
//! that mode is switched to, and an agent offering nothing of the kind gets a notice saying
//! so), reports the session to the core and then executes the session's commands until the
//! runtime drops them or the adapter goes away. Everything it reports goes through its
//! [`Reporter`], which stops a connection the runtime replaced from reaching the core.
//!
//! A turn ends when the agent reports its foreground work idle, not when the prompt is
//! answered: the agent takes a prompt in at once, and it may also start work of its own, for
//! example once a background command finishes.
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::schema::{
    self, CancelSessionNotification, CancelledPermissionOutcome, Implementation, InitializeRequest,
    InitializeResponse, McpServer, McpServerHttp, NewSessionRequest, PromptRequest, ReplayFrom,
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
    BackendEvent, BackendSessionId, ConfigId, ConfigOption, ConfigValueId, PermissionId,
    PermissionOptionId, PromptContent, RequestId, SessionId, StopReason, TOOL_SERVER, ToolCallId,
    ToolKind, ToolRole, Update,
};
use tokio::sync::mpsc;

use crate::connection_v1::{EXCLUDED_FOR_ORCHESTRATOR, V1Client};
use crate::map::{
    SessionState, map_config_options, map_permission_options, map_prompt, map_tool_kind,
    map_update, widget_tools,
};
use crate::permissions::{Answer, PendingPermissions, PermissionIds};
use crate::runtime::{AcpConfig, Reporter, lock};
use crate::v1::copilot_opt_in;

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
/// The environment variable that selects how the adapter reports reasoning.
const THINKING_DISPLAY_VAR: &str = "CLAUDE_CODE_THINKING_DISPLAY";
/// The environment variable naming the folders, comma-separated, whose `*.instructions.md`
/// files GitHub Copilot CLI inlines into its system message; Claude's adapter ignores it.
const INSTRUCTIONS_DIRS_VAR: &str = "COPILOT_CUSTOM_INSTRUCTIONS_DIRS";
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

/// Forwards one notification to the core, or holds it back while the gate is closed. A
/// notification for another session is dropped.
///
/// A state update reports the number of items its background list names, when that changed,
/// before what the state itself means: the `idle` that ends a turn while background work
/// still runs never shows the session done without it.
///
/// The gate stays locked while the event is sent, so updates keep the agent's order.
///
/// Returns what the connection has to ask the adapter next: the widgets the forwarded update
/// made the session read, and, from an adapter that serves widgets, whether to list the tools
/// that declare one again: after it reported an MCP server's status, or a call of an MCP tool
/// without a known widget that the session wants a new list for.
pub(crate) fn forward(
    gate: &Mutex<Gate>,
    reporter: &Reporter,
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
        let status = reports_server_status(&update);
        if let Some(update) = map_update(update, &mut gate.state) {
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

/// Whether `update` is a notice the Claude Code adapter marks as an MCP server's status, with
/// `_meta.claudeCode.kind` set to `mcpServerStatus`.
fn reports_server_status(update: &SessionUpdate) -> bool {
    let SessionUpdate::Notice(notice) = update else {
        return false;
    };
    let kind = notice.meta.as_ref().and_then(|meta| {
        let kind = meta.get("claudeCode")?.get("kind")?;
        kind.as_str()
    });
    kind == Some("mcpServerStatus")
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

/// The option and value pairs that keep a session with `config` asking before it acts, in
/// the order they are set: [`MODE_OPTION`] to [`DEFAULT_MODE`] when it offers that, else to
/// [`AGENT_MODE`] when it offers that, then [`ALLOW_ALL_OPTION`] to [`ALLOW_ALL_OFF`] when it
/// offers that. None when the agent offers none of them.
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
        reporter.update(Update::Notice(NO_DEFAULT_MODE_NOTICE.to_owned()));
    }
}

/// Tells the user, when the session's instructions folder could not be written
/// (`uninstructed`), that agents reading their instructions from a folder miss Partitur's.
pub(crate) fn notice_if_uninstructed(reporter: &Reporter, uninstructed: bool) {
    if uninstructed {
        reporter.update(Update::Notice(UNINSTRUCTED_NOTICE.to_owned()));
    }
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
    /// The client opened no session, as the agent must be started again with
    /// [`EXCLUDED_FOR_ORCHESTRATOR`].
    Restart,
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
        Ok(Ending::Closed | Ending::Restart) => return None,
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

/// The value of [`INSTRUCTIONS_DIRS_VAR`] naming `dir`: `dir` alone, or appended with a
/// comma to the `configured` folders, if there are any.
fn instructions_dirs(configured: Option<&str>, dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    match configured.filter(|configured| !configured.is_empty()) {
        Some(configured) => format!("{configured},{dir}"),
        None => dir.into_owned(),
    }
}

/// The adapter process for one session, with its child's stderr and the wire logged at
/// `debug`. A config with an instructions folder names it in [`INSTRUCTIONS_DIRS_VAR`], after
/// the folders the config's environment, else Partitur's own, already names there.
fn transport(config: &AcpConfig) -> AcpAgent {
    let mut agent_config = AcpAgentConfig::new(&config.command)
        .args(&config.args)
        .envs(config.env.iter().cloned())
        .env(THINKING_DISPLAY_VAR, config.thinking_display.env_value());
    if let Some(dir) = &config.instructions_dir {
        let configured = config
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == INSTRUCTIONS_DIRS_VAR)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(INSTRUCTIONS_DIRS_VAR).ok());
        agent_config = agent_config.env(
            INSTRUCTIONS_DIRS_VAR,
            instructions_dirs(configured.as_deref(), dir),
        );
    }
    AcpAgent::new(agent_config)
        .with_debug(|line, direction| tracing::debug!(?direction, "acp: {line}"))
}

/// `config` with [`EXCLUDED_FOR_ORCHESTRATOR`] appended to its arguments.
fn excluding_tools(config: &AcpConfig) -> AcpConfig {
    let mut config = config.clone();
    config.args.push(EXCLUDED_FOR_ORCHESTRATOR.to_owned());
    config
}

/// The MCP servers a session is opened with: Partitur's tool server at `tools_url`.
fn mcp_servers(tools_url: String) -> Vec<McpServer> {
    vec![McpServer::Http(McpServerHttp::new(TOOL_SERVER, tools_url))]
}

/// The request metadata that appends the tool instructions to the adapter's own system
/// prompt. `systemPrompt` must be an object with `append`: a plain string would replace the
/// whole prompt. The adapter keeps nothing between processes, so every opening request sends it.
fn session_meta() -> schema::Meta {
    let mut meta = schema::Meta::new();
    meta.insert(
        "systemPrompt".into(),
        serde_json::json!({ "append": partitur_core::tool_instructions() }),
    );
    meta
}

/// The request creating a session in `cwd`, with the tool server at `tools_url`.
fn new_session_request(cwd: PathBuf, tools_url: String) -> NewSessionRequest {
    NewSessionRequest::new(cwd)
        .mcp_servers(mcp_servers(tools_url))
        .meta(session_meta())
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
        .meta(session_meta())
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
}

impl<R: Answer> Shared<R> {
    /// The state of a connection reporting through `reporter`, behind `gate`, its permission
    /// requests taking their ids from `ids`.
    fn new(reporter: Reporter, gate: Arc<Mutex<Gate>>, ids: PermissionIds) -> Self {
        Self {
            reporter,
            gate,
            permissions: Arc::new(Mutex::new(PendingPermissions::new(ids))),
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
) -> Update {
    match answer {
        Ok(options) => Update::ConfigOptions(map_config_options(&options)),
        Err(error) => Update::ConfigRejected {
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
}

impl ConnectTo<Agent> for V2Client {
    async fn connect_to(
        self,
        agent: impl ConnectTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Self { shared, opening } = self;
        let notifications = Arc::clone(&shared);
        let permission_requests = Arc::clone(&shared);
        Client
            .v2()
            .on_receive_notification(
                async move |notification: UpdateSessionNotification, cx: ConnectionTo<Agent>| {
                    let shared = &notifications;
                    let id = notification.session_id.clone();
                    let follow_up = forward(&shared.gate, &shared.reporter, notification);
                    follow(&cx, shared, &id, follow_up)
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
                lock(&shared.gate).widgets = serves_widgets(&initialized);
                let commands = opening.commands()?;
                let ending = drive(cx, &shared, &opening, commands).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v2 `initialize`. It opts into Copilot's session events too: the connector reuses the
/// connection for the v1 client only when the v1 `initialize` is this one, and that one opts
/// in. Claude's adapter ignores the `_meta`.
pub(crate) fn initialize_request() -> InitializeRequest {
    InitializeRequest::new(
        ProtocolVersion::V2,
        Implementation::new("partitur", env!("CARGO_PKG_VERSION")),
    )
    .capabilities(schema::ClientCapabilities::new().meta(copilot_opt_in()))
}

/// Runs one session from spawning its adapter until its connection ends, then reports how
/// it ended and forgets the session, all through `reporter`, the session's current
/// connection when the runtime opened this one. Its permission requests take their ids from
/// `permission_ids`. The session is opened with the tool server at `tools_url`, and when
/// `uninstructed` the user is told its instructions folder could not be written.
///
/// The connection starts with ACP v2; an agent that answers with v1 continues on the same
/// process with the v1 client. When that client ends the connection with [`Ending::Restart`],
/// the adapter is started once more with [`EXCLUDED_FOR_ORCHESTRATOR`] appended to its
/// arguments, and the session is opened on that process.
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
        let spawned = Arc::clone(&config);
        let connected = Client
            .protocol_connector()
            .with_v2(move || V2Client {
                shared: Arc::clone(&v2),
                opening: Arc::clone(&v2_opening),
            })
            .with_v1(move || V1Client {
                shared: Arc::clone(&v1),
                opening: Arc::clone(&v1_opening),
                args: Arc::clone(&args),
            })
            .connect_to(move || transport(&spawned))
            .await;
        // The discarded process's exit, however it went, is no ending of the session.
        let ending = opening.ending();
        if !matches!(ending, Ending::Restart) {
            break connected.map(|()| ending);
        }
        tracing::info!(%session, "restarting the orchestrator's agent without editing tools");
        config = Arc::new(excluding_tools(&config));
    };
    tracing::info!(%session, ?result, "ACP session ended");
    reporter.retire(|live| ending_event(request, session.clone(), live, result));
}

/// The connection's main task, once the adapter answered `initialize`: opens the session as
/// `opening` says, with its tool server registered, asks an adapter that serves widgets for
/// the tools that declare one, keeps the session asking, reports it, then runs its commands.
async fn drive(
    cx: ConnectionTo<Agent>,
    shared: &Arc<Shared>,
    opening: &Opening,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
) -> Result<Ending, agent_client_protocol::Error> {
    let (request, open) = (opening.request, opening.open.clone());
    let (cwd, tools_url) = (opening.cwd.clone(), opening.tools_url.clone());
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
    if lock(&shared.gate).widgets {
        list_widget_tools(&cx, shared, &id)?;
    }
    let settings = asking_settings(&config);
    let mut config = config;
    for (option, value) in &settings {
        let switch = SetSessionConfigOptionRequest::new(id.clone(), *option, *value);
        config = cx.send_request(switch).block_task().await?.config_options;
    }
    let event = opened(request, &shared.reporter, &id, open);
    open_gate(&shared.gate, &shared.reporter, config, event);
    notice_unless_asking(&shared.reporter, &settings);
    notice_if_uninstructed(&shared.reporter, opening.uninstructed);
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
            lock(&shared.gate).prompted();
            let turn = cx.send_request(PromptRequest::new(id.clone(), map_prompt(content)));
            let gate = Arc::clone(&shared.gate);
            cx.spawn(async move {
                if let Err(error) = turn.block_task().await {
                    settle_refusal(&gate, &reporter, &error);
                }
                Ok(())
            })
        }
        SessionCommand::Cancel => {
            let subagents: Vec<schema::SessionId> =
                lock(&shared.gate).state.subagents().cloned().collect();
            std::iter::once(id.clone())
                .chain(subagents)
                .try_for_each(|session| {
                    cx.send_notification(CancelSessionNotification::new(session))
                })
        }
        SessionCommand::SetConfig { option, value } => {
            let change = cx.send_request(SetSessionConfigOptionRequest::new(
                id.clone(),
                option.as_str().to_owned(),
                SessionConfigOptionValue::id(value.as_str().to_owned()),
            ));
            cx.spawn(async move {
                let options = change
                    .block_task()
                    .await
                    .map(|response| response.config_options);
                reporter.update(config_change(option, value, options));
                Ok(())
            })
        }
        SessionCommand::RespondPermission { permission, option } => {
            respond_permission(shared, permission, option);
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
        BackgroundSubagent, BackgroundWork, CommandPermissionSubject, ConfigOptionUpdate,
        ContentBlock, ContentChunk, IdleStateUpdate, PermissionOption, PermissionOptionKind,
        RunningStateUpdate, SessionConfigSelectOption, StateUpdate, StopReason as SchemaStopReason,
        TextContent, ToolCallUpdate,
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
}
