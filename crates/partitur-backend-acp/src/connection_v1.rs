//! One session's connection to an agent that speaks ACP v1 only, such as GitHub Copilot CLI.
//!
//! The connector of [`crate::connection::run_session`] hands the connection to a [`V1Client`]
//! when the agent answers Partitur's v2 `initialize` with protocol version 1. The client sends
//! the v1 `initialize` with the same parameters, so the connector keeps the agent process it
//! already started, then creates the session with `session/new` or resumes it with
//! `session/load`, both with Partitur's MCP tool server registered, reports it to the core and
//! executes the session's commands until the runtime drops them or the agent goes away.
//!
//! Everything the agent reports is translated by [`crate::v1`] into the v2 values the v2 path
//! reads, and goes through the same gate, session state and permission bookkeeping. A resumed
//! session's replay arrives before the `session/load` answer names the session, so it is
//! dropped as a v2 replay is. Before it is reported, the session is kept asking before it acts
//! by the settings [`crate::connection::asking_settings`] finds in its config, one at a time,
//! the last answer's config being the one reported; for Copilot, which may start a session in
//! `#autopilot` or with `allow_all` on, its `mode` becomes `#agent` and `allow_all` `off`. An
//! agent offering none of them is reported with the notice saying so. Widgets stay off: no v1
//! agent serves them.
//!
//! An orchestrator must not edit files or run commands. Its permission requests for that are
//! denied, but Copilot may act without asking, so an orchestrator's Copilot is also started
//! without those tools, the [`EXCLUDED_FOR_ORCHESTRATOR`]: an `--excluded-tools=<list>` the
//! backend's arguments already hold gets the ones it lacks appended, and without one the
//! argument `--excluded-tools=<those tools>` is appended ([`with_exclusion`]). Copilot is known
//! only from its `initialize` answer, so a client that finds an orchestrator on a Copilot whose
//! arguments do not exclude all of them opens no session: it ends the connection, which ends
//! the process as any session's end does, and leaves [`Ending::Restart`], on which
//! [`crate::connection::run_session`] starts the process once more with the exclusion, which
//! the client then finds. Nothing is reported for the discarded process. Children and plain
//! sessions start once, unchanged.
//!
//! Copilot reports a call of an MCP tool with no name, titled `<server>-<tool>`, and asks
//! permission for it titled with the bare tool name, and asks about a subagent's call naming
//! only its id. So the client keeps what each tool call said about itself, by its id, a
//! subagent's call included although it is not shown, and a permission request takes the name,
//! title and kind it leaves out from there: a request about a call of Partitur's own tools is
//! flagged as such, and one about a subagent's edit asks with the kind `edit`.
//!
//! A v1 agent reports no running or idle state. A prompt sent to it opens the turn, and the
//! answer to the prompt, which arrives once the turn's work is done, ends it with the answer's
//! stop reason; a refused prompt ends it as the refusal says. GitHub Copilot CLI, which the
//! `initialize` opts into its session events, reports its state with them instead: each event
//! is translated by [`crate::v1::translate_event`] and forwarded as a v2 state update, so its
//! `assistant.idle` ends the turn and its `session.idle` the background work. The answer that
//! follows then changes nothing, but for a cancelled turn, which Copilot ends with no event.
//!
//! Copilot answers a prompt only once the session is idle, background work included, and a
//! prompt sent while an earlier one is unanswered cancels the earlier one. So a prompt is sent
//! only while no earlier one is unanswered; until then it is held, the turn the core opened for
//! it open, and the held prompts are sent one at a time, in order, each once the one before is
//! answered. A cancel cancels the agent's turn and drops the held prompts, which end as
//! cancelled: their turn ends with the agent's, or at once when the agent's turn already ended.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use agent_client_protocol::{Agent, Client, ConnectTo};
use partitur_core::{TOOL_SERVER, ToolRole};
use tokio::sync::mpsc;

use crate::connection::{
    Ending, Gate, Open, Opening, SessionCommand, Shared, admit, ask_permission, asking_settings,
    config_change, forward, notice_if_uninstructed, notice_unless_asking, open_gate, opened,
    respond_permission, settle_refusal,
};
use crate::map::map_prompt;
use crate::runtime::{Reporter, lock};
use crate::v1::{
    CopilotEvent, KnownCall, SessionEventNotification, copilot_opt_in, idle_after_refusal,
    idle_after_response, observe, running, translate_config, translate_event, translate_permission,
    translate_prompt,
};

/// Copilot's option, in its `=` form, that starts it without the tools its comma-separated
/// list names.
const EXCLUDED_TOOLS: &str = "--excluded-tools=";

/// The tools an orchestrator's GitHub Copilot CLI is started without: its editing tools
/// (`edit`, `create`, or `apply_patch` on Codex models) and its shell tools (`bash`, or
/// `powershell` on Windows).
pub(crate) const EXCLUDED_FOR_ORCHESTRATOR: [&str; 5] =
    ["edit", "create", "apply_patch", "bash", "powershell"];

/// The tools the `--excluded-tools=<list>` arguments among `args` name.
fn excluded(args: &[String]) -> impl Iterator<Item = &str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(EXCLUDED_TOOLS))
        .flat_map(|list| list.split(','))
}

/// `args` excluding [`EXCLUDED_FOR_ORCHESTRATOR`]: each `--excluded-tools=<list>` already
/// among them gets the tools its list lacks appended, and without one
/// `--excluded-tools=<those tools>` is appended. The space-separated form, whose list cannot
/// be told from the arguments after it, is left alone.
pub(crate) fn with_exclusion(args: &[String]) -> Vec<String> {
    let mut configured = false;
    let mut args: Vec<String> = args
        .iter()
        .map(|arg| match arg.strip_prefix(EXCLUDED_TOOLS) {
            Some(list) => {
                configured = true;
                let listed: Vec<&str> = list.split(',').filter(|tool| !tool.is_empty()).collect();
                let missing = EXCLUDED_FOR_ORCHESTRATOR
                    .iter()
                    .filter(|tool| !listed.contains(tool));
                let tools: Vec<&str> = listed.iter().copied().chain(missing.copied()).collect();
                format!("{EXCLUDED_TOOLS}{}", tools.join(","))
            }
            None => arg.clone(),
        })
        .collect();
    if !configured {
        args.push(format!(
            "{EXCLUDED_TOOLS}{}",
            EXCLUDED_FOR_ORCHESTRATOR.join(",")
        ));
    }
    args
}

/// The name GitHub Copilot CLI gives itself in its `initialize` answer.
const COPILOT: &str = "Copilot";

/// Whether a session in `role` on `agent`, started with `args`, must be started again
/// [`with_exclusion`]: an orchestrator on Copilot whose `--excluded-tools=<list>` arguments do
/// not name every tool of [`EXCLUDED_FOR_ORCHESTRATOR`].
pub(crate) fn needs_restart(role: ToolRole, agent: &one::Implementation, args: &[String]) -> bool {
    role == ToolRole::Orchestrator
        && agent.name == COPILOT
        && !EXCLUDED_FOR_ORCHESTRATOR
            .iter()
            .all(|tool| excluded(args).any(|listed| listed == *tool))
}

/// The parked responders of a v1 agent's permission requests.
pub(crate) type V1Responder = v1::Responder<one::RequestPermissionResponse>;

/// The client the connector runs for an agent that negotiated ACP v1.
pub(crate) struct V1Client {
    /// The state its handlers and command loop share.
    pub(crate) shared: Arc<Shared<V1Responder>>,
    /// What opening the session needs.
    pub(crate) opening: Arc<Opening>,
    /// The arguments the agent process was started with.
    pub(crate) args: Arc<[String]>,
}

impl ConnectTo<Agent> for V1Client {
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
        let calls: Arc<Mutex<HashMap<String, KnownCall>>> = Arc::default();
        let (reported_calls, asked_calls) = (Arc::clone(&calls), calls);
        Client
            .v1()
            .on_receive_notification(
                async move |notification: one::SessionNotification, _cx| {
                    let notification = observe(&mut lock(&reported_calls), notification);
                    if let Some(notification) = notification {
                        // A v1 agent serves no widgets, so nothing is followed up.
                        forward(&notifications.gate, &notifications.reporter, notification);
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_notification(
                async move |event: SessionEventNotification, _cx| {
                    let state =
                        CopilotEvent::parse(&event.0).and_then(|event| translate_event(&event));
                    if let Some(state) = state {
                        forward(&events.gate, &events.reporter, state);
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: one::RequestPermissionRequest, responder: V1Responder, _cx| {
                    let shared = &permission_requests;
                    let request = translate_permission(request, &lock(&asked_calls));
                    ask_permission(&shared.permissions, &shared.reporter, &request, responder);
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, async |cx: v1::ConnectionTo<Agent>| {
                let initialized = cx.send_request(initialize_request()).block_task().await?;
                let restart = initialized
                    .agent_info
                    .is_some_and(|agent| needs_restart(opening.role, &agent, &args));
                if restart {
                    opening.end(Ending::Restart);
                    return Ok(());
                }
                let commands = opening.commands()?;
                let ending = drive_v1(cx, &shared, &opening, commands).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v1 `initialize`: the parameters the connector derives from Partitur's v2 one, so that
/// it reuses the connection the v2 one opened instead of starting the agent again. Like the v2
/// one, it opts into Copilot's session events.
pub(crate) fn initialize_request() -> one::InitializeRequest {
    let session = one::ClientSessionCapabilities::new().config_options(
        one::SessionConfigOptionsCapabilities::new()
            .boolean(one::BooleanConfigOptionCapabilities::new()),
    );
    one::InitializeRequest::new(ProtocolVersion::V1)
        .client_info(one::Implementation::new(
            "partitur",
            env!("CARGO_PKG_VERSION"),
        ))
        .client_capabilities(
            one::ClientCapabilities::new()
                .session(session)
                .meta(copilot_opt_in()),
        )
}

/// The MCP servers a session is opened with: Partitur's tool server at `tools_url`.
fn mcp_servers(tools_url: String) -> Vec<one::McpServer> {
    vec![one::McpServer::Http(one::McpServerHttp::new(
        TOOL_SERVER,
        tools_url,
    ))]
}

/// The request creating a session in `cwd`, with the tool server at `tools_url`.
fn new_session_request(cwd: PathBuf, tools_url: String) -> one::NewSessionRequest {
    one::NewSessionRequest::new(cwd).mcp_servers(mcp_servers(tools_url))
}

/// The request resuming the session `id` in `cwd`, with the tool server at `tools_url`: an
/// agent knows a session's MCP servers only from the request that opens it.
fn load_session_request(
    id: one::SessionId,
    cwd: PathBuf,
    tools_url: String,
) -> one::LoadSessionRequest {
    one::LoadSessionRequest::new(id, cwd).mcp_servers(mcp_servers(tools_url))
}

/// The v1 connection's main task, once the agent answered `initialize`: opens the session as
/// `opening` says, with its tool server registered, keeps it asking, reports it, then runs its
/// commands.
pub(crate) async fn drive_v1(
    cx: v1::ConnectionTo<Agent>,
    shared: &Arc<Shared<V1Responder>>,
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
            let id = one::SessionId::new(id.as_str().to_owned());
            let response = cx
                .send_request(load_session_request(id.clone(), cwd, tools_url))
                .block_task()
                .await?;
            (id, response.config_options)
        }
    };
    let id = two::SessionId::new(id.0.to_string());
    admit(&shared.gate, id.clone());
    let mut config = translate_config(config.unwrap_or_default());
    let settings = asking_settings(&config);
    for (option, value) in &settings {
        let switch = one::SetSessionConfigOptionRequest::new(
            one::SessionId::new(id.0.to_string()),
            *option,
            one::SessionConfigOptionValue::value_id(*value),
        );
        let answer = cx.send_request(switch).block_task().await?;
        config = translate_config(answer.config_options);
    }
    let event = opened(request, &shared.reporter, &id, open);
    open_gate(&shared.gate, &shared.reporter, config, event);
    notice_unless_asking(&shared.reporter, &settings);
    notice_if_uninstructed(&shared.reporter, opening.uninstructed);
    let prompter = Prompter {
        cx: cx.clone(),
        gate: Arc::clone(&shared.gate),
        reporter: shared.reporter.clone(),
        id,
        prompts: Arc::default(),
    };
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = cx.incoming_closed() => return Ok(Ending::AgentExited),
        };
        let Some(command) = command else {
            return Ok(Ending::Closed);
        };
        execute_v1(&prompter, shared, command)?;
    }
}

/// The prompts of a session: whether one is unanswered, and the ones held behind it.
#[derive(Debug, Default)]
struct Prompts {
    /// Whether the agent has a prompt it has not answered yet.
    unanswered: bool,
    /// The prompts waiting for it, oldest first.
    held: VecDeque<Vec<one::ContentBlock>>,
}

/// Sends a session's prompts one at a time, holding each until the one before is answered.
#[derive(Clone)]
struct Prompter {
    /// The connection to the agent.
    cx: v1::ConnectionTo<Agent>,
    /// The session's gate, which reports the turns.
    gate: Arc<Mutex<Gate>>,
    /// Reports to the core.
    reporter: Reporter,
    /// The session's id.
    id: two::SessionId,
    /// The unanswered and the held prompts.
    prompts: Arc<Mutex<Prompts>>,
}

impl Prompter {
    /// The session's id as the agent knows it.
    fn session(&self) -> one::SessionId {
        one::SessionId::new(self.id.0.to_string())
    }

    /// Sends `prompt` now, or holds it while an earlier prompt is unanswered. A held prompt's
    /// turn is open from now on, as the core opened it.
    fn prompt(&self, prompt: Vec<one::ContentBlock>) -> Result<(), agent_client_protocol::Error> {
        {
            let mut prompts = lock(&self.prompts);
            if std::mem::replace(&mut prompts.unanswered, true) {
                prompts.held.push_back(prompt);
                drop(prompts);
                lock(&self.gate).prompted();
                return Ok(());
            }
        }
        self.send(prompt)
    }

    /// Opens the turn and sends `prompt`. Its answer ends the turn and is taken in before the
    /// agent's next message, so the end keeps its place among the session's updates; then the
    /// next held prompt goes out.
    fn send(&self, prompt: Vec<one::ContentBlock>) -> Result<(), agent_client_protocol::Error> {
        forward(&self.gate, &self.reporter, running(self.id.clone()));
        let prompter = self.clone();
        self.cx
            .send_request(one::PromptRequest::new(self.session(), prompt))
            .on_receiving_result(async move |answer| {
                let (gate, reporter, id) = (&prompter.gate, &prompter.reporter, &prompter.id);
                match answer {
                    Ok(answer) => {
                        forward(
                            gate,
                            reporter,
                            idle_after_response(id.clone(), answer.stop_reason),
                        );
                    }
                    Err(error) => {
                        settle_refusal(gate, reporter, &error);
                        forward(gate, reporter, idle_after_refusal(id.clone()));
                    }
                }
                prompter.answered()
            })
    }

    /// Sends the next held prompt, if one waits, now that the unanswered one was answered.
    ///
    /// This runs on the connection's dispatch task, beside the command loop. The prompt is sent
    /// with the prompts still locked, so a cancel either drops it first, or locks them only once
    /// it went out and then reaches the agent after it, cancelling it: a cancel never goes out
    /// between taking the prompt and sending it, which would leave it running.
    fn answered(&self) -> Result<(), agent_client_protocol::Error> {
        let mut prompts = lock(&self.prompts);
        let next = prompts.held.pop_front();
        prompts.unanswered = next.is_some();
        let sent = next.map_or(Ok(()), |prompt| self.send(prompt));
        drop(prompts);
        sent
    }

    /// Drops the held prompts, each ending as cancelled, and cancels the agent's turn.
    fn cancel(&self) -> Result<(), agent_client_protocol::Error> {
        let held = std::mem::take(&mut lock(&self.prompts).held);
        let cancelled = agent_client_protocol::Error::request_cancelled();
        for _ in held {
            settle_refusal(&self.gate, &self.reporter, &cancelled);
        }
        self.cx
            .send_notification(one::CancelNotification::new(self.session()))
    }
}

/// Executes one command without waiting for the agent, so a cancel or a permission answer is
/// never stuck behind a running turn.
fn execute_v1(
    prompter: &Prompter,
    shared: &Shared<V1Responder>,
    command: SessionCommand,
) -> Result<(), agent_client_protocol::Error> {
    match command {
        SessionCommand::Prompt(content) => prompter.prompt(translate_prompt(map_prompt(content))),
        SessionCommand::Cancel => prompter.cancel(),
        SessionCommand::SetConfig { option, value } => {
            let change = prompter
                .cx
                .send_request(one::SetSessionConfigOptionRequest::new(
                    prompter.session(),
                    option.as_str().to_owned(),
                    one::SessionConfigOptionValue::value_id(value.as_str().to_owned()),
                ));
            let reporter = shared.reporter.clone();
            prompter.cx.spawn(async move {
                let options = change
                    .block_task()
                    .await
                    .map(|response| translate_config(response.config_options));
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|&arg| arg.to_owned()).collect()
    }

    #[test]
    fn only_a_copilot_orchestrator_without_the_exclusion_is_restarted() {
        let copilot = one::Implementation::new("Copilot", "1.0.114");
        let plain = args(&["--acp"]);
        assert!(needs_restart(ToolRole::Orchestrator, &copilot, &plain));
        let excluded = with_exclusion(&plain);
        assert!(!needs_restart(ToolRole::Orchestrator, &copilot, &excluded));
        let claude = one::Implementation::new("claude-agent-acp", "1.0.0");
        assert!(!needs_restart(ToolRole::Orchestrator, &claude, &plain));
        assert!(!needs_restart(ToolRole::Child, &copilot, &plain));
        assert!(!needs_restart(ToolRole::Plain, &copilot, &plain));
    }

    #[test]
    fn a_partial_exclusion_restarts_and_a_full_one_does_not() {
        let copilot = one::Implementation::new("Copilot", "1.0.114");
        let restarts = |list: &[&str]| needs_restart(ToolRole::Orchestrator, &copilot, &args(list));
        assert!(restarts(&["--acp", "--excluded-tools=edit,create"]));
        // The space-separated form is not read.
        assert!(restarts(&[
            "--excluded-tools",
            "edit,create,apply_patch,bash,powershell"
        ]));
        // Every tool excluded, over two lists and among others, needs no restart.
        assert!(!restarts(&[
            "--excluded-tools=web_fetch,edit,create",
            "--excluded-tools=apply_patch,bash,powershell",
        ]));
    }

    #[test]
    fn the_exclusion_names_copilots_editing_and_shell_tools() {
        assert_eq!(
            with_exclusion(&args(&["--acp"])),
            args(&[
                "--acp",
                "--excluded-tools=edit,create,apply_patch,bash,powershell"
            ])
        );
    }

    #[test]
    fn a_configured_exclusion_list_gets_the_missing_tools() {
        assert_eq!(
            with_exclusion(&args(&["--acp", "--excluded-tools=web_fetch,bash"])),
            args(&[
                "--acp",
                "--excluded-tools=web_fetch,bash,edit,create,apply_patch,powershell"
            ])
        );
        // An empty list gets every tool, and every list gets them.
        assert_eq!(
            with_exclusion(&args(&["--excluded-tools=", "--excluded-tools=edit"])),
            args(&[
                "--excluded-tools=edit,create,apply_patch,bash,powershell",
                "--excluded-tools=edit,create,apply_patch,bash,powershell",
            ])
        );
        // The space-separated form is left alone, and the list added after it.
        assert_eq!(
            with_exclusion(&args(&["--excluded-tools", "web_fetch"])),
            args(&[
                "--excluded-tools",
                "web_fetch",
                "--excluded-tools=edit,create,apply_patch,bash,powershell"
            ])
        );
    }

    #[test]
    fn every_v1_session_is_opened_with_the_tool_server() {
        let cwd = PathBuf::from("/work");
        let url = "http://127.0.0.1:4711/session".to_owned();
        let tool_server = [one::McpServer::Http(one::McpServerHttp::new(
            "partitur",
            url.clone(),
        ))];
        let new = new_session_request(cwd.clone(), url.clone());
        assert_eq!(
            (new.cwd, new.mcp_servers),
            (cwd.clone(), tool_server.to_vec())
        );
        let load = load_session_request(one::SessionId::new("cp-1"), cwd.clone(), url);
        assert_eq!(
            (load.session_id, load.cwd, load.mcp_servers),
            (one::SessionId::new("cp-1"), cwd, tool_server.to_vec())
        );
    }

    #[test]
    fn the_v1_initialize_matches_what_the_connector_derives_from_the_v2_one() {
        let wire = serde_json::to_value(initialize_request()).unwrap();
        assert_eq!(wire["protocolVersion"], serde_json::json!(1));
        assert_eq!(
            wire["clientInfo"],
            serde_json::json!({"name": "partitur", "version": env!("CARGO_PKG_VERSION")})
        );
        assert_eq!(
            wire["clientCapabilities"]["session"],
            serde_json::json!({"configOptions": {"boolean": {}}})
        );
    }

    #[test]
    fn the_opt_in_is_sent_on_every_initialize() {
        let mut v2 = crate::connection::initialize_request();
        assert_eq!(v2.capabilities.meta, Some(copilot_opt_in()));
        assert_eq!(
            initialize_request().client_capabilities.meta,
            Some(copilot_opt_in())
        );
        // Apart from the opt-in and the version, the v2 initialize is the default one.
        v2.capabilities = two::ClientCapabilities::new();
        assert_eq!(
            v2,
            two::InitializeRequest::new(
                ProtocolVersion::V2,
                two::Implementation::new("partitur", env!("CARGO_PKG_VERSION")),
            )
        );
    }
}
