//! A scripted v1 agent in Copilot's or opencode's manner, speaking ACP over stdio, for the
//! backend's integration tests.
//!
//! Its traffic follows GitHub Copilot CLI's: it answers a v2 `initialize` with protocol
//! version 1 and the agent name `Copilot`, has no `state_update`, and answers a prompt only
//! once the turn's work is done, with the turn's stop reason. The `opencode-` scripts follow
//! opencode's (`opencode acp`) instead, as described below.
//!
//! The first argument picks the script:
//!
//! - `happy`: `session/new` answers with the id `cp-1` and the config options `mode`
//!   (`…#agent`, `…#plan`, `…#autopilot` of the session-modes URL; current `#autopilot`),
//!   `allow_all` (`on`, `off`; current `on`) and `model` (`gpt-5`, `claude-sonnet-4.5`; current
//!   `gpt-5`): a session that skips every permission prompt. `session/load` replays the user
//!   message `earlier` and the agent text `earlier reply`, then answers with the current config.
//!   `session/set_config_option` accepts only the asking values, `mode` set to `#agent` and
//!   `allow_all` set to `off`, and `model` set to an offered model, and answers with the changed
//!   config; any other option or value is an invalid-params error, so a client that switches to
//!   anything else fails its create or resume. Every script shares this config. `session/prompt`
//!   streams `AgentMessageChunk("Hello ")`,
//!   `AgentMessageChunk("world")`, a pending `read` tool call `t1` titled `Read README.md`, its
//!   update to `completed` with the text `ok`, then answers `end_turn`.
//! - `load-fails`: `session/load` answers with an error naming the session.
//! - `modeless`: as `happy`, but the config has only `model`: nothing keeps the session
//!   asking.
//! - `prompt-fails`: `session/prompt` answers with an authentication error, like Copilot without
//!   a login.
//! - `asks`: a prompt reports the pending tool call `t1`, then asks `session/request_permission`
//!   about it with the options `allow_once` and `reject_once`. Allowing completes the call with
//!   `ok`; any other answer fails it with the chosen option's id, or `cancelled`. The prompt is
//!   then answered `end_turn`.
//! - `slow`: a prompt streams `AgentMessageChunk("working…")`, then waits up to twenty seconds for
//!   `session/cancel`, and is answered `cancelled` when cancelled, else `end_turn`. It reports
//!   no session events, as Copilot sends no idle for a cancelled turn.
//! - `events`: a prompt reports Copilot's session events (`github.com/copilot/sessionEvent`)
//!   around its work: `assistant.turn_start`, then `AgentMessageChunk("done")`,
//!   `assistant.idle` (not aborted) and `session.idle`, and is then answered `end_turn`.
//! - `background`: as `events`, but `session.idle` comes 300 ms after `assistant.idle`, as
//!   when work goes on in the background.
//! - `lingers`: as `background`, but the background work runs until a `session/cancel` arrives
//!   (or twenty seconds pass), and only then comes `session.idle`.
//! - `subagent`: as `events`, but between `assistant.turn_start` and `done` a subagent reports
//!   `AgentMessageChunk("from the subagent")` tagged with its `agentId` in
//!   `_meta["github.com/copilot"]`, and its own `assistant.idle`, with a top-level `agentId`.
//! - `subagent-asks`: a prompt reports a subagent's pending `edit` tool call `s1` titled
//!   `Edit src/lib.rs`, tagged with its `agentId` as in `subagent`, then asks
//!   `session/request_permission` about it naming only its id, as Copilot asks about a
//!   subagent's call, with the options `allow_once` and `reject_once`. The subagent then ends the
//!   call, completed when allowed and failed otherwise, and the prompt is answered `end_turn`.
//! - `interrupts`: as `events`, but each turn works 500 ms before its events, and a prompt
//!   arriving while an earlier one is unanswered cancels the earlier one, as Copilot does: the
//!   earlier turn streams `AgentMessageChunk("Info: Operation cancelled by user")` and is
//!   answered `cancelled`, with no idle event.
//! - `orchestrates`: remembers the URL of the HTTP MCP server a session is opened with. A
//!   turn reports the pending call `p1` of Partitur's `propose_children`, titled
//!   `partitur-propose_children` with kind `other` and no name, as Copilot reports an MCP
//!   tool's call, then asks `session/request_permission` about it titled with the bare tool
//!   name `propose_children` and nothing else, with the options `allow_once`, `allow_always`
//!   and `reject_once`. Allowed, it calls the tool on the remembered server, one JSON-RPC
//!   `tools/call` sent as a `POST` over plain HTTP/1.1 as `fake-agent`'s `orchestrates` does,
//!   proposing the children `api` and `web`, completes the call with the text of the result
//!   and streams that text; otherwise it fails the call with the chosen option's id, or
//!   `cancelled`. The prompt is then answered `end_turn`.
//!
//! - `instructions`: a prompt streams one `AgentMessageChunk`: the value of
//!   `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` on its own line, then a line `<folder>: exists` or
//!   `<folder>: missing` for each comma-separated folder it lists, telling whether
//!   `partitur.instructions.md` is in it; the prompt is then answered `end_turn`.
//! - `records-args`: as `happy`, but on start it appends its arguments, the script's name
//!   first, one per line, to the file `PARTITUR_FAKE_LOG` names, so a test sees how often the
//!   agent was started, and with which arguments.
//! - `opencode-asks`: in opencode's manner. `initialize` is answered with the agent name
//!   `OpenCode`, and on start the script appends a line `OPENCODE_CONFIG_CONTENT <value>` with
//!   the value of that variable in its environment (empty when unset) to the file
//!   `PARTITUR_FAKE_LOG` names. A session's config options are `model` (`opencode/big-pickle`,
//!   `strata/strata`; current `opencode/big-pickle`), `effort` (`low`, `high`; current `high`)
//!   and `mode` (`build`, `plan`; current `build`), and `session/set_config_option` accepts any
//!   offered value of them, and refuses any other. A prompt reports the pending `shell` call
//!   `t1` of kind `execute`, then asks `session/request_permission` about it, named `shell` and
//!   titled `echo hi > a.txt`, with opencode's options `once`, `always` and `reject`. After
//!   `once` the call completes with `hi` and the prompt is answered `end_turn`; after any other
//!   answer the call fails with the chosen option's id, or `cancelled`, and the prompt is
//!   answered `cancelled`, as opencode does.
//! - `opencode-retries`: as `opencode-asks`, but a prompt sends one session info update whose
//!   `_meta["opencode/retry"]` names attempt 1 and the error `Cannot connect to API:
//!   ECONNREFUSED`, then one clearing it with `null`, and then answers the prompt with an
//!   internal error carrying that message, as opencode does once it gives up on its model.
//!
//! Every script exits once its client closes stdin. When `PARTITUR_FAKE_LOG` names a file, every
//! script appends the method of each request it receives to it, one per line, so a test sees
//! which requests reached the agent, and in how many processes: each process is initialized
//! once. The `subagent` script also logs `sent the subagent's updates` once it sent them, so a
//! test sees they were sent although the client shows nothing of them. When
//! `PARTITUR_FAKE_AGENT_NAME` is set, every script gives its value as the agent's name in the
//! `initialize` answer instead of `Copilot`, so a test sees what an ACP v1 agent that is not
//! Copilot gets.

use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::McpServer;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionId, SessionInfoUpdate,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StopReason, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, UntypedMessage};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

#[path = "fake/common.rs"]
mod common;

use common::{McpClient, agent_name, invalid_params, log_method, stdio, tool_title};
use partitur_backend_acp::{INSTRUCTIONS_DIRS_VAR, INSTRUCTIONS_FILE};

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
const SLOW_TURN: Duration = Duration::from_secs(20);
/// The variable naming the file the agent logs the methods of its requests to.
/// The notification carrying Copilot's session events.
const SESSION_EVENT: &str = "github.com/copilot/sessionEvent";
/// The key of Copilot's `_meta`.
const COPILOT: &str = "github.com/copilot";
/// The id of the subagent of the `subagent` script.
const SUBAGENT: &str = "sub-1";
/// The subagent's tool call of the `subagent-asks` script.
const SUBAGENT_CALL: &str = "s1";
/// The line the `subagent` script logs once its subagent reported, which the client drops.
const SUBAGENT_SENT: &str = "sent the subagent's updates";
/// How long the `background` script's work runs on after the foreground went idle.
const BACKGROUND_WORK: Duration = Duration::from_millis(300);
/// How long an `interrupts` turn works before its `done`.
const INTERRUPTIBLE_WORK: Duration = Duration::from_millis(500);
/// What an `interrupts` turn streams when a later prompt cancels it.
const INTERRUPTED: &str = "Info: Operation cancelled by user";
/// The call of Partitur's `propose_children` an `orchestrates` turn makes.
const PROPOSAL_CALL: &str = "p1";
/// The option that allows a tool call always.
const ALLOW_ALWAYS: &str = "allow_always";
/// The option that rejects a tool call.
const REJECT: &str = "reject_once";
/// The variable whose value the `opencode-` scripts log on start.
const OPENCODE_CONFIG_VAR: &str = "OPENCODE_CONFIG_CONTENT";
/// The models the `opencode-` scripts offer, the first one current.
const OPENCODE_MODELS: [&str; 2] = ["opencode/big-pickle", "strata/strata"];
/// The efforts the `opencode-` scripts offer.
const OPENCODE_EFFORTS: [&str; 2] = ["low", "high"];
/// The modes the `opencode-` scripts offer, the first one current.
const OPENCODE_MODES: [&str; 2] = ["build", "plan"];
/// opencode's options of a permission request: allow once, always, reject.
const ONCE: &str = "once";
const ALWAYS: &str = "always";
const OPENCODE_REJECT: &str = "reject";
/// The error the `opencode-retries` script's model fails with.
const UNREACHABLE: &str = "Cannot connect to API: ECONNREFUSED";

/// Where the unanswered `interrupts` turn waits for a later prompt to cancel it.
type Interrupt = Arc<Mutex<Option<oneshot::Sender<()>>>>;

/// What the agent does, chosen by its first argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Happy,
    LoadFails,
    PromptFails,
    Asks,
    Slow,
    Events,
    Background,
    Subagent,
    SubagentAsks,
    Interrupts,
    Lingers,
    Orchestrates,
    Modeless,
    Instructions,
    RecordsArgs,
    OpencodeAsks,
    OpencodeRetries,
}

impl Script {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "happy" => Some(Self::Happy),
            "load-fails" => Some(Self::LoadFails),
            "prompt-fails" => Some(Self::PromptFails),
            "asks" => Some(Self::Asks),
            "slow" => Some(Self::Slow),
            "events" => Some(Self::Events),
            "background" => Some(Self::Background),
            "subagent" => Some(Self::Subagent),
            "subagent-asks" => Some(Self::SubagentAsks),
            "interrupts" => Some(Self::Interrupts),
            "lingers" => Some(Self::Lingers),
            "orchestrates" => Some(Self::Orchestrates),
            "modeless" => Some(Self::Modeless),
            "instructions" => Some(Self::Instructions),
            "records-args" => Some(Self::RecordsArgs),
            "opencode-asks" => Some(Self::OpencodeAsks),
            "opencode-retries" => Some(Self::OpencodeRetries),
            _ => None,
        }
    }

    /// Whether the script plays opencode rather than Copilot.
    fn is_opencode(self) -> bool {
        matches!(self, Self::OpencodeAsks | Self::OpencodeRetries)
    }
}

type Result<T = ()> = std::result::Result<T, agent_client_protocol::Error>;

/// The current values of the agent's config options.
#[derive(Debug)]
struct Config {
    mode: String,
    allow_all: String,
    model: String,
    effort: String,
    /// Whether `mode` and `allow_all` are offered at all.
    modes: bool,
    /// Whether the options are opencode's: `model`, `effort` and `mode`.
    opencode: bool,
}

impl Config {
    /// The config a session of `script` starts with: for Copilot one that skips every
    /// permission prompt, for opencode its `build` mode.
    fn opening(script: Script) -> Self {
        if script.is_opencode() {
            return Self {
                mode: OPENCODE_MODES[0].to_owned(),
                allow_all: String::new(),
                model: OPENCODE_MODELS[0].to_owned(),
                effort: "high".to_owned(),
                modes: true,
                opencode: true,
            };
        }
        Self {
            mode: mode("autopilot"),
            allow_all: "on".to_owned(),
            model: MODELS[0].to_owned(),
            effort: String::new(),
            modes: script != Script::Modeless,
            opencode: false,
        }
    }
}

/// The agent's config, shared by its handlers.
type Shared = Arc<Mutex<Config>>;
/// The URL of the HTTP MCP server the session was opened with, once it was.
type ToolsUrl = Arc<Mutex<Option<String>>>;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let name = std::env::args().nth(1).unwrap_or_default();
    let script = Script::parse(&name).ok_or_else(|| format!("unknown script {name:?}"))?;
    if script == Script::RecordsArgs {
        std::env::args().skip(1).for_each(|arg| log_method(&arg));
    }
    if script.is_opencode() {
        let content = std::env::var(OPENCODE_CONFIG_VAR).unwrap_or_default();
        log_method(&format!("{OPENCODE_CONFIG_VAR} {content}"));
    }
    let (name, version) = if script.is_opencode() {
        ("OpenCode", "2.3.2")
    } else {
        ("Copilot", "1.0.114")
    };
    let cancelled = Arc::new(Notify::new());
    let on_cancel = Arc::clone(&cancelled);
    let interrupt = Interrupt::default();
    let config: Shared = Arc::new(Mutex::new(Config::opening(script)));
    let (created, loaded, changed) = (config.clone(), config.clone(), config);
    let tools_url = ToolsUrl::default();
    let (on_new, on_load, on_prompt) = (tools_url.clone(), tools_url.clone(), tools_url);
    Agent
        .v1()
        .name("fake-copilot")
        .on_receive_request(
            async move |_request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx| {
                log_method("initialize");
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(AgentCapabilities::new().load_session(true))
                        .agent_info(Implementation::new(agent_name(name), version)),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        _cx| {
                log_method("session/new");
                remember(&on_new, &request.mcp_servers);
                responder.respond(
                    NewSessionResponse::new(SESSION_ID).config_options(config_options(&created)),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: LoadSessionRequest,
                        responder: Responder<LoadSessionResponse>,
                        cx: ConnectionTo<Client>| {
                log_method("session/load");
                remember(&on_load, &request.mcp_servers);
                load(&cx, script, &loaded, &request, responder)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: SetSessionConfigOptionRequest,
                        responder: Responder<SetSessionConfigOptionResponse>,
                        _cx| {
                log_method("session/set_config_option");
                responder.respond_with_result(set_config(&changed, &request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                log_method("session/prompt");
                let waits = Waits {
                    cancelled: Arc::clone(&cancelled),
                    interrupt: Arc::clone(&interrupt),
                    tools_url: Arc::clone(&on_prompt),
                };
                prompt(&cx, script, waits, request, responder)
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

/// Remembers the first HTTP server in `servers` in `url`, if there is one.
fn remember(url: &ToolsUrl, servers: &[McpServer]) {
    let http = servers.iter().find_map(|server| match server {
        McpServer::Http(http) => Some(http.url.clone()),
        _ => None, // coverage: Partitur's connection registers only an HTTP server
    });
    if let Ok(mut url) = url.lock() {
        *url = http;
    }
}

/// Answers `session/load`: an error for `load-fails`, else the history replay and the
/// session's `config`.
fn load(
    cx: &ConnectionTo<Client>,
    script: Script,
    config: &Shared,
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
    responder.respond(LoadSessionResponse::new().config_options(config_options(config)))
}

/// Answers `session/set_config_option`, changing `config`: for Copilot `mode` can only become
/// `#agent`, `allow_all` only `off`, and `model` only a model the agent offers; for opencode
/// every option can take any value it offers.
///
/// # Errors
///
/// Returns an invalid-params error for any other option or value.
fn set_config(
    config: &Shared,
    request: &SetSessionConfigOptionRequest,
) -> Result<SetSessionConfigOptionResponse> {
    let value = serde_json::to_value(&request.value).unwrap_or_default();
    let chosen = value
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut current = config.lock().unwrap_or_else(PoisonError::into_inner);
    if current.opencode {
        match &*request.config_id.0 {
            "model" if OPENCODE_MODELS.contains(&chosen) => chosen.clone_into(&mut current.model),
            "effort" if OPENCODE_EFFORTS.contains(&chosen) => {
                chosen.clone_into(&mut current.effort);
            }
            "mode" if OPENCODE_MODES.contains(&chosen) => chosen.clone_into(&mut current.mode),
            _ => {
                return Err(invalid_params(format!(
                    "no such option or value; refusing {} = {value}",
                    request.config_id
                )));
            }
        }
        return Ok(SetSessionConfigOptionResponse::new(config_options_of(
            &current,
        )));
    }
    match &*request.config_id.0 {
        "mode" if chosen == mode("agent") => chosen.clone_into(&mut current.mode),
        "allow_all" if chosen == "off" => chosen.clone_into(&mut current.allow_all),
        "model" if MODELS.contains(&chosen) => chosen.clone_into(&mut current.model),
        _ => {
            return Err(invalid_params(format!(
                "only `mode` `#agent`, `allow_all` `off` and an offered `model` can be set; \
                 refusing {} = {value}",
                request.config_id
            )));
        }
    }
    let options = config_options_of(&current);
    Ok(SetSessionConfigOptionResponse::new(options))
}

/// What a turn may wait for: a `session/cancel`, or a later prompt interrupting it; and the
/// tool server it may call.
struct Waits {
    cancelled: Arc<Notify>,
    interrupt: Interrupt,
    tools_url: ToolsUrl,
}

/// Runs one turn as the script says; the prompt is answered once the turn is done.
fn prompt(
    cx: &ConnectionTo<Client>,
    script: Script,
    waits: Waits,
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
        Script::SubagentAsks => cx.spawn(subagent_asks(cx.clone(), session, responder)),
        Script::Slow => cx.spawn(work_until_cancelled(
            cx.clone(),
            waits.cancelled,
            session,
            responder,
        )),
        Script::Interrupts => {
            let (interrupt, interrupted) = oneshot::channel();
            let earlier = waits
                .interrupt
                .lock()
                .ok()
                .and_then(|mut slot| slot.replace(interrupt));
            // An answered turn no longer listens, which is fine.
            let _ = earlier.map(|earlier| earlier.send(()));
            cx.spawn(work_until_interrupted(
                cx.clone(),
                interrupted,
                session,
                responder,
            ))
        }
        Script::Events | Script::Background | Script::Subagent => {
            cx.spawn(report_events(cx.clone(), script, session, responder))
        }
        Script::Lingers => cx.spawn(linger(cx.clone(), waits.cancelled, session, responder)),
        Script::Orchestrates => {
            let url = waits.tools_url.lock().ok().and_then(|url| url.clone());
            cx.spawn(orchestrate(
                cx.clone(),
                url.unwrap_or_default(),
                session,
                responder,
            ))
        }
        Script::OpencodeAsks => cx.spawn(opencode_asks(cx.clone(), session, responder)),
        Script::OpencodeRetries => {
            let retry = json!({ "opencode/retry": {
                "attempt": 1,
                "next": 1_764_064_800_000_u64,
                "error": { "message": UNREACHABLE },
            }});
            let cleared = json!({ "opencode/retry": null });
            let updates = [retry, cleared].map(|meta| {
                SessionUpdate::SessionInfoUpdate(
                    SessionInfoUpdate::new().meta(meta.as_object().cloned()),
                )
            });
            notify_all(cx, &session, updates)?;
            responder.respond_with_error(
                agent_client_protocol::Error::internal_error().data(UNREACHABLE),
            )
        }
        Script::Instructions => {
            let text = SessionUpdate::AgentMessageChunk(chunk(&instructions_report()));
            notify(cx, &session, text)?;
            responder.respond(PromptResponse::new(StopReason::EndTurn))
        }
        Script::Happy | Script::LoadFails | Script::Modeless | Script::RecordsArgs => {
            let turn = [
                SessionUpdate::AgentMessageChunk(chunk("Hello ")),
                SessionUpdate::AgentMessageChunk(chunk("world")),
                pending_call(),
                ended(TOOL_CALL, ToolCallStatus::Completed, "ok"),
            ];
            notify_all(cx, &session, turn)?;
            responder.respond(PromptResponse::new(StopReason::EndTurn))
        }
    }
}

/// The `instructions` turn's text: the instructions folders the agent was started with, and
/// whether each holds Partitur's instructions.
fn instructions_report() -> String {
    let dirs = std::env::var(INSTRUCTIONS_DIRS_VAR).unwrap_or_default();
    let folders = dirs.split(',').filter(|dir| !dir.is_empty()).map(|dir| {
        let found = std::path::Path::new(dir).join(INSTRUCTIONS_FILE).is_file();
        format!("{dir}: {}", if found { "exists" } else { "missing" })
    });
    std::iter::once(dirs.clone())
        .chain(folders)
        .collect::<Vec<_>>()
        .join("\n")
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
        PermissionOption::new(REJECT, "Reject", PermissionOptionKind::RejectOnce),
    ];
    let (status, output) = match ask(&cx, &session, call, options, ALLOW).await? {
        Ok(()) => (ToolCallStatus::Completed, "ok".to_owned()),
        Err(refused) => (ToolCallStatus::Failed, refused),
    };
    notify(&cx, &session, ended(TOOL_CALL, status, &output))?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// The `opencode-asks` turn: asks for permission for a `shell` call as opencode does, and ends
/// the turn cancelled unless it is allowed once.
async fn opencode_asks(
    cx: ConnectionTo<Client>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let call = ToolCall::new(TOOL_CALL, "shell")
        .name("shell".to_owned())
        .kind(ToolKind::Execute)
        .status(ToolCallStatus::Pending);
    notify(&cx, &session, SessionUpdate::ToolCall(call))?;
    let asked = ToolCallUpdate::new(
        TOOL_CALL,
        ToolCallUpdateFields::new()
            .name("shell".to_owned())
            .title("echo hi > a.txt".to_owned())
            .kind(ToolKind::Execute)
            .status(ToolCallStatus::Pending),
    );
    let options = vec![
        PermissionOption::new(ONCE, "Allow once", PermissionOptionKind::AllowOnce),
        PermissionOption::new(ALWAYS, "Always allow", PermissionOptionKind::AllowAlways),
        PermissionOption::new(OPENCODE_REJECT, "Reject", PermissionOptionKind::RejectOnce),
    ];
    let (status, output, reason) = match ask(&cx, &session, asked, options, ONCE).await? {
        Ok(()) => (
            ToolCallStatus::Completed,
            "hi".to_owned(),
            StopReason::EndTurn,
        ),
        Err(refused) => (ToolCallStatus::Failed, refused, StopReason::Cancelled),
    };
    notify(&cx, &session, ended(TOOL_CALL, status, &output))?;
    responder.respond(PromptResponse::new(reason))
}

/// The `subagent-asks` turn: a subagent's `edit` call, then a permission request about it that
/// names only its id.
async fn subagent_asks(
    cx: ConnectionTo<Client>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let tag = json!({ COPILOT: { "agentId": SUBAGENT } })
        .as_object()
        .cloned();
    let call = ToolCall::new(SUBAGENT_CALL, "Edit src/lib.rs")
        .kind(ToolKind::Edit)
        .status(ToolCallStatus::Pending)
        .meta(tag.clone());
    notify(&cx, &session, SessionUpdate::ToolCall(call))?;
    let asked = ToolCallUpdate::new(SUBAGENT_CALL, ToolCallUpdateFields::new());
    let options = vec![
        PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new(REJECT, "Reject", PermissionOptionKind::RejectOnce),
    ];
    let status = match ask(&cx, &session, asked, options, ALLOW).await? {
        Ok(()) => ToolCallStatus::Completed,
        Err(_) => ToolCallStatus::Failed,
    };
    let fields = ToolCallUpdateFields::new().status(status);
    let ended = ToolCallUpdate::new(SUBAGENT_CALL, fields).meta(tag);
    notify(&cx, &session, SessionUpdate::ToolCallUpdate(ended))?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// Asks permission for `call` with `options`: `Ok` when the client chose `allow`, else the
/// id of the option it chose, or `cancelled`.
async fn ask(
    cx: &ConnectionTo<Client>,
    session: &SessionId,
    call: ToolCallUpdate,
    options: Vec<PermissionOption>,
    allow: &str,
) -> Result<std::result::Result<(), String>> {
    let request = RequestPermissionRequest::new(session.clone(), call, options);
    let answer = cx.send_request(request).block_task().await?;
    Ok(match answer.outcome {
        RequestPermissionOutcome::Selected(selected) if &*selected.option_id.0 == allow => Ok(()),
        RequestPermissionOutcome::Selected(selected) => Err(selected.option_id.to_string()),
        _ => Err("cancelled".to_owned()),
    })
}

/// The `orchestrates` turn: asks permission for its call of `propose_children`, then calls
/// the tool on the server at `url` if allowed.
async fn orchestrate(
    cx: ConnectionTo<Client>,
    url: String,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let call = ToolCall::new(PROPOSAL_CALL, "partitur-propose_children")
        .kind(ToolKind::Other)
        .status(ToolCallStatus::Pending);
    notify(&cx, &session, SessionUpdate::ToolCall(call))?;
    let asked = ToolCallUpdate::new(
        PROPOSAL_CALL,
        ToolCallUpdateFields::new().title("propose_children".to_owned()),
    );
    let options = vec![
        PermissionOption::new(ALLOW, "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new(
            ALLOW_ALWAYS,
            "Always allow",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new(REJECT, "Reject", PermissionOptionKind::RejectOnce),
    ];
    let ending = match ask(&cx, &session, asked, options, ALLOW).await? {
        Ok(()) => {
            let text = McpClient::new(&url)
                .propose_children()
                .await
                .unwrap_or_else(|error| error);
            let answer = SessionUpdate::AgentMessageChunk(chunk(&text));
            vec![
                ended(PROPOSAL_CALL, ToolCallStatus::Completed, &text),
                answer,
            ]
        }
        Err(refused) => vec![ended(PROPOSAL_CALL, ToolCallStatus::Failed, &refused)],
    };
    notify_all(&cx, &session, ending)?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// The `lingers` turn: reports its events as `background` does, but its background work runs
/// until cancelled, or ends by itself after [`SLOW_TURN`].
async fn linger(
    cx: ConnectionTo<Client>,
    cancelled: Arc<Notify>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    session_event(&cx, &session, "assistant.turn_start", None)?;
    let done = SessionUpdate::AgentMessageChunk(chunk("done"));
    notify(&cx, &session, done)?;
    session_event(&cx, &session, "assistant.idle", None)?;
    let _ = tokio::time::timeout(SLOW_TURN, cancelled.notified()).await;
    session_event(&cx, &session, "session.idle", None)?;
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

/// The `interrupts` turn: works for [`INTERRUPTIBLE_WORK`], then reports Copilot's session
/// events as `events` does, unless a later prompt interrupts the work, which then ends
/// `cancelled` with no event.
async fn work_until_interrupted(
    cx: ConnectionTo<Client>,
    interrupted: oneshot::Receiver<()>,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    let work = tokio::time::timeout(INTERRUPTIBLE_WORK, interrupted).await;
    if matches!(work, Ok(Ok(()))) {
        let interrupted = SessionUpdate::AgentMessageChunk(chunk(INTERRUPTED));
        return notify(&cx, &session, interrupted)
            .and_then(|()| responder.respond(PromptResponse::new(StopReason::Cancelled)));
    }
    report_events(cx, Script::Interrupts, session, responder).await
}

/// The turn of the scripts reporting Copilot's session events.
async fn report_events(
    cx: ConnectionTo<Client>,
    script: Script,
    session: SessionId,
    responder: Responder<PromptResponse>,
) -> Result {
    session_event(&cx, &session, "assistant.turn_start", None)?;
    if script == Script::Subagent {
        let tag = serde_json::json!({ COPILOT: { "agentId": SUBAGENT } });
        let tagged = chunk("from the subagent").meta(tag.as_object().cloned());
        notify(&cx, &session, SessionUpdate::AgentMessageChunk(tagged))?;
        session_event(&cx, &session, "assistant.idle", Some(SUBAGENT))?;
        log_method(SUBAGENT_SENT);
    }
    let done = SessionUpdate::AgentMessageChunk(chunk("done"));
    notify(&cx, &session, done)?;
    session_event(&cx, &session, "assistant.idle", None)?;
    if script == Script::Background {
        tokio::time::sleep(BACKGROUND_WORK).await;
    }
    session_event(&cx, &session, "session.idle", None)?;
    responder.respond(PromptResponse::new(StopReason::EndTurn))
}

/// Sends Copilot's session event `kind` for `session`, from the subagent `agent` if given.
fn session_event(
    cx: &ConnectionTo<Client>,
    session: &SessionId,
    kind: &str,
    agent: Option<&str>,
) -> Result {
    let mut params = serde_json::json!({
        "sessionId": session,
        "type": kind,
        "data": { "aborted": false },
    });
    if let Some((params, agent)) = params.as_object_mut().zip(agent) {
        params.insert("agentId".to_owned(), agent.into());
    }
    cx.send_notification(UntypedMessage::new(SESSION_EVENT, params)?)
}

/// The pending tool call [`TOOL_CALL`].
fn pending_call() -> SessionUpdate {
    SessionUpdate::ToolCall(
        ToolCall::new(TOOL_CALL, tool_title())
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Pending),
    )
}

/// The update ending the tool call `id` with `status` and the text `output`.
fn ended(id: &str, status: ToolCallStatus, output: &str) -> SessionUpdate {
    let fields = ToolCallUpdateFields::new()
        .status(status)
        .content(vec![ToolCallContent::from(output.to_owned())]);
    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id.to_owned(), fields))
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

/// The mode `name` of Copilot's: a fragment of the session-modes URL.
fn mode(name: &str) -> String {
    format!("{MODES}#{name}")
}

/// The agent's config options with the current values `config` holds.
fn config_options(config: &Shared) -> Vec<SessionConfigOption> {
    config_options_of(&config.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The agent's config options with the current values of `config`.
fn config_options_of(config: &Config) -> Vec<SessionConfigOption> {
    if config.opencode {
        return opencode_options(config);
    }
    let options = [
        SessionConfigOption::select(
            "mode",
            "Mode",
            config.mode.clone(),
            vec![
                SessionConfigSelectOption::new(mode("agent"), "Agent"),
                SessionConfigSelectOption::new(mode("plan"), "Plan"),
                SessionConfigSelectOption::new(mode("autopilot"), "Autopilot"),
            ],
        ),
        SessionConfigOption::select(
            "allow_all",
            "Allow all",
            config.allow_all.clone(),
            vec![
                SessionConfigSelectOption::new("on", "On"),
                SessionConfigSelectOption::new("off", "Off"),
            ],
        ),
        SessionConfigOption::select(
            "model",
            "Model",
            config.model.clone(),
            MODELS
                .iter()
                .map(|model| SessionConfigSelectOption::new(*model, *model))
                .collect::<Vec<_>>(),
        ),
    ];
    let offered = if config.modes { 0 } else { 2 };
    options.into_iter().skip(offered).collect()
}

/// opencode's config options with the current values of `config`.
fn opencode_options(config: &Config) -> Vec<SessionConfigOption> {
    vec![
        select(
            ("model", "Model"),
            SessionConfigOptionCategory::Model,
            &config.model,
            &OPENCODE_MODELS,
        ),
        select(
            ("effort", "Effort"),
            SessionConfigOptionCategory::ThoughtLevel,
            &config.effort,
            &OPENCODE_EFFORTS,
        ),
        select(
            ("mode", "Session Mode"),
            SessionConfigOptionCategory::Mode,
            &config.mode,
            &OPENCODE_MODES,
        ),
    ]
}

/// The select option `(id, name)` in `category`, at `current`, offering `values`.
fn select(
    (id, name): (&str, &str),
    category: SessionConfigOptionCategory,
    current: &str,
    values: &[&'static str],
) -> SessionConfigOption {
    let choices = values
        .iter()
        .map(|value| SessionConfigSelectOption::new(*value, *value))
        .collect::<Vec<_>>();
    SessionConfigOption::select(id.to_owned(), name.to_owned(), current.to_owned(), choices)
        .category(category)
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
            ("events", Script::Events),
            ("background", Script::Background),
            ("subagent", Script::Subagent),
            ("subagent-asks", Script::SubagentAsks),
            ("interrupts", Script::Interrupts),
            ("lingers", Script::Lingers),
            ("orchestrates", Script::Orchestrates),
            ("modeless", Script::Modeless),
            ("instructions", Script::Instructions),
            ("records-args", Script::RecordsArgs),
            ("opencode-asks", Script::OpencodeAsks),
            ("opencode-retries", Script::OpencodeRetries),
        ];
        for (name, script) in scripts {
            assert_eq!(Script::parse(name), Some(script), "{name}");
        }
        assert_eq!(Script::parse("unknown"), None);
    }

    #[test]
    fn only_the_opencode_scripts_play_opencode() {
        assert!(Script::OpencodeAsks.is_opencode());
        assert!(Script::OpencodeRetries.is_opencode());
        assert!(!Script::Asks.is_opencode());
    }
}
