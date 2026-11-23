//! The fixtures and helpers that more than one of the app's test suites use. A `Fixture`
//! holds an `App` loaded over the in-memory fakes, together with the fakes it was given, so
//! a test can drive the app and then look at what reached the backend, the store and git.
//! The other helpers build stored sessions and project folders, attach sessions, feed the
//! app backend updates and read back the prompts, closes and tree nodes that resulted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::app::{App, ChildChoice};
use crate::backend::{BackendEvent, PromptContent, Update};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, ProjectId, RepositoryId,
    RequestId, SessionId,
};
use crate::project::Project;
use crate::session::{BranchChoice, Scope, Session, Status, Workspace};
use crate::settings::GlobalSettings;
use crate::store::Store as _;
use crate::testing::{Call, MemoryGit, MemoryStore, RecordingBackend, single_claude};
use crate::view_model::SessionNode;

/// An app over the in-memory fakes, with the fakes it was loaded with.
pub(crate) struct Fixture {
    pub(crate) app: App,
    pub(crate) backend: RecordingBackend,
    pub(crate) store: MemoryStore,
    pub(crate) git: MemoryGit,
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
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(git.clone()),
        backends,
        default,
        global,
    )
    .unwrap();
    Fixture {
        app,
        backend,
        store,
        git,
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
        parent: None,
        auto_allow: false,
        instructions_injected: false,
        created_at,
        orchestrator: false,
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
pub(crate) fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
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
pub(crate) fn rejected(option: &str, value: &str) -> Update {
    Update::ConfigRejected {
        option: ConfigId::new(option),
        value: ConfigValueId::new(value),
        error: "refused".into(),
    }
}
