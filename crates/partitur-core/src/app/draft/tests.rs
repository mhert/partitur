#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::app::git_work::Work;
use crate::app::{App, AppError, Created};
use crate::archive::Archived;
use crate::backend::{
    BackendError, BackendEvent, ConfigCategory, ConfigChoice, ConfigOption, OrchestratorTool,
    Update,
};
use crate::git::{GitError, GitStep, Head, RepoInfo};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, ProjectId, RepositoryId,
    RequestId, SessionId, ToolRequestId,
};
use crate::project::{Project, Repository, discover_project};
use crate::session::{BranchChoice, BranchMode, Scope, Session, Status, Workspace};
use crate::settings::{GlobalSettings, Level, ProjectSettings, RepositorySettings, SettingsLayer};
use crate::store::Store as _;
use crate::testing::{
    Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, settle,
    single_claude,
};
use crate::transcript::Entry;
use crate::view_model::{BranchView, ChoiceView, DraftChoice, Warmup};

use super::super::tests::agent_options;

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
    git: MemoryGit,
}

fn fixture_with(store: MemoryStore) -> Fixture {
    let (backends, default) = single_claude();
    fixture_on(store, backends, default)
}

/// A fixture whose app knows `backends` and gives new sessions `default`.
fn fixture_on(store: MemoryStore, backends: Vec<BackendInfo>, default: BackendId) -> Fixture {
    fixture_global(store, backends, default, GlobalSettings::default())
}

/// A fixture like [`fixture_on`] whose app has the `global` settings.
fn fixture_global(
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

/// Creates the session being prepared, which needs no git work, and returns its id.
fn create(fixture: &mut Fixture) -> SessionId {
    match fixture.app.create_from_draft().unwrap() {
        Created::Session(id) => id,
        Created::Pending => panic!("the create waits for git"),
    }
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
    assert_eq!(create(&mut fixture), session);
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

    let created = create(&mut fixture);

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

    let created = create(&mut fixture);

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

    let created = create(&mut fixture);

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
    let created = create(&mut fixture);
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

// The branch box.

fn api_id() -> RepositoryId {
    RepositoryId::try_from("api").unwrap()
}

/// The folder of the repository `name` in the project at `dir`, as the project records it.
fn repo_path(dir: &TempDir, name: &str) -> PathBuf {
    fs::canonicalize(dir.path()).unwrap().join(name)
}

/// What git says about `api`: `main` checked out, `main` and `dev`, and `origin/main`.
fn api_info(path: &Path) -> RepoInfo {
    repo_info(path, "main", &["main", "dev"], &["origin/main"])
}

/// A project with the repository `api`, which git knows as [`api_info`] says, and the sheet
/// opened for an `api` session; git has not answered yet.
fn api_draft() -> (TempDir, Fixture, PathBuf) {
    let (dir, mut fixture, project) = open_project(&["api"]);
    let api = repo_path(&dir, "api");
    fixture.git.add_repository(api.clone(), api_info(&api));
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();
    (dir, fixture, api)
}

/// [`api_draft`] with git's answer applied.
fn settled_api_draft() -> (TempDir, Fixture, PathBuf) {
    let (dir, mut fixture, api) = api_draft();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    (dir, fixture, api)
}

fn branch(fixture: &Fixture) -> BranchView {
    fixture.app.draft().unwrap().branch
}

fn blocker(fixture: &Fixture) -> Option<String> {
    branch(fixture).blocker
}

fn can_create(fixture: &Fixture) -> bool {
    fixture.app.draft().unwrap().can_create()
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

const ALL_MODES: [BranchMode; 3] = [
    BranchMode::Worktree,
    BranchMode::NewBranch,
    BranchMode::CheckedOut,
];

#[test]
fn the_sheet_opens_at_once_and_reads_the_repository_in_the_background() {
    let (_dir, mut fixture, api) = api_draft();

    assert_eq!(fixture.git.calls(), [GitCall::Inspect(api)]);
    let loading = branch(&fixture);
    assert!(loading.loading);
    assert!(loading.bases.is_empty());
    assert_eq!(loading.base, None);
    assert_eq!(loading.checked_out, None);
    assert_eq!(loading.modes, ALL_MODES);
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    assert_eq!(
        blocker(&fixture).as_deref(),
        Some("reading the branches of api…")
    );
    assert!(!can_create(&fixture));
    fixture.app.draft_mode(BranchMode::CheckedOut).unwrap();
    assert!(can_create(&fixture));

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let read = branch(&fixture);
    assert!(!read.loading);
    assert_eq!(read.bases, names(&["dev", "main"]));
    assert_eq!(read.base.as_deref(), Some("main"));
    assert_eq!(read.checked_out.as_deref(), Some("main"));
}

#[test]
fn a_name_typed_while_loading_is_kept() {
    let (dir, mut fixture, _api) = api_draft();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();

    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert_eq!(view.branch, "feat/x");
    assert_eq!(view.mode, BranchMode::Worktree);
    assert_eq!(view.worktree, Some(repo_path(&dir, "api-wt/feat/x")));
    assert_eq!(view.blocker, None);
    assert!(can_create(&fixture));
}

#[test]
fn an_answer_for_a_closed_sheet_is_ignored() {
    let (dir, mut fixture, api) = api_draft();
    let project = fixture.app.tree().projects[0].id.clone();
    fixture.app.discard_draft().unwrap();
    let first = fixture.git.answer();
    fixture
        .git
        .add_repository(api.clone(), repo_info(&api, "dev", &["main", "dev"], &[]));
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    settle(&mut fixture.app, &fixture.git);
    for event in first {
        fixture.app.apply_git_event(event).unwrap();
    }

    assert_eq!(branch(&fixture).checked_out.as_deref(), Some("dev"));
    drop(dir);
}

#[test]
fn an_answer_for_a_closed_sheet_without_a_new_one_is_ignored() {
    let (_dir, mut fixture, _api) = api_draft();
    fixture.app.discard_draft().unwrap();

    let errors = settle(&mut fixture.app, &fixture.git);

    assert!(errors.is_empty());
    assert_eq!(fixture.app.draft(), None);
}

/// The stored project with the repository `api`, its folder, and a stored session in it
/// working in `workspace`, created at `created_at`.
fn stored_api(dir: &TempDir, workspace: Workspace, created_at: u64) -> (Project, Session) {
    let project = discover_project(dir.path()).unwrap();
    let mut session = stored_session(&project, "claude", created_at);
    session.workspace = workspace;
    (project, session)
}

fn worktree(fetched: bool) -> Workspace {
    Workspace::Worktree {
        branch: "old".into(),
        base: "main".into(),
        fetched,
        path: PathBuf::from("/elsewhere/old"),
    }
}

/// A fixture over the stored `project` with `sessions`, whose git knows `api` as
/// [`api_info`] says, with the sheet opened for an `api` session.
fn stored_api_draft(project: &Project, sessions: &[Session]) -> (Fixture, PathBuf) {
    let (backends, default) = single_claude();
    let mut fixture = stored_fixture(project, sessions, backends, default.as_str());
    let api = project.repositories[0].path.clone();
    fixture.git.add_repository(api.clone(), api_info(&api));
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Repository(api_id()))
        .unwrap();
    (fixture, api)
}

#[test]
fn a_seeded_worktree_mode_falls_back_when_the_repository_cannot_be_read() {
    let dir = project_dir(&["api"]);
    let (project, seed) = stored_api(&dir, worktree(true), 1);
    let api = project.repositories[0].path.clone();
    let (backends, default) = single_claude();
    let mut fixture = stored_fixture(&project, &[seed], backends, default.as_str());
    fixture.git.add_repository(api.clone(), api_info(&api));
    fixture.git.fail_next(GitOp::Inspect, &api);
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Repository(api_id()))
        .unwrap();
    assert_eq!(branch(&fixture).mode, BranchMode::Worktree);

    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert_eq!(view.mode, BranchMode::CheckedOut);
    assert_eq!(view.modes, [BranchMode::CheckedOut]);
    assert_eq!(
        view.note.as_deref(),
        Some("cannot read the git state of api: git worktree list failed: simulated failure")
    );
    assert!(can_create(&fixture));
}

/// Queues a changing git request in `repository` from elsewhere, which keeps it busy until
/// git answers.
fn keep_busy(fixture: &mut Fixture, repository: &Path) {
    let step = GitStep::DeleteBranch {
        repository: repository.to_path_buf(),
        branch: "elsewhere".into(),
    };
    fixture
        .app
        .git_work
        .run(
            &fixture.git,
            Work::Undo {
                repositories: vec!["api".into()],
            },
            vec![step],
        )
        .unwrap();
}

#[test]
fn a_busy_repository_blocks_worktree_and_new_branch() {
    let (_dir, mut fixture, api) = settled_api_draft();
    fixture.app.draft_branch("feat/x".into()).unwrap();
    keep_busy(&mut fixture, &api);
    let busy = Some("api is busy with another branch change".to_owned());

    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    assert_eq!(blocker(&fixture), busy);
    fixture.app.draft_mode(BranchMode::NewBranch).unwrap();
    assert_eq!(blocker(&fixture), busy);
    fixture.app.draft_mode(BranchMode::CheckedOut).unwrap();
    assert_eq!(blocker(&fixture), None);
}

/// A project with the repositories `api` and `web`, which git knows as given, and the
/// orchestrator sheet opened; git has not answered yet.
fn orchestrator_draft(api: RepoInfo, web: RepoInfo, root: bool) -> (TempDir, Fixture) {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    if root {
        // Discovery stops at a root repository, so the root is added by hand, as a project
        // whose root and children are all repositories.
        let path = fs::canonicalize(dir.path()).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        fixture.app.projects[0].repositories.insert(
            0,
            Repository {
                id: RepositoryId::try_from(name.as_str()).unwrap(),
                path,
                name,
                has_instructions: false,
                has_claude_md: false,
                is_root: true,
            },
        );
    }
    fixture.git.add_repository(repo_path(&dir, "api"), api);
    fixture.git.add_repository(repo_path(&dir, "web"), web);
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    (dir, fixture)
}

fn info(current: &str, branches: &[&str], remote: &[&str]) -> RepoInfo {
    repo_info(Path::new("/unused"), current, branches, remote)
}

#[test]
fn an_orchestrator_sheet_reads_every_repository_in_one_request() {
    let (dir, mut fixture) = orchestrator_draft(
        info("main", &["main"], &[]),
        info("main", &["main"], &[]),
        false,
    );

    assert_eq!(
        fixture.git.calls(),
        [
            GitCall::Inspect(repo_path(&dir, "api")),
            GitCall::Inspect(repo_path(&dir, "web"))
        ]
    );
    assert_eq!(fixture.git.waiting(), 1);
    assert!(branch(&fixture).loading);
    assert!(can_create(&fixture));
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    assert!(can_create(&fixture));
    assert_eq!(branch(&fixture).modes, ALL_MODES);
}

#[test]
fn a_new_project_starts_checked_out_with_fetch_on_and_an_empty_name() {
    let (_dir, fixture, _api) = settled_api_draft();

    let view = branch(&fixture);

    assert_eq!(view.mode, BranchMode::CheckedOut);
    assert_eq!(view.modes, ALL_MODES);
    assert_eq!(view.fetch_remote.as_deref(), Some("origin"));
    assert!(view.fetch);
    assert_eq!(view.branch, "");
    assert_eq!(view.base.as_deref(), Some("main"));
    assert_eq!(view.checked_out.as_deref(), Some("main"));
    assert_eq!(view.note, None);
    assert_eq!(view.worktree, None);
    assert_eq!(view.blocker, None);
}

#[test]
fn the_newest_session_seeds_mode_and_fetch() {
    let dir = project_dir(&["api"]);
    let (project, newest) = stored_api(&dir, worktree(false), 5);
    let mut older = stored_session(&project, "claude", 1);
    older.workspace = Workspace::NewBranch {
        branch: "older".into(),
        base: "main".into(),
        fetched: true,
    };

    let (fixture, _api) = stored_api_draft(&project, &[newest, older]);

    let view = branch(&fixture);
    assert_eq!(view.mode, BranchMode::Worktree);
    assert!(!view.fetch);
}

#[test]
fn a_checked_out_newest_session_seeds_fetching() {
    let dir = project_dir(&["api"]);
    let (project, newest) = stored_api(&dir, Workspace::CheckedOut, 5);

    let (mut fixture, _api) = stored_api_draft(&project, &[newest]);
    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert_eq!(view.mode, BranchMode::CheckedOut);
    assert!(view.fetch);
}

#[test]
fn an_orchestrators_child_defaults_seed_the_next_sheet() {
    let dir = project_dir(&["api"]);
    let (project, mut newest) = stored_api(&dir, Workspace::CheckedOut, 5);
    newest.orchestrator = true;
    newest.child_defaults = Some(BranchChoice {
        mode: BranchMode::NewBranch,
        branch: "kept/out".into(),
        base: Some("dev".into()),
        fetch: false,
    });

    let (fixture, _api) = stored_api_draft(&project, &[newest]);

    let view = branch(&fixture);
    assert_eq!(view.mode, BranchMode::NewBranch);
    assert!(!view.fetch);
    assert_eq!(view.branch, "");
}

fn layer(pattern: &str, level: Level) -> SettingsLayer {
    let mut layer = SettingsLayer::default();
    layer.set(level, "worktree_path", pattern).unwrap();
    layer
}

#[test]
fn the_worktree_line_resolves_the_pattern_of_the_most_specific_layer() {
    let dir = project_dir(&["api"]);
    let project = discover_project(dir.path()).unwrap();
    let store = store_with(&project, &[]);
    let mut settings = ProjectSettings {
        project: layer("/p/{project}/{branch}", Level::Project),
        ..ProjectSettings::default()
    };
    settings.repositories.insert(
        api_id(),
        RepositorySettings {
            layer: layer("/r/{repo}/{branch}", Level::Repository),
            ..RepositorySettings::default()
        },
    );
    store.set_settings(project.id.clone(), settings);
    let (backends, default) = single_claude();
    let mut fixture = fixture_on(store, backends, default);
    let api = project.repositories[0].path.clone();
    fixture.git.add_repository(api.clone(), api_info(&api));
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Repository(api_id()))
        .unwrap();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();

    assert_eq!(
        branch(&fixture).worktree,
        Some(PathBuf::from("/r/api/feat/x"))
    );

    let (dir, mut plain, _api) = settled_api_draft();
    plain.app.draft_mode(BranchMode::Worktree).unwrap();
    plain.app.draft_branch("feat/x".into()).unwrap();
    assert_eq!(
        branch(&plain).worktree,
        Some(repo_path(&dir, "api-wt/feat/x"))
    );
}

#[test]
fn the_global_layer_and_home_folder_apply_without_project_settings() {
    let dir = project_dir(&["api"]);
    let project = discover_project(dir.path()).unwrap();
    let global = GlobalSettings {
        layer: layer("~/wt/{project}/{repo}/{branch}", Level::Global),
        home: Some(PathBuf::from("/home/me")),
    };
    let (backends, default) = single_claude();
    let mut fixture = fixture_global(store_with(&project, &[]), backends, default, global);
    fixture
        .app
        .prepare_session(project.id.clone(), Scope::Repository(api_id()))
        .unwrap();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();

    let expected = PathBuf::from("/home/me/wt")
        .join(&project.name)
        .join("api/feat/x");
    assert_eq!(branch(&fixture).worktree, Some(expected));
}

#[test]
fn each_reason_blocks_create_with_its_text() {
    let (dir, mut fixture, _api) = settled_api_draft();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    let existing = repo_path(&dir, "api-wt/feat/y");
    fs::create_dir_all(&existing).unwrap();
    let cases = [
        ("", Some("enter a branch name".to_owned())),
        (
            "a b",
            Some("\"a b\" is not a valid git branch name".to_owned()),
        ),
        ("dev", Some("branch dev already exists in api".to_owned())),
        (
            "feat/y",
            Some(format!("{} already exists", existing.display())),
        ),
        ("feat/x", None),
    ];

    for (name, expected) in cases {
        fixture.app.draft_branch(name.into()).unwrap();
        assert_eq!(blocker(&fixture), expected, "{name}");
        assert_eq!(can_create(&fixture), expected.is_none(), "{name}");
    }
}

#[test]
fn a_repository_without_a_branch_to_start_from_blocks_a_new_branch() {
    let (dir, mut fixture, project) = open_project(&["api"]);
    let api = repo_path(&dir, "api");
    let mut detached = repo_info(&api, "main", &[], &[]);
    detached.worktrees[0].head = Head::Detached("abc123".into());
    fixture.git.add_repository(api, detached);
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();
    settle(&mut fixture.app, &fixture.git);
    fixture.app.draft_mode(BranchMode::NewBranch).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();

    let view = branch(&fixture);
    assert_eq!(view.checked_out, None);
    assert_eq!(view.base, None);
    assert_eq!(
        view.blocker.as_deref(),
        Some("api has no branch to start from")
    );
}

#[test]
fn a_name_with_surrounding_spaces_is_trimmed() {
    let (dir, mut fixture, _api) = settled_api_draft();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();

    fixture.app.draft_branch(" feat/x ".into()).unwrap();

    let view = branch(&fixture);
    assert_eq!(view.blocker, None);
    assert_eq!(view.branch, " feat/x ");
    assert_eq!(view.worktree, Some(repo_path(&dir, "api-wt/feat/x")));
}

#[test]
fn new_branch_is_refused_while_another_live_session_works_in_the_checkout() {
    let dir = project_dir(&["api"]);
    let project = discover_project(dir.path()).unwrap();
    let api = project.repositories[0].path.clone();
    let mut login = stored_session(&project, "claude", 1);
    login.title = "Fix login".into();
    login.scope = Scope::Repository(api_id());
    login.cwd = api.clone();
    let id = login.id.clone();
    let (mut fixture, _api) = stored_api_draft(&project, &[login]);
    settle(&mut fixture.app, &fixture.git);
    fixture.app.draft_mode(BranchMode::NewBranch).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();

    assert_eq!(
        blocker(&fixture).as_deref(),
        Some("api is in use by \"Fix login\" — use a worktree")
    );
    assert_eq!(
        branch(&fixture).note.as_deref(),
        Some("switches api itself to the new branch")
    );
    fixture.app.draft_mode(BranchMode::CheckedOut).unwrap();
    assert_eq!(blocker(&fixture), None);
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    assert_eq!(blocker(&fixture), None);

    let session = |fixture: &mut Fixture| {
        fixture
            .app
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .unwrap()
            .clone()
    };
    let mut archived = session(&mut fixture);
    archived.archived = Some(Archived { at: 1 });
    let mut elsewhere = session(&mut fixture);
    elsewhere.cwd = repo_path(&dir, "api-wt/other");
    for changed in [archived, elsewhere] {
        fixture.app.sessions = vec![changed];
        fixture.app.draft_mode(BranchMode::NewBranch).unwrap();
        assert_eq!(blocker(&fixture), None);
    }
}

#[test]
fn fetch_cannot_be_on_without_a_remote_counterpart() {
    let (_dir, mut fixture, _api) = settled_api_draft();

    fixture.app.draft_base("dev".into()).unwrap();
    let dev = branch(&fixture);
    assert_eq!(dev.base.as_deref(), Some("dev"));
    assert_eq!(dev.fetch_remote, None);
    assert!(!dev.fetch);
    assert!(matches!(
        fixture.app.draft_fetch(true),
        Err(AppError::Workspace(_))
    ));
    fixture.app.draft_fetch(false).unwrap();
    fixture.app.draft_base("main".into()).unwrap();
    assert!(!branch(&fixture).fetch);
    fixture.app.draft_fetch(true).unwrap();
    assert!(branch(&fixture).fetch);
    assert_eq!(branch(&fixture).fetch_remote.as_deref(), Some("origin"));
}

#[test]
fn a_fetch_wish_kept_through_a_base_without_a_remote_applies_again() {
    let (_dir, mut fixture, _api) = settled_api_draft();

    fixture.app.draft_base("dev".into()).unwrap();
    assert!(!branch(&fixture).fetch);
    fixture.app.draft_base("main".into()).unwrap();

    assert!(branch(&fixture).fetch);
}

#[test]
fn a_fetch_wish_set_while_loading_is_kept() {
    let (_dir, mut fixture, _api) = api_draft();

    fixture.app.draft_fetch(false).unwrap();
    assert!(!branch(&fixture).fetch);
    fixture.app.draft_fetch(true).unwrap();
    assert!(branch(&fixture).fetch);
    fixture.app.draft_fetch(false).unwrap();
    settle(&mut fixture.app, &fixture.git);

    assert!(!branch(&fixture).fetch);
}

#[test]
fn only_a_listed_base_can_be_picked() {
    let (_dir, mut fixture, _api) = api_draft();
    let refused = |fixture: &mut Fixture, base: &str| {
        matches!(
            fixture.app.draft_base(base.into()),
            Err(AppError::Workspace(_))
        )
    };

    assert!(refused(&mut fixture, "main"));
    settle(&mut fixture.app, &fixture.git);
    assert!(refused(&mut fixture, "nope"));
    assert_eq!(branch(&fixture).base.as_deref(), Some("main"));
    assert_eq!(
        fixture
            .app
            .draft_base("nope".into())
            .unwrap_err()
            .to_string(),
        "cannot use these branch settings: nope is not a branch to start from"
    );
}

#[test]
fn a_folder_project_offers_only_checked_out() {
    let (_dir, mut fixture, project) = open_project(&["api"]);

    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();

    let view = branch(&fixture);
    assert_eq!(view.modes, [BranchMode::CheckedOut]);
    assert_eq!(
        view.note.as_deref(),
        Some("the project folder is not a repository")
    );
    assert!(!view.loading);
    assert!(fixture.git.calls().is_empty());
    let refused = fixture.app.draft_mode(BranchMode::Worktree).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "cannot use these branch settings: Worktree is not offered for this session"
    );
    assert!(can_create(&fixture));
}

#[test]
fn an_unreadable_repository_offers_only_checked_out_and_says_why() {
    let (_dir, mut fixture, project) = open_project(&["api"]);
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert_eq!(view.modes, [BranchMode::CheckedOut]);
    assert_eq!(
        view.note.as_deref(),
        Some("cannot read the git state of api: git worktree list failed: not a git repository")
    );
    assert!(view.bases.is_empty());
    assert!(matches!(
        fixture.app.draft_mode(BranchMode::NewBranch),
        Err(AppError::Workspace(_))
    ));
}

#[test]
fn a_git_port_that_refuses_the_read_fails_it_at_once() {
    let (_dir, mut fixture, project) = open_project(&["api"]);
    fixture.git.refuse_next_request();

    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    let view = branch(&fixture);
    assert!(!view.loading);
    assert_eq!(view.modes, [BranchMode::CheckedOut]);
    assert!(
        view.note
            .unwrap()
            .starts_with("cannot read the git state of api: git is unavailable")
    );
    assert_eq!(fixture.git.waiting(), 0);
}

#[test]
fn an_orchestrator_offers_the_branches_every_repository_has() {
    let (_dir, mut fixture) = orchestrator_draft(
        info("main", &["main", "dev"], &["origin/main"]),
        info("main", &["main"], &["origin/main"]),
        false,
    );
    settle(&mut fixture.app, &fixture.git);
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();

    let view = branch(&fixture);
    assert_eq!(view.bases, names(&["main"]));
    assert_eq!(view.base.as_deref(), Some("main"));
    assert_eq!(view.fetch_remote.as_deref(), Some("origin"));
    assert_eq!(view.checked_out, None);
    assert_eq!(view.worktree, None);
    assert_eq!(view.note, None);
    assert_eq!(view.blocker, None);
    fixture.app.draft_branch("a b".into()).unwrap();
    assert_eq!(
        blocker(&fixture).as_deref(),
        Some("\"a b\" is not a valid git branch name")
    );
    fixture.app.draft_branch("feat/x".into()).unwrap();
    assert_eq!(branch(&fixture).worktree, None);
    assert_eq!(blocker(&fixture), None);

    let (_dir, mut none) = orchestrator_draft(
        info("main", &["main"], &[]),
        info("trunk", &["trunk"], &[]),
        false,
    );
    settle(&mut none.app, &none.git);
    none.app.draft_mode(BranchMode::Worktree).unwrap();
    let view = branch(&none);
    assert!(view.bases.is_empty());
    assert_eq!(view.base, None);
    assert_eq!(view.blocker, None);
    assert!(can_create(&none));
}

#[test]
fn an_orchestrator_starts_from_the_branch_its_repositories_agree_on() {
    let both = &["dev", "main"];
    let (_dir, mut agreed) =
        orchestrator_draft(info("main", both, &[]), info("main", both, &[]), false);
    let (_dir2, mut differ) =
        orchestrator_draft(info("main", both, &[]), info("dev", both, &[]), false);
    let (_dir3, mut uncommon) = orchestrator_draft(
        info("feat", &["feat", "main"], &[]),
        info("feat", &["main"], &[]),
        false,
    );

    for fixture in [&mut agreed, &mut differ, &mut uncommon] {
        settle(&mut fixture.app, &fixture.git);
    }

    assert_eq!(branch(&agreed).base.as_deref(), Some("main"));
    assert_eq!(branch(&differ).base.as_deref(), Some("dev"));
    assert_eq!(branch(&uncommon).base.as_deref(), Some("main"));
}

#[test]
fn an_orchestrator_fetches_only_when_every_repository_has_the_remote_base() {
    let (_dir, mut fixture) = orchestrator_draft(
        info("main", &["main"], &["origin/main"]),
        info("main", &["main"], &[]),
        false,
    );
    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert_eq!(view.fetch_remote, None);
    assert!(!view.fetch);
}

#[test]
fn an_orchestrator_whose_repository_cannot_be_read_has_no_bases() {
    let (dir, mut fixture) = orchestrator_draft(
        info("main", &["main"], &[]),
        info("main", &["main"], &[]),
        false,
    );
    fixture
        .git
        .fail_next(GitOp::Inspect, &repo_path(&dir, "web"));

    settle(&mut fixture.app, &fixture.git);

    let view = branch(&fixture);
    assert!(!view.loading);
    assert_eq!(view.modes, ALL_MODES);
    assert!(view.bases.is_empty());
    assert_eq!(view.base, None);
}

#[test]
fn invalid_settings_block_create_with_their_error() {
    let (_dir, mut fixture, project) = open_project(&["api"]);
    fixture.store.fail_next_settings_load();

    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    let text = blocker(&fixture).unwrap();
    assert!(text.contains("simulated failure"), "{text}");
    assert!(!can_create(&fixture));
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();
    assert_eq!(branch(&fixture).worktree, None);
    assert_eq!(blocker(&fixture), Some(text));
}

#[test]
fn a_failed_warm_up_cannot_be_created_whatever_the_branch_box_says() {
    let (_dir, mut fixture, project) = open_project(&["api"]);
    fixture.backend.fail_next_call();

    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    assert_eq!(blocker(&fixture), None);
    assert!(!can_create(&fixture));
}

#[test]
fn the_root_repository_of_a_project_session_is_its_target() {
    let dir = project_dir(&[]);
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fixture.git.add_repository(root.clone(), api_info(&root));

    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();
    settle(&mut fixture.app, &fixture.git);

    assert_eq!(fixture.git.calls(), [GitCall::Inspect(root)]);
    let view = branch(&fixture);
    assert_eq!(view.modes, ALL_MODES);
    assert_eq!(view.checked_out.as_deref(), Some("main"));
}

#[test]
fn an_orchestrator_is_not_retargeted_to_the_root_repository() {
    let (dir, mut fixture) = orchestrator_draft(
        info("main", &["main", "dev"], &[]),
        info("main", &["main", "web-only"], &[]),
        true,
    );
    assert!(fixture.app.draft().unwrap().orchestrator);
    settle(&mut fixture.app, &fixture.git);
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();

    assert_eq!(
        fixture.git.calls(),
        [
            GitCall::Inspect(repo_path(&dir, "api")),
            GitCall::Inspect(repo_path(&dir, "web"))
        ]
    );
    let view = branch(&fixture);
    assert_eq!(view.bases, names(&["main"]));
    assert_eq!(view.worktree, None);
    assert_eq!(view.blocker, None);
}

#[test]
fn changing_the_backend_keeps_the_branch_box() {
    let (_dir, mut fixture, _api) = settled_api_draft();
    fixture.app.draft_branch("feat/x".into()).unwrap();
    let before = branch(&fixture);

    fixture.app.draft_backend(backend_id("claude")).unwrap();

    assert_eq!(branch(&fixture), before);
}

#[test]
fn every_branch_operation_needs_a_draft() {
    let (_dir, mut fixture, _project) = open_project(&[]);
    let app = &mut fixture.app;

    assert!(matches!(
        app.draft_mode(BranchMode::Worktree),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(
        app.draft_branch("x".into()),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(
        app.draft_base("main".into()),
        Err(AppError::NoDraft)
    ));
    assert!(matches!(app.draft_fetch(true), Err(AppError::NoDraft)));
}

#[test]
fn a_git_port_error_reads_as_the_ports_error() {
    let error = AppError::from(GitError::Unavailable("stopped".into()));

    assert_eq!(error.to_string(), "git is unavailable: stopped");
}

#[test]
fn an_answer_for_a_closed_orchestrator_sheet_is_ignored() {
    let both = &["dev", "main"];
    let (dir, mut fixture) =
        orchestrator_draft(info("main", both, &[]), info("main", both, &[]), false);
    let project = fixture.app.tree().projects[0].id.clone();
    fixture.app.discard_draft().unwrap();
    let first = fixture.git.answer();
    for name in ["api", "web"] {
        fixture
            .git
            .add_repository(repo_path(&dir, name), info("dev", both, &[]));
    }
    fixture
        .app
        .prepare_session(project, Scope::Project)
        .unwrap();

    settle(&mut fixture.app, &fixture.git);
    for event in first {
        fixture.app.apply_git_event(event).unwrap();
    }

    assert_eq!(branch(&fixture).base.as_deref(), Some("dev"));
}

// Creating the session's branch or worktree.

const BUSY: &str = "api is busy with another branch change";

/// [`settled_api_draft`] in `mode`, naming the new branch `feat/x`.
fn api_draft_in(mode: BranchMode) -> (TempDir, Fixture, PathBuf) {
    let (dir, mut fixture, api) = settled_api_draft();
    fixture.app.draft_mode(mode).unwrap();
    fixture.app.draft_branch("feat/x".into()).unwrap();
    (dir, fixture, api)
}

/// Presses Create, which queues git work, and returns the warm session's id.
fn create_pending(fixture: &mut Fixture) -> SessionId {
    let warm = fixture.app.draft.as_ref().unwrap().id.clone();
    assert_eq!(fixture.app.create_from_draft().unwrap(), Created::Pending);
    warm
}

/// Presses Create and answers its fresh reading of the repository, so the git work is queued;
/// returns the warm session's id.
fn create_running(fixture: &mut Fixture) -> SessionId {
    let warm = create_pending(fixture);
    for event in fixture.git.answer() {
        fixture.app.apply_git_event(event).unwrap();
    }
    assert!(fixture.app.draft().unwrap().creating);
    warm
}

fn project_of(fixture: &Fixture) -> ProjectId {
    fixture.app.tree().projects[0].id.clone()
}

fn no_sessions(fixture: &Fixture) -> bool {
    fixture.app.tree().projects[0].sessions.is_empty()
}

fn failure(fixture: &Fixture) -> Option<String> {
    fixture.app.draft().unwrap().failure
}

fn closed(fixture: &Fixture, session: &SessionId) -> bool {
    fixture.backend.calls().contains(&Call::CloseSession {
        session: session.clone(),
    })
}

fn the_session_is_being_created(result: Result<(), AppError>) -> bool {
    matches!(result, Err(AppError::Workspace(text)) if text == "the session is being created")
}

fn worktree_steps(api: &Path, path: &Path) -> [GitCall; 2] {
    [
        GitCall::RemoveWorktree {
            repository: api.to_path_buf(),
            path: path.to_path_buf(),
        },
        GitCall::DeleteBranch {
            repository: api.to_path_buf(),
            branch: "feat/x".into(),
        },
    ]
}

#[test]
fn create_in_worktree_mode_queues_the_git_work_and_waits() {
    let (dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    let calls = fixture.backend.calls();

    create_pending(&mut fixture);

    assert_eq!(
        fixture.git.calls().last(),
        Some(&GitCall::Inspect(api.clone()))
    );
    assert!(fixture.app.draft().unwrap().creating);
    for event in fixture.git.answer() {
        fixture.app.apply_git_event(event).unwrap();
    }
    assert!(fixture.git.calls().ends_with(&[
        GitCall::Fetch {
            repository: api.clone(),
            remote: "origin".into(),
        },
        GitCall::AddWorktree {
            repository: api,
            path: repo_path(&dir, "api-wt/feat/x"),
            branch: "feat/x".into(),
            start: "origin/main".into(),
        },
    ]));
    let draft = fixture.app.draft().unwrap();
    assert!(draft.creating);
    assert!(!draft.can_create());
    assert_eq!(draft.failure, None);
    assert!(no_sessions(&fixture));
    assert_eq!(fixture.backend.calls(), calls);
}

#[test]
fn the_worktree_session_starts_there_once_git_reports_back() {
    let (dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    answer(&mut fixture, vec![model("sonnet", &["sonnet", "opus"])]);
    fixture.app.draft_model(value("opus")).unwrap();
    let warm = create_pending(&mut fixture);
    let before = fixture.backend.calls().len();

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let path = repo_path(&dir, "api-wt/feat/x");
    assert_eq!(fixture.app.draft(), None);
    let calls = fixture.backend.calls();
    let [close, start] = &calls[before..] else {
        panic!("{calls:?}");
    };
    assert_eq!(
        close,
        &Call::CloseSession {
            session: warm.clone()
        }
    );
    let (request, fresh, cwd) = start.as_new_session().unwrap();
    assert_ne!(fresh, &warm);
    assert_eq!(cwd, path);
    let fresh = fresh.clone();
    assert_eq!(fixture.app.current_session(), Some(fresh.clone()));
    let session = saved(&fixture.store, &project_of(&fixture), &fresh);
    assert_eq!(session.cwd, path);
    assert_eq!(session.backend_session, None);
    assert_eq!(
        session.workspace,
        Workspace::Worktree {
            branch: "feat/x".into(),
            base: "main".into(),
            fetched: true,
            path,
        }
    );

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: fresh.clone(),
            backend_session: BackendSessionId::new("fresh"),
            config: vec![model("sonnet", &["sonnet", "opus"])],
        })
        .unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&set(&fresh, "model", "opus"))
    );
}

#[test]
fn late_events_from_the_closed_warm_session_are_ignored() {
    let (_dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    let (request, warm) = warm_up(&fixture.backend);
    create_pending(&mut fixture);
    settle(&mut fixture.app, &fixture.git);
    let tree = fixture.app.tree();
    let calls = fixture.backend.calls();

    let late = [
        BackendEvent::SessionCreated {
            request,
            session: warm.clone(),
            backend_session: BackendSessionId::new("warm"),
            config: vec![model("sonnet", &["sonnet"])],
        },
        BackendEvent::Update {
            session: warm.clone(),
            update: Update::TurnEnded(crate::backend::StopReason::EndTurn),
        },
        BackendEvent::Update {
            session: warm.clone(),
            update: Update::Notice("late".into()),
        },
        BackendEvent::SessionClosed {
            session: warm,
            error: "gone".into(),
        },
    ];
    for event in late {
        fixture.app.apply_event(event).unwrap();
    }

    assert_eq!(fixture.app.tree(), tree);
    assert_eq!(fixture.backend.calls(), calls);
}

#[test]
fn create_in_new_branch_mode_switches_the_checkout_and_adopts_the_warm_session() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::NewBranch);
    fixture.app.draft_fetch(false).unwrap();

    let warm = create_running(&mut fixture);

    assert_eq!(
        fixture.git.calls().last(),
        Some(&GitCall::CreateBranch {
            repository: api.clone(),
            branch: "feat/x".into(),
            start: "main".into(),
        })
    );
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert_eq!(fixture.app.draft(), None);
    assert_eq!(fixture.app.current_session(), Some(warm.clone()));
    assert!(!closed(&fixture, &warm));
    let session = saved(&fixture.store, &project_of(&fixture), &warm);
    assert_eq!(session.cwd, api);
    assert_eq!(
        session.workspace,
        Workspace::NewBranch {
            branch: "feat/x".into(),
            base: "main".into(),
            fetched: false,
        }
    );
    let repository = fixture.git.repository(&api).unwrap();
    assert_eq!(repository.current_branch(), Some("feat/x"));
}

#[test]
fn create_checked_out_touches_no_git_and_creates_at_once() {
    let (_dir, mut fixture, api) = settled_api_draft();

    let created = create(&mut fixture);

    assert_eq!(fixture.git.calls(), [GitCall::Inspect(api)]);
    assert_eq!(fixture.app.draft(), None);
    let session = saved(&fixture.store, &project_of(&fixture), &created);
    assert_eq!(session.workspace, Workspace::CheckedOut);
    assert_eq!(session.child_defaults, None);
}

#[test]
fn the_draft_is_frozen_while_creating() {
    let (_dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    answer(&mut fixture, vec![model("sonnet", &["sonnet", "opus"])]);
    create_pending(&mut fixture);
    let view = fixture.app.draft();
    let calls = fixture.backend.calls();
    let app = &mut fixture.app;

    assert!(the_session_is_being_created(app.draft_branch("y".into())));
    assert!(the_session_is_being_created(
        app.draft_mode(BranchMode::CheckedOut)
    ));
    assert!(the_session_is_being_created(app.draft_base("dev".into())));
    assert!(the_session_is_being_created(app.draft_fetch(false)));
    assert!(the_session_is_being_created(
        app.draft_backend(backend_id("claude"))
    ));
    assert!(the_session_is_being_created(app.draft_model(value("opus"))));
    assert!(the_session_is_being_created(
        app.draft_effort(value("high"))
    ));
    assert!(the_session_is_being_created(app.draft_auto_allow(true)));
    assert!(the_session_is_being_created(
        app.create_from_draft().map(drop)
    ));

    assert_eq!(fixture.app.draft(), view);
    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(fixture.git.waiting(), 1);
}

#[test]
fn a_failed_git_step_creates_nothing_and_keeps_the_sheet() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.git.fail_next(GitOp::AddWorktree, &api);
    let warm = create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    let text = draft.failure.clone().unwrap();
    assert!(text.contains("worktree add"), "{text}");
    assert_eq!(draft.branch.blocker, None);
    assert!(draft.can_create());
    assert!(no_sessions(&fixture));
    assert!(!closed(&fixture, &warm));

    create_pending(&mut fixture);
    assert_eq!(failure(&fixture), None);
}

#[test]
fn a_failed_fetch_creates_nothing() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.git.fail_next(GitOp::Fetch, &api);
    create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
    assert!(failure(&fixture).unwrap().contains("fetch"));
    assert!(no_sessions(&fixture));

    fixture.app.draft_branch("feat/x".into()).unwrap();
    assert_eq!(failure(&fixture), None);
}

#[test]
fn a_refused_queue_leaves_the_draft_as_it_was() {
    let (_dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    let view = fixture.app.draft();
    fixture.git.refuse_next_request();

    let refused = fixture.app.create_from_draft();

    assert!(matches!(
        refused,
        Err(AppError::Git(GitError::Unavailable(_)))
    ));
    assert_eq!(fixture.app.draft(), view);
    assert!(can_create(&fixture));
    assert_eq!(fixture.git.waiting(), 0);
}

#[test]
fn a_failed_save_undoes_the_worktree() {
    let (dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.store.fail_next_session_save();
    let warm = create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let path = repo_path(&dir, "api-wt/feat/x");
    assert!(fixture.git.calls().ends_with(&worktree_steps(&api, &path)));
    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    assert!(draft.failure.unwrap().contains("sessions.json"));
    assert!(no_sessions(&fixture));
    assert!(!closed(&fixture, &warm));
    assert_eq!(fixture.app.current_session(), None);
}

#[test]
fn a_failed_save_switches_a_new_branch_back() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::NewBranch);
    fixture.app.draft_fetch(false).unwrap();
    fixture.store.fail_next_session_save();
    create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(fixture.git.calls().ends_with(&[
        GitCall::CreateBranch {
            repository: api.clone(),
            branch: "feat/x".into(),
            start: "main".into(),
        },
        GitCall::RestoreHead {
            repository: api.clone(),
            head: Head::Branch("main".into()),
        },
        GitCall::DeleteBranch {
            repository: api.clone(),
            branch: "feat/x".into(),
        },
    ]));
    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
    assert!(failure(&fixture).is_some());
    assert!(no_sessions(&fixture));
}

#[test]
fn a_warm_up_that_failed_meanwhile_undoes_the_new_branch() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::NewBranch);
    let (request, warm) = warm_up(&fixture.backend);
    create_pending(&mut fixture);
    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: warm,
            error: "no agent".into(),
        })
        .unwrap();

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
    assert_eq!(
        failure(&fixture).as_deref(),
        Some("the agent did not start; pick another backend or cancel")
    );
    assert!(no_sessions(&fixture));
}

#[test]
fn a_backend_error_starting_the_worktree_session_is_reported() {
    let (dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    create_pending(&mut fixture);
    fixture.backend.fail_next_call();

    let errors = settle(&mut fixture.app, &fixture.git);

    assert!(
        matches!(errors.as_slice(), [AppError::Backend(_)]),
        "{errors:?}"
    );
    let calls = fixture.backend.calls();
    let (_, fresh, cwd) = calls.last().unwrap().as_new_session().unwrap();
    assert_eq!(cwd, repo_path(&dir, "api-wt/feat/x"));
    assert_eq!(fixture.app.current_session().as_ref(), Some(fresh));
    assert_eq!(fixture.app.draft(), None);
}

#[test]
fn a_blocked_draft_refuses_create() {
    let (_dir, mut fixture, _api) = settled_api_draft();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();

    let unnamed = fixture.app.create_from_draft();

    assert!(matches!(unnamed, Err(AppError::Workspace(text)) if text == "enter a branch name"));

    let (_dir, mut fixture, _api) = api_draft();
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();

    let loading = fixture.app.create_from_draft();

    assert!(
        matches!(loading, Err(AppError::Workspace(text)) if text == "reading the branches of api…")
    );
    assert_eq!(fixture.git.waiting(), 1);
}

#[test]
fn create_checks_the_blockers_again_when_pressed() {
    let (dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    assert!(can_create(&fixture));
    keep_busy(&mut fixture, &api);

    let busy = fixture.app.create_from_draft();

    assert!(matches!(busy, Err(AppError::Workspace(text)) if text == BUSY));
    settle(&mut fixture.app, &fixture.git);
    assert!(can_create(&fixture));
    let path = repo_path(&dir, "api-wt/feat/x");
    fs::create_dir_all(&path).unwrap();

    let taken = fixture.app.create_from_draft();

    let exists = format!("{} already exists", path.display());
    assert!(matches!(taken, Err(AppError::Workspace(text)) if text == exists));
}

#[test]
fn an_orchestrator_remembers_its_child_defaults() {
    let both = &["main"];
    let remote = &["origin/main"];
    let (dir, mut fixture) = orchestrator_draft(
        info("main", both, remote),
        info("main", both, remote),
        false,
    );
    settle(&mut fixture.app, &fixture.git);
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    fixture.app.draft_branch(" feat/x ".into()).unwrap();
    fixture.app.draft_fetch(true).unwrap();

    let created = create(&mut fixture);

    let session = saved(&fixture.store, &project_of(&fixture), &created);
    assert_eq!(
        session.child_defaults,
        Some(BranchChoice {
            mode: BranchMode::Worktree,
            branch: "feat/x".into(),
            base: Some("main".into()),
            fetch: true,
        })
    );
    assert_eq!(session.workspace, Workspace::CheckedOut);
    assert_eq!(
        fixture.git.calls(),
        [
            GitCall::Inspect(repo_path(&dir, "api")),
            GitCall::Inspect(repo_path(&dir, "web"))
        ]
    );
}

#[test]
fn cancelling_while_creating_closes_the_sheet_and_undoes_what_git_made() {
    let (dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    let warm = create_running(&mut fixture);

    fixture.app.discard_draft().unwrap();

    assert_eq!(fixture.app.draft(), None);
    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::CloseSession { session: warm })
    );
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    let path = repo_path(&dir, "api-wt/feat/x");
    assert!(fixture.git.calls().ends_with(&worktree_steps(&api, &path)));
    assert!(no_sessions(&fixture));
    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
}

#[test]
fn cancelling_while_a_create_fails_leaves_nothing_to_undo() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.git.fail_next(GitOp::AddWorktree, &api);
    create_running(&mut fixture);
    fixture.app.discard_draft().unwrap();

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(!fixture.git.calls().iter().any(|call| matches!(
        call,
        GitCall::RemoveWorktree { .. } | GitCall::DeleteBranch { .. }
    )));
}

#[test]
fn a_failed_undo_after_cancel_is_reported() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.git.fail_next(GitOp::RemoveWorktree, &api);
    create_running(&mut fixture);
    fixture.app.discard_draft().unwrap();

    let errors = settle(&mut fixture.app, &fixture.git);

    let [error @ AppError::Undo { repository, .. }] = errors.as_slice() else {
        panic!("{errors:?}");
    };
    assert_eq!(repository, "api");
    assert_eq!(
        error.to_string(),
        "could not undo the branch work in api: git worktree remove failed: simulated failure"
    );
}

#[test]
fn an_undo_the_port_refuses_is_reported() {
    let (_dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    create_running(&mut fixture);
    fixture.app.discard_draft().unwrap();
    let made = fixture.git.answer();
    fixture.git.refuse_next_request();

    let errors: Vec<AppError> = made
        .into_iter()
        .filter_map(|event| fixture.app.apply_git_event(event).err())
        .collect();

    assert!(
        matches!(
            errors.as_slice(),
            [AppError::Undo { repository, error: GitError::Unavailable(_) }] if repository == "api"
        ),
        "{errors:?}"
    );
}

#[test]
fn the_repository_stays_busy_until_the_undo_is_done() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    create_running(&mut fixture);
    fixture.app.discard_draft().unwrap();
    let project = project_of(&fixture);
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();
    for event in fixture.git.answer() {
        fixture.app.apply_git_event(event).unwrap();
    }
    fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    // The new sheet read the repository while the abandoned worktree was still there.
    fixture.app.draft_branch("feat/y".into()).unwrap();

    assert_eq!(blocker(&fixture).as_deref(), Some(BUSY));
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert_eq!(blocker(&fixture), None);
    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
}

// Create reads the repository again before it changes anything.

/// No step that changes the repository was queued.
fn nothing_run(fixture: &Fixture) -> bool {
    !fixture
        .git
        .calls()
        .iter()
        .any(|call| !matches!(call, GitCall::Inspect(_)))
}

#[test]
fn the_undo_restores_the_head_the_fresh_reading_saw() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::NewBranch);
    fixture.app.draft_fetch(false).unwrap();
    // Switched outside Partitur while the sheet was open.
    let switched = repo_info(&api, "dev", &["main", "dev"], &["origin/main"]);
    fixture.git.add_repository(api.clone(), switched.clone());
    fixture.store.fail_next_session_save();
    create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(fixture.git.calls().ends_with(&[
        GitCall::RestoreHead {
            repository: api.clone(),
            head: Head::Branch("dev".into()),
        },
        GitCall::DeleteBranch {
            repository: api.clone(),
            branch: "feat/x".into(),
        },
    ]));
    assert_eq!(fixture.git.repository(&api).unwrap(), switched);
    assert_eq!(branch(&fixture).checked_out.as_deref(), Some("dev"));
}

#[test]
fn a_branch_made_outside_after_the_sheet_opened_blocks_create() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    let made = repo_info(&api, "main", &["main", "dev", "feat/x"], &["origin/main"]);
    fixture.git.add_repository(api, made);
    create_pending(&mut fixture);

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    assert_eq!(draft.failure, None);
    assert_eq!(
        draft.branch.blocker.as_deref(),
        Some("branch feat/x already exists in api")
    );
    assert!(nothing_run(&fixture));
    assert!(no_sessions(&fixture));
}

#[test]
fn a_base_deleted_outside_blocks_create() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.app.draft_base("dev".into()).unwrap();
    fixture
        .git
        .add_repository(api.clone(), repo_info(&api, "main", &["main"], &[]));
    create_pending(&mut fixture);

    settle(&mut fixture.app, &fixture.git);

    assert_eq!(
        blocker(&fixture).as_deref(),
        Some("dev is not a branch to start from")
    );
    assert!(nothing_run(&fixture));
}

#[test]
fn a_fetch_that_is_no_longer_possible_stops_create() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    assert!(branch(&fixture).fetch);
    fixture
        .git
        .add_repository(api.clone(), repo_info(&api, "main", &["main", "dev"], &[]));
    create_pending(&mut fixture);

    settle(&mut fixture.app, &fixture.git);

    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    assert_eq!(
        draft.failure.as_deref(),
        Some("api changed meanwhile; check the branch settings and create again")
    );
    assert!(nothing_run(&fixture));
}

#[test]
fn a_fresh_reading_that_fails_stops_create() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    fixture.git.fail_next(GitOp::Inspect, &api);
    create_pending(&mut fixture);

    settle(&mut fixture.app, &fixture.git);

    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    assert!(draft.failure.unwrap().contains("worktree list"));
    assert_eq!(draft.branch.mode, BranchMode::Worktree);
    assert!(nothing_run(&fixture));
}

#[test]
fn a_run_the_port_refuses_after_the_reading_stops_create() {
    let (_dir, mut fixture, _api) = api_draft_in(BranchMode::Worktree);
    create_pending(&mut fixture);
    let read = fixture.git.answer();
    fixture.git.refuse_next_request();

    for event in read {
        fixture.app.apply_git_event(event).unwrap();
    }

    let draft = fixture.app.draft().unwrap();
    assert!(!draft.creating);
    assert!(draft.failure.unwrap().contains("unavailable"));
    assert_eq!(fixture.git.waiting(), 0);
}

#[test]
fn cancelling_while_the_repository_is_read_again_queues_nothing() {
    let (_dir, mut fixture, api) = api_draft_in(BranchMode::Worktree);
    create_pending(&mut fixture);
    fixture.app.discard_draft().unwrap();
    let project = project_of(&fixture);
    fixture
        .app
        .prepare_session(project, Scope::Repository(api_id()))
        .unwrap();

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(nothing_run(&fixture));
    assert_eq!(fixture.git.repository(&api).unwrap(), api_info(&api));
    assert!(!fixture.app.draft().unwrap().creating);
    assert!(no_sessions(&fixture));
}
