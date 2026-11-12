//! What the view tests share: two configured backends, a session whose backend is no
//! longer configured, an orchestrator with a child it started, a count of the texts on
//! screen, and the answers of an in-memory git fed through the application.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use futures::channel::mpsc;
use iced::Task;
use iced_test::Simulator;
use iced_test::selector::Candidate;
use partitur_core::testing::{
    Call, MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude,
};
use partitur_core::{
    App, Approval, BackendEvent, BackendId, BackendInfo, BackendSessionId, BranchChoice,
    ChildChoice, GlobalSettings, OrchestratorTool, ProposalId, ProposedChild, RepositoryId, Scope,
    SessionId, ToolCall, Update,
};
use tempfile::TempDir;

use crate::app::{Message, State, boot, update};

/// A backend id that follows the naming rule.
pub(crate) fn backend_id(id: &str) -> BackendId {
    BackendId::try_from(id).unwrap()
}

/// The configured backend `id`, started by the command `command`.
pub(crate) fn backend(id: &str, command: &str) -> BackendInfo {
    BackendInfo {
        id: backend_id(id),
        name: id.to_owned(),
        command: command.to_owned(),
    }
}

/// `claude-main` (the default) and `claude-work`, each with its own command.
pub(crate) fn two_backends() -> (Vec<BackendInfo>, BackendId) {
    (
        vec![
            backend("claude-main", "claude-main-acp"),
            backend("claude-work", "claude-work-acp"),
        ],
        backend_id("claude-main"),
    )
}

/// A state whose current session was saved on the backend `gone`, which the configuration
/// (`claude-main` and `claude-work`) no longer names.
pub(crate) fn state_on_removed_backend(dir: &TempDir) -> State {
    let store = MemoryStore::new();
    let (backends, default) = two_backends();
    let before = vec![backends[0].clone(), backend("gone", "gone-acp")];
    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store.clone()),
        Box::new(MemoryGit::new()),
        before,
        default.clone(),
        GlobalSettings::default(),
    )
    .unwrap();
    let project = app.open_project(dir.path()).unwrap();
    let session = app.new_session(project, Scope::Project).unwrap();
    app.switch_backend(session.clone(), backend_id("gone"))
        .unwrap();
    drop(app);

    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    app.select_session(session).unwrap();
    boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0
}

/// A state whose current session in the project at `dir` was in a turn when Partitur closed,
/// loaded again on the same store, so the session is interrupted.
pub(crate) fn state_after_interrupted_turn(dir: &TempDir) -> State {
    let store = MemoryStore::new();
    let load = |store: &MemoryStore| {
        let (backends, default) = single_claude();
        App::load(
            Box::new(RecordingBackend::new()),
            Box::new(store.clone()),
            Box::new(MemoryGit::new()),
            backends,
            default,
            GlobalSettings::default(),
        )
        .unwrap()
    };
    let mut app = load(&store);
    let project = app.open_project(dir.path()).unwrap();
    let session = app.new_session(project, Scope::Project).unwrap();
    app.submit_prompt("Build it", Vec::new()).unwrap();
    drop(app);

    let mut app = load(&store);
    app.select_session(session).unwrap();
    boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0
}

/// A state whose project has an orchestrator that started one child in the repository `api`.
pub(crate) struct Orchestrated {
    /// The application state; the orchestrator is the current session.
    pub(crate) state: State,
    /// The orchestrator, whose turn is still running.
    pub(crate) orchestrator: SessionId,
    /// The child the orchestrator started; its turn is running.
    pub(crate) child: SessionId,
    /// The backend, which records the commands both sessions send.
    pub(crate) backend: RecordingBackend,
    /// The store the application saves to, for loading it again.
    pub(crate) store: MemoryStore,
    /// The project folder, which must outlive the state.
    _dir: TempDir,
}

/// How many texts on screen read `label`.
pub(crate) fn shown(ui: &mut Simulator<'_, Message>, label: &str) -> usize {
    let mut count = 0;
    let _ = ui.find(|candidate: Candidate<'_>| {
        if let Candidate::Text { content, .. } = candidate
            && content == label
        {
            count += 1;
        }
        None::<()>
    });
    count
}

/// Lets `git` answer every request it has queued, and every request those answers queue in
/// turn, feeding each event through `update` as [`Message::Git`]. Returns the tasks the
/// updates asked for, which the simulator cannot run.
pub(crate) fn settle_git(state: &mut State, git: &MemoryGit) -> Task<Message> {
    let mut tasks = Vec::new();
    loop {
        let events = git.answer();
        if events.is_empty() {
            return Task::batch(tasks);
        }
        for event in events {
            tasks.push(update(state, Message::Git(event)));
        }
    }
}

/// Feeds `event` through `update`, dropping the task: the simulator cannot run it.
pub(crate) fn deliver(state: &mut State, event: BackendEvent) {
    let _task = update(state, Message::Backend(event));
}

/// An orchestrator in a project with the repositories `api` and `web`, and the child in `api`
/// it started after the user approved its proposal. Both sessions are attached to their
/// backend sessions and running.
pub(crate) fn orchestrated() -> Orchestrated {
    let dir = TempDir::new().unwrap();
    for name in ["api", "web"] {
        std::fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    let backend = RecordingBackend::new();
    let store = MemoryStore::new();
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = state.app.open_project(dir.path()).unwrap();
    let orchestrator = state.app.new_session(project, Scope::Project).unwrap();
    attach(&mut state, &backend, &orchestrator, "b-orchestrator");

    let api = RepositoryId::try_from("api").unwrap();
    let id = ProposalId::new();
    let proposed = Update::ChildrenProposed {
        id: id.clone(),
        children: vec![ProposedChild {
            repository: api.clone(),
            prompt: "Add the endpoint".into(),
            title: "API endpoint".into(),
            title_source: partitur_core::TitleSource::Given,
        }],
    };
    deliver(
        &mut state,
        BackendEvent::Update {
            session: orchestrator.clone(),
            update: proposed,
        },
    );
    let choice = ChildChoice {
        repository: api,
        prompt: "Add the endpoint".into(),
        title: "API endpoint".into(),
        auto_allow: false,
        branch: BranchChoice::default(),
    };
    let Approval::Started(started) = state
        .app
        .approve_proposal(orchestrator.clone(), id, vec![choice])
        .unwrap()
    else {
        panic!("a checked-out child starts at once");
    };
    let child = started.first().cloned().unwrap();
    attach(&mut state, &backend, &child, "b-child");
    assert_eq!(state.error, None);
    Orchestrated {
        state,
        orchestrator,
        child,
        backend,
        store,
        _dir: dir,
    }
}

/// Answers the request of `backend` that created `session` with the backend session
/// `backend_session`.
pub(crate) fn attach(
    state: &mut State,
    backend: &RecordingBackend,
    session: &SessionId,
    backend_session: &str,
) {
    let request = backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .find_map(|(request, created, _)| (created == session).then_some(request))
        .unwrap();
    deliver(
        state,
        BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new(backend_session),
            config: Vec::new(),
        },
    );
}

/// Feeds a call of `tool` by `session` through `update`, dropping the task: the simulator
/// cannot run it.
pub(crate) fn deliver_tool(state: &mut State, session: SessionId, tool: OrchestratorTool) {
    let call = ToolCall {
        session,
        tool,
        reply: RecordingReply::new(),
    };
    let _task = update(state, Message::Tool(call));
}
