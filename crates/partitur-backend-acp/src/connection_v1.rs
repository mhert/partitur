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
//! What differs for GitHub Copilot CLI is its profile's, [`crate::copilot`]: the client takes
//! the agent's [`Quirks`] from its `initialize` answer and reads them, so a v1 agent that is
//! not Copilot gets none of what follows.
//!
//! An orchestrator must not edit files or run commands. Its permission requests for that are
//! denied, but Copilot may act without asking, so an orchestrator's Copilot is also started
//! without those tools ([`crate::copilot::needs_restart`]). Copilot is known only from its
//! `initialize` answer, so a client that finds an orchestrator on a Copilot whose arguments do
//! not exclude all of them opens no session: it ends the connection, which ends the process as
//! any session's end does, and leaves [`Ending::Restart`] with the tools to exclude, on which
//! [`crate::connection::run_session`] starts the process once more with the exclusion, which
//! the client then finds. Nothing is reported for the discarded process. Children and plain
//! sessions start once, unchanged. A Copilot whose `initialize` answer carries no `agentInfo`
//! is not recognised and so not restarted: an orchestrator on it keeps its editing and shell
//! tools, and only the denial of its permission requests protects it.
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
//! is translated by [`crate::copilot::event_state`] and forwarded as a v2 state update, so its
//! `assistant.idle` ends the turn and its `session.idle` the background work. The answer that
//! follows then changes nothing, but for a cancelled turn, which Copilot ends with no event.
//!
//! Copilot answers a prompt only once the session is idle, background work included, and a
//! prompt sent while an earlier one is unanswered cancels the earlier one. So on Copilot a
//! prompt is sent only while no earlier one is unanswered; until then it is held, the turn the
//! core opened for it open, and the held prompts are sent one at a time, in order, each once
//! the one before is answered. A cancel cancels the agent's turn and drops the held prompts,
//! which end as cancelled: their turn ends with the agent's, or at once when the agent's turn
//! already ended. Another v1 agent gets every prompt at once.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use agent_client_protocol::{Agent, Client, ConnectTo};
use partitur_core::TOOL_SERVER;
use tokio::sync::mpsc;

use crate::connection::{
    Ending, Gate, Open, Opening, SessionCommand, Shared, admit, ask_permission, asking_settings,
    config_change, forward, notice_if_uninstructed, notice_unless_asking, open_gate, opened,
    respond_permission, settle_refusal,
};
use crate::copilot::{
    Quirks, SessionEventNotification, event_state, initialize_meta, needs_restart,
};
use crate::map::map_prompt;
use crate::runtime::{Reporter, lock};
use crate::v1::{
    KnownCall, idle_after_refusal, idle_after_response, observe, running, translate_config,
    translate_permission, translate_prompt,
};

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
                    let quirks = *lock(&notifications.quirks);
                    let notification = observe(&mut lock(&reported_calls), &quirks, notification);
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
                    let state = event_state(&lock(&events.quirks), &event);
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
                let agent = initialized.agent_info.as_ref();
                let quirks = Quirks::for_agent(agent.map(|agent| agent.name.as_str()));
                *lock(&shared.quirks) = quirks;
                if let Some(tools) = needs_restart(opening.role, &quirks, &args) {
                    opening.end(Ending::Restart(tools));
                    return Ok(());
                }
                let commands = opening.commands()?;
                let ending = drive_v1(cx, &shared, &opening, commands, &quirks).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v1 `initialize`: the parameters the connector derives from Partitur's v2 one, so that
/// it reuses the connection the v2 one opened instead of starting the agent again. Like the v2
/// one, it carries the opt-in decided before the agent is known
/// ([`crate::copilot::initialize_meta`]).
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
                .meta(initialize_meta()),
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

/// The v1 connection's main task, once the agent with `quirks` answered `initialize`: opens
/// the session as `opening` says, with its tool server registered, keeps it asking, reports
/// it, then runs its commands.
pub(crate) async fn drive_v1(
    cx: v1::ConnectionTo<Agent>,
    shared: &Arc<Shared<V1Responder>>,
    opening: &Opening,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    quirks: &Quirks,
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
    let settings = asking_settings(&config, quirks);
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
        hold: quirks.hold_prompt_while_unanswered,
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
    /// Whether a prompt is held while an earlier one is unanswered.
    hold: bool,
}

impl Prompter {
    /// The session's id as the agent knows it.
    fn session(&self) -> one::SessionId {
        one::SessionId::new(self.id.0.to_string())
    }

    /// Sends `prompt` now, or, when the prompter holds prompts, holds it while an earlier
    /// prompt is unanswered. A held prompt's turn is open from now on, as the core opened it.
    fn prompt(&self, prompt: Vec<one::ContentBlock>) -> Result<(), agent_client_protocol::Error> {
        {
            let mut prompts = lock(&self.prompts);
            if std::mem::replace(&mut prompts.unanswered, true) && self.hold {
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
        assert_eq!(v2.capabilities.meta, Some(crate::copilot::opt_in()));
        assert_eq!(
            initialize_request().client_capabilities.meta,
            Some(crate::copilot::opt_in())
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
