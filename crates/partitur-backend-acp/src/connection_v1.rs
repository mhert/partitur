//! One session's connection to an agent that speaks ACP v1 only, such as GitHub Copilot CLI.
//!
//! The connector of [`crate::connection::run_session`] hands the connection to a [`V1Client`]
//! when the agent answers Partitur's v2 `initialize` with protocol version 1. The client sends
//! the v1 `initialize` with the same parameters, derived from the v2 one, so the connector
//! keeps the agent process it already started, then runs the session loop of
//! [`crate::protocol`] over v1: it creates the session with `session/new` or resumes it with
//! `session/load`, both with Partitur's MCP tool server registered and no `_meta`, reports it
//! to the core and executes the session's commands until the runtime drops them or the agent
//! goes away.
//!
//! Everything the agent reports is translated by [`crate::v1`] into the v2 values the v2 path
//! reads, and goes through the same gate, profile, session state and permission bookkeeping. A
//! resumed session's replay arrives before the `session/load` answer names the session, so it is
//! dropped as a v2 replay is. Before it is reported, the session is kept asking before it acts by
//! the settings [`crate::connection::asking_settings`] finds in its config, one at a time, the last
//! answer's config being the one reported; for Copilot, which may start a session in `#autopilot`
//! or with `allow_all` on, its `mode` becomes `#agent` and `allow_all` `off`. An agent offering
//! none of them is reported with the notice saying so. Widgets stay off: no v1 agent serves them.
//!
//! What differs for GitHub Copilot CLI is its profile's, [`crate::agent::copilot`]: the client
//! takes the agent's [`crate::agent::AgentProfile`] from its `initialize` answer and calls its
//! hooks, so a v1 agent that is not Copilot gets none of what follows.
//!
//! An orchestrator must not edit files or run commands. Its permission requests for that are
//! denied, but Copilot may act without asking, so an orchestrator's Copilot is also started
//! without those tools ([`crate::agent::restart_without`]). Copilot is known only from its
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
//! is translated by its profile ([`crate::agent::AgentProfile::session_event`]) and forwarded
//! as a v2 state update, so its `assistant.idle` ends the turn and its `session.idle` the
//! background work. The answer that
//! follows then changes nothing, but for a cancelled turn, which Copilot ends with no event.
//!
//! Copilot answers a prompt only once the session is idle, background work included, so the
//! session loop holds its prompts while one is unanswered, as its profile says; another v1
//! agent gets every prompt at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use agent_client_protocol::{Agent, Client, ConnectTo};

use crate::agent::copilot::SessionEventNotification;
use crate::agent::{for_agent, restart_without};
use crate::connection::{Ending, Open, Opening, Shared, ask_permission, forward, settle_refusal};
use crate::protocol::{OpenSession, Opened, Protocol, drive};
use crate::runtime::lock;
use crate::v1::{
    KnownCall, idle_after_refusal, idle_after_response, observe, running, translate_config,
    translate_initialize, translate_permission, translate_prompt, translate_servers,
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
                    let profile = *lock(&notifications.profile);
                    let notification = observe(&mut lock(&reported_calls), profile, notification);
                    if let Some(notification) = notification {
                        // A v1 agent serves no widgets, so nothing is followed up.
                        let (gate, reporter) = (&notifications.gate, &notifications.reporter);
                        forward(gate, reporter, profile, notification);
                    }
                    Ok(())
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
                let profile = for_agent(agent.map(|agent| agent.name.as_str()));
                tracing::debug!(profile = profile.label(), "agent profile");
                *lock(&shared.profile) = profile;
                if let Some(tools) = restart_without(profile, opening.role, &args) {
                    opening.end(Ending::Restart(tools));
                    return Ok(());
                }
                let commands = opening.commands()?;
                let v1 = V1 {
                    cx,
                    shared: Arc::clone(&shared),
                };
                let ending = drive(v1, &shared, &opening, commands, profile).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v1 `initialize`: the one the connector derives from Partitur's v2 one
/// ([`translate_initialize`]), so that it reuses the connection the v2 one opened instead of
/// starting the agent again. Like the v2 one, it carries the opt-in decided before the agent is
/// known ([`crate::agent::copilot::initialize_meta`]).
pub(crate) fn initialize_request() -> one::InitializeRequest {
    translate_initialize(&crate::connection::initialize_request())
}

/// The session's id as a v1 agent knows it.
fn session(id: &two::SessionId) -> one::SessionId {
    one::SessionId::new(id.0.to_string())
}

/// The v1 requests of an open session, over the connection `cx`, translated to and from the
/// v2 values the session loop reads.
#[derive(Clone)]
struct V1 {
    /// The connection to the agent.
    cx: v1::ConnectionTo<Agent>,
    /// The state its handlers and command loop share.
    shared: Arc<Shared<V1Responder>>,
}

impl Protocol for V1 {
    /// Creates the session with `session/new` or resumes it with `session/load`.
    async fn open(&self, request: OpenSession) -> Result<Opened, agent_client_protocol::Error> {
        let OpenSession {
            open,
            cwd,
            mcp_servers,
        } = request;
        let servers = translate_servers(&mcp_servers);
        let (id, config) = match open {
            Open::New => {
                let request = one::NewSessionRequest::new(cwd).mcp_servers(servers);
                let response = self.cx.send_request(request).block_task().await?;
                (response.session_id, response.config_options)
            }
            Open::Load(id) => {
                let id = one::SessionId::new(id.as_str().to_owned());
                let request = one::LoadSessionRequest::new(id.clone(), cwd).mcp_servers(servers);
                let response = self.cx.send_request(request).block_task().await?;
                (id, response.config_options)
            }
        };
        Ok(Opened {
            id: two::SessionId::new(id.0.to_string()),
            config: translate_config(config.unwrap_or_default()),
        })
    }

    /// Asks nothing: no v1 agent serves widgets.
    fn admitted(&self, _id: &two::SessionId) -> Result<(), agent_client_protocol::Error> {
        Ok(())
    }

    fn set_config(
        &self,
        id: &two::SessionId,
        option: &str,
        value: &str,
    ) -> impl Future<Output = Result<Vec<two::SessionConfigOption>, agent_client_protocol::Error>>
    + Send
    + 'static {
        let change = self
            .cx
            .send_request(one::SetSessionConfigOptionRequest::new(
                session(id),
                option.to_owned(),
                one::SessionConfigOptionValue::value_id(value.to_owned()),
            ));
        async move {
            change
                .block_task()
                .await
                .map(|response| translate_config(response.config_options))
        }
    }

    /// Opens the turn and sends `prompt`. Its answer ends the turn and is taken in before the
    /// agent's next message, so the end keeps its place among the session's updates.
    fn prompt(
        &self,
        id: &two::SessionId,
        prompt: Vec<two::ContentBlock>,
        answered: impl FnOnce() -> Result<(), agent_client_protocol::Error> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error> {
        let (gate, reporter) = (Arc::clone(&self.shared.gate), self.shared.reporter.clone());
        let profile = *lock(&self.shared.profile);
        forward(&gate, &reporter, profile, running(id.clone()));
        let id = id.clone();
        self.cx
            .send_request(one::PromptRequest::new(
                session(&id),
                translate_prompt(prompt),
            ))
            .on_receiving_result(async move |answer| {
                match answer {
                    Ok(answer) => {
                        forward(
                            &gate,
                            &reporter,
                            profile,
                            idle_after_response(id, answer.stop_reason),
                        );
                    }
                    Err(error) => {
                        settle_refusal(&gate, &reporter, &error);
                        forward(&gate, &reporter, profile, idle_after_refusal(id));
                    }
                }
                answered()
            })
    }

    fn cancel(&self, id: &two::SessionId) -> Result<(), agent_client_protocol::Error> {
        self.cx
            .send_notification(one::CancelNotification::new(session(id)))
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use agent_client_protocol::schema::ProtocolVersion;

    use super::*;

    #[test]
    fn every_v1_session_is_opened_with_the_tool_server() {
        let url = "http://127.0.0.1:4711/session".to_owned();
        let servers = crate::protocol::mcp_servers(url.clone());
        assert_eq!(
            translate_servers(&servers),
            [one::McpServer::Http(one::McpServerHttp::new(
                "partitur", url
            ))]
        );
    }

    #[test]
    fn a_servers_headers_are_translated_and_other_servers_dropped() {
        let header = two::HttpHeader::new("Authorization", "Bearer t");
        let http = two::McpServerHttp::new("web", "http://x").headers(vec![header]);
        let stdio = two::McpServerStdio::new("local", "/bin/server");
        let translated =
            translate_servers(&[two::McpServer::Http(http), two::McpServer::Stdio(stdio)]);
        let wire = serde_json::to_value(translated).unwrap();
        assert_eq!(
            wire,
            serde_json::json!([
                {
                    "type": "http",
                    "name": "web",
                    "url": "http://x",
                    "headers": [{"name": "Authorization", "value": "Bearer t"}],
                },
            ])
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
        assert_eq!(v2.capabilities.meta, Some(crate::agent::copilot::opt_in()));
        assert_eq!(
            initialize_request().client_capabilities.meta,
            Some(crate::agent::copilot::opt_in())
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
