//! The fixtures and helpers that more than one of the app's test suites use. A `Fixture`
//! holds an `App` loaded over the in-memory fakes, together with the fakes it was given, so
//! a test can drive the app and then look at what reached the backend, the store and git.
//! The other helpers build stored sessions and project folders, attach sessions, feed the
//! app backend updates and read back the prompts, closes and tree nodes that resulted. The
//! orchestration helpers build a `Family` of an orchestrator and its child, call its tools,
//! end its children's turns at gates and questions, and read back the child results and
//! waits it was sent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::Role;
use crate::app::scheduler::Generated;
use crate::app::{App, AppError, Approval, ChildChoice};
use crate::backend::AgentUpdate;
use crate::backend::{
    BackendEvent, ChildOutcome, ChildState, Decision, OrchestratorTool, PermissionKind,
    PermissionOption, PromptContent, ProposedChild, StopReason, TitleProposal, ToolKind,
    TurnOrigin,
};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
    PermissionOptionId, ProjectId, ProposalId, RepositoryId, RequestId, SessionId, ToolCallId,
};
use crate::orchestrator::{child_result_prompt, title_proposal_prompt};
use crate::project::Project;
use crate::session::{BranchChoice, Scope, Session, Status, TitleSource, Workspace};
use crate::settings::GlobalSettings;
use crate::store::Store as _;
use crate::testing::{
    Call, FixedClock, MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude,
};
use crate::tool_call::{PartiturTool, SessionTool, ToolCall};
use crate::transcript::{Entry, Transcript};
use crate::view_model::SessionNode;
use crate::workflow::{RECORDED, bmad_v6_12};

/// An app over the in-memory fakes, with the fakes it was loaded with.
pub(crate) struct Fixture {
    pub(crate) app: App,
    pub(crate) backend: RecordingBackend,
    pub(crate) store: MemoryStore,
    pub(crate) git: MemoryGit,
    pub(crate) clock: FixedClock,
}

pub(crate) fn fixture() -> Fixture {
    fixture_with(MemoryStore::new())
}

pub(crate) fn fixture_with(store: MemoryStore) -> Fixture {
    let (backends, default) = single_claude();
    fixture_on(store, backends, default)
}

/// A fixture whose app knows `backends` and gives new sessions `default`.
pub(crate) fn fixture_on(
    store: MemoryStore,
    backends: Vec<BackendInfo>,
    default: BackendId,
) -> Fixture {
    fixture_global(store, backends, default, GlobalSettings::default())
}

/// A fixture like [`fixture_on`] whose app has the `global` settings.
pub(crate) fn fixture_global(
    store: MemoryStore,
    backends: Vec<BackendInfo>,
    default: BackendId,
    global: GlobalSettings,
) -> Fixture {
    let backend = RecordingBackend::new();
    let git = MemoryGit::new();
    let clock = FixedClock::default();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(git.clone()),
        backends,
        default,
        global,
        Box::new(clock.clone()),
    )
    .unwrap();
    Fixture {
        app,
        backend,
        store,
        git,
        clock,
    }
}

/// An app over `store` and `backend`, with a fresh git fake and one Claude backend.
pub(crate) fn app_on(store: &MemoryStore, backend: &RecordingBackend) -> App {
    let (backends, default) = single_claude();
    App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings::default(),
        Box::new(crate::testing::FixedClock::default()),
    )
    .unwrap()
}

pub(crate) fn backend_id(name: &str) -> BackendId {
    BackendId::try_from(name).unwrap()
}

/// Two configured backends, `a` and `b`.
pub(crate) fn backends_a_and_b() -> Vec<BackendInfo> {
    ["a", "b"]
        .into_iter()
        .map(|name| BackendInfo {
            id: backend_id(name),
            name: name.into(),
            command: format!("agent-{name}"),
        })
        .collect()
}

/// A project folder whose children named in `repositories` are git repositories.
pub(crate) fn project_dir(repositories: &[&str]) -> TempDir {
    // Canonical, because the app stores canonical paths and macOS resolves the temp dir through /private.
    let dir = TempDir::new_in(crate::canonical(std::env::temp_dir()).unwrap()).unwrap();
    for name in repositories {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    dir
}

/// An idle project session of `project` titled `title`, as the store holds it, on the
/// `claude` backend and created at `created_at`.
pub(crate) fn stored_session(project: &Project, title: &str, created_at: u64) -> Session {
    Session {
        id: SessionId::new(),
        project: project.id.clone(),
        scope: Scope::Project,
        cwd: project.root.clone(),
        title: title.into(),
        backend_session: Some(BackendSessionId::new(format!("backend-{title}"))),
        backend: backend_id("claude"),
        model: None,
        effort: None,
        status: Status::Idle,
        role: Role::Plain,
        auto_allow: false,
        instructions_injected: false,
        created_at,
        archived: None,
        workspace: Workspace::CheckedOut,
        child_defaults: None,
        first_prompt: String::new(),
        skill: None,
        title_source: crate::TitleSource::Derived,
        last_activity: 0,
        children: crate::Fold::Folded,
        workflow: None,
        question: None,
        wait_reported: None,
        seen_branch: None,
        resolved_cwd: None,
    }
}

/// A store holding `project` with `sessions`.
pub(crate) fn store_with(project: &Project, sessions: &[Session]) -> MemoryStore {
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(project)).unwrap();
    store.save_sessions(project.id.clone(), sessions).unwrap();
    store
}

/// The session `session` of `project` as `store` holds it.
pub(crate) fn saved(store: &MemoryStore, project: &ProjectId, session: &SessionId) -> Session {
    store
        .load_sessions(project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| &saved.id == session)
        .unwrap()
}

/// The request, session and folder of the last `new_session` call.
pub(crate) fn new_session_request(backend: &RecordingBackend) -> (RequestId, SessionId, PathBuf) {
    let calls = backend.calls();
    let (request, session, cwd) = calls.last().unwrap().as_new_session().unwrap();
    (request, session.clone(), cwd.to_path_buf())
}

/// Creates a session in `scope` and attaches it, as though its instructions were already
/// sent, so its prompts reach the backend as given.
pub(crate) fn attached_session(
    fixture: &mut Fixture,
    project: &ProjectId,
    scope: Scope,
) -> SessionId {
    let session = fixture.app.new_session(project.clone(), scope).unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new(format!("b-{session}")),
            config: Vec::new(),
        })
        .unwrap();
    fixture
        .app
        .change_session(session.clone(), |attached| {
            attached.instructions_injected = true;
        })
        .unwrap();
    session
}

/// Feeds the app `update` from the backend session of `session`.
pub(crate) fn send(fixture: &mut Fixture, session: &SessionId, update: AgentUpdate) {
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update,
        })
        .unwrap();
}

/// The texts of the prompts `session` was sent, oldest first.
pub(crate) fn prompts(backend: &RecordingBackend, session: &SessionId) -> Vec<String> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Prompt {
                session: prompted,
                content,
            } if &prompted == session => Some(
                content
                    .into_iter()
                    .filter_map(|part| match part {
                        PromptContent::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

/// The text of the last prompt `session` was sent.
pub(crate) fn last_prompt(backend: &RecordingBackend, session: &SessionId) -> String {
    prompts(backend, session).pop().expect("no prompt was sent")
}

/// The sessions the backend was asked to close, in order.
pub(crate) fn closed(backend: &RecordingBackend) -> Vec<SessionId> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::CloseSession { session } => Some(session),
            _ => None,
        })
        .collect()
}

/// The tree node of `session`, searched through every project and every level.
pub(crate) fn tree_node(app: &App, session: &SessionId) -> SessionNode {
    fn find(nodes: &[SessionNode], session: &SessionId) -> Option<SessionNode> {
        nodes.iter().find_map(|node| {
            (node.id == *session)
                .then(|| node.clone())
                .or_else(|| find(&node.children, session))
        })
    }
    app.tree()
        .projects
        .iter()
        .find_map(|project| find(&project.sessions, session))
        .unwrap()
}

/// The choice of a child in the repository `name` with `prompt` and `title`, on the
/// default branch.
pub(crate) fn choice(name: &str, prompt: &str, title: &str, auto_allow: bool) -> ChildChoice {
    ChildChoice {
        repository: RepositoryId::try_from(name).unwrap(),
        prompt: prompt.into(),
        title: title.into(),
        auto_allow,
        branch: BranchChoice::default(),
    }
}

/// The update that reports the agent refused to set `option` to `value`.
pub(crate) fn rejected(option: &str, value: &str) -> AgentUpdate {
    AgentUpdate::ConfigRejected {
        option: ConfigId::new(option),
        value: ConfigValueId::new(value),
        error: "refused".into(),
    }
}

pub(crate) fn repository(name: &str) -> RepositoryId {
    RepositoryId::try_from(name).unwrap()
}

/// An attached child of `parent` working in `repository`, titled `title`.
pub(crate) fn attached_child(
    fixture: &mut Fixture,
    project: &ProjectId,
    parent: &SessionId,
    name: &str,
    title: &str,
) -> SessionId {
    let child = attached_session(fixture, project, Scope::Repository(repository(name)));
    fixture
        .app
        .change_session(child.clone(), |spawned| {
            spawned.role = Role::Child {
                parent: parent.clone(),
            };
            spawned.title = title.into();
        })
        .unwrap();
    child
}

/// An open two-repository project with an attached, idle orchestrator and an attached
/// child of it in `api` titled "API endpoint".
pub(crate) struct Family {
    pub(crate) dir: TempDir,
    pub(crate) fixture: Fixture,
    pub(crate) project: ProjectId,
    pub(crate) parent: SessionId,
    pub(crate) child: SessionId,
}

pub(crate) fn family() -> Family {
    family_on(MemoryStore::new())
}

/// [`family`] over `store`, which the project is opened on.
pub(crate) fn family_on(store: MemoryStore) -> Family {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(store);
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    assert!(fixture.app.session(&parent).unwrap().role.is_orchestrator());
    let child = attached_child(&mut fixture, &project, &parent, "api", "API endpoint");
    Family {
        dir,
        fixture,
        project,
        parent,
        child,
    }
}

/// Starts a turn of `child` as its orchestrator would.
pub(crate) fn orchestrate(fixture: &mut Fixture, child: &SessionId) {
    fixture
        .app
        .deliver(
            child,
            Generated::FromOrchestrator("Add the endpoint".into()),
        )
        .unwrap();
}

/// A child of `parent` in `api` titled "API endpoint" whose backend session is still
/// being created, with the id of the create request.
pub(crate) fn unattached_child(
    fixture: &mut Fixture,
    project: &ProjectId,
    parent: &SessionId,
) -> (SessionId, RequestId) {
    let child = fixture
        .app
        .new_session(project.clone(), Scope::Repository(repository("api")))
        .unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
    fixture
        .app
        .change_session(child.clone(), |spawned| {
            spawned.role = Role::Child {
                parent: parent.clone(),
            };
            spawned.title = "API endpoint".into();
        })
        .unwrap();
    (child, request)
}

/// Lets `child` finish a turn its orchestrator started with the answer `answer`.
pub(crate) fn finish(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    orchestrate(fixture, child);
    send(fixture, child, AgentUpdate::AgentText(answer.into()));
    send(fixture, child, AgentUpdate::TurnEnded(StopReason::EndTurn));
}

pub(crate) fn deferred(app: &App, session: &SessionId) -> usize {
    app.deferred_of(session).map_or(0, VecDeque::len)
}

pub(crate) fn status(app: &App, session: &SessionId) -> Status {
    app.session(session).unwrap().status
}

pub(crate) fn result_text(
    child: &SessionId,
    origin: TurnOrigin,
    outcome: &ChildOutcome,
    text: &str,
) -> String {
    child_result_prompt(
        child,
        &repository("api"),
        "API endpoint",
        origin,
        outcome,
        &ChildState::default(),
        text,
    )
}

pub(crate) fn proposed(name: &str, prompt: &str, title: &str) -> ProposedChild {
    ProposedChild {
        repository: repository(name),
        prompt: prompt.into(),
        title: title.into(),
        title_source: TitleSource::Given,
        base: None,
    }
}

/// Lets `session` call `tool` and returns the single answer its reply was given.
pub(crate) fn call_tool(
    fixture: &mut Fixture,
    session: &SessionId,
    tool: OrchestratorTool,
) -> Result<String, String> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: session.clone(),
            tool: tool.into(),
            reply: reply.clone(),
        })
        .unwrap();
    let mut answers = reply.answers();
    assert_eq!(answers.len(), 1);
    answers.pop().unwrap()
}

pub(crate) fn call_propose(
    fixture: &mut Fixture,
    session: &SessionId,
    children: Vec<ProposedChild>,
) -> Result<String, String> {
    call_tool(
        fixture,
        session,
        OrchestratorTool::ProposeChildren(children),
    )
}

pub(crate) fn call_send_to_child(
    fixture: &mut Fixture,
    session: &SessionId,
    child: &SessionId,
    prompt: &str,
) -> Result<String, String> {
    call_tool(
        fixture,
        session,
        OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: prompt.into(),
            decision: None,
        },
    )
}

impl App {
    /// The loaded transcript of `session`.
    pub(crate) fn loaded(&self, session: &SessionId) -> &Transcript {
        self.transcript(session).unwrap()
    }

    /// Drops the loaded transcript of `session`, as if it had never been opened.
    pub(crate) fn unload(&mut self, session: &SessionId) {
        if let Some(live) = self.live_mut(session) {
            live.transcript = None;
        }
    }

    /// Whether no session has an undecided proposal.
    pub(crate) fn no_proposals(&self) -> bool {
        self.live.values().all(|live| live.proposals.is_empty())
    }

    /// Whether no prompt is deferred for any session.
    pub(crate) fn nothing_deferred(&self) -> bool {
        self.live.values().all(|live| live.deferred.is_empty())
    }

    /// Whether no prompt waits for any session to be attached.
    pub(crate) fn nothing_queued(&self) -> bool {
        self.live.values().all(|live| live.queued.is_empty())
    }

    /// Whether no session has config options.
    pub(crate) fn no_config(&self) -> bool {
        self.live.values().all(|live| live.config.is_empty())
    }
}

/// The id of the session's only undecided proposal.
pub(crate) fn proposal_id(app: &App, session: &SessionId) -> ProposalId {
    let ids: Vec<_> = app.live[session].proposals.keys().cloned().collect();
    assert_eq!(ids.len(), 1);
    ids[0].clone()
}

/// Lets `session` report `event` about its workflow and returns the answer.
pub(crate) fn call_report(
    fixture: &mut Fixture,
    session: &SessionId,
    event: impl Into<crate::workflow::ModuleEvent>,
) -> Result<String, String> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: session.clone(),
            tool: PartiturTool::Workflow(event.into()),
            reply: reply.clone(),
        })
        .unwrap();
    let mut answers = reply.answers();
    assert_eq!(answers.len(), 1);
    answers.pop().unwrap()
}

/// The BMAD build plan of story `1.2` at `plans/1.2.md`, waiting for the user's approval.
pub(crate) fn build_plan_gate() -> bmad_v6_12::Event {
    bmad_v6_12::Event::RequestBuildPlanApproval {
        story: "1.2".into(),
        path: "plans/1.2.md".into(),
    }
}

/// Lets `child` run a turn its orchestrator started in which it asks for approval of its
/// build plan and answers `answer`.
pub(crate) fn turn_at_build_plan(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    orchestrate(fixture, child);
    assert_eq!(
        call_report(fixture, child, build_plan_gate()).map(|_| ()),
        Ok(())
    );
    send(fixture, child, AgentUpdate::AgentText(answer.into()));
    send(fixture, child, AgentUpdate::TurnEnded(StopReason::EndTurn));
}

/// A request to read `main.rs`, which an orchestrator asks the user about.
pub(crate) fn read_request() -> AgentUpdate {
    AgentUpdate::PermissionRequested {
        id: PermissionId(1),
        tool_call_id: ToolCallId::new("t-1"),
        title: "Read main.rs".into(),
        kind: ToolKind::Read,
        options: vec![PermissionOption {
            id: PermissionOptionId::new("allow"),
            name: "Allow".into(),
            kind: PermissionKind::AllowOnce,
        }],
        name: None,
        locations: Vec::new(),
        partitur_tool: false,
    }
}

/// Lets the orchestrator `parent` propose `children` and returns the proposal's id.
pub(crate) fn propose_now(
    fixture: &mut Fixture,
    parent: &SessionId,
    children: Vec<ProposedChild>,
) -> ProposalId {
    call_propose(fixture, parent, children).unwrap();
    proposal_id(&fixture.app, parent)
}

/// The two children the tests propose by default.
pub(crate) fn two_children() -> Vec<ProposedChild> {
    vec![
        proposed("api", "Add the endpoint", "API endpoint"),
        proposed("web", "Add the page", "Web page"),
    ]
}

/// The decision recorded on the proposal `id` in the session's transcript.
pub(crate) fn decision(app: &App, session: &SessionId, id: &ProposalId) -> Option<Decision> {
    app.loaded(session)
        .entries()
        .iter()
        .find_map(|entry| match entry {
            Entry::Proposal {
                id: existing,
                decision,
                ..
            } if existing == id => Some(decision.clone()),
            _ => None,
        })
        .flatten()
}

/// Lets the orchestrator `parent` start a turn, propose `children` and end the turn, so it
/// waits for the decision; returns the proposal's id.
pub(crate) fn waiting_on_the_card(
    fixture: &mut Fixture,
    parent: &SessionId,
    children: Vec<ProposedChild>,
) -> ProposalId {
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it").unwrap();
    let id = propose_now(fixture, parent, children);
    send(fixture, parent, AgentUpdate::TurnEnded(StopReason::EndTurn));
    assert_eq!(status(&fixture.app, parent), Status::WaitingForYou);
    id
}

/// The text of an [`AppError::InvalidChildren`] without its prefix.
pub(crate) fn refusal(result: Result<Approval, AppError>) -> String {
    match result {
        Err(AppError::InvalidChildren(text)) => text,
        other => panic!("{other:?}"),
    }
}

/// Lets `session` call the session tool `tool` and returns the single answer its reply was
/// given.
pub(crate) fn call_session_tool(
    fixture: &mut Fixture,
    session: &SessionId,
    tool: SessionTool,
) -> Result<String, String> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: session.clone(),
            tool: tool.into(),
            reply: reply.clone(),
        })
        .unwrap();
    let mut answers = reply.answers();
    assert_eq!(answers.len(), 1);
    answers.pop().unwrap()
}

pub(crate) fn call_propose_title(
    fixture: &mut Fixture,
    session: &SessionId,
    title: &str,
) -> Result<String, String> {
    call_session_tool(
        fixture,
        session,
        SessionTool::ProposeTitle {
            title: title.into(),
        },
    )
}

pub(crate) fn api_proposal(child: &SessionId, proposed: &str, by_user: bool) -> TitleProposal {
    TitleProposal {
        child: child.clone(),
        repository: repository("api"),
        title: "API endpoint".into(),
        proposed: proposed.into(),
        by_user,
    }
}

pub(crate) fn proposal_text(child: &SessionId, proposed: &str, by_user: bool) -> String {
    title_proposal_prompt(&api_proposal(child, proposed, by_user))
}

/// The superpowers report `event`, as a workflow report.
pub(crate) fn superpowers(event: crate::SuperpowersV7Event) -> crate::workflow::ModuleEvent {
    event.into()
}

/// Lets `child` report that it executes its plan inline and works on task 4.
pub(crate) fn at_task_four(fixture: &mut Fixture, child: &SessionId) {
    for event in [
        crate::SuperpowersV7Event::StartExecution {
            mode: crate::ExecutionMode::Inline,
        },
        crate::SuperpowersV7Event::StartTask { n: 4, note: None },
    ] {
        assert_eq!(
            call_report(fixture, child, superpowers(event)),
            Ok(RECORDED.to_owned())
        );
    }
}

/// Lets `child` stop at its checkpoint after task 4.
pub(crate) fn reach_checkpoint(fixture: &mut Fixture, child: &SessionId) {
    call_report(
        fixture,
        child,
        superpowers(crate::SuperpowersV7Event::ReachCheckpoint { after_task: 4 }),
    )
    .unwrap();
}

/// Lets `child` ask the user "Merge or PR?", with the options Merge and PR.
pub(crate) fn ask_merge_or_pr(fixture: &mut Fixture, child: &SessionId) {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: child.clone(),
            tool: PartiturTool::AskUser(merge_or_pr()),
            reply: reply.clone(),
        })
        .unwrap();
    assert_eq!(reply.answers().len(), 1);
}

pub(crate) fn merge_or_pr() -> crate::question::Question {
    crate::question::Question {
        text: "Merge or PR?".into(),
        options: ["Merge", "PR"]
            .into_iter()
            .map(|label| crate::question::QuestionOption {
                label: label.into(),
                description: None,
            })
            .collect(),
    }
}

/// Lets `child` end its turn with the answer `answer`.
pub(crate) fn end_with(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    send(fixture, child, AgentUpdate::AgentText(answer.into()));
    send(fixture, child, AgentUpdate::TurnEnded(StopReason::EndTurn));
}

/// The step the tree shows for a superpowers child at its checkpoint after task 4.
pub(crate) const CHECKPOINT: &str = "execute · checkpoint";

pub(crate) const GATE_FOOTER: &str = "Decide with send_to_child(session_id, decision: \"approve\" or \"changes\", prompt). Ask the user with ask_user first when the user should make this decision.";

/// Lets the orchestrator `parent` call `send_to_child` for `child` with `prompt` and
/// `decision`, and returns the answer.
pub(crate) fn call_decide(
    fixture: &mut Fixture,
    parent: &SessionId,
    child: &SessionId,
    prompt: &str,
    decision: Option<crate::GateDecision>,
) -> Result<String, String> {
    call_tool(
        fixture,
        parent,
        OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: prompt.into(),
            decision,
        },
    )
}

/// Lets `child` run a turn its orchestrator started that ends at its checkpoint after task 4.
pub(crate) fn ended_at_checkpoint(fixture: &mut Fixture, child: &SessionId) {
    orchestrate(fixture, child);
    at_task_four(fixture, child);
    reach_checkpoint(fixture, child);
    end_with(fixture, child, "Task 4 is done.");
}

/// Lets `child` run a turn its orchestrator started that ends with "Merge or PR?" open.
pub(crate) fn ended_with_question(fixture: &mut Fixture, child: &SessionId) {
    orchestrate(fixture, child);
    ask_merge_or_pr(fixture, child);
    end_with(fixture, child, "Tests pass.");
}

/// The children whose `child waits` prompts the orchestrator `parent` has queued, in order.
pub(crate) fn queued_waits<'a>(app: &'a App, parent: &SessionId) -> Vec<&'a SessionId> {
    app.deferred_of(parent)
        .into_iter()
        .flatten()
        .filter_map(|generated| match generated {
            Generated::ChildResult { child, state, .. } if state.waits.is_some() => Some(child),
            _ => None,
        })
        .collect()
}

/// The `child waits` prompt of the API child at its checkpoint after task 4.
pub(crate) fn checkpoint_wait(child: &SessionId) -> String {
    format!(
        "child waits · api · API endpoint\nsession {child}\n{CHECKPOINT} · waits at its checkpoint\n\nTask 4 is done.\n\n{GATE_FOOTER}"
    )
}
