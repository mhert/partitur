//! A scripted ACP v1 agent speaking over stdio, for the backend's integration tests.
//!
//! Its traffic follows GitHub Copilot CLI's: it answers a v2 `initialize` with protocol
//! version 1 and the agent name `Copilot`, has no `state_update`, and answers a prompt only
//! once the turn's work is done, with the turn's stop reason.
//!
//! The first argument picks the script:
//!
//! - `happy`: `session/new` answers with the id `cp-1` and the config options `mode`
//!   (`…#agent`, `…#plan`, `…#autopilot` of the session-modes URL; current `#agent`),
//!   `allow_all` (`on`, `off`; current `off`) and `model` (`gpt-5`, `claude-sonnet-4.5`; current
//!   `gpt-5`). `session/load` replays the user message `earlier` and the agent text `earlier
//!   reply`, then answers with the same config. `session/set_config_option` accepts `model` set
//!   to an offered model and answers with the changed config; any other option or value is an
//!   invalid-params error. `session/prompt` streams `AgentMessageChunk("Hello ")`,
//!   `AgentMessageChunk("world")`, a pending `read` tool call `t1` titled `Read README.md`, its
//!   update to `completed` with the text `ok`, then answers `end_turn`.
//! - `load-fails`: `session/load` answers with an error naming the session.
//! - `prompt-fails`: `session/prompt` answers with an authentication error, like Copilot without
//!   a login.
//! - `asks`: a prompt reports the pending tool call `t1`, then asks `session/request_permission`
//!   about it with the options `allow_once` and `reject_once`. Allowing completes the call with
//!   `ok`; any other answer fails it with the chosen option's id, or `cancelled`. The prompt is
//!   then answered `end_turn`.
//! - `slow`: a prompt streams `AgentMessageChunk("working…")`, then waits up to five seconds for
//!   `session/cancel`, and is answered `cancelled` when cancelled, else `end_turn`.
//!
//! Every script exits once its client closes stdin. When `PARTITUR_FAKE_LOG` names a file, every
//! script appends the method of each request it receives to it, one per line, so a test sees
//! which requests reached the agent, and in how many processes: each process is initialized
//! once.

use std::error::Error;
use std::io::{self, BufRead, Stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionConfigOption,
    SessionConfigSelectOption, SessionId, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, Lines};
use futures::channel::mpsc;
use futures::{Sink, Stream};
use tokio::sync::Notify;

/// The id every created session gets.
const SESSION_ID: &str = "cp-1";
/// The tool call every turn makes.
const TOOL_CALL: &str = "t1";
/// The option that allows the tool call.
const ALLOW: &str = "allow_once";
/// The session-modes URL Copilot's modes are fragments of.
const MODES: &str = "https://agentclientprotocol.com/protocol/session-modes";
/// The models the agent offers, the first one current.
const MODELS: [&str; 2] = ["gpt-5", "claude-sonnet-4.5"];
/// How long a `slow` turn waits for a cancel before it ends by itself.
const SLOW_TURN: Duration = Duration::from_secs(5);
/// The variable naming the file the agent logs the methods of its requests to.
const LOG_VAR: &str = "PARTITUR_FAKE_LOG";

/// What the agent does, chosen by its first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Happy,
    LoadFails,
    PromptFails,
    Asks,
    Slow,
}

impl Script {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "happy" => Some(Self::Happy),
            "load-fails" => Some(Self::LoadFails),
            "prompt-fails" => Some(Self::PromptFails),
            "asks" => Some(Self::Asks),
            "slow" => Some(Self::Slow),
            _ => None,
        }
    }
}

type Result<T = ()> = std::result::Result<T, agent_client_protocol::Error>;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let name = std::env::args().nth(1).unwrap_or_default();
    let script = Script::parse(&name).ok_or_else(|| format!("unknown script {name:?}"))?;
    let cancelled = Arc::new(Notify::new());
    let on_cancel = Arc::clone(&cancelled);
    Agent
        .v1()
        .name("fake-copilot")
        .on_receive_request(
            async |_request: InitializeRequest, responder: Responder<InitializeResponse>, _cx| {
                log_method("initialize");
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(AgentCapabilities::new().load_session(true))
                        .agent_info(Implementation::new("Copilot", "1.0.114")),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_request: NewSessionRequest, responder: Responder<NewSessionResponse>, _cx| {
                log_method("session/new");
                responder.respond(
                    NewSessionResponse::new(SESSION_ID).config_options(config_options(MODELS[0])),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: LoadSessionRequest,
                        responder: Responder<LoadSessionResponse>,
                        cx: ConnectionTo<Client>| {
                log_method("session/load");
                load(&cx, script, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: SetSessionConfigOptionRequest,
                   responder: Responder<SetSessionConfigOptionResponse>,
                   _cx| {
                log_method("session/set_config_option");
                responder.respond_with_result(set_config(&request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                log_method("session/prompt");
                prompt(&cx, script, Arc::clone(&cancelled), request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |_cancel: CancelNotification, _cx| {
                on_cancel.notify_one();
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(stdio()?, async |cx: ConnectionTo<Client>| {
            cx.incoming_closed().await;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Stdin and stdout as line streams, stdin read on its own thread and every accepted line
/// written before the connection ends, as in the v2 fake agent.
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

/// Appends `method` to the file [`LOG_VAR`] names, if it names one.
fn log_method(method: &str) {
    if let Some(path) = std::env::var_os(LOG_VAR) {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
        let _ = file.and_then(|mut file| writeln!(file, "{method}"));
    }
}

/// Answers `session/load`: an error for `load-fails`, else the history replay and the
/// session's config.
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
    responder.respond(LoadSessionResponse::new().config_options(config_options(MODELS[0])))
}

/// An invalid-params error explained by `message`.
fn invalid_params(message: String) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(message)
}

/// Answers `session/set_config_option`: only `model` can change, to a model the agent offers.
///
/// # Errors
///
/// Returns an invalid-params error for any other option or value.
fn set_config(request: &SetSessionConfigOptionRequest) -> Result<SetSessionConfigOptionResponse> {
    let value = serde_json::to_value(&request.value).unwrap_or_default();
    let model = value.get("value").and_then(serde_json::Value::as_str);
    match model {
        Some(model) if &*request.config_id.0 == "model" && MODELS.contains(&model) => {
            Ok(SetSessionConfigOptionResponse::new(config_options(model)))
        }
        _ => Err(invalid_params(format!(
            "only `model` can change, to an offered model; refusing {} = {value}",
            request.config_id
        ))),
    }
}

/// Runs one turn as the script says; the prompt is answered once the turn is done.
fn prompt(
    cx: &ConnectionTo<Client>,
    script: Script,
    cancelled: Arc<Notify>,
    request: PromptRequest,
    responder: Responder<PromptResponse>,
) -> Result {
    let session = request.session_id;
    match script {
        Script::PromptFails => responder.respond_with_error(
            agent_client_protocol::Error::auth_required().data("Authentication required"),
        ),
        // Waiting for an answer or a cancel would block the dispatch of that very message.
        Script::Asks => cx.spawn(ask_then_answer(cx.clone(), session, responder)),
        Script::Slow => cx.spawn(work_until_cancelled(
            cx.clone(),
            cancelled,
            session,
            responder,
        )),
        Script::Happy | Script::LoadFails => {
            let turn = [
                SessionUpdate::AgentMessageChunk(chunk("Hello ")),
                SessionUpdate::AgentMessageChunk(chunk("world")),
                pending_call(),
                ended_call(ToolCallStatus::Completed, "ok"),
            ];
            notify_all(cx, &session, turn)?;
            responder.respond(PromptResponse::new(StopReason::EndTurn))
        }
    }
}

/// The `asks` turn: asks for permission for the tool call and runs it only if allowed.
async fn ask_then_answer(
    cx: ConnectionTo<Client>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    notify(&cx, &session, pending_call())?;
    let call = ToolCallUpdate::new(
        TOOL_CALL,
        ToolCallUpdateFields::new()
            .title(tool_title())
            .kind(ToolKind::Read),
    );
    let options = vec![
        PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new("reject_once", "Reject", PermissionOptionKind::RejectOnce),
    ];
    let request = RequestPermissionRequest::new(session.clone(), call, options);
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
    notify(&cx, &session, ended_call(status, &output))?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// The `slow` turn: works until cancelled, or ends by itself after [`SLOW_TURN`].
async fn work_until_cancelled(
    cx: ConnectionTo<Client>,
    cancelled: Arc<Notify>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let working = SessionUpdate::AgentMessageChunk(chunk("working…"));
    notify(&cx, &session, working)?;
    let cancelled = tokio::time::timeout(SLOW_TURN, cancelled.notified()).await;
    let reason = cancelled.map_or(StopReason::EndTurn, |()| StopReason::Cancelled);
    responder.respond(PromptResponse::new(reason))
}

/// The pending tool call [`TOOL_CALL`].
fn pending_call() -> SessionUpdate {
    SessionUpdate::ToolCall(
        ToolCall::new(TOOL_CALL, tool_title())
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Pending),
    )
}

/// The update ending the tool call [`TOOL_CALL`] with `status` and the text `output`.
fn ended_call(status: ToolCallStatus, output: &str) -> SessionUpdate {
    let fields = ToolCallUpdateFields::new()
        .status(status)
        .content(vec![ToolCallContent::from(output.to_owned())]);
    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(TOOL_CALL, fields))
}

fn tool_title() -> String {
    "Read README.md".to_owned()
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
    ContentChunk::new(ContentBlock::from(text))
}

/// The agent's config options with `model` as the current model: Copilot's `mode` in its
/// asking `#agent` mode and `allow_all` off.
fn config_options(model: &str) -> Vec<SessionConfigOption> {
    let mode = |name: &str| format!("{MODES}#{name}");
    vec![
        SessionConfigOption::select(
            "mode",
            "Mode",
            mode("agent"),
            vec![
                SessionConfigSelectOption::new(mode("agent"), "Agent"),
                SessionConfigSelectOption::new(mode("plan"), "Plan"),
                SessionConfigSelectOption::new(mode("autopilot"), "Autopilot"),
            ],
        ),
        SessionConfigOption::select(
            "allow_all",
            "Allow all",
            "off",
            vec![
                SessionConfigSelectOption::new("on", "On"),
                SessionConfigSelectOption::new("off", "Off"),
            ],
        ),
        SessionConfigOption::select(
            "model",
            "Model",
            model.to_owned(),
            MODELS
                .iter()
                .map(|model| SessionConfigSelectOption::new(*model, *model))
                .collect::<Vec<_>>(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::Script;

    #[test]
    fn every_script_is_chosen_by_its_name_and_nothing_else_is() {
        let scripts = [
            ("happy", Script::Happy),
            ("load-fails", Script::LoadFails),
            ("prompt-fails", Script::PromptFails),
            ("asks", Script::Asks),
            ("slow", Script::Slow),
        ];
        for (name, script) in scripts {
            assert_eq!(Script::parse(name), Some(script), "{name}");
        }
        assert_eq!(Script::parse("unknown"), None);
    }
}
