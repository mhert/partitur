//! The one session loop both protocol versions run, over a small [`Protocol`] each implements.
//!
//! Once the agent answered `initialize`, [`drive`] opens the session as the [`Opening`] says,
//! with Partitur's tool server registered ([`mcp_servers`], built once for either version)
//! unless the agent's profile has it learn of the server otherwise
//! ([`AgentProfile::registers_tools_in_session`]), lets the session's notifications reach its
//! gate, keeps it asking before it acts, reports it to the core and then executes the session's
//! commands until the runtime drops them or the agent goes away. An agent whose profile is kept
//! asking by its process environment ([`Asking::EnsuredAtStart`]) gets no config option switched
//! and no notice that it is not kept asking. The protocol only sends: the v2 client of [`v2`]
//! and the v1 client of [`v1`] implement it, the v1 one translating its values to and from the
//! v2 ones this loop reads.
//!
//! A command is executed without waiting for the agent, so a cancel or a permission answer is
//! never stuck behind a running turn. A config change reports the options the agent answered
//! with, or why it refused, once it answers.
//!
//! An agent whose profile holds prompts ([`AgentProfile::holds_prompts`]), over either version,
//! answers a prompt only once the session is idle, background work included, and a prompt sent
//! while an earlier one is unanswered cancels the earlier one. So for it a prompt is sent only
//! while no earlier one is unanswered; until then it is held, the turn the core opened for it
//! open, and the held prompts are sent one at a time, in order, each once the one before is
//! answered. A cancel drops the held prompts, which end as cancelled: their turn ends with the
//! agent's, or at once when the agent's turn already ended; then it cancels the agent's turn.
//! Any other agent gets every prompt at once.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::v2::schema::{
    self, ContentBlock, McpServer, McpServerHttp, SessionConfigOption,
};
use partitur_core::TOOL_SERVER;
use tokio::sync::mpsc;

use crate::agent::{AgentProfile, Asking};
use crate::connection::{
    Ending, Gate, Open, Opening, SessionCommand, Shared, admit, asking_settings, config_change,
    notice_if_uninstructed, notice_unless_asking, open_gate, opened, respond_permission,
    settle_refusal,
};
use crate::map::map_prompt;
use crate::permissions::Answer;
use crate::runtime::{Reporter, lock};

pub(crate) mod v1;
pub(crate) mod v2;

/// What opening a session asks of the agent: to create or to resume it, in a folder, with
/// MCP servers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenSession {
    /// Whether to create or resume.
    pub(crate) open: Open,
    /// The directory the agent works in.
    pub(crate) cwd: PathBuf,
    /// The MCP servers the session is opened with.
    pub(crate) mcp_servers: Vec<McpServer>,
}

/// What the agent answered to opening a session, in v2 values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Opened {
    /// The session's id at the agent.
    pub(crate) id: schema::SessionId,
    /// The session's config.
    pub(crate) config: Vec<SessionConfigOption>,
}

/// The requests one protocol version sends for an open session, in v2 values.
///
/// Every method but [`Protocol::open`] returns at once, so the command loop never waits for the
/// agent; a value that is cheap to clone and shared by the tasks it spawns, like the connection
/// it wraps, implements it.
pub(crate) trait Protocol: Clone + Send + Sync + 'static {
    /// Creates or resumes the session as `request` says.
    ///
    /// # Errors
    ///
    /// Returns the agent's refusal, or the error of a request that cannot be sent.
    async fn open(&self, request: OpenSession) -> Result<Opened, agent_client_protocol::Error>;

    /// Follows up on the session `id`, once its notifications reach the gate. It is not part of
    /// [`Protocol::open`], as it must come after [`admit`] and before the asking settings.
    ///
    /// # Errors
    ///
    /// Returns the error of a request that cannot be sent.
    fn admitted(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error>;

    /// Sets `option` of the session `id` to `value`; the future gives the config the agent
    /// answered with.
    fn set_config(
        &self,
        id: &schema::SessionId,
        option: &str,
        value: &str,
    ) -> impl Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
    + Send
    + 'static;

    /// Sends `prompt` to the session `id` and opens its turn. Once the agent answered it, the
    /// refusal, if it was one, has been settled through the session's gate, and the turn's end
    /// reported where the protocol reports it, `answered` runs.
    ///
    /// # Errors
    ///
    /// Returns the error of a request that cannot be sent.
    fn prompt(
        &self,
        id: &schema::SessionId,
        prompt: Vec<ContentBlock>,
        answered: impl FnOnce() -> Result<(), agent_client_protocol::Error> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error>;

    /// Cancels the session's running turn.
    ///
    /// # Errors
    ///
    /// Returns the error of a notification that cannot be sent.
    fn cancel(&self, id: &schema::SessionId) -> Result<(), agent_client_protocol::Error>;

    /// Runs `task` beside the command loop for as long as the connection lives.
    ///
    /// # Errors
    ///
    /// Returns the error of a connection that takes no more tasks.
    fn spawn(
        &self,
        task: impl Future<Output = Result<(), agent_client_protocol::Error>> + Send + 'static,
    ) -> Result<(), agent_client_protocol::Error>;

    /// Resolves once the agent closed its output.
    fn closed(&self) -> impl Future<Output = ()> + Send;
}

/// The MCP servers a session is opened with: Partitur's tool server at `tools_url`.
pub(crate) fn mcp_servers(tools_url: String) -> Vec<McpServer> {
    vec![McpServer::Http(McpServerHttp::new(TOOL_SERVER, tools_url))]
}

/// The connection's main task, once the agent with `profile` answered `initialize`: opens the
/// session as `opening` says, with its tool server registered if the profile says so, keeps it
/// asking as the profile says, reports it, then runs its commands.
///
/// # Errors
///
/// Returns the error that ended the connection.
pub(crate) async fn drive<P: Protocol, R: Answer>(
    protocol: P,
    shared: &Shared<R>,
    opening: &Opening,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    profile: &dyn AgentProfile,
) -> Result<Ending, agent_client_protocol::Error> {
    let open = opening.open.clone();
    let servers = if profile.registers_tools_in_session() {
        mcp_servers(opening.tools_url.clone())
    } else {
        Vec::new()
    };
    let request = OpenSession {
        open: open.clone(),
        cwd: opening.cwd.clone(),
        mcp_servers: servers,
    };
    let Opened { id, config } = protocol.open(request).await?;
    // Notifications that arrive between the response and this point are dropped as not yet
    // the session's; the agent sends none there that the reported session would miss, as the
    // asking settings below answer with the whole config.
    admit(&shared.gate, id.clone());
    protocol.admitted(&id)?;
    let ensured = profile.asking() == Asking::EnsuredAtStart;
    let settings = if ensured {
        Vec::new()
    } else {
        asking_settings(&config)
    };
    let mut config = config;
    for (option, value) in &settings {
        config = protocol.set_config(&id, option, value).await?;
    }
    let event = opened(opening.request, &shared.reporter, &id, open);
    open_gate(&shared.gate, &shared.reporter, config, event);
    if !ensured {
        notice_unless_asking(&shared.reporter, &settings);
    }
    notice_if_uninstructed(&shared.reporter, opening.uninstructed);
    let prompter = Prompter {
        protocol,
        gate: Arc::clone(&shared.gate),
        reporter: shared.reporter.clone(),
        id,
        prompts: Arc::default(),
        hold: profile.holds_prompts(),
    };
    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            () = prompter.protocol.closed() => return Ok(Ending::AgentExited),
        };
        let Some(command) = command else {
            return Ok(Ending::Closed);
        };
        execute(&prompter, shared, command)?;
    }
}

/// Executes one command without waiting for the agent.
fn execute<P: Protocol, R: Answer>(
    prompter: &Prompter<P>,
    shared: &Shared<R>,
    command: SessionCommand,
) -> Result<(), agent_client_protocol::Error> {
    match command {
        SessionCommand::Prompt(content) => prompter.prompt(map_prompt(content)),
        SessionCommand::Cancel => prompter.cancel(),
        SessionCommand::SetConfig { option, value } => {
            let protocol = &prompter.protocol;
            let change = protocol.set_config(&prompter.id, option.as_str(), value.as_str());
            let reporter = shared.reporter.clone();
            protocol.spawn(async move {
                reporter.update(config_change(option, value, change.await));
                Ok(())
            })
        }
        SessionCommand::RespondPermission { permission, option } => {
            respond_permission(shared, permission, option);
            Ok(())
        }
    }
}

/// The prompts of a session: whether one is unanswered, and the ones held behind it.
#[derive(Debug, Default)]
struct Prompts {
    /// Whether the agent has a prompt it has not answered yet.
    unanswered: bool,
    /// The prompts waiting for it, oldest first.
    held: VecDeque<Vec<ContentBlock>>,
}

/// Sends a session's prompts, when it holds them one at a time, each once the one before is
/// answered.
#[derive(Clone)]
struct Prompter<P> {
    /// The protocol the prompts go out in.
    protocol: P,
    /// The session's gate, which reports the turns.
    gate: Arc<Mutex<Gate>>,
    /// Reports to the core.
    reporter: Reporter,
    /// The session's id.
    id: schema::SessionId,
    /// The unanswered and the held prompts.
    prompts: Arc<Mutex<Prompts>>,
    /// Whether a prompt is held while an earlier one is unanswered.
    hold: bool,
}

impl<P: Protocol> Prompter<P> {
    /// Sends `prompt` now, or, when the prompter holds prompts, holds it while an earlier
    /// prompt is unanswered. A held prompt's turn is open from now on, as the core opened it.
    fn prompt(&self, prompt: Vec<ContentBlock>) -> Result<(), agent_client_protocol::Error> {
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

    /// Sends `prompt`; once it is answered, the next held prompt goes out.
    fn send(&self, prompt: Vec<ContentBlock>) -> Result<(), agent_client_protocol::Error> {
        let prompter = self.clone();
        self.protocol
            .prompt(&self.id, prompt, move || prompter.answered())
    }

    /// Sends the next held prompt, if one waits, now that the unanswered one was answered.
    ///
    /// This runs beside the command loop. The prompt is sent with the prompts still locked, so
    /// a cancel either drops it first, or locks them only once it went out and then reaches the
    /// agent after it, cancelling it: a cancel never goes out between taking the prompt and
    /// sending it, which would leave it running.
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
        self.protocol.cancel(&self.id)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use std::future::{pending, ready};
    use std::pin::Pin;

    use agent_client_protocol::v2::schema::SessionConfigSelectOption;
    use futures::channel::mpsc::{UnboundedReceiver, unbounded};
    use partitur_core::{
        AgentUpdate, BackendEvent, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
        PromptContent, SessionId, StopReason,
    };

    use super::*;
    use crate::agent::copilot::Copilot;
    use crate::agent::generic::Generic;
    use crate::agent::opencode::OpenCode;
    use crate::connection::{NO_DEFAULT_MODE_NOTICE, UNINSTRUCTED_NOTICE};
    use crate::map::map_config_options;
    use crate::runtime::Connections;

    type Error = agent_client_protocol::Error;
    /// A task the protocol was asked to run.
    type Task = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
    /// What runs once a prompt is answered.
    type Answered = Box<dyn FnOnce() -> Result<(), Error> + Send>;

    /// What the loop asked of the protocol.
    #[derive(Debug, Clone, PartialEq)]
    enum Sent {
        Open(OpenSession),
        Config(String, String),
        Prompt(Vec<ContentBlock>),
        Cancel(schema::SessionId),
    }

    /// The session id the recording protocol opens.
    const ID: &str = "s-1";

    /// A protocol that records what it is asked, answers every opening and config change with
    /// `config`, and answers prompts only when the test says so.
    #[derive(Clone, Default)]
    struct Recording {
        sent: Arc<Mutex<Vec<Sent>>>,
        answers: Arc<Mutex<VecDeque<Answered>>>,
        tasks: Arc<Mutex<Vec<Task>>>,
        config: Vec<SessionConfigOption>,
        /// Whether the agent has closed its output.
        exited: bool,
    }

    impl Recording {
        fn sent(&self) -> Vec<Sent> {
            lock(&self.sent).clone()
        }

        /// Answers the oldest unanswered prompt.
        fn answer(&self) {
            let answered = lock(&self.answers).pop_front().expect("a prompt to answer");
            answered().unwrap();
        }

        /// The tasks the protocol was asked to run, taken.
        fn tasks(&self) -> Vec<Task> {
            std::mem::take(&mut *lock(&self.tasks))
        }
    }

    impl Protocol for Recording {
        async fn open(&self, request: OpenSession) -> Result<Opened, Error> {
            lock(&self.sent).push(Sent::Open(request));
            Ok(Opened {
                id: schema::SessionId::new(ID),
                config: self.config.clone(),
            })
        }

        fn admitted(&self, _id: &schema::SessionId) -> Result<(), Error> {
            Ok(())
        }

        fn set_config(
            &self,
            _id: &schema::SessionId,
            option: &str,
            value: &str,
        ) -> impl Future<Output = Result<Vec<SessionConfigOption>, Error>> + Send + 'static
        {
            let sent = Sent::Config(option.to_owned(), value.to_owned());
            lock(&self.sent).push(sent);
            ready(Ok(self.config.clone()))
        }

        fn prompt(
            &self,
            _id: &schema::SessionId,
            prompt: Vec<ContentBlock>,
            answered: impl FnOnce() -> Result<(), Error> + Send + 'static,
        ) -> Result<(), Error> {
            lock(&self.sent).push(Sent::Prompt(prompt));
            lock(&self.answers).push_back(Box::new(answered));
            Ok(())
        }

        fn cancel(&self, id: &schema::SessionId) -> Result<(), Error> {
            lock(&self.sent).push(Sent::Cancel(id.clone()));
            Ok(())
        }

        fn spawn(
            &self,
            task: impl Future<Output = Result<(), Error>> + Send + 'static,
        ) -> Result<(), Error> {
            lock(&self.tasks).push(Box::pin(task));
            Ok(())
        }

        fn closed(&self) -> impl Future<Output = ()> + Send {
            let exited = self.exited;
            async move {
                if !exited {
                    pending::<()>().await;
                }
            }
        }
    }

    /// A reporter of a fresh session and the events it sends.
    fn reporter() -> (Connections, Reporter, UnboundedReceiver<BackendEvent>) {
        let (events, inbox) = unbounded();
        let connections = Connections::default();
        let reporter = connections.claim(SessionId::new(), events);
        (connections, reporter, inbox)
    }

    fn drain(inbox: &mut UnboundedReceiver<BackendEvent>) -> Vec<BackendEvent> {
        std::iter::from_fn(|| inbox.try_recv().ok()).collect()
    }

    /// The prompt saying `text`.
    fn said(text: &str) -> Vec<ContentBlock> {
        vec![ContentBlock::from(text)]
    }

    /// A prompter over `protocol`, holding prompts when `hold`, and the events it reports.
    fn prompter(
        protocol: &Recording,
        hold: bool,
    ) -> (
        Connections,
        Prompter<Recording>,
        UnboundedReceiver<BackendEvent>,
    ) {
        let (connections, reporter, inbox) = reporter();
        let prompter = Prompter {
            protocol: protocol.clone(),
            gate: Arc::default(),
            reporter,
            id: schema::SessionId::new(ID),
            prompts: Arc::default(),
            hold,
        };
        (connections, prompter, inbox)
    }

    #[test]
    fn without_holding_every_prompt_goes_out_at_once() {
        let protocol = Recording::default();
        let (_connections, prompter, _inbox) = prompter(&protocol, false);
        prompter.prompt(said("a")).unwrap();
        prompter.prompt(said("b")).unwrap();
        assert_eq!(
            protocol.sent(),
            [Sent::Prompt(said("a")), Sent::Prompt(said("b"))]
        );
    }

    #[test]
    fn a_held_prompt_goes_out_once_the_one_before_is_answered() {
        let protocol = Recording::default();
        let (_connections, prompter, _inbox) = prompter(&protocol, true);
        prompter.prompt(said("a")).unwrap();
        prompter.prompt(said("b")).unwrap();
        assert_eq!(protocol.sent(), [Sent::Prompt(said("a"))]);
        protocol.answer();
        assert_eq!(
            protocol.sent(),
            [Sent::Prompt(said("a")), Sent::Prompt(said("b"))]
        );
        // With every prompt answered, the next one goes out at once.
        protocol.answer();
        prompter.prompt(said("c")).unwrap();
        assert_eq!(
            protocol.sent(),
            [
                Sent::Prompt(said("a")),
                Sent::Prompt(said("b")),
                Sent::Prompt(said("c"))
            ]
        );
    }

    #[test]
    fn cancel_ends_the_held_prompts_and_cancels_the_agents_turn() {
        let protocol = Recording::default();
        let (_connections, prompter, mut inbox) = prompter(&protocol, true);
        prompter.prompt(said("a")).unwrap();
        prompter.prompt(said("b")).unwrap();
        prompter.prompt(said("c")).unwrap();
        prompter.cancel().unwrap();
        // The held prompts' turn, which no agent work holds open, ends once, cancelled.
        let session = prompter.reporter.session().clone();
        assert_eq!(
            drain(&mut inbox),
            [BackendEvent::Update {
                session,
                update: AgentUpdate::TurnEnded(StopReason::Cancelled),
            }]
        );
        // The held prompts never go out, not even once the agent answers.
        protocol.answer();
        assert_eq!(
            protocol.sent(),
            [
                Sent::Prompt(said("a")),
                Sent::Cancel(schema::SessionId::new(ID))
            ]
        );
    }

    /// A config whose `mode` offers [`crate::connection`]'s asking mode `default`.
    fn asking_config() -> Vec<SessionConfigOption> {
        vec![SessionConfigOption::select(
            "mode",
            "Mode",
            "auto",
            vec![
                SessionConfigSelectOption::new("default", "Default"),
                SessionConfigSelectOption::new("auto", "Auto"),
            ],
        )]
    }

    /// Runs [`drive`] over `protocol` for a session opened as `opening`, on `profile`, on the
    /// commands `commands`, then the tasks it spawned; gives how it ended and what it reported.
    fn run(
        protocol: &Recording,
        opening: &Opening,
        profile: &dyn AgentProfile,
        commands: mpsc::UnboundedReceiver<SessionCommand>,
    ) -> (Result<Ending, Error>, Vec<BackendEvent>, SessionId) {
        let (_connections, reporter, mut inbox) = reporter();
        let session = reporter.session().clone();
        let shared: Shared<v1::V1Responder> = Shared::new(reporter, Arc::default(), Arc::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let ending = runtime.block_on(drive(protocol.clone(), &shared, opening, commands, profile));
        for task in protocol.tasks() {
            runtime.block_on(task).unwrap();
        }
        (ending, drain(&mut inbox), session)
    }

    #[test]
    fn drive_opens_keeps_asking_reports_and_runs_the_commands_until_they_end() {
        let protocol = Recording {
            config: asking_config(),
            ..Recording::default()
        };
        let opening = Opening::for_test(Open::New, "http://tools", true);
        let (commands, inbox) = mpsc::unbounded_channel();
        let model = (ConfigId::new("model"), ConfigValueId::new("opus"));
        commands
            .send(SessionCommand::SetConfig {
                option: model.0,
                value: model.1,
            })
            .unwrap();
        for text in ["a", "b"] {
            let prompt = vec![PromptContent::Text(text.to_owned())];
            commands.send(SessionCommand::Prompt(prompt)).unwrap();
        }
        commands.send(SessionCommand::Cancel).unwrap();
        commands
            .send(SessionCommand::RespondPermission {
                permission: PermissionId(9),
                option: None,
            })
            .unwrap();
        drop(commands);
        let (ending, events, session) = run(&protocol, &opening, &Generic, inbox);
        assert_eq!(ending.unwrap(), Ending::Closed);
        let id = schema::SessionId::new(ID);
        assert_eq!(
            protocol.sent(),
            [
                Sent::Open(OpenSession {
                    open: Open::New,
                    cwd: PathBuf::from("/work"),
                    mcp_servers: mcp_servers("http://tools".to_owned()),
                }),
                Sent::Config("mode".to_owned(), "default".to_owned()),
                Sent::Config("model".to_owned(), "opus".to_owned()),
                Sent::Prompt(said("a")),
                Sent::Prompt(said("b")),
                Sent::Cancel(id),
            ]
        );
        let config = map_config_options(&asking_config());
        let update = |update| BackendEvent::Update {
            session: session.clone(),
            update,
        };
        assert_eq!(
            events,
            [
                BackendEvent::SessionCreated {
                    request: partitur_core::RequestId(7),
                    session: session.clone(),
                    backend_session: BackendSessionId::new(ID),
                    config: config.clone(),
                },
                update(AgentUpdate::Notice(UNINSTRUCTED_NOTICE.to_owned())),
                update(AgentUpdate::ConfigOptions(config)),
            ]
        );
    }

    #[test]
    fn drive_holds_prompts_as_the_profile_says() {
        let protocol = Recording::default();
        let opening = Opening::for_test(Open::New, "http://tools", false);
        let (commands, inbox) = mpsc::unbounded_channel();
        for text in ["a", "b"] {
            let prompt = vec![PromptContent::Text(text.to_owned())];
            commands.send(SessionCommand::Prompt(prompt)).unwrap();
        }
        drop(commands);
        let (ending, events, session) = run(&protocol, &opening, &Copilot, inbox);
        assert_eq!(ending.unwrap(), Ending::Closed);
        let prompts: Vec<Sent> = protocol
            .sent()
            .into_iter()
            .filter(|sent| matches!(sent, Sent::Prompt(_)))
            .collect();
        assert_eq!(prompts, [Sent::Prompt(said("a"))]);
        // Nothing keeps this session asking, which the user is told.
        assert!(events.contains(&BackendEvent::Update {
            session,
            update: AgentUpdate::Notice(NO_DEFAULT_MODE_NOTICE.to_owned()),
        }));
    }

    #[test]
    fn drive_registers_no_server_and_switches_nothing_for_a_profile_that_ensures_both() {
        let protocol = Recording {
            config: asking_config(),
            ..Recording::default()
        };
        let opening = Opening::for_test(Open::New, "http://tools", false);
        let (commands, inbox) = mpsc::unbounded_channel();
        drop(commands);
        let (ending, events, session) = run(&protocol, &opening, &OpenCode, inbox);
        assert_eq!(ending.unwrap(), Ending::Closed);
        assert_eq!(
            protocol.sent(),
            [Sent::Open(OpenSession {
                open: Open::New,
                cwd: PathBuf::from("/work"),
                mcp_servers: Vec::new(),
            })]
        );
        // Only the session's report: no option switched, no notice.
        assert_eq!(
            events,
            [BackendEvent::SessionCreated {
                request: partitur_core::RequestId(7),
                session,
                backend_session: BackendSessionId::new(ID),
                config: map_config_options(&asking_config()),
            }]
        );
    }

    #[test]
    fn a_profile_that_ensures_asking_gets_no_notice_without_options() {
        let protocol = Recording::default();
        let opening = Opening::for_test(Open::New, "http://tools", false);
        let (commands, inbox) = mpsc::unbounded_channel();
        drop(commands);
        let (_ending, events, _session) = run(&protocol, &opening, &OpenCode, inbox);
        // The session's report alone, with no notice after it.
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], BackendEvent::SessionCreated { .. }));
    }

    #[test]
    fn drive_ends_when_the_agent_exits() {
        let protocol = Recording {
            exited: true,
            ..Recording::default()
        };
        let opening = Opening::for_test(Open::Load(BackendSessionId::new(ID)), "http://t", false);
        let (_commands, inbox) = mpsc::unbounded_channel();
        let (ending, events, session) = run(&protocol, &opening, &Generic, inbox);
        assert_eq!(ending.unwrap(), Ending::AgentExited);
        assert!(matches!(
            &events[0],
            BackendEvent::SessionLoaded { session: loaded, .. } if *loaded == session
        ));
    }

    #[test]
    fn every_session_is_opened_with_the_tool_server() {
        let url = "http://127.0.0.1:4711/session".to_owned();
        assert_eq!(
            mcp_servers(url.clone()),
            [McpServer::Http(McpServerHttp::new("partitur", url))]
        );
    }
}
