//! What the view tests share: two configured backends, a session whose backend is no
//! longer configured, an orchestrator with a child it started, and a count of the texts on
//! screen.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use futures::channel::mpsc;
use iced_test::Simulator;
use iced_test::selector::Candidate;
use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
use partitur_core::{
    App, BackendEvent, BackendId, BackendInfo, BackendSessionId, ChildChoice, ProposalId,
    ProposedChild, RepositoryId, Scope, SessionId, Update,
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
        before,
        default.clone(),
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
        backends,
        default,
    )
    .unwrap();
    app.select_session(session).unwrap();
    boot(app, mpsc::unbounded().1).0
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
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(MemoryStore::new()),
        backends,
        default,
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1).0;
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
    };
    let started = state
        .app
        .approve_proposal(orchestrator.clone(), id, vec![choice])
        .unwrap();
    let child = started.first().cloned().unwrap();
    attach(&mut state, &backend, &child, "b-child");
    assert_eq!(state.error, None);
    Orchestrated {
        state,
        orchestrator,
        child,
        backend,
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
