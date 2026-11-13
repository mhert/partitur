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
//!   server's `superpowers_start_brainstorming` tool and streams the URL, a space and the
//!   text of the result as one `AgentMessageChunk`, so a test sees which URL the session was
//!   opened with. Without a URL the call fails and its error is streamed instead.
//! - `continues`: like `happy`, but after the turn its background work ends: an `idle`
//!   without a stop reason, then a turn of its own without a prompt (`running`,
//!   `AgentMessageChunk("background done")`, `idle` with `end_turn`).
//! - `subagent`: like `happy`, but right after `running` a subagent's session `fake-sub` on
//!   the same connection reports `running`, `AgentMessageChunk("subagent")` and `idle` with
//!   `end_turn`.
//!
//! Every other script exits once its client closes stdin.

use std::error::Error;
use std::io::{self, BufRead, Stdout, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agent_client_protocol::v2::schema::{
    AgentCapabilities, CancelSessionNotification, ConfigOptionUpdate, ContentBlock, ContentChunk,
    IdleStateUpdate, Implementation, InitializeRequest, InitializeResponse, McpServer,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionSubject,
    RequiresActionStateUpdate, ResumeSessionRequest, ResumeSessionResponse, RunningStateUpdate,
    SessionCapabilities, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOption, SessionConfigValueId, SessionId, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StateUpdate, StopReason,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind, UpdateSessionNotification,
    UsageUpdate, UserMessage,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, Lines};
use futures::channel::mpsc;
use futures::{Sink, Stream};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;

/// The id every created session gets, except under `env`.
const SESSION_ID: &str = "fake-1";
/// The session of the subagent the `subagent` script runs on the same connection.
const SUBAGENT_SESSION: &str = "fake-sub";
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
}

impl Script {
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
}

impl Fake {
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
    let fake = Arc::new(Fake {
        script,
        cancelled: Notify::new(),
        exit: Notify::new(),
        tools_url: Mutex::new(None),
    });
    let (on_prompt, on_cancel) = (Arc::clone(&fake), Arc::clone(&fake));
    let (on_new, on_resume) = (Arc::clone(&fake), Arc::clone(&fake));
    Agent
        .v2()
        .name("fake-agent")
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx| {
                responder.respond(
                    InitializeResponse::new(
                        request.protocol_version,
                        Implementation::new("fake-agent", env!("CARGO_PKG_VERSION")),
                    )
                    .capabilities(AgentCapabilities::new().session(SessionCapabilities::new())),
                )
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
                if script == Script::RefusedIdles {
                    let own_turn = [
                        idle(None),
                        running(),
                        SessionUpdate::AgentMessageChunk(chunk("agent-2", "on its own")),
                        idle(Some(StopReason::EndTurn)),
                    ];
                    return notify_all(&cx, &cancel.session_id, own_turn);
                }
                on_cancel.cancelled.notify_one();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
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

/// Stdin and stdout as line streams.
///
/// The crate's own `Stdio` transport does not flush before `connect_with` returns, so a
/// script that exits right after answering could lose that answer; `Lines` writes every
/// accepted line before the connection ends. Stdin is read on its own thread, so a waiting
/// read never stalls the connection's writes.
fn stdio() -> io::Result<
    Lines<
        impl Sink<String, Error = io::Error> + Send + 'static,
        impl Stream<Item = io::Result<String>> + Send + 'static,
    >,
> {
    let (lines, incoming) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("stdin".to_owned())
        .spawn(move || {
            // Forwards lines until stdin ends or the connection stops listening.
            let _ = io::stdin()
                .lock()
                .lines()
                .try_for_each(|line| lines.unbounded_send(line));
        })?;
    let outgoing = futures::sink::unfold(
        io::stdout(),
        |mut stdout: Stdout, line: String| async move {
            writeln!(stdout, "{line}")?;
            stdout.flush()?;
            Ok(stdout)
        },
    );
    Ok(Lines::new(outgoing, incoming))
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

/// An invalid-params error explained by `message`.
fn invalid_params(message: String) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message)
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
        Some(url) => propose_children(&url).await.unwrap_or_else(|error| error),
        None => NO_TOOLS.to_owned(),
    };
    let answer = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, &text));
    notify_all(&cx, &session, [answer, idle(Some(StopReason::EndTurn))])
}

/// The `reports-tools` turn: reports the start of brainstorming through the tool server and
/// streams the server's URL with what it answered.
async fn report_tools(cx: ConnectionTo<Client>, fake: Arc<Fake>, session: SessionId) -> Result {
    let url = fake.tools_url().unwrap_or_default();
    let answer = call_tool(&url, "superpowers_start_brainstorming", json!({})).await;
    let text = format!("{url} {}", answer.unwrap_or_else(|error| error));
    let answer = SessionUpdate::AgentMessageChunk(chunk(AGENT_MESSAGE, &text));
    notify_all(&cx, &session, [answer, idle(Some(StopReason::EndTurn))])
}

/// Calls `propose_children` on the MCP server at `url` and returns the text of the result.
///
/// # Errors
///
/// Returns what went wrong when the server cannot be reached or its answer has no text.
async fn propose_children(url: &str) -> std::result::Result<String, String> {
    let arguments = json!({"children": [
        {"repository": "api", "prompt": "Add the endpoint", "title": "API endpoint"},
        {"repository": "web", "prompt": "Add the page", "title": "Web page"},
    ]});
    call_tool(url, "propose_children", arguments).await
}

/// Calls the tool `name` with `arguments` on the MCP server at `url` and returns the text of
/// the result.
///
/// # Errors
///
/// Returns what went wrong when the server cannot be reached or its answer has no text.
async fn call_tool(url: &str, name: &str, arguments: Value) -> std::result::Result<String, String> {
    let call = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });
    let response = post(url, &call.to_string())
        .await
        .map_err(|error| format!("the tool call failed: {error}"))?;
    result_text(&response)
}

/// POSTs `body` as JSON to `url`, an `http://` URL, and returns the whole raw response.
///
/// # Errors
///
/// Returns the I/O error of the exchange, or an invalid-input error for a URL that is not
/// `http://{host}:{port}/{path}`.
async fn post(url: &str, body: &str) -> io::Result<Vec<u8>> {
    let (host, path) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, url.to_owned()))?;
    let request = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(host).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

/// The text of the first content block of the JSON-RPC result in the raw HTTP `response`.
///
/// # Errors
///
/// Returns what is missing when the response has no body, no JSON or no such text.
fn result_text(response: &[u8]) -> std::result::Result<String, String> {
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .and_then(|end| response.get(end + 4..))
        .ok_or("the tool server's answer has no body")?;
    let answer: Value = serde_json::from_slice(body).map_err(|error| error.to_string())?;
    answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the tool server's answer has no text: {answer}"))
}

/// The foreground work started.
fn running() -> SessionUpdate {
    SessionUpdate::StateUpdate(StateUpdate::Running(RunningStateUpdate::new()))
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

fn tool_title() -> String {
    "Read README.md".to_owned()
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
    use super::{Script, SessionConfigValueId, result_text, switch_mode};

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
        ];
        for (name, script) in scripts {
            assert_eq!(Script::parse(name), Some(script), "{name}");
        }
        assert_eq!(Script::parse("unknown"), None);
    }

    #[test]
    fn the_result_text_is_read_from_the_body_of_the_answer() {
        let answer = b"HTTP/1.1 200 OK\r\n\r\n{\"result\":{\"content\":[{\"text\":\"recorded\"}]}}";
        assert_eq!(result_text(answer), Ok("recorded".to_owned()));
        assert_eq!(
            result_text(b"HTTP/1.1 200 OK"),
            Err("the tool server's answer has no body".to_owned())
        );
        assert!(result_text(b"HTTP/1.1 200 OK\r\n\r\n{").is_err());
        assert_eq!(
            result_text(b"HTTP/1.1 200 OK\r\n\r\n{}"),
            Err("the tool server's answer has no text: {}".to_owned())
        );
    }
}
