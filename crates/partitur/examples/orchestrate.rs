//! Runs one orchestration end to end against the real Claude Code adapter, without a window.
//!
//! The example builds a throwaway project in a temporary folder: two `git init`ed
//! repositories, `api` and `web`, each with an `AGENTS.md` saying `Reply briefly.`. Partitur's
//! state goes to a second temporary folder, so neither the user's projects nor their Partitur
//! data are touched. It then opens a project session (an orchestrator, since the project has
//! two repositories), lets it grant permission requests without asking, and prompts it to
//! propose one child per repository. The first pending proposal is approved as proposed, every
//! child granting permissions without asking. The run succeeds once the orchestrator has
//! received both children's results and ended the turn that followed.
//!
//! Every state transition is printed as one line, prefixed with the seconds since the start:
//! what the adapter reports (sessions created, tool calls, permission requests with the tool's
//! raw name, notices, turn ends, failures), every call of Partitur's own tools, what the
//! orchestrator's transcript gains (the proposal, the decision, each child result, Partitur's
//! notices) and every session's status change. The example exits with status 1 after 300 seconds without success, on a failed
//! session or child, and when the orchestrator ends its first turn without a proposal.
//!
//! ```text
//! CLAUDE_CONFIG_DIR=$HOME/.claude-main cargo run -p partitur --example orchestrate
//! ```
//!
//! Environment:
//! - `PARTITUR_E2E_COMMAND`: the adapter command line, split on whitespace (default
//!   `claude-agent-acp`; that default runs the pinned adapter package through `npx` when it is
//!   not on `PATH`).
//! - The adapter inherits the example's environment, so `CLAUDE_CONFIG_DIR` selects the
//!   Claude Code login it uses.
//! - `RUST_LOG` filters the log written to stderr; the default is `warn`.

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use futures::executor::block_on;
use futures::stream;
use partitur_backend_acp::{ADAPTER_COMMAND, AcpBackend, AcpConfig};
use partitur_core::{
    App, Approval, BackendEvent, BackendId, BackendInfo, BranchChoice, ChildChoice, ChildOutcome,
    Entry, GlobalSettings, OrchestratorTool, Scope, SessionId, SessionNode, Status, StopReason,
    ToolCall, ToolStatus, Update,
};
use partitur_git::GitCli;
use partitur_mcp::ToolServer;
use partitur_store::{FileStore, StatePaths};
use tracing_subscriber::EnvFilter;

/// How long the whole run may take.
const DEADLINE: Duration = Duration::from_secs(300);

/// The orchestrator's first prompt.
const PROMPT: &str = "Use propose_children now to propose one child per repository; each child must create a file HELLO.md containing its repository's name and reply with one line. End your turn right after proposing.";

/// The repositories of the throwaway project.
const REPOSITORIES: [&str; 2] = ["api", "web"];

/// What every repository's `AGENTS.md` says.
const INSTRUCTIONS: &str = "Reply briefly.\n";

/// The backend every session runs on.
const BACKEND: &str = "claude";

/// The longest text printed on one line; longer text is cut with an ellipsis.
const LINE_LIMIT: usize = 160;

type BoxError = Box<dyn Error>;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(io::stderr)
        .init();
    let started = Instant::now();
    match run(started) {
        Ok(()) => {
            say(started, "done");
            ExitCode::SUCCESS
        }
        Err(error) => {
            say(started, format!("failed: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Builds the project and the app, prompts the orchestrator and drives it to the end.
fn run(started: Instant) -> Result<(), BoxError> {
    let project_dir = tempfile::tempdir()?;
    let data_dir = tempfile::tempdir()?;
    create_project(project_dir.path())?;
    say(
        started,
        format!(
            "project {} · data {}",
            project_dir.path().display(),
            data_dir.path().display()
        ),
    );

    let backend_id = BackendId::try_from(BACKEND)?;
    let config = adapter_config()?;
    say(
        started,
        format!(
            "adapter {} {}",
            config.command.display(),
            config.args.join(" ")
        ),
    );
    let info = BackendInfo {
        id: backend_id.clone(),
        name: BACKEND.to_owned(),
        command: config.command.display().to_string(),
    };
    // Declared before the app, so it stops only after the app dropped the backend.
    let (tool_server, tool_calls) = ToolServer::start()?;
    let urls = tool_server.urls();
    let (backend, events) = AcpBackend::start(
        HashMap::from([(backend_id.clone(), config)]),
        move |session| urls.session_url(session),
    )?;
    let store = FileStore::new(StatePaths::under(data_dir.path()));
    // Every child here works on the checked-out repository, so nothing queues git work and
    // the receiver stays unread.
    let (git, _git_events) = GitCli::start();
    let mut app = App::load(
        Box::new(backend),
        Box::new(store),
        Box::new(git),
        vec![info],
        backend_id,
        GlobalSettings::default(),
    )?;

    let project = app.open_project(project_dir.path())?;
    let orchestrator = app.new_session(project, Scope::Project)?;
    // An orchestrator asks before reading or searching too; granting those without asking
    // keeps the run from waiting on a user who is not there. Changing calls stay denied.
    app.set_auto_allow(orchestrator.clone(), true)?;
    app.submit_prompt(PROMPT, Vec::new())?;
    say(started, "prompted the orchestrator");

    let mut run = Run::new(started, orchestrator);
    run.observe(&mut app)?;
    let received = forward(events, tool_calls);
    loop {
        let remaining = DEADLINE
            .checked_sub(started.elapsed())
            .ok_or("the deadline passed")?;
        let incoming = match received.recv_timeout(remaining) {
            Ok(incoming) => incoming,
            Err(RecvTimeoutError::Timeout) => return Err("the deadline passed".into()),
            Err(RecvTimeoutError::Disconnected) => return Err("the backend stopped".into()),
        };
        match incoming {
            Incoming::Event(event) => {
                run.report_event(&app, &event);
                app.apply_event(event)?;
            }
            Incoming::Tool(call) => {
                run.report_tool(&call.session, &call.tool);
                app.call_tool(call)?;
            }
        }
        if run.observe(&mut app)? {
            break;
        }
    }
    for repository in REPOSITORIES {
        let hello = project_dir.path().join(repository).join("HELLO.md");
        match fs::read_to_string(&hello) {
            Ok(content) => say(
                started,
                format!("{repository}/HELLO.md · {}", one_line(&content)),
            ),
            Err(error) => say(started, format!("{repository}/HELLO.md · {error}")),
        }
    }
    Ok(())
}

/// Creates the repositories of the throwaway project under `root`.
fn create_project(root: &Path) -> Result<(), BoxError> {
    for repository in REPOSITORIES {
        let path = root.join(repository);
        fs::create_dir_all(&path)?;
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()?;
        if !status.success() {
            return Err(format!("git init failed in {}", path.display()).into());
        }
        fs::write(path.join("AGENTS.md"), INSTRUCTIONS)?;
    }
    Ok(())
}

/// How to start the adapter: `PARTITUR_E2E_COMMAND` split on whitespace, or the pinned adapter
/// package through `npx` when the command is the default adapter and not on `PATH`.
fn adapter_config() -> Result<AcpConfig, BoxError> {
    let line = env::var("PARTITUR_E2E_COMMAND").unwrap_or_else(|_| ADAPTER_COMMAND.to_owned());
    let mut words = line.split_whitespace();
    let program = words.next().ok_or("PARTITUR_E2E_COMMAND is empty")?;
    let args: Vec<String> = words.map(str::to_owned).collect();
    if program == ADAPTER_COMMAND && args.is_empty() && !on_path(program) {
        return Ok(AcpConfig::default());
    }
    Ok(AcpConfig {
        command: PathBuf::from(program),
        args,
        ..AcpConfig::default()
    })
}

/// Whether `program` is a file in one of the folders of `PATH`.
fn on_path(program: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|folder| folder.join(program).is_file()))
}

/// What reaches the main loop: an event of the backend or a call of Partitur's tools.
enum Incoming {
    Event(BackendEvent),
    Tool(ToolCall),
}

/// Moves the backend's events and the tool server's calls, as they arrive, to a channel the
/// main loop can wait on with a timeout.
fn forward(
    events: UnboundedReceiver<BackendEvent>,
    tool_calls: UnboundedReceiver<ToolCall>,
) -> mpsc::Receiver<Incoming> {
    let (sender, received) = mpsc::channel();
    let mut merged = stream::select(events.map(Incoming::Event), tool_calls.map(Incoming::Tool));
    thread::spawn(move || {
        while let Some(incoming) = block_on(merged.next()) {
            if sender.send(incoming).is_err() {
                break;
            }
        }
    });
    received
}

/// What the run has seen so far.
struct Run {
    started: Instant,
    orchestrator: SessionId,
    /// How many of the orchestrator's transcript entries were looked at.
    seen: usize,
    /// Every session's last printed status.
    statuses: HashMap<SessionId, Status>,
    /// Each session's name in the printed lines.
    labels: HashMap<SessionId, String>,
    /// Whether the orchestrator proposed anything yet.
    proposed: bool,
}

impl Run {
    fn new(started: Instant, orchestrator: SessionId) -> Self {
        let labels = HashMap::from([(orchestrator.clone(), "orchestrator".to_owned())]);
        Self {
            started,
            orchestrator,
            seen: 0,
            statuses: HashMap::new(),
            labels,
            proposed: false,
        }
    }

    /// The name of `session` in the printed lines.
    fn label(&self, session: &SessionId) -> String {
        self.labels
            .get(session)
            .cloned()
            .unwrap_or_else(|| session.to_string())
    }

    /// Prints what the adapter reported in `event`, when it is a state transition; text
    /// chunks, usage and configuration are left out.
    fn report_event(&self, app: &App, event: &BackendEvent) {
        let text = match event {
            BackendEvent::SessionCreated { session, .. } => {
                format!("session created · {}", self.label(session))
            }
            BackendEvent::SessionLoaded { session, .. } => {
                format!("session loaded · {}", self.label(session))
            }
            BackendEvent::SessionClosed { session, error } => {
                format!("session closed · {} · {error}", self.label(session))
            }
            BackendEvent::RequestFailed { session, error, .. } => {
                format!("request failed · {} · {error}", self.label(session))
            }
            BackendEvent::ToolCalled { session, tool, .. } => self.describe_tool(session, tool),
            BackendEvent::Update { session, update } => {
                let Some(text) = describe_update(update) else {
                    return;
                };
                format!("{} · {text}", self.label(session))
            }
        };
        say(self.started, text);
        if let BackendEvent::Update {
            update: Update::TurnEnded(_),
            session,
        } = event
            && session == &self.orchestrator
            && let Some(Entry::Agent(text)) = app.session_view().and_then(|view| {
                view.entries
                    .iter()
                    .rev()
                    .find(|entry| matches!(entry, Entry::Agent(_)))
                    .cloned()
            })
        {
            say(
                self.started,
                format!("orchestrator said · {}", one_line(&text)),
            );
        }
    }

    /// Prints the call of `tool` by `session`.
    fn report_tool(&self, session: &SessionId, tool: &OrchestratorTool) {
        say(self.started, self.describe_tool(session, tool));
    }

    /// One line for the call of `tool` by `session`.
    fn describe_tool(&self, session: &SessionId, tool: &OrchestratorTool) -> String {
        let tool = match tool {
            OrchestratorTool::ProposeChildren(children) => {
                format!("propose_children · {} children", children.len())
            }
            OrchestratorTool::SendToChild { child, prompt } => format!(
                "send_to_child · {} · {}",
                self.label(child),
                one_line(prompt)
            ),
        };
        format!("tool called · {} · {tool}", self.label(session))
    }

    /// Prints the status changes and the orchestrator's new transcript entries, approves its
    /// first pending proposal, and tells whether the run is complete.
    ///
    /// # Errors
    ///
    /// Fails when the orchestrator or a child failed, when the orchestrator ended its first
    /// turn without a proposal, or when the approval is refused.
    fn observe(&mut self, app: &mut App) -> Result<bool, BoxError> {
        self.report_statuses(app);
        let (fresh, results, ended) = {
            let Some(view) = app.session_view() else {
                return Err("the orchestrator is not selected".into());
            };
            let fresh: Vec<Entry> = view.entries.iter().skip(self.seen).cloned().collect();
            self.seen = view.entries.len();
            let results = view
                .entries
                .iter()
                .filter(|entry| matches!(entry, Entry::ChildResult { .. }))
                .count();
            let ended = matches!(view.entries.last(), Some(Entry::TurnEnded(_)));
            (fresh, results, ended)
        };
        let mut approve = None;
        for entry in fresh {
            match entry {
                Entry::Proposal {
                    id,
                    children,
                    decision: None,
                } => {
                    self.proposed = true;
                    say(
                        self.started,
                        format!("proposal: {} children", children.len()),
                    );
                    for child in &children {
                        say(
                            self.started,
                            format!(
                                "  {} · {} · {}",
                                child.repository,
                                child.title,
                                one_line(&child.prompt)
                            ),
                        );
                    }
                    approve.get_or_insert((id, children));
                }
                Entry::ChildResult {
                    repository,
                    title,
                    outcome,
                    text,
                    ..
                } => {
                    say(
                        self.started,
                        format!(
                            "child result · {repository} · {title} · {} · {}",
                            describe_outcome(&outcome),
                            one_line(&text)
                        ),
                    );
                    if let ChildOutcome::Failed(error) = outcome {
                        return Err(format!("the child in {repository} failed: {error}").into());
                    }
                }
                Entry::Notice(text) => say(self.started, format!("notice · {}", one_line(&text))),
                Entry::PartiturPrompt(text) => {
                    say(self.started, format!("woken · {}", one_line(&text)));
                }
                Entry::Failed(error) => {
                    return Err(format!("the orchestrator failed: {error}").into());
                }
                Entry::TurnEnded(_) if !self.proposed => {
                    return Err("the orchestrator ended its turn without a proposal".into());
                }
                _ => {}
            }
        }
        if let Some((proposal, children)) = approve {
            let choices = children
                .into_iter()
                .map(|child| ChildChoice {
                    repository: child.repository,
                    prompt: child.prompt,
                    title: child.title,
                    auto_allow: true,
                    branch: BranchChoice::default(),
                })
                .collect();
            let approval = app.approve_proposal(self.orchestrator.clone(), proposal, choices)?;
            let Approval::Started(started) = approval else {
                return Err("checked-out children should start at once".into());
            };
            say(
                self.started,
                format!("approved: {} children started", started.len()),
            );
            self.report_statuses(app);
            return Ok(false);
        }
        Ok(results >= REPOSITORIES.len() && ended)
    }

    /// Prints every session whose status changed since the last call, learning the names of
    /// sessions it has not seen before from the tree.
    fn report_statuses(&mut self, app: &App) {
        let mut nodes = Vec::new();
        for project in app.tree().projects {
            collect(project.sessions, &mut nodes);
        }
        for node in nodes {
            self.labels.entry(node.id.clone()).or_insert_with(|| {
                if node.scope_label.is_empty() {
                    node.title.clone()
                } else {
                    node.scope_label.clone()
                }
            });
            if self.statuses.get(&node.id) != Some(&node.status) {
                say(
                    self.started,
                    format!("status · {} · {:?}", self.label(&node.id), node.status),
                );
                self.statuses.insert(node.id, node.status);
            }
        }
    }
}

/// Flattens `sessions` and their children into `out`.
fn collect(sessions: Vec<SessionNode>, out: &mut Vec<SessionNode>) {
    for mut session in sessions {
        let children = std::mem::take(&mut session.children);
        out.push(session);
        collect(children, out);
    }
}

/// One line for an update the adapter sent, or `None` when it is not a state transition.
fn describe_update(update: &Update) -> Option<String> {
    Some(match update {
        Update::ToolCall {
            title,
            kind,
            status,
            ..
        } => format!("tool call · {kind:?} · {status:?} · {}", one_line(title)),
        Update::ToolCallUpdate {
            id,
            status: Some(status @ (ToolStatus::Completed | ToolStatus::Failed)),
            ..
        } => format!("tool call {id} · {status:?}"),
        Update::PermissionRequested {
            title,
            kind,
            options,
            name,
            ..
        } => {
            let options: Vec<String> = options
                .iter()
                .map(|option| format!("{}={:?}", option.id, option.kind))
                .collect();
            format!(
                "permission requested · {kind:?} · name {} · {} · options {}",
                name.as_deref().unwrap_or("(none)"),
                one_line(title),
                options.join(",")
            )
        }
        Update::Notice(text) => format!("notice · {}", one_line(text)),
        Update::TurnEnded(reason) => format!("turn ended · {reason:?}"),
        Update::Failed(error) => format!("failed · {}", one_line(error)),
        _ => return None,
    })
}

/// One word or phrase for how a child's turn ended.
fn describe_outcome(outcome: &ChildOutcome) -> String {
    match outcome {
        ChildOutcome::Ended(StopReason::EndTurn) => "ended".to_owned(),
        ChildOutcome::Ended(reason) => format!("ended {reason:?}"),
        ChildOutcome::Failed(error) => format!("failed {}", one_line(error)),
    }
}

/// `text` on one line: whitespace runs collapsed to one space, cut at [`LINE_LIMIT`]
/// characters.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= LINE_LIMIT {
        return joined;
    }
    let mut cut: String = joined.chars().take(LINE_LIMIT).collect();
    cut.push('…');
    cut
}

/// Prints `text` with the seconds since `started`.
fn say(started: Instant, text: impl Display) {
    println!("[{:6.1}s] {text}", started.elapsed().as_secs_f64());
}
