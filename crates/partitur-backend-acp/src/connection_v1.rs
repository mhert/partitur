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
//! dropped as a v2 replay is. The agent offers no `default` mode, so the session is reported
//! with the notice saying so. Widgets stay off: no v1 agent serves them.
//!
//! A v1 agent reports no running or idle state. A prompt sent to it opens the turn, and the
//! answer to the prompt, which arrives once the turn's work is done, ends it with the answer's
//! stop reason; a refused prompt ends it as the refusal says.

use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use agent_client_protocol::{Agent, Client, ConnectTo};
use partitur_core::{RequestId, TOOL_SERVER, Update};
use tokio::sync::mpsc;

use crate::connection::{
    Ending, NO_DEFAULT_MODE_NOTICE, Open, Opening, SessionCommand, Shared, admit, ask_permission,
    config_change, forward, open_gate, opened, respond_permission, settle_refusal,
};
use crate::map::map_prompt;
use crate::v1::{
    idle_after_refusal, idle_after_response, running, translate_config, translate_permission,
    translate_prompt, translate_update,
};

/// The parked responders of a v1 agent's permission requests.
pub(crate) type V1Responder = v1::Responder<one::RequestPermissionResponse>;

/// The client the connector runs for an agent that negotiated ACP v1.
pub(crate) struct V1Client {
    /// The state its handlers and command loop share.
    pub(crate) shared: Arc<Shared<V1Responder>>,
    /// What opening the session needs.
    pub(crate) opening: Arc<Opening>,
}

impl ConnectTo<Agent> for V1Client {
    async fn connect_to(
        self,
        agent: impl ConnectTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let Self { shared, opening } = self;
        let notifications = Arc::clone(&shared);
        let permission_requests = Arc::clone(&shared);
        Client
            .v1()
            .on_receive_notification(
                async move |notification: one::SessionNotification, _cx| {
                    if let Some(notification) = translate_update(notification) {
                        // A v1 agent serves no widgets, so nothing is followed up.
                        forward(&notifications.gate, &notifications.reporter, notification);
                    }
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: one::RequestPermissionRequest, responder: V1Responder, _cx| {
                    let shared = &permission_requests;
                    let request = translate_permission(request);
                    ask_permission(&shared.permissions, &shared.reporter, &request, responder);
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, async |cx: v1::ConnectionTo<Agent>| {
                cx.send_request(initialize_request()).block_task().await?;
                let commands = opening.commands()?;
                let ending = drive_v1(
                    cx,
                    &shared,
                    opening.request,
                    opening.open.clone(),
                    opening.cwd.clone(),
                    opening.tools_url.clone(),
                    commands,
                )
                .await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v1 `initialize`: the parameters the connector derives from Partitur's v2 one, so that
/// it reuses the connection the v2 one opened instead of starting the agent again.
fn initialize_request() -> one::InitializeRequest {
    let session = one::ClientSessionCapabilities::new().config_options(
        one::SessionConfigOptionsCapabilities::new()
            .boolean(one::BooleanConfigOptionCapabilities::new()),
    );
    one::InitializeRequest::new(ProtocolVersion::V1)
        .client_info(one::Implementation::new(
            "partitur",
            env!("CARGO_PKG_VERSION"),
        ))
        .client_capabilities(one::ClientCapabilities::new().session(session))
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

/// The v1 connection's main task, once the agent answered `initialize`: opens the session with
/// the tool server at `tools_url` registered, reports it with the notice that the agent offers
/// no `default` mode, then runs its commands.
pub(crate) async fn drive_v1(
    cx: v1::ConnectionTo<Agent>,
    shared: &Arc<Shared<V1Responder>>,
    request: RequestId,
    open: Open,
    cwd: PathBuf,
    tools_url: String,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
) -> Result<Ending, agent_client_protocol::Error> {
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
    let config = translate_config(config.unwrap_or_default());
    let event = opened(request, &shared.reporter, &id, open);
    open_gate(&shared.gate, &shared.reporter, config, event);
    shared
        .reporter
        .update(Update::Notice(NO_DEFAULT_MODE_NOTICE.to_owned()));
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = cx.incoming_closed() => return Ok(Ending::AgentExited),
        };
        let Some(command) = command else {
            return Ok(Ending::Closed);
        };
        execute_v1(&cx, shared, &id, command)?;
    }
}

/// Executes one command without waiting for the agent, so a cancel or a permission answer is
/// never stuck behind a running turn.
///
/// A prompt opens the turn at once, and its answer ends it: a v1 agent answers a prompt only
/// once the turn's work is done. The answer is taken in before the agent's next message, so
/// the turn's end keeps its place among the session's updates.
fn execute_v1(
    cx: &v1::ConnectionTo<Agent>,
    shared: &Shared<V1Responder>,
    id: &two::SessionId,
    command: SessionCommand,
) -> Result<(), agent_client_protocol::Error> {
    let session = one::SessionId::new(id.0.to_string());
    let reporter = shared.reporter.clone();
    match command {
        SessionCommand::Prompt(content) => {
            forward(&shared.gate, &reporter, running(id.clone()));
            let prompt = one::PromptRequest::new(session, translate_prompt(map_prompt(content)));
            let (gate, id) = (Arc::clone(&shared.gate), id.clone());
            cx.send_request(prompt)
                .on_receiving_result(async move |answer| {
                    match answer {
                        Ok(answer) => {
                            forward(
                                &gate,
                                &reporter,
                                idle_after_response(id, answer.stop_reason),
                            );
                        }
                        Err(error) => {
                            settle_refusal(&gate, &reporter, &error);
                            forward(&gate, &reporter, idle_after_refusal(id));
                        }
                    }
                    Ok(())
                })
        }
        SessionCommand::Cancel => cx.send_notification(one::CancelNotification::new(session)),
        SessionCommand::SetConfig { option, value } => {
            let change = cx.send_request(one::SetSessionConfigOptionRequest::new(
                session,
                option.as_str().to_owned(),
                one::SessionConfigOptionValue::value_id(value.as_str().to_owned()),
            ));
            cx.spawn(async move {
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
}
