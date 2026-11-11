#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::app::{App, AppError};
use crate::archive::Archived;
use crate::backend::{
    BackendError, BackendEvent, ConfigCategory, ConfigChoice, ConfigOption, OrchestratorTool,
    Update,
};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, ProjectId, RepositoryId,
    RequestId, SessionId, ToolRequestId,
};
use crate::project::{Project, discover_project};
use crate::session::{Scope, Session, Status, Workspace};
use crate::store::Store as _;
use crate::testing::{Call, MemoryStore, RecordingBackend, single_claude};
use crate::transcript::Entry;
use crate::view_model::{ChoiceView, DraftChoice, Warmup};

use super::super::tests::agent_options;

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
}

fn fixture_with(store: MemoryStore) -> Fixture {
    let (backends, default) = single_claude();
    fixture_on(store, backends, default)
}

/// A fixture whose app knows `backends` and gives new sessions `default`.
fn fixture_on(store: MemoryStore, backends: Vec<BackendInfo>, default: BackendId) -> Fixture {
    let backend = RecordingBackend::new();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        backends,
        default,
    )
    .unwrap();
    Fixture {
        app,
        backend,
        store,
    }
}

/// A project folder whose children named in `repositories` are git repositories.
fn project_dir(repositories: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for name in repositories {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    dir
}

fn backend_id(name: &str) -> BackendId {
    BackendId::try_from(name).unwrap()
}

/// Two configured backends, `a` and `b`.
fn backends_a_and_b() -> Vec<BackendInfo> {
    ["a", "b"]
        .into_iter()
        .map(|name| BackendInfo {
            id: backend_id(name),
            name: name.into(),
            command: format!("agent-{name}"),
        })
        .collect()
}

fn value(id: &str) -> ConfigValueId {
    ConfigValueId::new(id)
}

/// A session of `project` as the store holds it, on `backend`, created at `created_at`.
fn stored_session(project: &Project, backend: &str, created_at: u64) -> Session {
    Session {
        id: SessionId::new(),
        project: project.id.clone(),
        scope: Scope::Project,
        cwd: project.root.clone(),
        title: format!("created at {created_at}"),
        backend_session: Some(BackendSessionId::new(format!("backend-{created_at}"))),
        backend: backend_id(backend),
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
    }
}

/// A store holding `project` with `sessions`.
fn store_with(project: &Project, sessions: &[Session]) -> MemoryStore {
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(project)).unwrap();
    store.save_sessions(project.id.clone(), sessions).unwrap();
    store
}

/// A stored session on `claude` that records `model` and `effort`.
fn seeded(project: &Project, model: &str, effort: &str) -> Session {
    let mut seed = stored_session(project, "claude", 1);
    seed.model = Some(value(model));
    seed.effort = Some(value(effort));
    seed
}

/// An option of `category` whose current value is `current`, offering `choices`.
fn option(id: &str, category: ConfigCategory, current: &str, choices: &[&str]) -> ConfigOption {
    ConfigOption {
        id: ConfigId::new(id),
        name: id.into(),
        category,
        current: value(current),
        choices: choices
            .iter()
            .map(|choice| ConfigChoice {
                id: value(choice),
                name: (*choice).into(),
            })
            .collect(),
    }
}

fn model(current: &str, choices: &[&str]) -> ConfigOption {
    option("model", ConfigCategory::Model, current, choices)
}

fn effort(current: &str, choices: &[&str]) -> ConfigOption {
    option("effort", ConfigCategory::Effort, current, choices)
}

/// The picker of `option` showing `current`.
fn offered(option: &ConfigOption, current: &str) -> DraftChoice {
    DraftChoice::Offered(ChoiceView {
        current: value(current),
        current_name: current.into(),
        choices: option.choices.clone(),
    })
}

/// The request and session of the last `new_session` call.
fn warm_up(backend: &RecordingBackend) -> (RequestId, SessionId) {
    let calls = backend.calls();
    let (request, session, _) = calls.last().unwrap().as_new_session().unwrap();
    (request, session.clone())
}

/// Lets the backend answer the last warm-up with `config`.
fn answer(fixture: &mut Fixture, config: Vec<ConfigOption>) -> SessionId {
    let (request, session) = warm_up(&fixture.backend);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("warm"),
            config,
        })
        .unwrap();
    session
}

fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update,
        })
        .unwrap();
}

fn saved(store: &MemoryStore, project: &ProjectId, session: &SessionId) -> Session {
    store
        .load_sessions(project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| &saved.id == session)
        .unwrap()
}

/// An empty open project in a fixture without stored sessions.
fn open_project(repositories: &[&str]) -> (TempDir, Fixture, ProjectId) {
    let dir = project_dir(repositories);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    (dir, fixture, project)
}

/// A fixture over the stored `project` with `sessions`, on `backends` with `default`.
fn stored_fixture(
    project: &Project,
    sessions: &[Session],
    backends: Vec<BackendInfo>,
    default: &str,
) -> Fixture {
    fixture_on(store_with(project, sessions), backends, backend_id(default))
}

#[test]
fn preparing_starts_a_warm_session_outside_the_tree_and_the_store() {
    let (dir, mut fixture, project) = open_project(&[]);

    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();

    let root = fs::canonicalize(dir.path()).unwrap();
    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession { backend, cwd, orchestrator: false, .. })
            if *backend == backend_id("claude") && *cwd == root
    ));
    assert!(fixture.app.tree().projects[0].sessions.is_empty());
    assert_eq!(fixture.store.session_saves(&project), 0);
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.warmup, Warmup::Loading);
    assert_eq!(draft.model, DraftChoice::Waiting(None));
    assert_eq!(draft.effort, DraftChoice::Waiting(None));
    assert!(!draft.auto_allow);
    assert_eq!(draft.backend, backend_id("claude"));
    assert_eq!(draft.backends, single_claude().0);
    assert_eq!(
        draft.project,
        root.file_name().unwrap().to_string_lossy().into_owned()
    );
    assert_eq!(draft.repository, None);
    assert_eq!(fixture.app.current_session(), None);
}

#[test]
fn a_project_draft_with_two_repositories_is_an_orchestrator() {
    let (dir, mut fixture, project) = open_project(&["api", "web"]);

    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();

    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession {
            orchestrator: true,
            ..
        })
    ));
    let draft = fixture.app.draft().unwrap();
    assert!(draft.orchestrator);
    assert_eq!(draft.repositories, 2);

    let api = RepositoryId::try_from("api").unwrap();
    fixture
        .app
        .prepare_session(project, Scope::Repository(api))
        .unwrap();

    let cwd: PathBuf = fs::canonicalize(dir.path()).unwrap().join("api");
    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession { cwd: called, orchestrator: false, .. }) if *called == cwd
    ));
    let draft = fixture.app.draft().unwrap();
    assert!(!draft.orchestrator);
    assert_eq!(draft.repository.as_deref(), Some("api"));
}

#[test]
fn preparing_rejects_unknown_projects_and_repositories() {
    let (_dir, mut fixture, project) = open_project(&[]);

    let unknown = fixture
        .app
        .prepare_session(ProjectId::new(), Scope::Project);
    let missing = fixture.app.prepare_session(
        project,
        Scope::Repository(RepositoryId::try_from("gone").unwrap()),
    );

    assert!(matches!(unknown, Err(AppError::UnknownProject(_))));
    assert!(matches!(missing, Err(AppError::UnknownRepository(_))));
    assert!(fixture.backend.calls().is_empty());
    assert_eq!(fixture.app.draft(), None);
}

#[test]
fn the_newest_session_of_the_project_seeds_the_draft() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let other_dir = project_dir(&[]);
    let other = discover_project(other_dir.path()).unwrap();
    let older = stored_session(&project, "a", 1);
    let mut newest = stored_session(&project, "b", 2);
    newest.archived = Some(Archived { at: 3 });
    newest.model = Some(value("opus"));
    newest.effort = Some(value("high"));
    newest.auto_allow = true;
    let store = store_with(&project, &[older.clone(), newest.clone()]);
    store
        .save_projects(&[project.clone(), other.clone()])
        .unwrap();
    let elsewhere = stored_session(&other, "a", 9);
    store
        .save_sessions(other.id.clone(), std::slice::from_ref(&elsewhere))
        .unwrap();
    let mut fixture = fixture_on(store, backends_a_and_b(), backend_id("a"));

    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();

    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession { backend, .. }) if *backend == backend_id("b")
    ));
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.backend, backend_id("b"));
    assert_eq!(draft.model, DraftChoice::Waiting(Some(value("opus"))));
    assert_eq!(draft.effort, DraftChoice::Waiting(Some(value("high"))));
    assert!(draft.auto_allow);

    let tied = stored_session(&project, "a", 2);
    let mut fixture = stored_fixture(&project, &[older, newest, tied], backends_a_and_b(), "b");

    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();

    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.backend, backend_id("a"));
    assert_eq!(draft.model, DraftChoice::Waiting(None));
    assert!(!draft.auto_allow);
}

#[test]
fn a_seed_on_a_removed_backend_warms_the_default_without_its_model() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut seed = stored_session(&project, "zzz", 1);
    seed.model = Some(value("opus"));
    seed.effort = Some(value("high"));
    seed.auto_allow = true;
    let (backends, _) = single_claude();
    let mut fixture = stored_fixture(&project, &[seed], backends, "claude");

    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();

    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::NewSession { backend, .. }) if *backend == backend_id("claude")
    ));
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.model, DraftChoice::Waiting(None));
    assert_eq!(draft.effort, DraftChoice::Waiting(None));
    assert!(draft.auto_allow);
}

#[test]
fn the_agents_answer_fills_the_choices() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut seed = stored_session(&project, "claude", 1);
    seed.model = Some(value("opus"));
    let (backends, _) = single_claude();
    let mut fixture = stored_fixture(&project, &[seed.clone()], backends.clone(), "claude");
    let models = model("fast", &["fast", "opus"]);
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();

    answer(&mut fixture, vec![models.clone()]);

    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.warmup, Warmup::Ready);
    assert_eq!(draft.model, offered(&models, "opus"));
    assert_eq!(draft.effort, DraftChoice::NotOffered);

    seed.model = Some(value("gone"));
    let mut fixture = stored_fixture(&project, &[seed], backends, "claude");
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();

    answer(&mut fixture, vec![models.clone()]);

    assert_eq!(fixture.app.draft().unwrap().model, offered(&models, "fast"));
}

#[test]
fn config_options_before_the_answer_are_dropped() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let (_, draft) = warm_up(&fixture.backend);

    send(
        &mut fixture,
        &draft,
        Update::ConfigOptions(vec![effort("low", &["low"])]),
    );
    answer(&mut fixture, vec![model("fast", &["fast"])]);

    assert_eq!(fixture.app.draft().unwrap().effort, DraftChoice::NotOffered);
    fixture.app.create_from_draft().unwrap();
    assert!(fixture.store.load_updates(draft).unwrap().is_empty());
}

#[test]
fn a_failed_warm_up_marks_the_draft_failed() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, session) = warm_up(&fixture.backend);

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session,
            error: "no login".into(),
        })
        .unwrap();

    assert_eq!(
        fixture.app.draft().unwrap().warmup,
        Warmup::Failed("no login".into())
    );

    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let session = answer(&mut fixture, Vec::new());

    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session,
            error: "exit".into(),
        })
        .unwrap();

    assert_eq!(
        fixture.app.draft().unwrap().warmup,
        Warmup::Failed("connection to the agent closed: exit".into())
    );

    fixture.app.discard_draft().unwrap();
    fixture.backend.fail_next_call();
    let unavailable = fixture.app.prepare_session(project, Scope::Project);

    assert!(unavailable.is_ok());
    assert_eq!(
        fixture.app.draft().unwrap().warmup,
        Warmup::Failed("backend unavailable: recording backend".into())
    );
}

#[test]
fn an_answer_to_another_request_is_ignored() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let (request, session) = warm_up(&fixture.backend);
    let other = RequestId(request.0 + 100);

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request: other,
            session: session.clone(),
            backend_session: BackendSessionId::new("stray"),
            config: Vec::new(),
        })
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request: other,
            session: session.clone(),
            error: "stray".into(),
        })
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session,
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(fixture.app.draft().unwrap().warmup, Warmup::Loading);
}

#[test]
fn a_tool_call_of_the_warm_session_is_refused() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let (_, session) = warm_up(&fixture.backend);
    let call = |call: u64| BackendEvent::ToolCalled {
        call: ToolRequestId(call),
        session: session.clone(),
        tool: OrchestratorTool::ProposeChildren(Vec::new()),
    };

    fixture.app.apply_event(call(1)).unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::AnswerTool {
            call: ToolRequestId(1),
            result: Err("unknown session".into()),
        })
    );

    fixture
        .backend
        .fail_next_call_with(BackendError::UnknownToolCall(ToolRequestId(2)));
    fixture.app.apply_event(call(2)).unwrap();

    fixture.backend.fail_next_call();
    let failed = fixture.app.apply_event(call(3));

    assert!(matches!(
        failed,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
}

#[test]
fn picking_a_model_sets_it_on_the_warm_session() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();

    let loading = fixture.app.draft_model(value("opus"));

    assert!(matches!(
        loading,
        Err(AppError::NoSuchConfig(ConfigCategory::Model))
    ));

    let models = model("fast", &["fast", "opus"]);
    let session = answer(&mut fixture, vec![models.clone(), effort("low", &["low"])]);
    fixture.app.draft_model(value("opus")).unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::SetConfig {
            session: session.clone(),
            option: ConfigId::new("model"),
            value: value("opus"),
        })
    );
    assert_eq!(fixture.app.draft().unwrap().model, offered(&models, "opus"));

    fixture.app.draft_effort(value("max")).unwrap();
    let efforts = effort("low", &["low", "max"]);
    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(vec![model("opus", &["fast", "opus"]), efforts.clone()]),
    );

    assert_eq!(
        fixture.app.draft().unwrap().effort,
        offered(&efforts, "max")
    );

    let lower = effort("low", &["low"]);
    send(
        &mut fixture,
        &session,
        Update::ConfigOptions(vec![models.clone(), lower.clone()]),
    );

    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.effort, offered(&lower, "low"));
    assert_eq!(draft.model, offered(&models, "opus"));
}

#[test]
fn a_model_that_cannot_be_set_changes_nothing() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let models = model("fast", &["fast", "opus"]);
    answer(&mut fixture, vec![models.clone()]);

    fixture.backend.fail_next_call();
    let failed = fixture.app.draft_model(value("opus"));

    assert!(matches!(failed, Err(AppError::Backend(_))));
    assert_eq!(fixture.app.draft().unwrap().model, offered(&models, "fast"));
    fixture.app.create_from_draft().unwrap();
    let session = fixture.app.current_session().unwrap();
    assert_eq!(
        fixture.app.session(&session).unwrap().model,
        Some(value("fast"))
    );
}

#[test]
fn a_draft_without_a_model_option_cannot_pick_one() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    answer(&mut fixture, vec![effort("low", &["low"])]);

    let picked = fixture.app.draft_model(value("opus"));

    assert!(matches!(
        picked,
        Err(AppError::NoSuchConfig(ConfigCategory::Model))
    ));
}

#[test]
fn picking_an_effort_or_auto_allow_only_records_it() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let calls = fixture.backend.calls().len();

    fixture.app.draft_effort(value("high")).unwrap();
    fixture.app.draft_auto_allow(true).unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.effort, DraftChoice::Waiting(Some(value("high"))));
    assert!(draft.auto_allow);
}

#[test]
fn changing_the_backend_warms_a_fresh_session() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut seed = stored_session(&project, "a", 1);
    seed.model = Some(value("opus"));
    let mut fixture = stored_fixture(&project, &[seed], backends_a_and_b(), "a");
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();
    let (_, old) = warm_up(&fixture.backend);
    fixture.app.draft_effort(value("high")).unwrap();
    fixture.app.draft_auto_allow(true).unwrap();

    fixture.app.draft_backend(backend_id("b")).unwrap();

    let calls = fixture.backend.calls();
    let (_, new) = warm_up(&fixture.backend);
    assert_ne!(new, old);
    assert_eq!(
        calls[calls.len() - 2],
        Call::CloseSession {
            session: old.clone()
        }
    );
    assert!(matches!(
        &calls[calls.len() - 1],
        Call::NewSession { backend, session, .. } if *backend == backend_id("b") && *session == new
    ));
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.backend, backend_id("b"));
    assert_eq!(draft.warmup, Warmup::Loading);
    assert_eq!(draft.model, DraftChoice::Waiting(Some(value("opus"))));
    assert_eq!(draft.effort, DraftChoice::Waiting(Some(value("high"))));
    assert!(draft.auto_allow);

    fixture.app.draft_backend(backend_id("b")).unwrap();
    let unknown = fixture.app.draft_backend(backend_id("zzz"));

    assert_eq!(fixture.backend.calls().len(), calls.len());
    assert!(matches!(unknown, Err(AppError::UnknownBackend(_))));
}

#[test]
fn a_backend_change_that_cannot_close_the_warm_session_changes_nothing() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let session = answer(&mut fixture, Vec::new());

    fixture.backend.fail_next_call();
    let failed = fixture.app.draft_backend(backend_id("b"));

    assert!(matches!(failed, Err(AppError::Backend(_))));
    let draft = fixture.app.draft().unwrap();
    assert_eq!(draft.backend, backend_id("a"));
    assert_eq!(draft.warmup, Warmup::Ready);
    assert_eq!(fixture.app.create_from_draft().unwrap(), session);
}

#[test]
fn a_late_answer_from_the_previous_backend_is_ignored() {
    let dir = project_dir(&[]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let (request, old) = warm_up(&fixture.backend);
    fixture.app.draft_backend(backend_id("b")).unwrap();

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: old.clone(),
            backend_session: BackendSessionId::new("late"),
            config: Vec::new(),
        })
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: old,
            error: "late".into(),
        })
        .unwrap();

    assert_eq!(fixture.app.draft().unwrap().warmup, Warmup::Loading);
}

#[test]
fn discarding_closes_the_warm_session_and_leaves_nothing() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let (_, session) = warm_up(&fixture.backend);

    fixture.app.discard_draft().unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::CloseSession { session })
    );
    assert_eq!(fixture.app.draft(), None);
    assert!(fixture.app.tree().projects[0].sessions.is_empty());
    assert_eq!(fixture.store.session_saves(&project), 0);
    assert!(matches!(
        fixture.app.discard_draft(),
        Err(AppError::NoDraft)
    ));
}

#[test]
fn a_discard_that_cannot_be_queued_still_drops_the_draft() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let (request, session) = warm_up(&fixture.backend);

    fixture.backend.fail_next_call();
    let failed = fixture.app.discard_draft();

    assert!(matches!(failed, Err(AppError::Backend(_))));
    assert_eq!(fixture.app.draft(), None);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session,
            backend_session: BackendSessionId::new("late"),
            config: Vec::new(),
        })
        .unwrap();
}

#[test]
fn late_events_of_a_discarded_draft_are_ignored() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    let (request, session) = warm_up(&fixture.backend);
    fixture.app.discard_draft().unwrap();

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("late"),
            config: Vec::new(),
        })
        .unwrap();
    send(&mut fixture, &session, Update::Notice("late".into()));
    fixture
        .app
        .apply_event(BackendEvent::ToolCalled {
            call: ToolRequestId(7),
            session: session.clone(),
            tool: OrchestratorTool::ProposeChildren(Vec::new()),
        })
        .unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::AnswerTool {
            call: ToolRequestId(7),
            result: Err("the session was deleted".into()),
        })
    );
    assert_eq!(fixture.app.draft(), None);
    assert!(
        fixture
            .store
            .load_updates(session.clone())
            .unwrap()
            .is_empty()
    );
    assert!(fixture.app.select_session(session).is_err());
}

#[test]
fn preparing_again_discards_the_open_draft() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, first) = warm_up(&fixture.backend);

    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();

    let calls = fixture.backend.calls();
    let (_, second) = warm_up(&fixture.backend);
    assert_ne!(second, first);
    assert_eq!(
        calls[calls.len() - 2],
        Call::CloseSession {
            session: first.clone()
        }
    );
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: first,
            backend_session: BackendSessionId::new("late"),
            config: Vec::new(),
        })
        .unwrap();
    assert_eq!(fixture.app.draft().unwrap().warmup, Warmup::Loading);
}

#[test]
fn creating_after_the_answer_adopts_the_warm_session() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let draft = answer(
        &mut fixture,
        vec![
            model("fast", &["fast", "opus"]),
            effort("low", &["low", "high"]),
        ],
    );
    fixture.app.draft_effort(value("high")).unwrap();
    fixture.app.draft_auto_allow(true).unwrap();
    send(&mut fixture, &draft, Update::Notice("hello".into()));
    let calls = fixture.backend.calls().len();

    let created = fixture.app.create_from_draft().unwrap();

    assert_eq!(created, draft);
    assert_eq!(fixture.app.current_session(), Some(created.clone()));
    let tree = fixture.app.tree();
    assert_eq!(tree.projects[0].sessions.len(), 1);
    assert_eq!(tree.projects[0].sessions[0].title, "New session");
    let session = saved(&fixture.store, &project, &created);
    assert_eq!(session.backend_session, Some(BackendSessionId::new("warm")));
    assert_eq!(session.model, Some(value("fast")));
    assert_eq!(session.effort, Some(value("high")));
    assert!(session.auto_allow);
    assert!(!session.orchestrator);
    assert_eq!(session.status, Status::Idle);
    assert_eq!(session.backend, backend_id("claude"));
    assert_eq!(
        fixture.backend.calls()[calls..],
        [Call::SetConfig {
            session: created.clone(),
            option: ConfigId::new("effort"),
            value: value("high"),
        }]
    );
    assert_eq!(
        fixture.store.load_updates(created.clone()).unwrap(),
        [Update::Notice("hello".into())]
    );
    assert_eq!(
        fixture.app.session_view().unwrap().entries,
        [Entry::Notice("hello".into())]
    );
    assert_eq!(fixture.app.draft(), None);

    fixture.app.submit_prompt("go", Vec::new()).unwrap();

    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::Prompt { session, .. }) if *session == created
    ));
}

#[test]
fn creating_before_the_answer_applies_the_choices_when_it_arrives() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let (backends, _) = single_claude();
    let mut fixture = stored_fixture(
        &project,
        &[seeded(&project, "opus", "high")],
        backends,
        "claude",
    );
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();
    let (request, draft) = warm_up(&fixture.backend);
    let calls = fixture.backend.calls().len();

    let created = fixture.app.create_from_draft().unwrap();

    assert_eq!(created, draft);
    assert_eq!(fixture.backend.calls().len(), calls);
    let session = saved(&fixture.store, &project.id, &created);
    assert_eq!(session.model, Some(value("opus")));
    assert_eq!(session.effort, Some(value("high")));
    assert_eq!(session.backend_session, None);

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: created.clone(),
            backend_session: BackendSessionId::new("warm"),
            config: vec![
                model("fast", &["fast", "opus"]),
                effort("low", &["low", "high"]),
            ],
        })
        .unwrap();

    let set = |option: &str, to: &str| Call::SetConfig {
        session: created.clone(),
        option: ConfigId::new(option),
        value: value(to),
    };
    assert_eq!(fixture.backend.calls()[calls..], [set("model", "opus")]);
    let session = saved(&fixture.store, &project.id, &created);
    assert_eq!(session.backend_session, Some(BackendSessionId::new("warm")));

    send(
        &mut fixture,
        &created,
        Update::ConfigOptions(vec![
            model("opus", &["fast", "opus"]),
            effort("low", &["low", "high"]),
        ]),
    );

    assert_eq!(
        fixture.backend.calls()[calls..],
        [set("model", "opus"), set("effort", "high")]
    );
}

#[test]
fn creating_before_the_answer_drops_a_seed_the_agent_does_not_offer() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let (backends, _) = single_claude();
    let mut fixture = stored_fixture(
        &project,
        &[seeded(&project, "opus", "high")],
        backends,
        "claude",
    );
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();
    let (request, draft) = warm_up(&fixture.backend);
    fixture.app.create_from_draft().unwrap();
    let calls = fixture.backend.calls().len();

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: draft.clone(),
            backend_session: BackendSessionId::new("warm"),
            config: vec![model("fast", &["fast"]), effort("low", &["low", "high"])],
        })
        .unwrap();

    assert_eq!(
        fixture.backend.calls()[calls..],
        [Call::SetConfig {
            session: draft.clone(),
            option: ConfigId::new("effort"),
            value: value("high"),
        }]
    );
    let session = saved(&fixture.store, &project.id, &draft);
    assert_eq!(session.model, Some(value("fast")));
    assert_eq!(session.effort, Some(value("high")));
}

/// The value a picker shows when it offers choices.
fn shown(choice: &DraftChoice) -> Option<ConfigValueId> {
    match choice {
        DraftChoice::Offered(view) => Some(view.current.clone()),
        _ => None,
    }
}

fn set(session: &SessionId, option: &str, to: &str) -> Call {
    Call::SetConfig {
        session: session.clone(),
        option: ConfigId::new(option),
        value: value(to),
    }
}

/// A draft seeded with `opus` and `xhigh` whose agent answered on `sonnet`, which lacks
/// `xhigh`.
fn seeded_opus_answered_on_sonnet() -> (TempDir, Fixture, ProjectId, SessionId) {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let (backends, _) = single_claude();
    let seed = seeded(&project, "opus", "xhigh");
    let mut fixture = stored_fixture(&project, &[seed], backends, "claude");
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Project)
        .unwrap();
    let draft = answer(&mut fixture, agent_options("sonnet"));
    (dir, fixture, project.id, draft)
}

#[test]
fn a_seeded_model_is_set_on_the_warm_session_when_the_agent_answers() {
    let (_dir, mut fixture, project, draft) = seeded_opus_answered_on_sonnet();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&set(&draft, "model", "opus"))
    );

    let options = agent_options("opus");
    send(&mut fixture, &draft, Update::ConfigOptions(options.clone()));

    let view = fixture.app.draft().unwrap();
    assert_eq!(shown(&view.model), Some(value("opus")));
    assert_eq!(shown(&view.effort), Some(value("xhigh")));

    let calls = fixture.backend.calls().len();
    fixture.app.create_from_draft().unwrap();

    assert_eq!(
        fixture.backend.calls()[calls..],
        [set(&draft, "effort", "xhigh")]
    );
    let session = saved(&fixture.store, &project, &draft);
    assert_eq!(session.model, Some(value("opus")));
    assert_eq!(session.effort, Some(value("xhigh")));
}

#[test]
fn creating_while_the_model_is_on_its_way_applies_the_effort_with_its_options() {
    let (_dir, mut fixture, project, draft) = seeded_opus_answered_on_sonnet();
    let calls = fixture.backend.calls().len();

    fixture.app.create_from_draft().unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    let session = saved(&fixture.store, &project, &draft);
    assert_eq!(session.model, Some(value("opus")));
    assert_eq!(session.effort, Some(value("xhigh")));

    send(
        &mut fixture,
        &draft,
        Update::ConfigOptions(agent_options("opus")),
    );

    assert_eq!(
        fixture.backend.calls()[calls..],
        [set(&draft, "effort", "xhigh")]
    );
    let session = saved(&fixture.store, &project, &draft);
    assert_eq!(session.model, Some(value("opus")));
    assert_eq!(session.effort, Some(value("xhigh")));
}

fn rejected(option: &str, to: &str) -> Update {
    Update::ConfigRejected {
        option: ConfigId::new(option),
        value: value(to),
        error: "refused".into(),
    }
}

/// The options of an agent on `sonnet` whose effort is `low`.
fn sonnet_on_low() -> Vec<ConfigOption> {
    let mut options = agent_options("sonnet");
    options[1].current = value("low");
    options
}

#[test]
fn a_rejected_model_change_on_the_draft_falls_back_to_the_agents_model() {
    let (_dir, mut fixture, project, draft) = seeded_opus_answered_on_sonnet();

    send(&mut fixture, &draft, rejected("model", "opus"));

    let view = fixture.app.draft().unwrap();
    assert_eq!(shown(&view.model), Some(value("sonnet")));
    assert_eq!(shown(&view.effort), Some(value("high")));

    let calls = fixture.backend.calls().len();
    fixture.app.create_from_draft().unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    let session = saved(&fixture.store, &project, &draft);
    assert_eq!(session.model, Some(value("sonnet")));
    assert_eq!(session.effort, Some(value("high")));
    let entries = fixture.app.session_view().unwrap().entries.to_vec();
    assert_eq!(
        entries,
        [Entry::Notice("could not change model: refused".into())]
    );

    // Nothing waits for a model change: the next options are taken as they are.
    send(&mut fixture, &draft, Update::ConfigOptions(sonnet_on_low()));
    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(
        saved(&fixture.store, &project, &draft).effort,
        Some(value("low"))
    );
    fixture.app.submit_prompt("hi", Vec::new()).unwrap();
    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::Prompt { session, .. }) if *session == draft
    ));
}

#[test]
fn the_rejection_of_an_earlier_pick_keeps_the_latest_one() {
    let (_dir, mut fixture, project, draft) = seeded_opus_answered_on_sonnet();
    fixture.app.draft_model(value("haiku")).unwrap();
    send(&mut fixture, &draft, rejected("effort", "low"));
    assert_eq!(
        shown(&fixture.app.draft().unwrap().model),
        Some(value("haiku"))
    );

    send(&mut fixture, &draft, rejected("model", "opus"));

    assert_eq!(
        shown(&fixture.app.draft().unwrap().model),
        Some(value("haiku"))
    );

    send(
        &mut fixture,
        &draft,
        Update::ConfigOptions(agent_options("haiku")),
    );

    assert_eq!(
        shown(&fixture.app.draft().unwrap().model),
        Some(value("haiku"))
    );
    let calls = fixture.backend.calls().len();
    fixture.app.create_from_draft().unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(
        saved(&fixture.store, &project, &draft).model,
        Some(value("haiku"))
    );
}

#[test]
fn a_rejection_of_the_latest_pick_falls_back_to_the_model_the_agent_runs() {
    let (_dir, mut fixture, _project, draft) = seeded_opus_answered_on_sonnet();
    fixture.app.draft_model(value("haiku")).unwrap();

    send(&mut fixture, &draft, rejected("model", "haiku"));

    assert_eq!(
        shown(&fixture.app.draft().unwrap().model),
        Some(value("sonnet"))
    );
}

#[test]
fn a_model_change_rejected_after_create_falls_back_to_the_agents_model() {
    let (_dir, mut fixture, project, draft) = seeded_opus_answered_on_sonnet();
    let calls = fixture.backend.calls().len();
    fixture.app.create_from_draft().unwrap();

    send(&mut fixture, &draft, rejected("model", "opus"));

    assert_eq!(fixture.backend.calls().len(), calls);
    let session = saved(&fixture.store, &project, &draft);
    assert_eq!(session.model, Some(value("sonnet")));
    assert_eq!(session.effort, Some(value("high")));

    send(&mut fixture, &draft, Update::ConfigOptions(sonnet_on_low()));
    assert_eq!(fixture.backend.calls().len(), calls);
    assert_eq!(
        saved(&fixture.store, &project, &draft).effort,
        Some(value("low"))
    );
}

#[test]
fn a_seeded_model_the_agent_runs_already_or_does_not_list_is_not_set() {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let (backends, _) = single_claude();
    for seed in ["sonnet", "gone"] {
        let seed = seeded(&project, seed, "high");
        let mut fixture = stored_fixture(&project, &[seed], backends.clone(), "claude");
        fixture
            .app
            .prepare_session(project.id.clone(), Scope::Project)
            .unwrap();
        let calls = fixture.backend.calls().len();

        answer(&mut fixture, agent_options("sonnet"));

        assert_eq!(fixture.backend.calls().len(), calls);
    }
}

#[test]
fn a_seeded_model_that_cannot_be_set_is_sent_again_on_create() {
    let (_dir, mut fixture, project) = {
        let dir = project_dir(&[]);
        let project = discover_project(dir.path()).unwrap();
        let (backends, _) = single_claude();
        let seed = seeded(&project, "opus", "xhigh");
        let fixture = stored_fixture(&project, &[seed], backends, "claude");
        (dir, fixture, project.id)
    };
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let (request, draft) = warm_up(&fixture.backend);

    fixture.backend.fail_next_call();
    let failed = fixture.app.apply_event(BackendEvent::SessionCreated {
        request,
        session: draft.clone(),
        backend_session: BackendSessionId::new("warm"),
        config: agent_options("sonnet"),
    });

    assert!(matches!(failed, Err(AppError::Backend(_))));
    let view = fixture.app.draft().unwrap();
    assert_eq!(view.warmup, Warmup::Ready);
    assert_eq!(shown(&view.model), Some(value("opus")));
    let calls = fixture.backend.calls().len();

    fixture.app.create_from_draft().unwrap();

    assert_eq!(
        fixture.backend.calls()[calls..],
        [set(&draft, "model", "opus")]
    );
    assert_eq!(
        saved(&fixture.store, &project, &draft).model,
        Some(value("opus"))
    );
}

#[test]
fn a_create_whose_choices_cannot_be_sent_still_selects_the_session() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();
    let draft = answer(&mut fixture, vec![effort("low", &["low", "high"])]);
    fixture.app.draft_effort(value("high")).unwrap();

    fixture.backend.fail_next_call();
    let failed = fixture.app.create_from_draft();

    assert!(matches!(failed, Err(AppError::Backend(_))));
    assert_eq!(fixture.app.current_session(), Some(draft.clone()));
    assert_eq!(fixture.app.draft(), None);
    assert_eq!(saved(&fixture.store, &project, &draft).id, draft);
}

#[test]
fn an_orchestrator_draft_becomes_an_orchestrator_session() {
    let (_dir, mut fixture, project) = open_project(&["api", "web"]);
    fixture
        .app
        .prepare_session(project.clone(), Scope::Project)
        .unwrap();

    let created = fixture.app.create_from_draft().unwrap();

    let session = saved(&fixture.store, &project, &created);
    assert!(session.orchestrator);
    assert_eq!(session.scope, Scope::Project);
}

#[test]
fn a_failed_draft_is_not_created() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture.backend.fail_next_call();
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();

    let failed = fixture.app.create_from_draft();

    assert!(matches!(failed, Err(AppError::DraftFailed)));
    assert!(fixture.app.draft().is_some());
    assert!(fixture.app.tree().projects[0].sessions.is_empty());
}

#[test]
fn a_store_failure_on_create_keeps_the_draft() {
    let (_dir, mut fixture, project) = open_project(&[]);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    answer(&mut fixture, Vec::new());

    fixture.store.fail_next_session_save();
    let failed = fixture.app.create_from_draft();

    assert!(matches!(failed, Err(AppError::Store(_))));
    assert!(fixture.app.tree().projects[0].sessions.is_empty());
    assert_eq!(fixture.app.draft().unwrap().warmup, Warmup::Ready);
    assert_eq!(fixture.app.current_session(), None);
    let created = fixture.app.create_from_draft().unwrap();
    assert_eq!(fixture.app.current_session(), Some(created));
}

#[test]
fn every_draft_operation_needs_a_draft() {
    let (_dir, mut fixture, _project) = open_project(&[]);
    let app = &mut fixture.app;

    assert!(matches!(
        app.draft_backend(backend_id("claude")),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(
        app.draft_model(value("opus")),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(
        app.draft_effort(value("high")),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(app.draft_auto_allow(true), Err(AppError::NoDraft)));
    assert!(matches!(app.create_from_draft(), Err(AppError::NoDraft)));
    assert!(matches!(app.discard_draft(), Err(AppError::NoDraft)));
    assert_eq!(app.draft(), None);
    assert!(fixture.backend.calls().is_empty());
}
