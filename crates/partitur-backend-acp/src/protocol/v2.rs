//! The ACP v2 client of one session's connection, and the connector that starts it.
//!
//! [`run_session`] spawns the adapter and initializes it with ACP v2. An agent that answers
//! with protocol version 1 is handed to the v1 client of [`crate::protocol::v1`] on the same
//! process, which translates its traffic into the v2 values the shared code of
//! [`crate::connection`] reads. Either client then runs the one session loop of
//! [`crate::protocol`] over its protocol version. When either ends the connection with
//! [`Ending::Restart`], the adapter is started once more with the arguments it carries, and the
//! session is opened on that process.
//!
//! The v2 client takes the agent's profile from its `initialize` answer ([`for_agent`]) and
//! calls its hooks, so what differs for one agent is its profile's. It creates or resumes the
//! session with Partitur's MCP tool server registered (unless the profile says otherwise) and
//! the `_meta` the profile opens
//! sessions with ([`crate::agent::AgentProfile::session_meta`]), the history of a resumed
//! session replayed from the start. Updates the profile drops ([`dropped`]) never reach the
//! gate, and every notification of a method v2 does not define is forwarded as the v2 update its
//! profile reads in it, if any ([`crate::agent::AgentProfile::vendor_notification`]).
//!
//! A turn ends when the agent reports its foreground work idle, not when the prompt is
//! answered: the agent takes a prompt in at once, and it may also start work of its own, for
//! example once a background command finishes, so the answer to a prompt reports nothing but
//! a refusal. A cancel cancels the turn and every subagent the agent lists as running. An
//! adapter that says so in its `initialize` answer serves widgets, which the session then
//! lists once it is admitted.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v2::schema::{
    self, CancelSessionNotification, CancelledPermissionOutcome, ContentBlock, Implementation,
    InitializeRequest, InitializeResponse, McpServer, NewSessionRequest, PromptRequest, ReplayFrom,
    ReplayFromStart, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigOption, SessionConfigOptionValue,
    SetSessionConfigOptionRequest, UpdateSessionNotification,
};
use agent_client_protocol::v2::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, Client, ConnectTo, UntypedMessage};
use partitur_core::PermissionOptionId;
use tokio::sync::mpsc;

use crate::agent::{dropped, for_agent, initialize_meta, restart_args};
use crate::connection::{
    Ending, Gate, Open, Opening, SessionCommand, SessionStart, Shared, ask_permission,
    ending_event, follow, forward, forward_vendor, list_widget_tools, settle_refusal, transport,
};
use crate::permissions::{Answer, PermissionIds};
use crate::protocol::v1::V1Client;
use crate::protocol::{OpenSession, Opened, Protocol, drive};
use crate::runtime::{AcpConfig, Reporter, lock};

impl Answer for Responder<RequestPermissionResponse> {
    fn answer(
        self,
        option: Option<PermissionOptionId>,
    ) -> Result<(), agent_client_protocol::Error> {
        let outcome = match option {
            Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.as_str().to_owned(),
            )),
            None => RequestPermissionOutcome::Cancelled(CancelledPermissionOutcome::new()),
        };
        self.respond(RequestPermissionResponse::new(outcome))
    }
}

/// Whether the adapter that answered `initialize` with `response` serves widgets: it says so
/// in `_meta.partitur.widgets`.
fn serves_widgets(response: &InitializeResponse) -> bool {
    let widgets = response.meta.as_ref().and_then(|meta| {
        let widgets = meta.get("partitur")?.get("widgets")?;
        Some(widgets)
    });
    widgets.is_some_and(|widgets| !widgets.is_null())
}

/// The request creating a session in `cwd`, with `servers` and `meta`, the profile's
/// [`crate::agent::AgentProfile::session_meta`].
fn new_session_request(
    cwd: PathBuf,
    servers: Vec<McpServer>,
    meta: Option<schema::Meta>,
) -> NewSessionRequest {
    NewSessionRequest::new(cwd).mcp_servers(servers).meta(meta)
}

/// The request resuming the session `id` in `cwd` with its history replayed from the start,
/// with `servers`: an agent knows a session's MCP servers only from the request that opens it.
/// It carries `meta`, the profile's [`crate::agent::AgentProfile::session_meta`].
fn resume_session_request(
    id: schema::SessionId,
    cwd: PathBuf,
    servers: Vec<McpServer>,
    meta: Option<schema::Meta>,
) -> ResumeSessionRequest {
    ResumeSessionRequest::new(id, cwd)
        .mcp_servers(servers)
        .meta(meta)
        .replay_from(ReplayFrom::Start(ReplayFromStart::new()))
}

/// The client the connector runs for an agent that speaks ACP v2.
struct V2Client {
    /// The state its handlers and command loop share.
    shared: Arc<Shared>,
    /// What opening the session needs.
    opening: Arc<Opening>,
    /// The arguments the agent process was started with.
    args: Arc<[String]>,
}

impl ConnectTo<Agent> for V2Client {
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
        let vendor = Arc::clone(&shared);
        Client
            .v2()
            .on_receive_notification(
                async move |notification: UpdateSessionNotification, cx: ConnectionTo<Agent>| {
                    let shared = &notifications;
                    let profile = *lock(&shared.profile);
                    if dropped(profile, &notification.update) {
                        return Ok(());
                    }
                    let id = notification.session_id.clone();
                    let follow_up = forward(&shared.gate, &shared.reporter, profile, notification);
                    follow(&cx, shared, &id, follow_up)
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_notification(
                async move |message: UntypedMessage, _cx| {
                    forward_vendor(&vendor, &message);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _cx| {
                    let shared = &permission_requests;
                    ask_permission(&shared.permissions, &shared.reporter, &request, responder);
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, async |cx: ConnectionTo<Agent>| {
                let initialized = cx.send_request(initialize_request()).block_task().await?;
                let profile = for_agent(Some(initialized.info.name.as_str()));
                tracing::debug!(profile = profile.label(), "agent profile");
                *lock(&shared.profile) = profile;
                if let Some(args) = restart_args(profile, opening.role, &args) {
                    opening.end(Ending::Restart(args));
                    return Ok(());
                }
                lock(&shared.gate).widgets = serves_widgets(&initialized);
                let commands = opening.commands()?;
                let v2 = V2 {
                    cx,
                    shared: Arc::clone(&shared),
                };
                let ending = drive(v2, &shared, &opening, commands, profile).await?;
                opening.end(ending);
                Ok(())
            })
            .await
    }
}

/// The v2 `initialize`. It carries the `_meta` keys decided before the agent is known
/// ([`initialize_meta`]): the connector reuses the connection for the v1 client only when the
/// v1 `initialize` is this one, and that one carries them too. Each agent is expected to
/// ignore the keys of the other profiles, as it is the other profiles' process environment,
/// which every agent's process and the commands it runs inherit (opencode's
/// `OPENCODE_CONFIG_CONTENT` with that session's tools URL among it).
pub(crate) fn initialize_request() -> InitializeRequest {
    InitializeRequest::new(
        ProtocolVersion::V2,
        Implementation::new("partitur", env!("CARGO_PKG_VERSION")),
    )
    .capabilities(schema::ClientCapabilities::new().meta(initialize_meta()))
}

/// Runs one session from spawning its adapter until its connection ends, then reports how
/// it ended and forgets the session, all through `reporter`, the session's current
/// connection when the runtime opened this one. Its permission requests take their ids from
/// `permission_ids`. The session is opened with the tool server at `tools_url`, and when
/// `uninstructed` the user is told its instructions folder could not be written.
///
/// The connection starts with ACP v2; an agent that answers with v1 continues on the same
/// process with the v1 client. When either client ends the connection with
/// [`Ending::Restart`], the adapter is started once more with the arguments it carries, and the
/// session is opened on that process.
pub(crate) async fn run_session(
    mut config: Arc<AcpConfig>,
    start: SessionStart,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    reporter: Reporter,
    permission_ids: PermissionIds,
    tools_url: String,
    uninstructed: bool,
) {
    let SessionStart {
        request,
        open,
        cwd,
        role,
        ..
    } = start;
    let gate: Arc<Mutex<Gate>> = Arc::default();
    let v2 = Arc::new(Shared::new(
        reporter.clone(),
        Arc::clone(&gate),
        Arc::clone(&permission_ids),
    ));
    let v1 = Arc::new(Shared::new(reporter.clone(), gate, permission_ids));
    let opening = Arc::new(Opening::new(
        request,
        open,
        cwd,
        tools_url,
        role,
        uninstructed,
        commands,
    ));
    let session = reporter.session();
    let result = loop {
        let (v2, v1) = (Arc::clone(&v2), Arc::clone(&v1));
        let (v2_opening, v1_opening) = (Arc::clone(&opening), Arc::clone(&opening));
        let args: Arc<[String]> = config.args.clone().into();
        let v2_args = Arc::clone(&args);
        let spawned = Arc::clone(&config);
        let tools_url = opening.tools_url.clone();
        let connected = Client
            .protocol_connector()
            .with_v2(move || V2Client {
                shared: Arc::clone(&v2),
                opening: Arc::clone(&v2_opening),
                args: Arc::clone(&v2_args),
            })
            .with_v1(move || V1Client {
                shared: Arc::clone(&v1),
                opening: Arc::clone(&v1_opening),
                args: Arc::clone(&args),
            })
            .connect_to(move || transport(&spawned, role, &tools_url))
            .await;
        // The discarded process's exit, however it went, is no ending of the session.
        let ending = opening.ending();
        let Ending::Restart(args) = ending else {
            break connected.map(|()| ending);
        };
        tracing::info!(%session, "restarting the orchestrator's agent without editing tools");
        config = Arc::new(AcpConfig {
            args,
            ..AcpConfig::clone(&config)
        });
    };
    tracing::info!(%session, ?result, "ACP session ended");
    reporter.retire(|live| ending_event(request, session.clone(), live, result));
}

/// The v2 requests of an open session, over the connection `cx`.
#[derive(Clone)]
struct V2 {
    /// The connection to the adapter.
    cx: ConnectionTo<Agent>,
    /// The state its handlers and command loop share.
    shared: Arc<Shared>,
}

impl Protocol for V2 {
    async fn open(&self, request: OpenSession) -> Result<Opened, agent_client_protocol::Error> {
        let OpenSession {
            open,
            cwd,
            mcp_servers,
        } = request;
        Ok(match open {
            Open::New => {
                let meta = lock(&self.shared.profile).session_meta();
                let request = new_session_request(cwd, mcp_servers, meta);
                let response = self.cx.send_request(request).block_task().await?;
                Opened {
                    id: response.session_id,
                    config: response.config_options,
                }
            }
            Open::Load(id) => {
                let id = schema::SessionId::new(id.as_str().to_owned());
                let meta = lock(&self.shared.profile).session_meta();
                let request = resume_session_request(id.clone(), cwd, mcp_servers, meta);
                let response = self.cx.send_request(request).block_task().await?;
                Opened {
                    id,
                    config: response.config_options,
                }
            }
        })
    }

    /// Asks an adapter that serves widgets for the tools that declare one.
    fn admitted(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error> {
        if lock(&self.shared.gate).widgets {
            list_widget_tools(&self.cx, &self.shared, id)?;
        }
        Ok(())
    }

    fn set_config(
        &self,
        id: &schema::SessionId,
        option: &str,
        value: &str,
    ) -> impl Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
    + Send
    + 'static {
        let change = self.cx.send_request(SetSessionConfigOptionRequest::new(
            id.clone(),
            option.to_owned(),
            SessionConfigOptionValue::id(value.to_owned()),
        ));
        async move {
            change
                .block_task()
                .await
                .map(|response| response.config_options)
        }
    }

    /// The adapter takes a prompt in at once and reports the turn's end in the session's
    /// state, so its answer reports nothing but a refusal. A prompt sent while the agent
    /// already works on a turn of its own, one whose start had not reached the core yet, is
    /// held by the agent behind that work; the `idle` ending that work then ends the turn the
    /// core opened for the prompt, and the prompt's own work reads as a turn the agent started.
    fn prompt(
        &self,
        id: &schema::SessionId,
        prompt: Vec<ContentBlock>,
        answered: impl FnOnce() -> Result<(), agent_client_protocol::Error> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error> {
        lock(&self.shared.gate).prompted();
        let turn = self.cx.send_request(PromptRequest::new(id.clone(), prompt));
        let (gate, reporter) = (Arc::clone(&self.shared.gate), self.shared.reporter.clone());
        self.cx.spawn(async move {
            if let Err(error) = turn.block_task().await {
                settle_refusal(&gate, &reporter, &error);
            }
            answered()
        })
    }

    /// Cancels the session and every subagent the agent lists as running.
    fn cancel(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error> {
        let subagents: Vec<schema::SessionId> =
            lock(&self.shared.gate).state.subagents().cloned().collect();
        std::iter::once(id.clone())
            .chain(subagents)
            .try_for_each(|session| {
                self.cx
                    .send_notification(CancelSessionNotification::new(session))
            })
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

    use super::*;
    use crate::agent::AgentProfile;
    use crate::agent::claude::Claude;

    #[test]
    fn only_an_adapter_saying_so_serves_widgets() {
        let response = |meta: Option<serde_json::Value>| {
            InitializeResponse::new(ProtocolVersion::V2, Implementation::new("a", "1"))
                .meta(meta.and_then(|meta| meta.as_object().cloned()))
        };
        let widgets = |value| response(Some(serde_json::json!({"partitur": {"widgets": value}})));
        assert!(serves_widgets(&widgets(serde_json::json!(1))));
        assert!(!serves_widgets(&widgets(serde_json::Value::Null)));
        assert!(!serves_widgets(&response(Some(
            serde_json::json!({"partitur": {}})
        ))));
        assert!(!serves_widgets(&response(Some(serde_json::json!({})))));
        assert!(!serves_widgets(&response(None)));
    }

    #[test]
    fn both_opening_requests_carry_the_servers_and_resume_replays() {
        let cwd = PathBuf::from("/work");
        let servers = crate::protocol::mcp_servers("http://127.0.0.1:4711/session".to_owned());
        let id = schema::SessionId::new("b-1");

        assert_eq!(
            new_session_request(cwd.clone(), servers.clone(), None).mcp_servers,
            servers
        );
        let resume = resume_session_request(id.clone(), cwd.clone(), servers.clone(), None);
        assert_eq!(resume.mcp_servers, servers);
        assert_eq!(
            resume.replay_from,
            Some(ReplayFrom::Start(ReplayFromStart::new()))
        );
        assert_eq!(
            (resume.session_id, resume.cwd),
            (id, schema::AbsolutePath::from(cwd))
        );
    }

    #[test]
    fn both_opening_requests_carry_the_profiles_session_meta() {
        let cwd = PathBuf::from("/work");
        let expected = Claude.session_meta();
        let id = || schema::SessionId::new("b-1");
        let new = new_session_request(cwd.clone(), Vec::new(), expected.clone());
        let resume = resume_session_request(id(), cwd.clone(), Vec::new(), expected.clone());
        assert!(expected.is_some());
        assert_eq!(new.meta, expected);
        assert_eq!(resume.meta, expected);
        let new = new_session_request(cwd.clone(), Vec::new(), None);
        let resume = resume_session_request(id(), cwd, Vec::new(), None);
        assert_eq!((new.meta, resume.meta), (None, None));
    }
}
