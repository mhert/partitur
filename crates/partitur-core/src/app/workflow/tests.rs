#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::backend::{
    BackendEvent, OrchestratorTool, PermissionKind, PermissionOption, ToolKind, Update,
};
use crate::ids::RepositoryId;
use crate::ids::{
    BackendSessionId, PermissionId, PermissionOptionId, ProjectId, SessionId, ToolCallId,
};
use crate::session::{Scope, Status};
use crate::settings::GlobalSettings;
use crate::store::Store as _;
use crate::testing::{
    Call, MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude,
};
use crate::tool_call::{PartiturTool, ToolCall};
use crate::workflow::{CHECKPOINT_REACHED, GATE_REQUESTED, GateCloser, GateKind, RECORDED, Step};

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
    project: ProjectId,
    _dir: TempDir,
}

fn app_on(store: &MemoryStore, backend: &RecordingBackend) -> App {
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

/// An open project with `repositories` as git repositories.
fn fixture(repositories: &[&str]) -> Fixture {
    let dir = TempDir::new().unwrap();
    for name in repositories {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    let store = MemoryStore::new();
    let backend = RecordingBackend::new();
    let mut app = app_on(&store, &backend);
    let project = app.open_project(dir.path()).unwrap();
    Fixture {
        app,
        backend,
        store,
        project,
        _dir: dir,
    }
}

fn created(fixture: &mut Fixture, scope: Scope) -> SessionId {
    fixture
        .app
        .new_session(fixture.project.clone(), scope)
        .unwrap()
}

/// A session in `scope` whose backend session exists.
fn attached(fixture: &mut Fixture, scope: Scope) -> SessionId {
    let session = created(fixture, scope);
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
    session
}

fn repository_session(fixture: &mut Fixture) -> SessionId {
    attached(
        fixture,
        Scope::Repository(RepositoryId::try_from("api").unwrap()),
    )
}

fn superpowers(event: SuperpowersEvent) -> WorkflowEvent {
    WorkflowEvent::Superpowers(event)
}

fn call(
    fixture: &mut Fixture,
    session: &SessionId,
    tool: impl Into<PartiturTool>,
) -> Arc<RecordingReply> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: session.clone(),
            tool: tool.into(),
            reply: reply.clone(),
        })
        .unwrap();
    reply
}

fn report(event: SuperpowersEvent) -> PartiturTool {
    PartiturTool::Superpowers(event)
}

fn saved_view(fixture: &Fixture, session: &SessionId) -> Option<crate::workflow::WorkflowStatus> {
    fixture
        .store
        .load_sessions(fixture.project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| &saved.id == session)
        .unwrap()
        .workflow
}

#[test]
fn a_reported_event_is_appended_then_saved_with_the_session() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);

    let reply = call(
        &mut fixture,
        &session,
        report(SuperpowersEvent::StartBrainstorming),
    );

    assert_eq!(reply.answers(), [Ok(RECORDED.to_owned())]);
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    assert!(matches!(
        updates.last(),
        Some(Update::Workflow { event, .. })
            if *event == superpowers(SuperpowersEvent::StartBrainstorming)
    ));
    let view = saved_view(&fixture, &session).unwrap();
    assert_eq!(view.step, Some(Step::Brainstorm));
    assert_eq!(view.applied, 1);
}

#[test]
fn a_report_reaches_a_loaded_transcript() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();

    call(
        &mut fixture,
        &session,
        report(SuperpowersEvent::StartBrainstorming),
    );

    assert_eq!(fixture.app.transcripts[&session].workflow_events(), 1);
}

#[test]
fn gate_requests_answer_with_the_end_your_turn_text() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);

    let gate = call(
        &mut fixture,
        &session,
        report(SuperpowersEvent::RequestDesignApproval {
            path: "design.md".into(),
        }),
    );
    let checkpoint = call(
        &mut fixture,
        &session,
        report(SuperpowersEvent::ReachCheckpoint { after_task: 2 }),
    );

    assert_eq!(gate.answers(), [Ok(GATE_REQUESTED.to_owned())]);
    assert_eq!(checkpoint.answers(), [Ok(CHECKPOINT_REACHED.to_owned())]);
}

#[test]
fn an_orchestrator_may_report_and_a_plain_session_may_not_orchestrate() {
    let mut fixture = fixture(&["api", "web"]);
    let orchestrator = attached(&mut fixture, Scope::Project);
    assert!(fixture.app.session(&orchestrator).unwrap().orchestrator);
    let plain = repository_session(&mut fixture);

    let reported = call(
        &mut fixture,
        &orchestrator,
        report(SuperpowersEvent::StartBrainstorming),
    );
    let refused = call(&mut fixture, &plain, OrchestratorTool::ListChildren);

    assert_eq!(reported.answers(), [Ok(RECORDED.to_owned())]);
    assert_eq!(
        refused.answers(),
        [Err("this session is not an orchestrator".to_owned())]
    );
    assert!(saved_view(&fixture, &plain).is_none());
}

#[test]
fn reports_from_unknown_or_deleted_sessions_are_refused_and_record_nothing() {
    let mut fixture = fixture(&["api", "web"]);
    let unattached = created(
        &mut fixture,
        Scope::Repository(RepositoryId::try_from("api").unwrap()),
    );
    let deleted = repository_session(&mut fixture);
    fixture.app.delete(deleted.clone(), &[]).unwrap();

    let unknown = call(
        &mut fixture,
        &unattached,
        report(SuperpowersEvent::StartBrainstorming),
    );
    let gone = call(
        &mut fixture,
        &deleted,
        report(SuperpowersEvent::StartBrainstorming),
    );

    assert_eq!(unknown.answers(), [Err("unknown session".to_owned())]);
    assert_eq!(gone.answers(), [Err("the session was deleted".to_owned())]);
    assert!(fixture.store.load_updates(unattached).unwrap().is_empty());
}

#[test]
fn a_view_behind_its_history_is_rebuilt_when_the_transcript_loads() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    fixture
        .store
        .append_update(session.clone(), &Update::AgentText("Working on it".into()))
        .unwrap();
    for (at, event) in [
        (
            5,
            superpowers(SuperpowersEvent::RequestDesignApproval {
                path: "design.md".into(),
            }),
        ),
        (
            9,
            WorkflowEvent::GateClosed {
                gate: GateKind::Design,
                by: GateCloser::Approve,
            },
        ),
    ] {
        fixture
            .store
            .append_update(session.clone(), &Update::Workflow { at, event })
            .unwrap();
    }
    assert!(saved_view(&fixture, &session).is_none());

    let mut restarted = app_on(&fixture.store, &fixture.backend);
    restarted.select_session(session.clone()).unwrap();

    let view = saved_view(&fixture, &session).unwrap();
    assert_eq!(view.step, Some(Step::Design));
    assert!(view.gate.is_none());
    assert_eq!(view.design.unwrap().approved_at, Some(9));
    assert_eq!(view.applied, 2);
}

#[test]
fn a_view_in_step_with_its_history_is_not_saved_again() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    call(
        &mut fixture,
        &session,
        report(SuperpowersEvent::StartBrainstorming),
    );
    let mut restarted = app_on(&fixture.store, &fixture.backend);
    let saves = fixture.store.session_saves(&fixture.project);

    restarted.select_session(session).unwrap();

    assert_eq!(fixture.store.session_saves(&fixture.project), saves);
}

#[test]
fn partitur_tools_are_allowed_without_asking_in_every_session() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    assert!(!fixture.app.session(&session).unwrap().auto_allow);

    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update: Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-1"),
                title: "superpowers_start_brainstorming".into(),
                kind: ToolKind::Other,
                options: vec![
                    PermissionOption {
                        id: PermissionOptionId::new("allow-once"),
                        name: "Allow".into(),
                        kind: PermissionKind::AllowOnce,
                    },
                    PermissionOption {
                        id: PermissionOptionId::new("reject"),
                        name: "Reject".into(),
                        kind: PermissionKind::RejectOnce,
                    },
                ],
                name: None,
                locations: Vec::new(),
                partitur_tool: true,
            },
        })
        .unwrap();

    assert!(fixture.backend.calls().iter().any(|call| matches!(
        call,
        Call::RespondPermission { option: Some(option), .. } if option.as_str() == "allow-once"
    )));
    assert_ne!(
        fixture.app.session(&session).unwrap().status,
        Status::WaitingForYou
    );
}

#[test]
fn recording_for_an_unknown_session_fails_and_changes_nothing() {
    let mut fixture = fixture(&["api", "web"]);
    let stranger = SessionId::new();

    let result = fixture
        .app
        .record_workflow(&stranger, superpowers(SuperpowersEvent::StartBrainstorming));

    assert!(matches!(result, Err(AppError::UnknownSession(id)) if id == stranger));
    assert!(fixture.store.load_updates(stranger).unwrap().is_empty());
}
