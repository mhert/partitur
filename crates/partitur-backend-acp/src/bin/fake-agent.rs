//! A scripted ACP agent speaking over stdio, for the backend's integration tests.
//!
//! The first argument picks the script:
//!
//! - `happy`: `initialize` reports `load_session`. `session/new` answers with the id `fake-1`,
//!   the config options `model` (`sonnet`, `opus`; current `sonnet`) and `effort` (`low`,
//!   `high`; current `low`) and the modes `default` (current) and `auto`. `session/set_mode`
//!   accepts only `default`, so a backend that switches to any other mode fails its create or
//!   load: Partitur keeps every session asking for permissions. Switching to `default`
//!   announces the config with effort `high` in a `ConfigOptionUpdate` before it answers, like
//!   the Claude Code adapter announcing its changed mode option; since that differs from the
//!   response, a test sees which config the backend reported. `session/load` replays
//!   `UserMessageChunk("earlier")` and `AgentMessageChunk("earlier reply")`, then answers with
//!   the modes and the config of `session/new`. `session/set_config_option` accepts `model` set
//!   to `sonnet` or `opus` and answers with the changed options (effort `high`); any other
//!   option or value is an invalid-params error. `session/prompt` streams `AgentThoughtChunk("thinking…")`,
//!   `AgentMessageChunk("Hello ")`, `AgentMessageChunk("world")`, a pending `Read` tool call
//!   `t1`, its update to `Completed` with the text `ok` and `UsageUpdate(10, 1000)`, then ends
//!   the turn with `EndTurn`.
//! - `modeless`: like `happy`, but `session/new` offers no modes.
//! - `asks`: like `happy`, but before the tool call it asks `session/request_permission` with
//!   the options `allow-once` and `reject`. Allowing completes the call with `ok`; any other
//!   answer fails it with the chosen option's id, or `cancelled`.
//! - `load-fails`: `session/load` answers with an error naming the session.
//! - `prompt-fails`: `session/prompt` answers with an authentication error, like the adapter
//!   without a login.
//! - `dies`: exits when asked to create a session, right after `initialize`.
//! - `dies-later`: ends the first turn with `EndTurn`, then closes the connection and exits.
//! - `slow`: a turn streams `AgentMessageChunk("working…")`, then waits up to five seconds for
//!   `session/cancel`; it ends with `Cancelled` when cancelled, else with `EndTurn`.
//! - `hangs`: never reads or answers anything, ignores stdin closing and exits after thirty
//!   seconds, far longer than the backend's shutdown grace.
//!
//! Every other script exits once its client closes stdin.

use std::error::Error;
use std::io::{self, BufRead, Stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ConfigOptionUpdate, Content, ContentChunk,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionId, SessionMode, SessionModeId,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse, StopReason,
    ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    UsageUpdate,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, Lines};
use futures::channel::mpsc;
use futures::{Sink, Stream};
use tokio::sync::Notify;

/// The id every created session gets.
const SESSION_ID: &str = "fake-1";
/// The tool call every turn makes.
const TOOL_CALL: &str = "t1";
/// The option that allows the tool call.
const ALLOW: &str = "allow-once";
/// The effort a session is created or loaded with.
const OPENING_EFFORT: &str = "low";
/// The effort the mode switch announces.
const CURRENT_EFFORT: &str = "high";
/// How long a `slow` turn waits for a cancel before it ends by itself.
const SLOW_TURN: Duration = Duration::from_secs(5);
/// How long the `hangs` script lives: far past the backend's five-second shutdown grace, which
/// kills it first, so the hang never ends by itself while a test waits.
const HANG: Duration = Duration::from_secs(30);
/// The only mode the agent switches to.
const DEFAULT_MODE: &str = "default";

/// What the agent does, chosen by its first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Happy,
    Modeless,
    Asks,
    LoadFails,
    PromptFails,
    Dies,
    DiesLater,
    Slow,
    Hangs,
}

impl Script {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "happy" => Some(Self::Happy),
            "modeless" => Some(Self::Modeless),
            "asks" => Some(Self::Asks),
            "load-fails" => Some(Self::LoadFails),
            "prompt-fails" => Some(Self::PromptFails),
            "dies" => Some(Self::Dies),
            "dies-later" => Some(Self::DiesLater),
            "slow" => Some(Self::Slow),
            "hangs" => Some(Self::Hangs),
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
    });
    let (on_prompt, on_cancel) = (Arc::clone(&fake), Arc::clone(&fake));
    Agent
        .v1()
        .name("fake-agent")
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx| {
                responder.respond(
                    InitializeResponse::new(request.protocol_version)
                        .agent_capabilities(AgentCapabilities::new().load_session(true)),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_: NewSessionRequest, responder: Responder<NewSessionResponse>, _cx| {
                if script == Script::Dies {
                    std::process::exit(0);
                }
                responder.respond(new_session(script))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: LoadSessionRequest,
                        responder: Responder<LoadSessionResponse>,
                        cx: ConnectionTo<Client>| {
                load(&cx, script, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: SetSessionModeRequest,
                        responder: Responder<SetSessionModeResponse>,
                        cx: ConnectionTo<Client>| {
                // Only an accepted switch announces the config before it answers.
                let switched = switch_mode(&request.mode_id).and_then(|response| {
                    let current = config_options("sonnet", CURRENT_EFFORT);
                    let update =
                        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(current));
                    notify(&cx, &request.session_id, update).map(|()| response)
                });
                responder.respond_with_result(switched)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: SetSessionConfigOptionRequest,
                        responder: Responder<SetSessionConfigOptionResponse>,
                        _cx| { set_config(&request, responder) },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                prompt(&cx, Arc::clone(&on_prompt), request.session_id, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_: CancelNotification, _cx| {
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

/// The answer to `session/new`: the session, its config and, unless `modeless`, its modes.
fn new_session(script: Script) -> NewSessionResponse {
    let response = NewSessionResponse::new(SESSION_ID)
        .config_options(config_options("sonnet", OPENING_EFFORT));
    if script == Script::Modeless {
        response
    } else {
        response.modes(modes())
    }
}

/// Answers `session/load`: an error for `load-fails`, else the history replay and the session.
fn load(
    cx: &ConnectionTo<Client>,
    script: Script,
    request: &LoadSessionRequest,
    responder: Responder<LoadSessionResponse>,
) -> Result {
    if script == Script::LoadFails {
        let error = invalid_params(format!("no session {}", request.session_id)); // coverage: a failed load makes the transport SIGKILL this process
        return responder.respond_with_error(error); // coverage: a failed load makes the transport SIGKILL this process
    }
    let history = [
        SessionUpdate::UserMessageChunk(chunk("earlier")),
        SessionUpdate::AgentMessageChunk(chunk("earlier reply")),
    ];
    notify_all(cx, &request.session_id, history)?;
    responder.respond(
        LoadSessionResponse::new()
            .modes(modes())
            .config_options(config_options("sonnet", OPENING_EFFORT)),
    )
}

/// The answer to `session/set_mode`: only [`DEFAULT_MODE`] is accepted.
fn switch_mode(mode: &SessionModeId) -> Result<SetSessionModeResponse> {
    if &*mode.0 == DEFAULT_MODE {
        Ok(SetSessionModeResponse::new())
    } else {
        Err(invalid_params(format!(
            "only `{DEFAULT_MODE}` is accepted, not {mode}"
        )))
    }
}

/// An invalid-params error explained by `message`.
fn invalid_params(message: String) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message)
}

/// Answers `session/set_config_option`: only `model` can change, to a model the agent offers.
fn set_config(
    request: &SetSessionConfigOptionRequest,
    responder: Responder<SetSessionConfigOptionResponse>,
) -> Result {
    let value = request.value.as_value_id().map(ToString::to_string);
    match value.as_deref() {
        Some(model @ ("sonnet" | "opus")) if &*request.config_id.0 == "model" => responder.respond(
            SetSessionConfigOptionResponse::new(config_options(model, CURRENT_EFFORT)),
        ),
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
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    match fake.script {
        // Waiting for an answer or a cancel would block the dispatch of that very message.
        Script::Asks => cx.spawn(ask_then_answer(cx.clone(), session, responder)),
        Script::Slow => cx.spawn(work_until_cancelled(cx.clone(), fake, session, responder)),
        Script::PromptFails => responder.respond_with_error(
            agent_client_protocol::Error::auth_required().data("Authentication required"),
        ),
        Script::DiesLater => {
            responder.respond(PromptResponse::new(StopReason::EndTurn))?;
            fake.exit.notify_one();
            Ok(())
        }
        Script::Happy | Script::Modeless | Script::LoadFails | Script::Dies | Script::Hangs => {
            notify_all(cx, &session, opening())?;
            notify_all(cx, &session, closing(ToolCallStatus::Completed, "ok"))?;
            responder.respond(PromptResponse::new(StopReason::EndTurn))
        }
    }
}

/// The `asks` turn: asks for permission before the tool call and runs it only if allowed.
async fn ask_then_answer(
    cx: ConnectionTo<Client>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    notify_all(&cx, &session, opening())?;
    let fields = ToolCallUpdateFields::new()
        .title(tool_title())
        .kind(ToolKind::Read);
    let options = vec![
        PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
    ];
    let request = RequestPermissionRequest::new(
        session.clone(),
        ToolCallUpdate::new(TOOL_CALL, fields),
        options,
    );
    let answer = cx.send_request(request).block_task().await?;
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
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// The `slow` turn: works until cancelled, or ends by itself after [`SLOW_TURN`].
async fn work_until_cancelled(
    cx: ConnectionTo<Client>,
    fake: Arc<Fake>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let working = SessionUpdate::AgentMessageChunk(chunk("working…"));
    notify(&cx, &session, working)?;
    let cancelled = tokio::time::timeout(SLOW_TURN, fake.cancelled.notified()).await;
    let reason = cancelled.map_or(StopReason::EndTurn, |()| StopReason::Cancelled);
    responder.respond(PromptResponse::new(reason))
}

/// The start of every turn: a thought and two chunks of text.
fn opening() -> [SessionUpdate; 3] {
    [
        SessionUpdate::AgentThoughtChunk(chunk("thinking…")),
        SessionUpdate::AgentMessageChunk(chunk("Hello ")),
        SessionUpdate::AgentMessageChunk(chunk("world")),
    ]
}

/// The end of every turn: the tool call, its result with `status` and `output`, and the
/// context usage.
fn closing(status: ToolCallStatus, output: &str) -> [SessionUpdate; 3] {
    let call = ToolCall::new(TOOL_CALL, tool_title())
        .kind(ToolKind::Read)
        .status(ToolCallStatus::Pending);
    let result = ToolCallUpdateFields::new()
        .status(status)
        .content(vec![ToolCallContent::Content(Content::new(output))]);
    [
        SessionUpdate::ToolCall(call),
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(TOOL_CALL, result)),
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
    cx.send_notification(SessionNotification::new(session.clone(), update))
}

fn chunk(text: &str) -> ContentChunk {
    ContentChunk::new(text.into())
}

fn tool_title() -> String {
    "Read README.md".to_owned()
}

fn modes() -> SessionModeState {
    SessionModeState::new(
        "default",
        vec![
            SessionMode::new("default", "Default"),
            SessionMode::new("auto", "Auto"),
        ],
    )
}

/// The agent's config options with `model` and `effort` as the current values.
fn config_options(model: &str, effort: &str) -> Vec<SessionConfigOption> {
    vec![
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
    ]
}

#[cfg(test)]
mod tests {
    use super::{Script, SessionModeId, switch_mode};

    #[test]
    fn only_the_default_mode_is_accepted() {
        assert!(switch_mode(&SessionModeId::new("default")).is_ok());
        let refused = switch_mode(&SessionModeId::new("auto"));
        assert!(refused.is_err_and(|error| error.to_string().contains("not auto")));
    }

    #[test]
    fn every_script_is_chosen_by_its_name_and_nothing_else_is() {
        let scripts = [
            ("happy", Script::Happy),
            ("modeless", Script::Modeless),
            ("asks", Script::Asks),
            ("load-fails", Script::LoadFails),
            ("prompt-fails", Script::PromptFails),
            ("dies", Script::Dies),
            ("dies-later", Script::DiesLater),
            ("slow", Script::Slow),
            ("hangs", Script::Hangs),
        ];
        for (name, script) in scripts {
            assert_eq!(Script::parse(name), Some(script), "{name}");
        }
        assert_eq!(Script::parse("unknown"), None);
    }
}
