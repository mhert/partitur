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
use crate::app::Attachment;
use crate::backend::{
    BackendEvent, OrchestratorTool, PermissionKind, PermissionOption, PromptContent, ProposedChild,
    StopReason, ToolKind, Update,
};
use crate::ids::RepositoryId;
use crate::ids::{
    BackendSessionId, PermissionId, PermissionOptionId, ProjectId, SessionId, ToolCallId,
};
use crate::session::{Scope, Status, TitleSource};
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
    dir: TempDir,
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
        dir,
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

fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
    fixture
        .app
        .apply_event(BackendEvent::Update {
            session: session.clone(),
            update,
        })
        .unwrap();
}

fn status_of(fixture: &Fixture, session: &SessionId) -> Status {
    fixture.app.session(session).unwrap().status
}

/// The texts of the prompts `session` was sent, oldest first.
fn prompts(fixture: &Fixture, session: &SessionId) -> Vec<String> {
    fixture
        .backend
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

fn last_prompt(fixture: &Fixture, session: &SessionId) -> String {
    prompts(fixture, session).pop().expect("no prompt was sent")
}

fn design_gate() -> SuperpowersEvent {
    SuperpowersEvent::RequestDesignApproval {
        path: "design.md".into(),
    }
}

fn plan_gate() -> SuperpowersEvent {
    SuperpowersEvent::RequestPlanApproval {
        path: "plan.md".into(),
        tasks: Vec::new(),
        checkpoints: Vec::new(),
    }
}

fn closed(gate: GateKind, by: GateCloser) -> WorkflowEvent {
    WorkflowEvent::GateClosed { gate, by }
}

/// Lets the user prompt `session`, and its agent report `gate` and answer `answer` before
/// the turn ends.
fn turn_at_gate(fixture: &mut Fixture, session: &SessionId, gate: SuperpowersEvent, answer: &str) {
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    call(fixture, session, report(gate));
    send(fixture, session, Update::AgentText(answer.into()));
    send(fixture, session, Update::TurnEnded(StopReason::EndTurn));
}

/// Lets the agent of `session` answer `answer` and end its running turn.
fn answer_and_end(fixture: &mut Fixture, session: &SessionId, answer: &str) {
    send(fixture, session, Update::AgentText(answer.into()));
    send(fixture, session, Update::TurnEnded(StopReason::EndTurn));
}

/// The workflow events in the session's history, oldest first.
fn workflow_history(fixture: &Fixture, session: &SessionId) -> Vec<WorkflowEvent> {
    fixture
        .store
        .load_updates(session.clone())
        .unwrap()
        .into_iter()
        .filter_map(|update| match update {
            Update::Workflow { event, .. } => Some(event),
            _ => None,
        })
        .collect()
}

/// An orchestrator and a child of it in `api`, both attached.
fn family(fixture: &mut Fixture) -> (SessionId, SessionId) {
    let orchestrator = attached(fixture, Scope::Project);
    assert!(fixture.app.session(&orchestrator).unwrap().orchestrator);
    let child = repository_session(fixture);
    fixture
        .app
        .change_session(child.clone(), |spawned| {
            spawned.parent = Some(orchestrator.clone());
        })
        .unwrap();
    (orchestrator, child)
}

/// Opens `kind`'s gate of `session` at `since`, in the view stored with the session.
fn gate_since(fixture: &mut Fixture, session: &SessionId, gate: SuperpowersEvent, since: u64) {
    fixture
        .app
        .change_session(session.clone(), |gated| {
            gated.workflow = Some(crate::workflow::apply(None, &superpowers(gate), since));
        })
        .unwrap();
}

#[test]
fn a_turn_that_ends_with_a_gate_open_waits_for_you() {
    let mut fixture = fixture(&["api", "web"]);
    let gated = repository_session(&mut fixture);
    let plain = repository_session(&mut fixture);

    turn_at_gate(&mut fixture, &gated, design_gate(), "The design is ready");
    fixture.app.select_session(plain.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    answer_and_end(&mut fixture, &plain, "Done");

    assert_eq!(status_of(&fixture, &gated), Status::WaitingForYou);
    assert_eq!(status_of(&fixture, &plain), Status::Done);
}

#[test]
fn approving_sends_the_fixed_text_and_records_the_closing() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");
    let other = repository_session(&mut fixture);
    fixture.app.select_session(other).unwrap();

    fixture.app.approve_gate(session.clone()).unwrap();

    let text = "Design approved. Write the implementation plan.";
    assert!(last_prompt(&fixture, &session).ends_with(text));
    let updates = fixture.store.load_updates(session.clone()).unwrap();
    let [.., closing, prompt] = updates.as_slice() else {
        panic!("too few updates");
    };
    assert!(matches!(
        closing,
        Update::Workflow { event, .. } if *event == closed(GateKind::Design, GateCloser::Approve)
    ));
    assert_eq!(*prompt, Update::UserText(text.into()));
    let view = saved_view(&fixture, &session).unwrap();
    assert!(view.gate.is_none());
    assert!(view.design.unwrap().approved_at.is_some());
    assert_eq!(status_of(&fixture, &session), Status::Running);
}

#[test]
fn each_gate_kind_sends_its_own_text() {
    let mut fixture = fixture(&["api", "web"]);
    let plan = repository_session(&mut fixture);
    let checkpoint = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &plan, plan_gate(), "The plan is ready");
    turn_at_gate(
        &mut fixture,
        &checkpoint,
        SuperpowersEvent::ReachCheckpoint { after_task: 2 },
        "Task 2 is done",
    );

    fixture.app.approve_gate(plan.clone()).unwrap();
    fixture.app.approve_gate(checkpoint.clone()).unwrap();

    assert!(last_prompt(&fixture, &plan).ends_with("Plan approved. Execute it."));
    assert!(last_prompt(&fixture, &checkpoint).ends_with("Continue."));
    assert!(
        saved_view(&fixture, &plan)
            .unwrap()
            .plan
            .unwrap()
            .approved_at
            .is_some()
    );
    assert_eq!(
        workflow_history(&fixture, &checkpoint).last(),
        Some(&closed(GateKind::Checkpoint, GateCloser::Approve))
    );
}

#[test]
fn any_prompt_closes_the_gate_as_a_prompt() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");

    fixture.app.submit_prompt("  ", Vec::new()).unwrap();
    let unreadable = Attachment {
        path: fixture.dir.path().join("missing.png"),
    };
    let failed = fixture.app.submit_prompt("Look", vec![unreadable]);
    assert!(matches!(failed, Err(AppError::Attachment { .. })));
    assert_eq!(
        saved_view(&fixture, &session).unwrap().gate.unwrap().kind,
        GateKind::Design
    );

    fixture
        .app
        .submit_prompt("Changes to the design: x", Vec::new())
        .unwrap();

    assert_eq!(
        workflow_history(&fixture, &session).last(),
        Some(&closed(GateKind::Design, GateCloser::Prompt))
    );
    let view = saved_view(&fixture, &session).unwrap();
    assert!(view.gate.is_none());
    assert_eq!(view.design.unwrap().approved_at, None);
    assert!(last_prompt(&fixture, &session).ends_with("Changes to the design: x"));
}

#[test]
fn approving_without_a_gate_is_refused() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    let stranger = SessionId::new();

    let refused = fixture.app.approve_gate(session.clone());
    let unknown = fixture.app.approve_gate(stranger.clone());

    assert!(matches!(refused, Err(AppError::NoGate(id)) if id == session));
    assert!(matches!(unknown, Err(AppError::UnknownSession(id)) if id == stranger));
    assert!(prompts(&fixture, &session).is_empty());
    assert!(workflow_history(&fixture, &session).is_empty());
}

#[test]
fn approving_during_the_turn_is_refused_and_records_nothing() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    call(&mut fixture, &session, report(design_gate()));
    assert_eq!(status_of(&fixture, &session), Status::Running);

    let refused = fixture.app.approve_gate(session.clone());

    assert!(matches!(refused, Err(AppError::TurnInProgress)));
    assert_eq!(
        workflow_history(&fixture, &session),
        [superpowers(design_gate())]
    );
    assert!(saved_view(&fixture, &session).unwrap().gate.is_some());
}

#[test]
fn an_archived_session_cannot_be_approved() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");
    fixture.app.archive(session.clone()).unwrap();

    let refused = fixture.app.approve_gate(session.clone());

    assert!(matches!(refused, Err(AppError::Archived(id)) if id == session));
    assert!(saved_view(&fixture, &session).unwrap().gate.is_some());
}

#[test]
fn a_gate_open_at_quit_loads_waiting_and_approves_after_restart() {
    let mut fixture = fixture(&["api", "web"]);
    let idle = repository_session(&mut fixture);
    fixture
        .app
        .record_workflow(&idle, superpowers(plan_gate()))
        .unwrap();
    assert_eq!(status_of(&fixture, &idle), Status::Idle);
    let waiting = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &waiting, design_gate(), "The design is ready");
    let running = repository_session(&mut fixture);
    fixture.app.select_session(running.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    call(&mut fixture, &running, report(design_gate()));
    let plain = repository_session(&mut fixture);
    let loads = fixture.store.update_loads();

    let mut restarted = app_on(&fixture.store, &fixture.backend);

    let status = |id: &SessionId| restarted.session(id).unwrap().status;
    assert_eq!(status(&idle), Status::WaitingForYou);
    assert_eq!(status(&waiting), Status::WaitingForYou);
    assert_eq!(status(&running), Status::Interrupted);
    assert_eq!(status(&plain), Status::Idle);
    // Only the session stored as waiting is read, for its permission requests.
    assert_eq!(fixture.store.update_loads(), loads + 1);

    restarted.approve_gate(idle.clone()).unwrap();
    let Some(Call::LoadSession { request, .. }) = fixture.backend.calls().last().cloned() else {
        panic!("the session was not loaded");
    };
    assert!(prompts(&fixture, &idle).is_empty());
    restarted
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: idle.clone(),
            config: Vec::new(),
        })
        .unwrap();

    let sent = prompts(&fixture, &idle);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].ends_with("Plan approved. Execute it."));
    assert_eq!(restarted.session(&idle).unwrap().status, Status::Running);
}

#[test]
fn a_child_at_its_gate_does_not_report_until_a_turn_ends_without_one() {
    let mut fixture = fixture(&["api", "web"]);
    let (orchestrator, child) = family(&mut fixture);

    turn_at_gate(&mut fixture, &child, design_gate(), "The design is ready");

    assert!(prompts(&fixture, &orchestrator).is_empty());
    assert!(!fixture.app.deferred.contains_key(&orchestrator));
    assert_eq!(status_of(&fixture, &child), Status::WaitingForYou);

    fixture.app.approve_gate(child.clone()).unwrap();
    answer_and_end(&mut fixture, &child, "The plan is written");

    let reported = prompts(&fixture, &orchestrator);
    assert_eq!(reported.len(), 1);
    assert!(
        reported[0].contains("The plan is written"),
        "{}",
        reported[0]
    );
    assert!(!reported[0].contains("The design is ready"));
}

#[test]
fn a_failed_child_at_its_gate_still_reports() {
    let mut fixture = fixture(&["api", "web"]);
    let (orchestrator, child) = family(&mut fixture);
    fixture.app.select_session(child.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    call(&mut fixture, &child, report(design_gate()));

    send(&mut fixture, &child, Update::Failed("boom".into()));

    let text = last_prompt(&fixture, &orchestrator);
    assert!(text.contains("(failed: boom)"), "{text}");
}

#[test]
fn send_to_child_is_refused_while_the_child_waits_at_its_gate() {
    let mut fixture = fixture(&["api", "web"]);
    let (orchestrator, child) = family(&mut fixture);
    turn_at_gate(&mut fixture, &child, design_gate(), "The design is ready");
    let before = fixture.store.load_updates(orchestrator.clone()).unwrap();

    let reply = call(
        &mut fixture,
        &orchestrator,
        OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: "Go on".into(),
        },
    );

    assert_eq!(
        reply.answers(),
        [Err(format!(
            "child {child} waits for the user's approval of its design; ask again after the \
             user decided"
        ))]
    );
    assert_eq!(fixture.store.load_updates(orchestrator).unwrap(), before);
    assert_eq!(prompts(&fixture, &child).len(), 1);
    assert!(!fixture.app.deferred.contains_key(&child));
    assert_eq!(status_of(&fixture, &child), Status::WaitingForYou);
}

#[test]
fn send_to_child_names_the_kind_of_the_gate() {
    let mut fixture = fixture(&["api", "web"]);
    let (orchestrator, child) = family(&mut fixture);
    let refusal = |fixture: &mut Fixture, gate: SuperpowersEvent| {
        gate_since(fixture, &child, gate, 1);
        call(
            fixture,
            &orchestrator,
            OrchestratorTool::SendToChild {
                child: child.clone(),
                prompt: "Go on".into(),
            },
        )
        .answers()
    };

    let plan = refusal(&mut fixture, plan_gate());
    let checkpoint = refusal(
        &mut fixture,
        SuperpowersEvent::ReachCheckpoint { after_task: 1 },
    );

    let text = |kind: &str| {
        format!(
            "child {child} waits for the user's approval of its {kind}; ask again after the user \
             decided"
        )
    };
    assert_eq!(plan, [Err(text("plan"))]);
    assert_eq!(checkpoint, [Err(text("checkpoint"))]);
}

#[test]
fn generated_prompts_wait_while_the_session_waits_at_its_gate() {
    let mut fixture = fixture(&["api", "web"]);
    let (orchestrator, child) = family(&mut fixture);
    turn_at_gate(
        &mut fixture,
        &orchestrator,
        design_gate(),
        "The design is ready",
    );
    turn_at_gate(&mut fixture, &child, plan_gate(), "unused");
    fixture.app.approve_gate(child.clone()).unwrap();
    answer_and_end(&mut fixture, &child, "The endpoint is in");

    assert_eq!(prompts(&fixture, &orchestrator).len(), 1);
    assert_eq!(fixture.app.deferred[&orchestrator].len(), 1);
    assert_eq!(status_of(&fixture, &orchestrator), Status::WaitingForYou);

    fixture.app.approve_gate(orchestrator.clone()).unwrap();
    assert!(
        last_prompt(&fixture, &orchestrator)
            .ends_with("Design approved. Write the implementation plan.")
    );
    answer_and_end(&mut fixture, &orchestrator, "Writing the plan");

    let result = last_prompt(&fixture, &orchestrator);
    assert!(result.contains("The endpoint is in"), "{result}");
    assert_eq!(status_of(&fixture, &orchestrator), Status::Running);
}

#[test]
fn deciding_a_proposal_keeps_an_orchestrator_waiting_at_its_gate() {
    let mut fixture = fixture(&["api", "web"]);
    let orchestrator = attached(&mut fixture, Scope::Project);
    fixture.app.select_session(orchestrator.clone()).unwrap();
    fixture.app.submit_prompt("Split it", Vec::new()).unwrap();
    call(
        &mut fixture,
        &orchestrator,
        OrchestratorTool::ProposeChildren(vec![ProposedChild {
            repository: RepositoryId::try_from("api").unwrap(),
            prompt: "Add the endpoint".into(),
            title: "API endpoint".into(),
            title_source: TitleSource::Given,
        }]),
    );
    call(&mut fixture, &orchestrator, report(design_gate()));
    answer_and_end(&mut fixture, &orchestrator, "Proposed");
    let proposal = fixture
        .app
        .proposals
        .keys()
        .map(|(_, id)| id.clone())
        .next()
        .unwrap();

    fixture
        .app
        .reject_proposal(orchestrator.clone(), proposal, None)
        .unwrap();

    assert_eq!(status_of(&fixture, &orchestrator), Status::WaitingForYou);
    assert_eq!(prompts(&fixture, &orchestrator).len(), 1);
    assert_eq!(fixture.app.deferred[&orchestrator].len(), 1);
}

#[test]
fn next_gate_is_the_one_waiting_longest_across_projects() {
    let mut fixture = fixture(&["api", "web"]);
    let other = TempDir::new().unwrap();
    let closed_dir = TempDir::new().unwrap();
    let second = fixture.app.open_project(other.path()).unwrap();
    let third = fixture.app.open_project(closed_dir.path()).unwrap();
    let late = repository_session(&mut fixture);
    let early = fixture.app.new_session(second, Scope::Project).unwrap();
    let tied = repository_session(&mut fixture);
    let archived = repository_session(&mut fixture);
    let hidden = fixture
        .app
        .new_session(third.clone(), Scope::Project)
        .unwrap();
    assert_eq!(fixture.app.next_gate(), None);

    gate_since(&mut fixture, &late, design_gate(), 30);
    gate_since(&mut fixture, &tied, design_gate(), 30);
    assert_eq!(fixture.app.next_gate(), Some(late.clone()));
    gate_since(&mut fixture, &early, plan_gate(), 10);
    gate_since(&mut fixture, &archived, design_gate(), 5);
    fixture.app.archive(archived).unwrap();
    gate_since(&mut fixture, &hidden, design_gate(), 1);
    fixture.app.close_project(third).unwrap();

    assert_eq!(fixture.app.next_gate(), Some(early));
}

#[test]
fn ties_between_gates_go_to_the_older_session() {
    let mut fixture = fixture(&["api", "web"]);
    let newer = repository_session(&mut fixture);
    let older = repository_session(&mut fixture);
    fixture
        .app
        .change_session(older.clone(), |session| session.created_at = 1)
        .unwrap();

    gate_since(&mut fixture, &newer, design_gate(), 7);
    gate_since(&mut fixture, &older, design_gate(), 7);

    assert_eq!(fixture.app.next_gate(), Some(older));
}

#[test]
fn deciding_the_last_proposal_frees_an_orchestrator_with_no_gate_open() {
    let mut fixture = fixture(&["api", "web"]);
    let orchestrator = attached(&mut fixture, Scope::Project);
    fixture.app.select_session(orchestrator.clone()).unwrap();
    fixture.app.submit_prompt("Split it", Vec::new()).unwrap();
    call(
        &mut fixture,
        &orchestrator,
        OrchestratorTool::ProposeChildren(vec![ProposedChild {
            repository: RepositoryId::try_from("api").unwrap(),
            prompt: "Add the endpoint".into(),
            title: "API endpoint".into(),
            title_source: TitleSource::Given,
        }]),
    );
    answer_and_end(&mut fixture, &orchestrator, "Proposed");
    assert_eq!(status_of(&fixture, &orchestrator), Status::WaitingForYou);
    let proposal = fixture
        .app
        .proposals
        .keys()
        .map(|(_, id)| id.clone())
        .next()
        .unwrap();
    // In a closed project the wake waits, so the status the decision leaves shows.
    fixture.app.close_project(fixture.project.clone()).unwrap();

    fixture
        .app
        .reject_proposal(orchestrator.clone(), proposal, None)
        .unwrap();

    assert_eq!(status_of(&fixture, &orchestrator), Status::Idle);
    assert_eq!(fixture.app.deferred[&orchestrator].len(), 1);
}

#[test]
fn a_gate_stays_open_when_its_approval_cannot_be_sent() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");
    let history = fixture.store.load_updates(session.clone()).unwrap();
    fixture.backend.fail_next_call();

    let failed = fixture.app.approve_gate(session.clone());

    assert!(matches!(failed, Err(AppError::Backend(_))));
    let view = saved_view(&fixture, &session).unwrap();
    assert_eq!(view.gate.unwrap().kind, GateKind::Design);
    assert_eq!(view.design.unwrap().approved_at, None);
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        history
    );
    assert_eq!(status_of(&fixture, &session), Status::WaitingForYou);

    fixture.app.approve_gate(session.clone()).unwrap();
    assert_eq!(
        workflow_history(&fixture, &session).last(),
        Some(&closed(GateKind::Design, GateCloser::Approve))
    );
}

#[test]
fn a_gate_stays_open_when_a_prompt_to_it_cannot_be_sent() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");
    let history = fixture.store.load_updates(session.clone()).unwrap();
    fixture.backend.fail_next_call();

    let failed = fixture
        .app
        .submit_prompt("Changes to the design: x", Vec::new());

    assert!(matches!(failed, Err(AppError::Backend(_))));
    assert_eq!(
        saved_view(&fixture, &session).unwrap().gate.unwrap().kind,
        GateKind::Design
    );
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        history
    );
    assert_eq!(status_of(&fixture, &session), Status::WaitingForYou);
}

#[test]
fn a_gate_stays_open_when_the_session_to_approve_cannot_be_loaded() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    fixture
        .app
        .record_workflow(&session, superpowers(plan_gate()))
        .unwrap();
    let history = fixture.store.load_updates(session.clone()).unwrap();
    let mut restarted = app_on(&fixture.store, &fixture.backend);
    fixture.backend.fail_next_call();

    let failed = restarted.approve_gate(session.clone());

    assert!(matches!(failed, Err(AppError::Backend(_))));
    assert_eq!(
        saved_view(&fixture, &session).unwrap().gate.unwrap().kind,
        GateKind::Plan
    );
    assert_eq!(
        fixture.store.load_updates(session.clone()).unwrap(),
        history
    );
}

#[test]
fn the_selected_sessions_workflow_and_approvable_gate_are_told_without_a_view() {
    let mut fixture = fixture(&["api", "web"]);
    assert_eq!(fixture.app.current_workflow(), None);
    assert_eq!(fixture.app.approvable_gate(), None);
    let session = repository_session(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    assert_eq!(fixture.app.current_workflow(), None);
    assert_eq!(fixture.app.approvable_gate(), None);

    // While the turn that opened the gate runs, the gate cannot be approved yet.
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    call(&mut fixture, &session, report(plan_gate()));
    assert_eq!(
        fixture
            .app
            .current_workflow()
            .and_then(|status| status.step),
        Some(Step::Plan)
    );
    assert_eq!(fixture.app.approvable_gate(), None);

    answer_and_end(&mut fixture, &session, "The plan is ready");
    assert_eq!(fixture.app.approvable_gate(), Some(GateKind::Plan));

    fixture.app.archive(session.clone()).unwrap();
    fixture.app.select_session(session.clone()).unwrap();
    assert_eq!(fixture.app.approvable_gate(), None);
    assert!(fixture.app.current_workflow().is_some());
}

#[test]
fn a_gate_waiting_beside_a_permission_request_cannot_be_approved() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    turn_at_gate(&mut fixture, &session, design_gate(), "The design is ready");
    assert_eq!(fixture.app.approvable_gate(), Some(GateKind::Design));
    fixture
        .app
        .submit_prompt("Fix the typo first", Vec::new())
        .unwrap();
    call(&mut fixture, &session, report(design_gate()));
    send(
        &mut fixture,
        &session,
        Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Edit design.md".into(),
            kind: ToolKind::Edit,
            options: vec![PermissionOption {
                id: PermissionOptionId::new("allow"),
                name: "Allow".into(),
                kind: PermissionKind::AllowOnce,
            }],
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        },
    );

    assert_eq!(status_of(&fixture, &session), Status::WaitingForYou);
    assert!(fixture.app.current_workflow().unwrap().gate.is_some());
    assert_eq!(fixture.app.approvable_gate(), None);
}

/// A restart of `fixture` after the view stored with `session` opened the design's gate and
/// `history` was appended to its history alone, as a crash between the two leaves it.
fn restarted_behind(fixture: &mut Fixture, session: &SessionId, history: &[Update]) -> App {
    fixture
        .app
        .record_workflow(session, superpowers(design_gate()))
        .unwrap();
    for update in history {
        fixture
            .store
            .append_update(session.clone(), update)
            .unwrap();
    }
    app_on(&fixture.store, &fixture.backend)
}

/// The update that closes the design's gate by a prompt.
fn closed_by_prompt() -> Update {
    Update::Workflow {
        at: 9,
        event: closed(GateKind::Design, GateCloser::Prompt),
    }
}

#[test]
fn a_gate_the_history_closed_leaves_the_session_idle_once_rebuilt() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    let mut restarted = restarted_behind(&mut fixture, &session, &[closed_by_prompt()]);
    assert_eq!(
        restarted.session(&session).unwrap().status,
        Status::WaitingForYou
    );

    restarted.select_session(session.clone()).unwrap();

    assert_eq!(restarted.session(&session).unwrap().status, Status::Idle);
    let saved = fixture
        .store
        .load_sessions(fixture.project.clone())
        .unwrap()
        .into_iter()
        .find(|saved| saved.id == session)
        .unwrap();
    assert_eq!(saved.status, Status::Idle);
    assert!(saved.workflow.unwrap().gate.is_none());
}

#[test]
fn a_rebuilt_session_still_waits_for_an_open_gate_or_an_undecided_proposal() {
    let mut fixture = fixture(&["api", "web"]);
    let gated = repository_session(&mut fixture);
    let proposing = repository_session(&mut fixture);
    let brainstorming = Update::Workflow {
        at: 9,
        event: superpowers(SuperpowersEvent::StartBrainstorming),
    };
    fixture
        .app
        .record_workflow(&gated, superpowers(design_gate()))
        .unwrap();
    fixture
        .store
        .append_update(gated.clone(), &brainstorming)
        .unwrap();
    let proposal = Update::ChildrenProposed {
        id: crate::ids::ProposalId::new(),
        children: vec![ProposedChild {
            repository: RepositoryId::try_from("web").unwrap(),
            prompt: "Add the page".into(),
            title: "Web page".into(),
            title_source: TitleSource::Given,
        }],
    };
    let mut restarted = restarted_behind(&mut fixture, &proposing, &[proposal, closed_by_prompt()]);

    restarted.select_session(gated.clone()).unwrap();
    restarted.select_session(proposing.clone()).unwrap();

    for session in [&gated, &proposing] {
        assert_eq!(
            restarted.session(session).unwrap().status,
            Status::WaitingForYou
        );
    }
    assert!(
        restarted
            .session(&gated)
            .unwrap()
            .workflow
            .as_ref()
            .unwrap()
            .gate
            .is_some()
    );
}

#[test]
fn a_rebuilt_interrupted_session_stays_interrupted() {
    let mut fixture = fixture(&["api", "web"]);
    let session = repository_session(&mut fixture);
    fixture.app.select_session(session.clone()).unwrap();
    fixture.app.submit_prompt("Work on it", Vec::new()).unwrap();
    let mut restarted = restarted_behind(&mut fixture, &session, &[closed_by_prompt()]);
    assert_eq!(
        restarted.session(&session).unwrap().status,
        Status::Interrupted
    );

    restarted.select_session(session.clone()).unwrap();

    assert_eq!(
        restarted.session(&session).unwrap().status,
        Status::Interrupted
    );
}
