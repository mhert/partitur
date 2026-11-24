//! A scripted ACP v2 agent speaking over stdio, for the backend's integration tests.
//!
//! Its traffic follows the Claude Code adapter's: a prompt is answered at once with the id of
//! the echoed user message, the turn's work is bracketed by a `running` and an `idle`
//! `state_update` carrying the stop reason, and tool calls arrive as upserts.
//!
//! The first argument picks the script:
//!
//! - `happy`: `session/new` answers with the id `fake-1` and the config options `model`
//!   (`sonnet`, `opus`; current `sonnet`), `effort` (`low`, `high`; current `low`) and `mode`
//!   (`default`, `auto`; current `auto`), then reports an `idle` snapshot without a stop
//!   reason. `session/set_config_option` accepts `mode` only set to `default`, so a backend
//!   that switches to any other mode fails its create or resume: Partitur keeps every session
//!   asking for permissions. Switching announces the config with effort `high` and mode
//!   `default` in a `config_option_update` before it answers with the same config; since that
//!   differs from the create response, a test sees which config the backend reported. It also
//!   accepts `model` set to `sonnet` or `opus` and answers with the changed options (effort
//!   `high`, mode `default`); any other option or value is an invalid-params error.
//!   `session/resume` replays the user message `earlier` and a whole turn (`running`, the
//!   agent text `earlier reply`, `idle` with `end_turn`), then reports an `idle` snapshot
//!   without a stop reason and answers with the config of `session/new`. `session/prompt`
//!   answers with the message id, then reports the user message, `running`,
//!   `AgentThoughtChunk("thinking…")`, `AgentMessageChunk("Hello ")`,
//!   `AgentMessageChunk("world")`, a pending `Read` tool call `t1`, its update to `completed`
//!   with the text `ok`, `UsageUpdate(10, 1000)` and `idle` with `end_turn`.
//! - `modeless`: like `happy`, but the config has no `mode` option.
//! - `env`: like `happy`, but `session/new` answers with the id `fake-{mark}`, where the mark
//!   is the value of `PARTITUR_FAKE_MARK` in the agent's environment, or `unmarked` without
//!   it, so a test sees which environment the agent was spawned with.
//! - `asks`: like `happy`, but before the tool call it asks `session/request_permission` for
//!   the tool call `t1` with the options `allow-once` and `reject`, reporting
//!   `requires_action` until answered and `running` after. Allowing completes the call with
//!   `ok`; any other answer fails it with the chosen option's id, or `cancelled`.
//! - `load-fails`: `session/resume` answers with an error naming the session.
//! - `prompt-fails`: `session/prompt` answers with an authentication error, like the adapter
//!   without a login.
//! - `prompt-cancelled`: `session/prompt` answers with a request-cancelled error, like a
//!   prompt cancelled before the agent took it in.
//! - `refused-working`: a prompt arrives while the agent works: it reports `running`, answers
//!   the prompt with a request-cancelled error, then reports `idle` with `end_turn`.
//! - `refused-idles`: a prompt reports `running`, then is answered with an authentication
//!   error. On `session/cancel` the work goes `idle` without a stop reason, and a turn of the
//!   agent's own follows (`running`, `AgentMessageChunk("on its own")`, `idle` with
//!   `end_turn`).
//! - `dies`: exits when asked to create a session, right after `initialize`.
//! - `dies-later`: a turn reports `running` and `idle` with `end_turn`, then the agent closes
//!   the connection and exits.
//! - `slow`: a turn streams `AgentMessageChunk("working…")`, then waits up to five seconds for
//!   `session/cancel`; it ends with `cancelled` when cancelled, else with `end_turn`.
//! - `hangs`: never reads or answers anything, ignores stdin closing and exits after thirty
//!   seconds, far longer than the backend's shutdown grace.
//! - `orchestrates`: like `happy`, but remembers the URL of the first HTTP MCP server that
//!   `session/new` or `session/resume` names. A turn with a remembered URL calls the server's
//!   `propose_children` tool, one JSON-RPC `tools/call` sent as a `POST` over plain HTTP/1.1
//!   with `Connection: close`, proposing `api` ("Add the endpoint", "API endpoint") and `web`
//!   ("Add the page", "Web page"). It streams the text of the result's first content block
//!   as one `AgentMessageChunk` and ends with `end_turn`; a call that fails streams its error
//!   instead. Without a URL the turn streams `no tools` and ends with `end_turn`.
//! - `reports-tools`: like `orchestrates`, but a turn with a remembered URL calls the
//!   server's `superpowers_v7_start_brainstorming` tool and streams the URL, a space and the
//!   text of the result as one `AgentMessageChunk`, so a test sees which URL the session was
//!   opened with. Without a URL the call fails and its error is streamed instead.
//! - `continues`: like `happy`, but after the turn its background work ends: an `idle`
//!   without a stop reason, then a turn of its own without a prompt (`running`,
//!   `AgentMessageChunk("background done")`, `idle` with `end_turn`).
//! - `subagent`: like `happy`, but right after `running` a subagent's session `fake-sub` on
//!   the same connection reports `running`, `AgentMessageChunk("subagent")` and `idle` with
//!   `end_turn`.
//! - `subagent-runs`: a turn's `running` lists the subagent `fake-sub` and the command
//!   terminal `fake-term` as background work; then `fake-sub` reports `running` listing a
//!   subagent `fake-sub-2` of its own, the turn streams `AgentMessageChunk("main done")` and
//!   ends with `idle` and `end_turn`, still listing `fake-sub` and `fake-term`, and an `idle`
//!   without a stop reason lists nothing.
//! - `subagent-waits`: a turn's `running` lists only the subagent `fake-sub`, the turn
//!   streams `AgentMessageChunk("main done")` and ends with `idle` and `end_turn` that still
//!   lists `fake-sub`, without `fake-sub`'s own report. The subagent runs until `session/cancel`
//!   names `fake-sub`; then the session reports an `idle` without a stop reason listing
//!   nothing. A cancel of the session itself spares it, as Claude Code spares background
//!   subagents, and streams `AgentMessageChunk("subagent spared")`.
//! - `widgets`: like Partitur's patched adapter, `initialize` answers with
//!   `_meta.partitur.widgets` set to `1`. `_partitur/widget_tools` lists the one tool
//!   `getJiraIssue` of the server `atlassian`, with the widget `ui://jira/issue` and the
//!   visibility `model` and `app`, and `_partitur/read_widget` answers with the HTML
//!   `<p>issue</p>` of the type `text/html;profile=mcp-app` and an empty `meta`. A turn reports
//!   the pending tool call `w1` of `mcp__atlassian__getJiraIssue` (in `_meta.claudeCode.toolName`;
//!   the next turn's call is `w2`, and so on)
//!   with the raw input `{"issueKey":"PROJ-1"}`, then its completion with the text
//!   `PROJ-1: Fix the login` and, in `_meta.partitur.callToolResult`, that text as the content,
//!   the structured content `{"key":"PROJ-1","summary":"Fix the login"}` and `isError` false,
//!   and ends with `idle` and `end_turn`.
//! - `widgets-failing`: like `widgets`, but `_partitur/read_widget` answers with an internal
//!   error whose message is `Server not found: atlassian`, as the adapter passes on the SDK's.
//! - `widgets-later`: like `widgets`, but `_partitur/widget_tools` lists no tool until the agent
//!   reported the server connected: its first turn reports only the notice `MCP server
//!   atlassian connected` with `_meta.claudeCode.kind` `mcpServerStatus`, as the adapter does
//!   for a server's status, and ends with `idle` and `end_turn`; every later turn is a
//!   `widgets` turn.
//! - `widgets-late`: like `widgets`, but the first `_partitur/widget_tools` lists no tool, as
//!   before a server that connects late is up, and every later one lists the widget tool. A
//!   turn's widget tool call `w1` (`w2`, and so on) is followed by the pending call `u1` (`u2`,
//!   and so on) of `mcp__other__lookup`, a tool no list names, given in the call's `name`.
//!
//! Every script but `hangs` exits once its client closes stdin. Every script but the `widgets`
//! ones answers a `_partitur/` request, like any other method it does not know, with a
//! method-not-found error, as the unpatched adapter does. When `PARTITUR_FAKE_LOG` names a file,
//! every script appends the method of each request no typed handler takes to it, one per line,
//! so a test sees which extension requests the agent received.

use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agent_client_protocol::v2::schema::{
    AgentCapabilities, BackgroundSubagent, BackgroundTerminal, BackgroundWork,
    CancelSessionNotification, ConfigOptionUpdate, ContentBlock, ContentChunk, IdleStateUpdate,
    Implementation, InitializeRequest, InitializeResponse, McpServer, NewSessionRequest,
    NewSessionResponse, Notice, NoticeSeverity, PermissionOption, PermissionOptionKind,
    PromptRequest, PromptResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionSubject, RequiresActionStateUpdate, ResumeSessionRequest,
    ResumeSessionResponse, RunningStateUpdate, SessionCapabilities, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionConfigValueId, SessionId,
    SessionUpdate, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StateUpdate,
    StopReason, ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind,
    UpdateSessionNotification, UsageUpdate, UserMessage,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, UntypedMessage};
use serde_json::{Value, json};
use tokio::sync::Notify;

#[path = "fake/common.rs"]
mod common;

use common::{McpClient, invalid_params, log_method, stdio, tool_title};

/// The id every created session gets, except under `env`.
const SESSION_ID: &str = "fake-1";
/// The session of the subagent the `subagent` script runs on the same connection.
const SUBAGENT_SESSION: &str = "fake-sub";
/// The terminal of the command the `subagent-runs` script leaves running beside its subagent.
const BACKGROUND_TERMINAL: &str = "fake-term";
/// The variable whose value the `env` script puts into its session ids.
const MARK_VAR: &str = "PARTITUR_FAKE_MARK";
/// The mark of an `env` session when [`MARK_VAR`] is not set.
const UNMARKED: &str = "unmarked";
/// The tool call every turn makes.
const TOOL_CALL: &str = "t1";
/// The option that allows the tool call.
const ALLOW: &str = "allow-once";
/// The effort a session is created or resumed with.
const OPENING_EFFORT: &str = "low";
/// The effort the mode switch announces.
const CURRENT_EFFORT: &str = "high";
/// The mode a session is created or resumed in, which skips permission prompts.
const OPENING_MODE: &str = "auto";
/// The only mode the agent switches to.
const DEFAULT_MODE: &str = "default";
/// The id of the user message every prompt is echoed as.
const USER_MESSAGE: &str = "user-1";
/// The id of the agent message every turn streams.
const AGENT_MESSAGE: &str = "agent-1";
/// How long a `slow` turn waits for a cancel before it ends by itself.
const SLOW_TURN: Duration = Duration::from_secs(5);
/// How long the `hangs` script lives: far past the backend's five-second shutdown grace, which
/// kills it first, so the hang never ends by itself while a test waits.
const HANG: Duration = Duration::from_secs(30);
/// What an `orchestrates` turn says when its session was opened without a tool server.
const NO_TOOLS: &str = "no tools";
/// The request that lists the tools with a widget, served by Partitur's adapter patch.
const WIDGET_TOOLS: &str = "_partitur/widget_tools";
/// The request that reads a widget's HTML, served by Partitur's adapter patch.
const READ_WIDGET: &str = "_partitur/read_widget";
/// The variable naming the file the agent logs the methods of untyped requests to.
/// The error message of a failed `_partitur/read_widget` under `widgets-failing`.
const WIDGET_READ_FAILURE: &str = "Server not found: atlassian";

/// What the agent does, chosen by its first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Happy,
    Modeless,
    Env,
    Asks,
    LoadFails,
    PromptFails,
    PromptCancelled,
    RefusedWorking,
    RefusedIdles,
    Dies,
    DiesLater,
    Slow,
    Hangs,
    Orchestrates,
    ReportsTools,
    Continues,
    Subagent,
    SubagentRuns,
    SubagentWaits,
    Widgets,
    WidgetsFailing,
    WidgetsLater,
    WidgetsLate,
}

impl Script {
    /// Whether the script serves Partitur's widget requests, like the patched adapter.
    fn serves_widgets(self) -> bool {
        matches!(
            self,
            Self::Widgets | Self::WidgetsFailing | Self::WidgetsLater | Self::WidgetsLate
        )
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "happy" => Some(Self::Happy),
            "modeless" => Some(Self::Modeless),
            "env" => Some(Self::Env),
            "asks" => Some(Self::Asks),
            "load-fails" => Some(Self::LoadFails),
            "prompt-fails" => Some(Self::PromptFails),
            "prompt-cancelled" => Some(Self::PromptCancelled),
            "refused-working" => Some(Self::RefusedWorking),
            "refused-idles" => Some(Self::RefusedIdles),
            "dies" => Some(Self::Dies),
            "dies-later" => Some(Self::DiesLater),
            "slow" => Some(Self::Slow),
            "hangs" => Some(Self::Hangs),
            "orchestrates" => Some(Self::Orchestrates),
            "reports-tools" => Some(Self::ReportsTools),
            "continues" => Some(Self::Continues),
            "subagent" => Some(Self::Subagent),
            "subagent-runs" => Some(Self::SubagentRuns),
            "subagent-waits" => Some(Self::SubagentWaits),
            "widgets" => Some(Self::Widgets),
            "widgets-failing" => Some(Self::WidgetsFailing),
            "widgets-later" => Some(Self::WidgetsLater),
            "widgets-late" => Some(Self::WidgetsLate),
            _ => None,
        }
    }
}

/// The state the handlers share.
struct Fake {
    script: Script,
    /// Signalled by `session/cancel`.
    cancelled: Notify,
    /// Signalled when the agent should close the connection and exit.
    exit: Notify,
    /// The URL of the first HTTP MCP server the session was opened with.
    tools_url: Mutex<Option<String>>,
    /// How many widget tool calls the agent made.
    widget_calls: AtomicUsize,
    /// Whether the agent reported its MCP server connected.
    connected: AtomicBool,
    /// How many tool lists the agent answered.
    tool_lists: AtomicUsize,
}

impl Fake {
    fn new(script: Script) -> Self {
        Self {
            script,
            cancelled: Notify::new(),
            exit: Notify::new(),
            tools_url: Mutex::new(None),
            widget_calls: AtomicUsize::new(0),
            connected: AtomicBool::new(false),
            tool_lists: AtomicUsize::new(0),
        }
    }

    /// Whether the next `_partitur/widget_tools` lists the widget tool: under `widgets-later`
    /// only once the server was reported connected, under `widgets-late` from the second list
    /// on.
    fn lists_widget_tools(&self) -> bool {
        let earlier = self.tool_lists.fetch_add(1, Ordering::SeqCst);
        match self.script {
            Script::WidgetsLater => self.connected.load(Ordering::SeqCst),
            Script::WidgetsLate => earlier > 0,
            _ => true,
        }
    }

    /// The id of the next widget tool call: `w1`, `w2` and so on.
    fn next_widget_call(&self) -> String {
        format!("w{}", self.widget_calls.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Remembers the first HTTP server in `servers`, if any.
    fn remember_tools(&self, servers: &[McpServer]) {
        let url = servers.iter().find_map(|server| match server {
            McpServer::Http(http) => Some(http.url.clone()),
            _ => None, // coverage: Partitur's connection registers only an HTTP server
        });
        *self
            .tools_url
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = url;
    }

    /// The remembered tool server URL.
    fn tools_url(&self) -> Option<String> {
        self.tools_url
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

type Result<T = ()> = std::result::Result<T, agent_client_protocol::Error>;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let name = std::env::args().nth(1).unwrap_or_default();
    let script = Script::parse(&name).ok_or_else(|| format!("unknown script {name:?}"))?;
    if script == Script::Hangs {
        tokio::time::sleep(HANG).await; // coverage: the shutdown grace SIGKILLs this process first
        return Ok(()); // coverage: the shutdown grace SIGKILLs this process first
    }
    let fake = Arc::new(Fake::new(script));
    let (on_prompt, on_cancel) = (Arc::clone(&fake), Arc::clone(&fake));
    let on_extension = Arc::clone(&fake);
    let (on_new, on_resume) = (Arc::clone(&fake), Arc::clone(&fake));
    Agent
        .v2()
        .name("fake-agent")
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx| {
                let response = InitializeResponse::new(
                    request.protocol_version,
                    Implementation::new("fake-agent", env!("CARGO_PKG_VERSION")),
                )
                .capabilities(AgentCapabilities::new().session(SessionCapabilities::new()));
                responder.respond(if script.serves_widgets() {
                    response.meta(meta("partitur", json!({"widgets": 1})))
                } else {
                    response
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        cx: ConnectionTo<Client>| {
                if script == Script::Dies {
                    std::process::exit(0);
                }
                on_new.remember_tools(&request.mcp_servers);
                let id = SessionId::new(session_id(script));
                let config = config_options(script, "sonnet", OPENING_EFFORT, OPENING_MODE);
                // Like the adapter, the agent reports its state once the session exists.
                responder
                    .respond(NewSessionResponse::new(id.clone()).config_options(config))
                    .and_then(|()| notify(&cx, &id, idle(None)))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ResumeSessionRequest,
                        responder: Responder<ResumeSessionResponse>,
                        cx: ConnectionTo<Client>| {
                on_resume.remember_tools(&request.mcp_servers);
                resume(&cx, script, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: SetSessionConfigOptionRequest,
                        responder: Responder<SetSessionConfigOptionResponse>,
                        cx: ConnectionTo<Client>| {
                set_config(&cx, script, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                prompt(&cx, Arc::clone(&on_prompt), request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |cancel: CancelSessionNotification, cx: ConnectionTo<Client>| {
                cancel_work(&cx, &on_cancel, &cancel.session_id)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        // Last, so it only sees the requests no typed handler took.
        .on_receive_request(
            async move |request: UntypedMessage, responder: Responder<Value>, _cx| {
                log_method(&request.method);
                responder.respond_with_result(extension(&on_extension, &request.method))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(stdio()?, async |cx: ConnectionTo<Client>| {
            tokio::select! {
                () = fake.exit.notified() => {}
                () = cx.incoming_closed() => {}
            }
            Ok(())
        })
        .await?;
    Ok(())
}

/// The id of a created session: [`SESSION_ID`], or under `env` the one naming its mark.
fn session_id(script: Script) -> String {
    if script == Script::Env {
        let mark = std::env::var(MARK_VAR).unwrap_or_else(|_| UNMARKED.to_owned());
        format!("fake-{mark}")
    } else {
        SESSION_ID.to_owned()
    }
}

/// Answers `session/resume`: an error for `load-fails`, else the history replay, the state
/// snapshot and the session's config.
fn resume(
    cx: &ConnectionTo<Client>,
    script: Script,
    request: &ResumeSessionRequest,
    responder: Responder<ResumeSessionResponse>,
) -> Result {
    if script == Script::LoadFails {
        let error = invalid_params(format!("no session {}", request.session_id)); // coverage: a failed resume makes the transport SIGKILL this process
        return responder.respond_with_error(error); // coverage: a failed resume makes the transport SIGKILL this process
    }
    let history = [
        SessionUpdate::UserMessage(UserMessage::new("user-0").content(Vec::new())),
        SessionUpdate::UserMessageChunk(chunk("user-0", "earlier")),
        running(),
        SessionUpdate::AgentMessageChunk(chunk("agent-0", "earlier reply")),
        idle(Some(StopReason::EndTurn)),
        idle(None),
    ];
    notify_all(cx, &request.session_id, history)?;
    responder.respond(ResumeSessionResponse::new().config_options(config_options(
        script,
        "sonnet",
        OPENING_EFFORT,
        OPENING_MODE,
    )))
}

/// Whether `value` is the only mode the agent switches to, [`DEFAULT_MODE`].
///
/// # Errors
///
/// Returns an invalid-params error naming any other value.
fn switch_mode(value: Option<&SessionConfigValueId>) -> Result {
    match value {
        Some(mode) if &*mode.0 == DEFAULT_MODE => Ok(()),
        _ => Err(invalid_params(format!(
            "only `{DEFAULT_MODE}` is accepted, not {value:?}"
        ))),
    }
}

/// Answers `session/set_config_option`: `mode` can only become [`DEFAULT_MODE`], which is
/// announced before the answer, and `model` only a model the agent offers.
fn set_config(
    cx: &ConnectionTo<Client>,
    script: Script,
    request: &SetSessionConfigOptionRequest,
    responder: Responder<SetSessionConfigOptionResponse>,
) -> Result {
    let value = request.value.as_id();
    let current = |model| config_options(script, model, CURRENT_EFFORT, DEFAULT_MODE);
    match &*request.config_id.0 {
        "mode" => {
            let switched = switch_mode(value).and_then(|()| {
                let update =
                    SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(current("sonnet")));
                notify(cx, &request.session_id, update)
            });
            responder.respond_with_result(
                switched.map(|()| SetSessionConfigOptionResponse::new(current("sonnet"))),
            )
        }
        "model" if value.is_some_and(|model| matches!(&*model.0, "sonnet" | "opus")) => {
            let model = value.map(ToString::to_string).unwrap_or_default();
            responder.respond(SetSessionConfigOptionResponse::new(current(&model)))
        }
        _ => {
            let error = format!(
                "only `model` can change, to `sonnet` or `opus`; refusing {}",
                request.config_id
            );
            responder.respond_with_error(invalid_params(error))
        }
    }
}

/// Runs one turn as the script says.
fn prompt(
    cx: &ConnectionTo<Client>,
    fake: Arc<Fake>,
    request: PromptRequest,
    responder: Responder<PromptResponse>,
) -> Result {
    let session = request.session_id;
    match fake.script {
        Script::PromptFails => {
            return responder.respond_with_error(
                agent_client_protocol::Error::auth_required().data("Authentication required"),
            );
        }
        Script::PromptCancelled => {
            return responder.respond_with_error(agent_client_protocol::Error::request_cancelled());
        }
        Script::RefusedWorking => {
            notify(cx, &session, running())?;
            responder.respond_with_error(agent_client_protocol::Error::request_cancelled())?;
            return notify(cx, &session, idle(Some(StopReason::EndTurn)));
        }
        Script::RefusedIdles => {
            notify(cx, &session, running())?;
            return responder.respond_with_error(
                agent_client_protocol::Error::auth_required().data("Authentication required"),
            );
        }
        _ => {}
    }
    // The prompt is taken in at once; the turn's end is reported as an `idle` state.
    responder.respond(PromptResponse::new(USER_MESSAGE))?;
    let echo = UserMessage::new(USER_MESSAGE).content(request.prompt);
    notify_all(cx, &session, [SessionUpdate::UserMessage(echo), running()])?;
    match fake.script {
        // Waiting for an answer or a cancel would block the dispatch of that very message.
        Script::Asks => cx.spawn(ask_then_answer(cx.clone(), session)),
        Script::Slow => cx.spawn(work_until_cancelled(cx.clone(), fake, session)),
        Script::Orchestrates => cx.spawn(orchestrate(cx.clone(), fake, session)),
        Script::ReportsTools => cx.spawn(report_tools(cx.clone(), fake, session)),
        Script::Widgets | Script::WidgetsFailing | Script::WidgetsLater | Script::WidgetsLate => {
            widget_turn(cx, &fake, &session)
        }
        Script::DiesLater => {
            notify(cx, &session, idle(Some(StopReason::EndTurn)))?;
            fake.exit.notify_one();
            Ok(())
        }
        Script::Subagent => {
            let subagent = SessionId::new(SUBAGENT_SESSION);
            let work = [
                running(),
                SessionUpdate::AgentMessageChunk(chunk("sub-1", "subagent")),
                idle(Some(StopReason::EndTurn)),
            ];
            notify_all(cx, &subagent, work)?;
            whole_turn(cx, &session)
        }
        Script::SubagentRuns => {
            let main = subagent_and_command(SUBAGENT_SESSION, BACKGROUND_TERMINAL);
            notify(cx, &session, listing(running_state(), main.clone()))?;
            let own = listing(running_state(), subagents(&["fake-sub-2"]));
            notify(cx, &SessionId::new(SUBAGENT_SESSION), own)?;
            let ended = IdleStateUpdate::new().stop_reason(StopReason::EndTurn);
            let rest = [
                SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "main done")),
                listing(StateUpdate::Idle(ended), main),
                listing(StateUpdate::Idle(IdleStateUpdate::new()), Vec::new()),
            ];
            notify_all(cx, &session, rest)
        }
        Script::SubagentWaits => {
            let ended = IdleStateUpdate::new().stop_reason(StopReason::EndTurn);
            let turn = [
                listing(running_state(), subagents(&[SUBAGENT_SESSION])),
                SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "main done")),
                listing(StateUpdate::Idle(ended), subagents(&[SUBAGENT_SESSION])),
            ];
            notify_all(cx, &session, turn)
        }
        Script::Continues => {
            whole_turn(cx, &session)?;
            let own_turn = [
                idle(None),
                running(),
                SessionUpdate::AgentMessageChunk(chunk("agent-2", "background done")),
                idle(Some(StopReason::EndTurn)),
            ];
            notify_all(cx, &session, own_turn)
        }
        Script::Happy
        | Script::Modeless
        | Script::Env
        | Script::LoadFails
        | Script::PromptFails
        | Script::PromptCancelled
        | Script::RefusedWorking
        | Script::RefusedIdles
        | Script::Dies
        | Script::Hangs => whole_turn(cx, &session),
    }
}

/// The answer to a request of `method` that no typed handler took: Partitur's widget requests
/// under the `widgets` scripts, and a method-not-found error for anything else.
///
/// # Errors
///
/// Returns a method-not-found error for a method the script does not serve, and an internal
/// error for `_partitur/read_widget` under `widgets-failing`.
fn extension(fake: &Fake, method: &str) -> Result<Value> {
    let script = fake.script;
    let widgets = script.serves_widgets();
    match method {
        WIDGET_TOOLS if widgets && !fake.lists_widget_tools() => Ok(json!({"tools": []})),
        WIDGET_TOOLS if widgets => Ok(json!({"tools": [{
            "server": "atlassian",
            "tool": "getJiraIssue",
            "resourceUri": "ui://jira/issue",
            "visibility": ["model", "app"],
        }]})),
        READ_WIDGET if script == Script::WidgetsFailing => Err(agent_client_protocol::Error::new(
            agent_client_protocol::ErrorCode::INTERNAL_ERROR,
            WIDGET_READ_FAILURE,
        )),
        READ_WIDGET if widgets => Ok(json!({
            "mimeType": "text/html;profile=mcp-app",
            "html": "<p>issue</p>",
            "meta": {},
        })),
        _ => Err(agent_client_protocol::Error::method_not_found().data(method.to_owned())),
    }
}

/// The rest of a turn of the `widgets` scripts after `running`: the server's connection notice
/// on the first `widgets-later` turn, else the widget tool call (and under `widgets-late` the
/// call of a tool without a widget), then the `idle` that ends it.
fn widget_turn(cx: &ConnectionTo<Client>, fake: &Fake, session: &SessionId) -> Result {
    if fake.script == Script::WidgetsLater && !fake.connected.swap(true, Ordering::SeqCst) {
        notify(cx, session, server_connected())?;
    } else {
        let call = fake.next_widget_call();
        notify_all(cx, session, widget_call(&call))?;
        if fake.script == Script::WidgetsLate {
            notify(cx, session, unknown_call(&call.replacen('w', "u", 1)))?;
        }
    }
    notify(cx, session, idle(Some(StopReason::EndTurn)))
}

/// The pending call `id` of `mcp__other__lookup`, a tool without a widget, named in `name`.
fn unknown_call(id: &str) -> SessionUpdate {
    let call = ToolCallUpdate::new(id.to_owned())
        .name("mcp__other__lookup".to_owned())
        .title("lookup".to_owned())
        .status(ToolCallStatus::Pending);
    SessionUpdate::ToolCallUpdate(call)
}

/// The notice that the MCP server `atlassian` connected, marked as a server status the way the
/// adapter marks it.
fn server_connected() -> SessionUpdate {
    let notice = Notice::new(NoticeSeverity::Info, "MCP server atlassian connected").meta(meta(
        "claudeCode",
        json!({"kind": "mcpServerStatus", "server": "atlassian", "status": "connected"}),
    ));
    SessionUpdate::Notice(notice)
}

/// The `widgets` turn's tool call `id` of the Jira issue widget's tool, pending and then
/// completed with its MCP result.
fn widget_call(id: &str) -> [SessionUpdate; 2] {
    let text = "PROJ-1: Fix the login";
    let call = ToolCallUpdate::new(id.to_owned())
        .title("getJiraIssue".to_owned())
        .kind(ToolKind::Other)
        .status(ToolCallStatus::Pending)
        .raw_input(json!({"issueKey": "PROJ-1"}))
        .meta(meta(
            "claudeCode",
            json!({"toolName": "mcp__atlassian__getJiraIssue"}),
        ));
    let result = ToolCallUpdate::new(id.to_owned())
        .status(ToolCallStatus::Completed)
        .content(vec![ToolCallContent::from(text)])
        .meta(meta(
            "partitur",
            json!({"callToolResult": {
                "content": [{"type": "text", "text": text}],
                "structuredContent": {"key": "PROJ-1", "summary": "Fix the login"},
                "isError": false,
            }}),
        ));
    [
        SessionUpdate::ToolCallUpdate(call),
        SessionUpdate::ToolCallUpdate(result),
    ]
}

/// A `_meta` with the one member `key` set to `value`.
fn meta(key: &str, value: Value) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([(key.to_owned(), value)])
}

/// The rest of a turn after `running`: its updates and the `idle` that ends it.
fn whole_turn(cx: &ConnectionTo<Client>, session: &SessionId) -> Result {
    notify_all(cx, session, opening())?;
    notify_all(cx, session, closing(ToolCallStatus::Completed, "ok"))?;
    notify(cx, session, idle(Some(StopReason::EndTurn)))
}

/// The `asks` turn: asks for permission before the tool call and runs it only if allowed.
async fn ask_then_answer(cx: ConnectionTo<Client>, session: SessionId) -> Result {
    notify_all(&cx, &session, opening())?;
    let call = ToolCallUpdate::new(TOOL_CALL)
        .title(tool_title())
        .kind(ToolKind::Read);
    let options = vec![
        PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
    ];
    let request = RequestPermissionRequest::new(session.clone(), tool_title(), options)
        .subject(RequestPermissionSubject::from(call));
    let asked = cx.send_request(request);
    let blocked = StateUpdate::RequiresAction(RequiresActionStateUpdate::new());
    notify(&cx, &session, SessionUpdate::StateUpdate(blocked))?;
    let answer = asked.block_task().await?;
    notify(&cx, &session, running())?;
    let (status, output) = match answer.outcome {
        RequestPermissionOutcome::Selected(selected) if &*selected.option_id.0 == ALLOW => {
            (ToolCallStatus::Completed, "ok".to_owned())
        }
        RequestPermissionOutcome::Selected(selected) => {
            (ToolCallStatus::Failed, selected.option_id.to_string())
        }
        _ => (ToolCallStatus::Failed, "cancelled".to_owned()),
    };
    notify_all(&cx, &session, closing(status, &output))?;
    notify(&cx, &session, idle(Some(StopReason::EndTurn)))
}

/// Answers `session/cancel` for `cancelled`: `refused-idles` goes idle and starts a turn of
/// its own, `subagent-waits` stops its subagent when the cancel names it and says it spared
/// it otherwise, and every other script signals [`Fake::cancelled`].
fn cancel_work(cx: &ConnectionTo<Client>, fake: &Fake, cancelled: &SessionId) -> Result {
    if fake.script == Script::RefusedIdles {
        let own_turn = [
            idle(None),
            running(),
            SessionUpdate::AgentMessageChunk(chunk("agent-2", "on its own")),
            idle(Some(StopReason::EndTurn)),
        ];
        return notify_all(cx, cancelled, own_turn);
    }
    if fake.script == Script::SubagentWaits {
        if cancelled.0.as_ref() == SUBAGENT_SESSION {
            let stopped = listing(StateUpdate::Idle(IdleStateUpdate::new()), Vec::new());
            return notify(cx, &SessionId::new(SESSION_ID), stopped);
        }
        let spared = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "subagent spared"));
        return notify(cx, cancelled, spared);
    }
    fake.cancelled.notify_one();
    Ok(())
}

/// The `slow` turn: works until cancelled, or ends by itself after [`SLOW_TURN`].
async fn work_until_cancelled(
    cx: ConnectionTo<Client>,
    fake: Arc<Fake>,
    session: SessionId,
) -> Result {
    let working = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "working…"));
    notify(&cx, &session, working)?;
    let cancelled = tokio::time::timeout(SLOW_TURN, fake.cancelled.notified()).await;
    let reason = cancelled.map_or(StopReason::EndTurn, |()| StopReason::Cancelled);
    notify(&cx, &session, idle(Some(reason)))
}

/// The `orchestrates` turn: proposes children through the tool server, if the session has
/// one, and streams what it answered.
async fn orchestrate(cx: ConnectionTo<Client>, fake: Arc<Fake>, session: SessionId) -> Result {
    let text = match fake.tools_url() {
        Some(url) => McpClient::new(&url)
            .propose_children()
            .await
            .unwrap_or_else(|error| error),
        None => NO_TOOLS.to_owned(),
    };
    let answer = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, &text));
    notify_all(&cx, &session, [answer, idle(Some(StopReason::EndTurn))])
}

/// The `reports-tools` turn: reports the start of brainstorming through the tool server and
/// streams the server's URL with what it answered.
async fn report_tools(cx: ConnectionTo<Client>, fake: Arc<Fake>, session: SessionId) -> Result {
    let url = fake.tools_url().unwrap_or_default();
    let answer = McpClient::new(&url)
        .call_tool("superpowers_v7_start_brainstorming", json!({}))
        .await;
    let text = format!("{url} {}", answer.unwrap_or_else(|error| error));
    let answer = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, &text));
    notify_all(&cx, &session, [answer, idle(Some(StopReason::EndTurn))])
}

/// The foreground work started.
fn running() -> SessionUpdate {
    SessionUpdate::StateUpdate(running_state())
}

/// The state of foreground work that started.
fn running_state() -> StateUpdate {
    StateUpdate::Running(RunningStateUpdate::new())
}

/// The subagent sessions `ids` as background work.
fn subagents(ids: &[&str]) -> Vec<BackgroundWork> {
    ids.iter()
        .map(|id| BackgroundWork::Subagent(BackgroundSubagent::new(*id)))
        .collect()
}

/// The subagent session `id` and a command's terminal `terminal` as background work.
fn subagent_and_command(id: &str, terminal: &str) -> Vec<BackgroundWork> {
    let mut work = subagents(&[id]);
    work.push(BackgroundWork::Terminal(BackgroundTerminal::new(terminal)));
    work
}

/// `state` listing `background` as its background work.
fn listing(state: StateUpdate, background: Vec<BackgroundWork>) -> SessionUpdate {
    let state = match state {
        StateUpdate::Running(running) => StateUpdate::Running(running.background(background)),
        StateUpdate::Idle(idle) => StateUpdate::Idle(idle.background(background)),
        other => other,
    };
    SessionUpdate::StateUpdate(state)
}

/// No foreground work runs; with a `reason`, the work it ended stopped for it.
fn idle(reason: Option<StopReason>) -> SessionUpdate {
    SessionUpdate::StateUpdate(StateUpdate::Idle(
        IdleStateUpdate::new().stop_reason(reason),
    ))
}

/// The start of every turn's work: a thought and two chunks of text.
fn opening() -> [SessionUpdate; 3] {
    [
        SessionUpdate::AgentThoughtChunk(chunk("thought-1", "thinking…")),
        SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "Hello ")),
        SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, "world")),
    ]
}

/// The end of every turn's work: the tool call, its result with `status` and `output`, and
/// the context usage.
fn closing(status: ToolCallStatus, output: &str) -> [SessionUpdate; 3] {
    let call = ToolCallUpdate::new(TOOL_CALL)
        .title(tool_title())
        .kind(ToolKind::Read)
        .status(ToolCallStatus::Pending);
    let result = ToolCallUpdate::new(TOOL_CALL)
        .status(status)
        .content(vec![ToolCallContent::from(output)]);
    [
        SessionUpdate::ToolCallUpdate(call),
        SessionUpdate::ToolCallUpdate(result),
        SessionUpdate::UsageUpdate(UsageUpdate::new(10, 1000)),
    ]
}

/// Sends `updates` for `session` in order.
fn notify_all(
    cx: &ConnectionTo<Client>,
    session: &SessionId,
    updates: impl IntoIterator<Item = SessionUpdate>,
) -> Result {
    updates
        .into_iter()
        .try_for_each(|update| notify(cx, session, update))
}

fn notify(cx: &ConnectionTo<Client>, session: &SessionId, update: SessionUpdate) -> Result {
    cx.send_notification(UpdateSessionNotification::new(session.clone(), update))
}

fn chunk(message: &str, text: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::from(text), message)
}

/// The agent's config options with `model`, `effort` and, unless `modeless`, `mode` as the
/// current values.
fn config_options(
    script: Script,
    model: &str,
    effort: &str,
    mode: &str,
) -> Vec<SessionConfigOption> {
    let mut options = vec![
        SessionConfigOption::select(
            "model",
            "Model",
            model.to_owned(),
            vec![
                SessionConfigSelectOption::new("sonnet", "Sonnet"),
                SessionConfigSelectOption::new("opus", "Opus"),
            ],
        )
        .category(SessionConfigOptionCategory::Model),
        SessionConfigOption::select(
            "effort",
            "Effort",
            effort.to_owned(),
            vec![
                SessionConfigSelectOption::new("low", "Low"),
                SessionConfigSelectOption::new("high", "High"),
            ],
        )
        .category(SessionConfigOptionCategory::ThoughtLevel),
    ];
    if script != Script::Modeless {
        options.push(
            SessionConfigOption::select(
                "mode",
                "Mode",
                mode.to_owned(),
                vec![
                    SessionConfigSelectOption::new(DEFAULT_MODE, "Default"),
                    SessionConfigSelectOption::new(OPENING_MODE, "Auto"),
                ],
            )
            .category(SessionConfigOptionCategory::Mode),
        );
    }
    options
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{Fake, Script, SessionConfigValueId, extension, json, switch_mode};

    #[test]
    fn only_the_default_mode_is_accepted() {
        assert!(switch_mode(Some(&SessionConfigValueId::new("default"))).is_ok());
        let refused = switch_mode(Some(&SessionConfigValueId::new("auto")));
        assert!(refused.is_err_and(|error| error.to_string().contains("auto")));
        assert!(switch_mode(None).is_err());
    }

    #[test]
    fn every_script_is_chosen_by_its_name_and_nothing_else_is() {
        let scripts = [
            ("happy", Script::Happy),
            ("modeless", Script::Modeless),
            ("env", Script::Env),
            ("asks", Script::Asks),
            ("load-fails", Script::LoadFails),
            ("prompt-fails", Script::PromptFails),
            ("prompt-cancelled", Script::PromptCancelled),
            ("refused-working", Script::RefusedWorking),
            ("refused-idles", Script::RefusedIdles),
            ("dies", Script::Dies),
            ("dies-later", Script::DiesLater),
            ("slow", Script::Slow),
            ("hangs", Script::Hangs),
            ("orchestrates", Script::Orchestrates),
            ("reports-tools", Script::ReportsTools),
            ("continues", Script::Continues),
            ("subagent", Script::Subagent),
            ("subagent-runs", Script::SubagentRuns),
            ("subagent-waits", Script::SubagentWaits),
            ("widgets", Script::Widgets),
            ("widgets-failing", Script::WidgetsFailing),
            ("widgets-later", Script::WidgetsLater),
            ("widgets-late", Script::WidgetsLate),
        ];
        for (name, script) in scripts {
            assert_eq!(Script::parse(name), Some(script), "{name}");
        }
        assert_eq!(Script::parse("unknown"), None);
    }

    #[test]
    fn only_the_widget_scripts_serve_the_widget_requests() {
        let widgets = Fake::new(Script::Widgets);
        let tools = extension(&widgets, "_partitur/widget_tools");
        let uri = tools
            .ok()
            .and_then(|tools| tools.pointer("/tools/0/resourceUri").cloned());
        assert_eq!(uri, Some(json!("ui://jira/issue")));
        let read = extension(&widgets, "_partitur/read_widget");
        let html = read.ok().and_then(|read| read.get("html").cloned());
        assert_eq!(html, Some(json!("<p>issue</p>")));
        let failing = Fake::new(Script::WidgetsFailing);
        assert!(extension(&failing, "_partitur/widget_tools").is_ok());
        let failed = extension(&failing, "_partitur/read_widget");
        assert!(failed.is_err_and(|error| error.message == "Server not found: atlassian"));
        let unknown = extension(&Fake::new(Script::Happy), "_partitur/widget_tools");
        assert!(
            unknown.is_err_and(
                |error| error.code == agent_client_protocol::ErrorCode::METHOD_NOT_FOUND
            )
        );
        assert!(extension(&widgets, "_other").is_err());
    }

    #[test]
    fn a_later_server_lists_its_widget_tool_only_once_connected() {
        let later = Fake::new(Script::WidgetsLater);
        let listed = |fake: &Fake| {
            extension(fake, "_partitur/widget_tools")
                .ok()
                .and_then(|tools| tools.pointer("/tools/0/tool").cloned())
        };
        assert_eq!(listed(&later), None);
        later.connected.store(true, Ordering::SeqCst);
        assert_eq!(listed(&later), Some(json!("getJiraIssue")));
        assert_eq!(
            listed(&Fake::new(Script::Widgets)),
            Some(json!("getJiraIssue"))
        );
        let late = Fake::new(Script::WidgetsLate);
        assert_eq!(listed(&late), None);
        assert_eq!(listed(&late), Some(json!("getJiraIssue")));
        assert_eq!(listed(&late), Some(json!("getJiraIssue")));
    }
}
