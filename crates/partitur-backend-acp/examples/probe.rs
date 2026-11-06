//! Throwaway probe: drives the Claude ACP adapter once and prints every protocol
//! message it sends, so the backend maps what actually arrives.
//!
//! ```text
//! cargo run -p partitur-backend-acp --example probe -- "List the files here."
//! ```
//!
//! Environment:
//! - `PARTITUR_ACP_COMMAND`: adapter command line, split on whitespace
//!   (default: `npx --yes @agentclientprotocol/claude-agent-acp`).
//! - `PARTITUR_PROBE_WIRE=1`: forward the child's wire and stderr lines to stderr
//!   prefixed `WIRE `.

use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    ClientCapabilities, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SetSessionModeRequest, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client};

const DEFAULT_COMMAND: &str = "npx --yes @agentclientprotocol/claude-agent-acp";
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);

type BoxError = Box<dyn std::error::Error>;

/// Builds the transport: the adapter process, with the thinking summary enabled.
fn transport(wire: bool) -> Result<AcpAgent, BoxError> {
    let command = std::env::var("PARTITUR_ACP_COMMAND").unwrap_or_else(|_| DEFAULT_COMMAND.into());
    let mut words = command.split_whitespace();
    let program = words.next().ok_or("PARTITUR_ACP_COMMAND is empty")?;
    let config = AcpAgentConfig::new(program)
        .args(words)
        .env("CLAUDE_CODE_THINKING_DISPLAY", "summarized");
    let agent = AcpAgent::new(config);
    Ok(if wire {
        agent.with_debug(|line, direction| eprintln!("WIRE {direction:?} {line}"))
    } else {
        agent
    })
}

/// Picks the `AllowOnce` option, or cancels when there is none.
fn permission_outcome(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    request
        .options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::AllowOnce)
        .map_or(RequestPermissionOutcome::Cancelled, |option| {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
            ))
        })
}

/// Runs `main_fn` on a fresh adapter connection. Notifications are printed with
/// `update_tag`; permission requests are answered at once, inside the dispatch loop.
async fn with_agent<R>(
    update_tag: &'static str,
    wire: bool,
    main_fn: impl AsyncFnOnce(ConnectionTo<Agent>) -> Result<R, agent_client_protocol::Error>,
) -> Result<R, BoxError> {
    let connection = Client
        .v1()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                println!("{update_tag} {:?}", notification.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let outcome = permission_outcome(&request);
                println!("PERMISSION request={request:?} answer={outcome:?}");
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport(wire)?, main_fn);
    Ok(tokio::time::timeout(CONNECTION_TIMEOUT, connection).await??)
}

/// Prints a serialization result under `tag`; the JSON shows the exact wire field names.
fn print_json(tag: &str, json: Result<String, serde_json::Error>) {
    match json {
        Ok(json) => println!("{tag} {json}"),
        Err(error) => println!("{tag} <not serializable: {error}>"),
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let prompt = std::env::args().nth(1).ok_or("usage: probe <prompt>")?;
    let wire = std::env::var("PARTITUR_PROBE_WIRE").is_ok_and(|value| value == "1");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

    let session_id = with_agent("UPDATE", wire, {
        let cwd = cwd.clone();
        async move |cx: ConnectionTo<Agent>| {
            let init = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(ClientCapabilities::new()),
                )
                .block_task()
                .await?;
            println!("INIT capabilities={:?}", init.agent_capabilities);
            println!("INIT auth_methods={:?}", init.auth_methods);
            print_json("INIT json", serde_json::to_string(&init));

            let session = cx
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            println!("SESSION modes={:?}", session.modes);
            println!("SESSION config_options={:?}", session.config_options);
            print_json("SESSION json", serde_json::to_string(&session));

            let mode = cx
                .send_request(SetSessionModeRequest::new(
                    session.session_id.clone(),
                    "default",
                ))
                .block_task()
                .await?;
            println!("MODE {mode:?}");

            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ))
                .block_task()
                .await?;
            println!("STOP {:?}", response.stop_reason);
            Ok(session.session_id)
        }
    })
    .await?;

    with_agent("REPLAY", wire, async move |cx: ConnectionTo<Agent>| {
        cx.send_request(
            InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(ClientCapabilities::new()),
        )
        .block_task()
        .await?;
        let loaded = cx
            .send_request(LoadSessionRequest::new(SessionId::clone(&session_id), cwd))
            .block_task()
            .await?;
        println!("LOAD {loaded:?}");
        print_json("LOAD json", serde_json::to_string(&loaded));
        Ok(())
    })
    .await
}
