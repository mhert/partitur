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
use crate::Role;
use crate::app::scheduler::Generated;
use crate::app::test_support::app_on;
use crate::backend::{AgentUpdate, BackendEvent, PromptContent, StopReason};
use crate::ids::{BackendSessionId, ProjectId, RepositoryId};
use crate::question::{QUESTION_RECORDED, QuestionOption};
use crate::session::Scope;
use crate::store::Store as _;
use crate::testing::{Call, MemoryStore, RecordingBackend, RecordingReply};
use crate::tool_call::{PartiturTool, ToolCall};
use crate::transcript::Entry;
use crate::{ModuleEvent, SuperpowersV7Event};

struct Fixture {
    app: App,
    backend: RecordingBackend,
    store: MemoryStore,
    project: ProjectId,
    _dir: TempDir,
}

/// An open project with two git repositories, `api` and `web`.
fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    for name in ["api", "web"] {
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

/// An attached session in `scope`, as though its instructions were already sent.
fn attached(fixture: &mut Fixture, scope: Scope) -> SessionId {
    let session = fixture
        .app
        .new_session(fixture.project.clone(), scope)
        .unwrap();
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
        .change_session(session.clone(), |ready| ready.instructions_injected = true)
        .unwrap();
    session
}

fn plain(fixture: &mut Fixture) -> SessionId {
    attached(
        fixture,
        Scope::Repository(RepositoryId::try_from("api").unwrap()),
    )
}

fn question(labels: &[&str]) -> Question {
    Question {
        text: "Which way?".into(),
        options: labels
            .iter()
            .map(|label| QuestionOption {
                label: (*label).into(),
                description: None,
            })
            .collect(),
    }
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

fn ask(fixture: &mut Fixture, session: &SessionId, labels: &[&str]) -> Arc<RecordingReply> {
    call(fixture, session, PartiturTool::AskUser(question(labels)))
}

fn send(fixture: &mut Fixture, session: &SessionId, update: AgentUpdate) {
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update,
        })
        .unwrap();
}

fn end_turn(fixture: &mut Fixture, session: &SessionId) {
    send(
        fixture,
        session,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
}

fn status(fixture: &Fixture, session: &SessionId) -> Status {
    fixture.app.session(session).unwrap().status
}

/// Selects `session` and runs a turn of it that asks a question and ends.
fn asked_and_ended(fixture: &mut Fixture, session: &SessionId) {
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();
    ask(fixture, session, &["Inline", "Subagent"]);
    end_turn(fixture, session);
}

fn questions(fixture: &Fixture, session: &SessionId) -> Vec<Option<QuestionOutcome>> {
    fixture
        .app
        .loaded(session)
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Question { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

fn child_results(fixture: &Fixture, parent: &SessionId) -> usize {
    fixture
        .store
        .load_records(fixture.store.project_of(parent).unwrap(), parent.clone())
        .unwrap()
        .iter()
        .filter(|update| matches!(update, TranscriptRecord::ChildResult { .. }))
        .count()
}

#[test]
fn ask_records_the_question_and_answers_at_once() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();

    let reply = ask(&mut fixture, &session, &["Inline", "Subagent"]);

    assert_eq!(reply.answers(), [Ok(QUESTION_RECORDED.to_owned())]);
    assert_eq!(questions(&fixture, &session), [None]);
    let id = fixture.app.current_question().cloned().unwrap();
    assert_eq!(fixture.app.loaded(&session).open_question().unwrap().0, &id);
    let saved = fixture
        .store
        .load_sessions(fixture.project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| saved.id == session)
        .unwrap();
    assert_eq!(saved.question, Some(id));
}

#[test]
fn no_question_is_current_without_a_selection_or_in_another_session() {
    let mut fixture = fixture();
    assert_eq!(fixture.app.current_question(), None);
    let asking = plain(&mut fixture);
    ask(&mut fixture, &asking, &["a", "b"]);
    assert!(fixture.app.question_open(&asking));
    let other = plain(&mut fixture);

    fixture.app.select_session(other).unwrap();
    assert_eq!(fixture.app.current_question(), None);

    fixture.app.select_session(asking).unwrap();
    assert!(fixture.app.current_question().is_some());
}

#[test]
fn an_invalid_question_is_a_tool_error_and_records_nothing() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);

    let reply = ask(&mut fixture, &session, &["Only"]);

    assert_eq!(
        reply.answers(),
        [Err("ask_user takes 2 to 6 options, got 1".to_owned())]
    );
    assert!(
        !fixture
            .store
            .load_records(fixture.store.project_of(&session).unwrap(), session.clone())
            .unwrap()
            .iter()
            .any(|update| matches!(update, TranscriptRecord::QuestionAsked { .. }))
    );
    assert!(!fixture.app.question_open(&session));
}

#[test]
fn a_plain_session_may_ask() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    assert!(
        !fixture
            .app
            .session(&session)
            .unwrap()
            .role
            .is_orchestrator()
    );

    let reply = ask(&mut fixture, &session, &["a", "b"]);

    assert_eq!(reply.answers(), [Ok(QUESTION_RECORDED.to_owned())]);
}

#[test]
fn an_unknown_session_may_not_ask() {
    let mut fixture = fixture();
    let session = fixture
        .app
        .new_session(
            fixture.project.clone(),
            Scope::Repository(RepositoryId::try_from("api").unwrap()),
        )
        .unwrap();

    let reply = ask(&mut fixture, &session, &["a", "b"]);

    assert_eq!(reply.answers(), [Err("unknown session".to_owned())]);
    assert!(!fixture.app.question_open(&session));
}

#[test]
fn a_second_question_supersedes_the_first() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();

    ask(&mut fixture, &session, &["a", "b"]);
    let first = fixture.app.current_question().cloned().unwrap();
    ask(&mut fixture, &session, &["c", "d"]);

    assert_eq!(
        questions(&fixture, &session),
        [Some(QuestionOutcome::Superseded), None]
    );
    assert_ne!(fixture.app.current_question(), Some(&first));
    assert!(fixture.app.current_question().is_some());
}

#[test]
fn a_turn_ending_with_a_question_open_waits_for_you() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();

    ask(&mut fixture, &session, &["a", "b"]);
    end_turn(&mut fixture, &session);

    assert_eq!(status(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn answering_sends_the_label_as_the_users_prompt() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);

    fixture
        .app
        .answer_question(session.clone(), "Inline")
        .unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: session.clone(),
            content: vec![PromptContent::Text("Inline".into())],
        })
    );
    assert_eq!(
        questions(&fixture, &session),
        [Some(QuestionOutcome::Answered("Inline".into()))]
    );
    assert!(!fixture.app.question_open(&session));
    assert_eq!(status(&fixture, &session), Status::Running);
    end_turn(&mut fixture, &session);
    assert_eq!(status(&fixture, &session), Status::Done);
}

#[test]
fn a_blank_answer_does_nothing() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    let calls = fixture.backend.calls().len();

    fixture.app.answer_question(session.clone(), "  ").unwrap();

    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(fixture.app.question_open(&session));
    assert_eq!(status(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn an_answer_during_a_turn_is_refused_and_keeps_the_question() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();
    ask(&mut fixture, &session, &["a", "b"]);

    let refused = fixture.app.answer_question(session.clone(), "a");

    assert!(matches!(refused, Err(AppError::TurnInProgress)));
    assert!(fixture.app.question_open(&session));
}

#[test]
fn a_prepared_prompt_answers_the_open_question_with_its_text() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);

    fixture
        .app
        .submit_prepared(&session, "do X", Vec::new())
        .unwrap();

    assert_eq!(
        questions(&fixture, &session),
        [Some(QuestionOutcome::Answered("do X".into()))]
    );
    assert!(!fixture.app.question_open(&session));
}

#[test]
fn a_typed_prompt_answers_the_open_question() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);

    fixture.app.submit_prompt("do X").unwrap();

    assert_eq!(
        questions(&fixture, &session),
        [Some(QuestionOutcome::Answered("do X".into()))]
    );
    assert!(!fixture.app.question_open(&session));
    end_turn(&mut fixture, &session);
    assert_eq!(status(&fixture, &session), Status::Done);
}

#[test]
fn a_prompt_that_cannot_be_queued_leaves_the_question_open() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    fixture.backend.fail_next_call();

    assert!(fixture.app.submit_prompt("do X").is_err());

    assert!(fixture.app.question_open(&session));
}

#[test]
fn cancel_closes_the_question_and_sends_nothing() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    let calls = fixture.backend.calls();

    fixture.app.cancel_question(session.clone()).unwrap();

    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(
        questions(&fixture, &session),
        [Some(QuestionOutcome::Cancelled)]
    );
    assert!(!fixture.app.question_open(&session));
    assert_eq!(status(&fixture, &session), Status::Done);
}

#[test]
fn a_question_closes_with_the_decider_that_answered_it() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    let by_user = fixture
        .store
        .load_records(fixture.store.project_of(&session).unwrap(), session.clone())
        .unwrap();
    assert!(
        by_user
            .iter()
            .all(|update| !matches!(update, TranscriptRecord::QuestionClosed { .. }))
    );

    fixture
        .app
        .close_question(&session, QuestionOutcome::Cancelled, Decider::Orchestrator)
        .unwrap();

    let updates = fixture
        .store
        .load_records(fixture.store.project_of(&session).unwrap(), session.clone())
        .unwrap();
    assert!(matches!(
        updates.last(),
        Some(TranscriptRecord::QuestionClosed {
            outcome: QuestionOutcome::Cancelled,
            decider: Decider::Orchestrator,
            ..
        })
    ));
    asked_and_ended(&mut fixture, &session);
    fixture.app.cancel_question(session.clone()).unwrap();
    let updates = fixture
        .store
        .load_records(fixture.store.project_of(&session).unwrap(), session)
        .unwrap();
    assert!(matches!(
        updates.last(),
        Some(TranscriptRecord::QuestionClosed {
            decider: Decider::User,
            ..
        })
    ));
}

#[test]
fn cancel_keeps_waiting_for_an_open_gate() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();
    call(
        &mut fixture,
        &session,
        PartiturTool::from(ModuleEvent::from(
            SuperpowersV7Event::RequestDesignApproval {
                path: "design.md".into(),
            },
        )),
    );
    ask(&mut fixture, &session, &["a", "b"]);
    end_turn(&mut fixture, &session);

    fixture.app.cancel_question(session.clone()).unwrap();

    assert_eq!(status(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn cancel_keeps_waiting_for_an_undecided_proposal() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    fixture
        .app
        .live_entry(&session)
        .proposals
        .insert(crate::ids::ProposalId::new(), Vec::new());

    fixture.app.cancel_question(session.clone()).unwrap();

    assert_eq!(status(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn cancel_keeps_waiting_for_a_permission_request() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    fixture
        .app
        .permissions
        .waiting
        .insert((session.clone(), crate::ids::PermissionId(1)), Vec::new());

    fixture.app.cancel_question(session.clone()).unwrap();

    assert_eq!(status(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn cancel_leaves_a_running_session_running() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();
    ask(&mut fixture, &session, &["a", "b"]);

    fixture.app.cancel_question(session.clone()).unwrap();

    assert_eq!(status(&fixture, &session), Status::Running);
}

#[test]
fn answering_without_a_question_is_refused() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);

    assert!(matches!(
        fixture.app.answer_question(session.clone(), "a"),
        Err(AppError::NoQuestion(refused)) if refused == session
    ));
    assert!(matches!(
        fixture.app.cancel_question(session.clone()),
        Err(AppError::NoQuestion(refused)) if refused == session
    ));
    assert!(matches!(
        fixture.app.cancel_question(SessionId::new()),
        Err(AppError::UnknownSession(_))
    ));
}

#[test]
fn a_child_result_waits_while_the_orchestrators_question_is_open() {
    let mut fixture = fixture();
    let parent = attached(&mut fixture, Scope::Project);
    assert!(fixture.app.session(&parent).unwrap().role.is_orchestrator());
    let child = plain(&mut fixture);
    fixture
        .app
        .change_session(child.clone(), |spawned| {
            spawned.role = Role::Child {
                parent: parent.clone(),
            };
        })
        .unwrap();
    asked_and_ended(&mut fixture, &parent);
    assert_eq!(status(&fixture, &parent), Status::WaitingForYou);

    fixture
        .app
        .deliver(&child, Generated::FromOrchestrator("Add it".into()))
        .unwrap();
    end_turn(&mut fixture, &child);

    assert_eq!(child_results(&fixture, &parent), 0);
    assert_eq!(fixture.app.live[&parent].deferred.len(), 1);
    assert!(fixture.app.question_open(&parent));
    assert_eq!(status(&fixture, &parent), Status::WaitingForYou);
}

#[test]
fn a_child_with_an_open_question_reports_that_it_waits() {
    let mut fixture = fixture();
    let parent = attached(&mut fixture, Scope::Project);
    let child = plain(&mut fixture);
    fixture
        .app
        .change_session(child.clone(), |spawned| {
            spawned.role = Role::Child {
                parent: parent.clone(),
            };
        })
        .unwrap();
    fixture
        .app
        .deliver(&child, Generated::FromOrchestrator("Add it".into()))
        .unwrap();
    ask(&mut fixture, &child, &["a", "b"]);
    end_turn(&mut fixture, &child);

    assert_eq!(child_results(&fixture, &parent), 1);
    assert_eq!(status(&fixture, &child), Status::WaitingForYou);

    fixture.app.answer_question(child.clone(), "a").unwrap();
    end_turn(&mut fixture, &child);

    // The second result waits behind the orchestrator's turn the first one started.
    end_turn(&mut fixture, &parent);
    assert_eq!(child_results(&fixture, &parent), 2);
}

#[test]
fn an_open_question_survives_a_restart() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    asked_and_ended(&mut fixture, &session);
    let id = fixture.app.current_question().cloned().unwrap();

    let mut restarted = app_on(&fixture.store, &fixture.backend);

    assert_eq!(
        restarted.session(&session).unwrap().status,
        Status::WaitingForYou
    );
    restarted.select_session(session.clone()).unwrap();
    assert_eq!(restarted.current_question(), Some(&id));
    restarted
        .answer_question(session.clone(), "Inline")
        .unwrap();
    assert_eq!(restarted.loaded(&session).open_question(), None);
    assert_eq!(restarted.current_question(), None);
}

#[test]
fn a_rebuilt_workflow_view_keeps_a_session_with_a_question_waiting() {
    let mut fixture = fixture();
    let session = plain(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("start").unwrap();
    call(
        &mut fixture,
        &session,
        PartiturTool::from(ModuleEvent::from(SuperpowersV7Event::StartBrainstorming)),
    );
    ask(&mut fixture, &session, &["a", "b"]);
    end_turn(&mut fixture, &session);
    fixture
        .app
        .change_session(session.clone(), |behind| behind.workflow = None)
        .unwrap();

    let mut restarted = app_on(&fixture.store, &fixture.backend);
    restarted.select_session(session.clone()).unwrap();

    assert!(restarted.session(&session).unwrap().workflow.is_some());
    assert_eq!(
        restarted.session(&session).unwrap().status,
        Status::WaitingForYou
    );
}
