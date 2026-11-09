//! Throwaway probe: drives the Claude ACP adapter over ACP v2 and prints every
//! `session/update` as JSON, so the backend maps what actually arrives.
//!
//! ```text
//! cargo run -p partitur-backend-acp --example probe -- [OPTIONS] "List the files here."
//! ```
//!
//! Options:
//! - `--wire`: forward the child's wire and stderr lines to stderr prefixed `WIRE `.
//! - `--resume <id>`: resume that session with `replayFrom: start` instead of
//!   opening a new one; without a prompt the probe only collects the replay.
//! - `--cancel-after <secs>`: send `session/cancel` that many seconds after the prompt.
//! - `--linger <secs>`: keep listening this long after the first idle (default 5).
//!
//! Environment:
//! - `PARTITUR_ACP_COMMAND`: adapter command line, split on whitespace
//!   (default: `npx --yes @agentclientprotocol/claude-agent-acp`).

use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::schema::{
    CancelSessionNotification, ContentBlock, Implementation, InitializeRequest, NewSessionRequest,
    PermissionOptionKind, PromptRequest, ReplayFrom, ReplayFromStart, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionId, SessionUpdate, SetSessionConfigOptionRequest,
    StateUpdate, TextContent, UpdateSessionNotification,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const DEFAULT_COMMAND: &str = "npx --yes @agentclientprotocol/claude-agent-acp";
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_LINGER_SECS: u64 = 5;

type BoxError = Box<dyn std::error::Error>;

#[derive(Default)]
struct Options {
    wire: bool,
    resume: Option<String>,
    cancel_after: Option<u64>,
    linger: u64,
    prompt: Option<String>,
}

fn parse_options() -> Result<Options, BoxError> {
    let mut options = Options {
        linger: DEFAULT_LINGER_SECS,
        ..Options::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--wire" => options.wire = true,
            "--resume" => options.resume = Some(args.next().ok_or("--resume needs an id")?),
            "--cancel-after" => {
                options.cancel_after =
                    Some(args.next().ok_or("--cancel-after needs secs")?.parse()?);
            }
            "--linger" => options.linger = args.next().ok_or("--linger needs secs")?.parse()?,
            _ => options.prompt = Some(arg),
        }
    }
    if options.prompt.is_none() && options.resume.is_none() {
        return Err("usage: probe [--wire] [--resume <id>] [--cancel-after <secs>] [--linger <secs>] [prompt]".into());
    }
    Ok(options)
}

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

/// Picks the first allow option, or cancels when there is none.
fn permission_outcome(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    request
        .options
        .iter()
        .find(|option| {
            matches!(
                option.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            )
        })
        .map_or(
            agent_client_protocol::v2::schema::CancelledPermissionOutcome::new().into(),
            |option| {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    option.option_id.clone(),
                ))
            },
        )
}

/// Prints a serialization result under `tag`; the JSON shows the exact wire field names.
fn print_json<T: serde::Serialize>(tag: &str, value: &T) {
    match serde_json::to_string(value) {
        Ok(json) => println!("{tag} {json}"),
        Err(error) => println!("{tag} <not serializable: {error}>"),
    }
}

/// Prints updates until the deadline logic in `options` says stop.
///
/// Waits for the first idle after activity, then lingers `options.linger` seconds
/// so that turns the agent starts on its own are observed too.
async fn listen(
    updates: &mut UnboundedReceiver<UpdateSessionNotification>,
    session_id: &SessionId,
    options: &Options,
    expect_activity: bool,
) {
    let mut observed_active = !expect_activity;
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let wait = deadline.map_or(Duration::from_secs(3600), |at| {
            at.saturating_duration_since(tokio::time::Instant::now())
        });
        let Ok(Some(notification)) = tokio::time::timeout(wait, updates.recv()).await else {
            return;
        };
        print_json("UPDATE", &notification);
        if &notification.session_id != session_id {
            continue;
        }
        if let SessionUpdate::StateUpdate(state) = &notification.update {
            if matches!(state, StateUpdate::Idle(_)) {
                if observed_active {
                    deadline =
                        Some(tokio::time::Instant::now() + Duration::from_secs(options.linger));
                }
            } else {
                observed_active = true;
                deadline = None;
            }
        }
    }
}

async fn run(
    cx: ConnectionTo<Agent>,
    options: &Options,
    cwd: PathBuf,
    mut updates: UnboundedReceiver<UpdateSessionNotification>,
) -> Result<(), agent_client_protocol::Error> {
    let init = cx
        .send_request(InitializeRequest::new(
            ProtocolVersion::V2,
            Implementation::new("partitur-probe", env!("CARGO_PKG_VERSION")),
        ))
        .block_task()
        .await?;
    print_json("INIT", &init);

    let session_id = if let Some(id) = &options.resume {
        let mut request = ResumeSessionRequest::new(id.clone(), cwd);
        request.replay_from = Some(ReplayFrom::Start(ReplayFromStart::new()));
        let resumed = cx.send_request(request).block_task().await?;
        print_json("RESUME", &resumed);
        SessionId::new(id.clone())
    } else {
        let session = cx
            .send_request(NewSessionRequest::new(cwd))
            .block_task()
            .await?;
        print_json("SESSION", &session);
        println!("SESSION_ID {}", session.session_id);
        session.session_id
    };

    let mode = cx
        .send_request(SetSessionConfigOptionRequest::new(
            session_id.clone(),
            "mode",
            "default",
        ))
        .block_task()
        .await?;
    print_json("MODE", &mode);

    let Some(prompt) = &options.prompt else {
        listen(&mut updates, &session_id, options, false).await;
        return Ok(());
    };
    let accepted = cx
        .send_request(PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt.clone()))],
        ))
        .block_task()
        .await;
    println!("PROMPT {accepted:?}");
    if let Some(secs) = options.cancel_after {
        let cx = cx.clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            println!("CANCEL sent");
            drop(cx.send_notification(CancelSessionNotification::new(session_id)));
        });
    }
    listen(&mut updates, &session_id, options, true).await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let options = parse_options()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let (update_tx, update_rx) = unbounded_channel();
    let connection = Client
        .v2()
        .name("partitur-probe")
        .on_receive_notification(
            async move |notification: UpdateSessionNotification, _cx| {
                update_tx
                    .send(notification)
                    .map_err(agent_client_protocol::Error::into_internal_error)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest,
                        responder: Responder<RequestPermissionResponse>,
                        _cx| {
                let outcome = permission_outcome(&request);
                print_json("PERMISSION request", &request);
                print_json("PERMISSION answer", &outcome);
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport(options.wire)?, {
            let options = &options;
            async move |cx: ConnectionTo<Agent>| run(cx, options, cwd, update_rx).await
        });
    tokio::time::timeout(CONNECTION_TIMEOUT, connection).await??;
    Ok(())
}
