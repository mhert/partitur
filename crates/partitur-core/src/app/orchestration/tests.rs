#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use std::collections::VecDeque;

use crate::backend::{
    ApprovedChild, BackendError, BackendEvent, ChildOutcome, Decision, OrchestratorTool,
    PermissionKind, PermissionOption, PromptContent, ProposedChild, StopReason, ToolKind,
    ToolStatus, TurnOrigin, Update,
};
use crate::git::Head;
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
    PermissionOptionId, ProjectId, RepositoryId, RequestId, SessionId, ToolCallId, ToolRequestId,
};
use crate::instructions::{inject, instruction_chain};
use crate::orchestrator::{PROPOSAL_RECORDED, child_result_prompt, children_started_prompt};
use crate::session::{BranchChoice, BranchMode, Scope, Status, Workspace};
use crate::settings::{GlobalSettings, Level, ProjectSettings, SettingsLayer};
use crate::store::Store as _;
use crate::testing::{
    Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, settle,
    single_claude,
};
use crate::transcript::Entry;
use crate::view_model::{ApprovalView, ChildCheck};

use super::super::tests::{agent_options, load_request};

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
    let backend = RecordingBackend::new();
    let git = MemoryGit::new();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store.clone()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
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

fn repository(name: &str) -> RepositoryId {
    RepositoryId::try_from(name).unwrap()
}

/// Creates a session in `scope` and attaches it, as though its instructions were already
/// sent, so its prompts reach the backend as given.
fn attached_session(fixture: &mut Fixture, project: &ProjectId, scope: Scope) -> SessionId {
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

/// An attached child of `parent` working in `repository`, titled `title`.
fn attached_child(
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
            spawned.parent = Some(parent.clone());
            spawned.title = title.into();
        })
        .unwrap();
    child
}

/// An open two-repository project with an attached, idle orchestrator and an attached
/// child of it in `api` titled "API endpoint".
struct Family {
    dir: TempDir,
    fixture: Fixture,
    project: ProjectId,
    parent: SessionId,
    child: SessionId,
}

fn family() -> Family {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    assert!(fixture.app.session(&parent).unwrap().orchestrator);
    let child = attached_child(&mut fixture, &project, &parent, "api", "API endpoint");
    Family {
        dir,
        fixture,
        project,
        parent,
        child,
    }
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

/// Starts a turn of `child` as its orchestrator would.
fn orchestrate(fixture: &mut Fixture, child: &SessionId) {
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
fn unattached_child(
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
            spawned.parent = Some(parent.clone());
            spawned.title = "API endpoint".into();
        })
        .unwrap();
    (child, request)
}

/// Lets `child` finish a turn its orchestrator started with the answer `answer`.
fn finish(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    orchestrate(fixture, child);
    send(fixture, child, Update::AgentText(answer.into()));
    send(fixture, child, Update::TurnEnded(StopReason::EndTurn));
}

fn deferred(app: &App, session: &SessionId) -> usize {
    app.deferred.get(session).map_or(0, VecDeque::len)
}

fn status(app: &App, session: &SessionId) -> Status {
    app.session(session).unwrap().status
}

/// The texts of the prompts `session` was sent, oldest first.
fn prompts(backend: &RecordingBackend, session: &SessionId) -> Vec<String> {
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

fn last_prompt(backend: &RecordingBackend, session: &SessionId) -> String {
    prompts(backend, session).pop().expect("no prompt was sent")
}

fn result_text(
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
        text,
    )
}

fn proposed(name: &str, prompt: &str, title: &str) -> ProposedChild {
    ProposedChild {
        repository: repository(name),
        prompt: prompt.into(),
        title: title.into(),
    }
}

/// Lets `session` call `tool` and returns the answer the backend was given.
fn call_tool(
    fixture: &mut Fixture,
    session: &SessionId,
    tool: OrchestratorTool,
) -> Result<String, String> {
    fixture
        .app
        .apply_event(BackendEvent::ToolCalled {
            call: ToolRequestId(7),
            session: session.clone(),
            tool,
        })
        .unwrap();
    let calls = fixture.backend.calls();
    assert!(matches!(
        calls.last(),
        Some(Call::AnswerTool {
            call: ToolRequestId(7),
            ..
        })
    ));
    let mut answers: Vec<_> = calls
        .into_iter()
        .filter_map(|call| match call {
            Call::AnswerTool { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    answers.pop().unwrap()
}

fn call_propose(
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

fn call_send_to_child(
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
        },
    )
}

/// The id of the session's only undecided proposal.
fn proposal_id(app: &App, session: &SessionId) -> ProposalId {
    let ids: Vec<_> = app
        .proposals
        .keys()
        .filter(|(proposer, _)| proposer == session)
        .map(|(_, id)| id.clone())
        .collect();
    assert_eq!(ids.len(), 1);
    ids[0].clone()
}

#[test]
fn generated_prompts_are_recorded_by_their_kind_and_sent_as_written() {
    let wake = Generated::Wake("children started".into());
    let prompt = Generated::FromOrchestrator("Add the endpoint".into());

    assert_eq!(
        wake.record(),
        Update::PartiturPrompt("children started".into())
    );
    assert_eq!(wake.text(), "children started");
    assert_eq!(
        prompt.record(),
        Update::OrchestratorPrompt("Add the endpoint".into())
    );
    assert_eq!(prompt.text(), "Add the endpoint");
}

#[test]
fn the_final_message_is_the_last_answer_of_the_latest_turn() {
    let answered = Transcript::from_updates(&[
        Update::UserText("go".into()),
        Update::AgentText("first".into()),
        Update::Notice("note".into()),
        Update::AgentText("last".into()),
        Update::TurnEnded(StopReason::EndTurn),
    ]);
    let silent = Transcript::from_updates(&[
        Update::UserText("go".into()),
        Update::AgentText("earlier turn".into()),
        Update::TurnEnded(StopReason::EndTurn),
        Update::OrchestratorPrompt("again".into()),
        Update::TurnEnded(StopReason::EndTurn),
    ]);

    let resumed = Transcript::from_updates(&[
        Update::UserText("go".into()),
        Update::AgentText("earlier turn".into()),
        Update::TurnEnded(StopReason::EndTurn),
        Update::TurnStarted,
        Update::TurnEnded(StopReason::EndTurn),
    ]);

    assert_eq!(final_message(&answered), "last");
    assert_eq!(final_message(&silent), "(no final message)");
    assert_eq!(final_message(&resumed), "(no final message)");
}

#[test]
fn a_child_result_waits_behind_a_user_prompt_with_an_attachment() {
    let Family {
        dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let notes = dir.path().join("notes.txt");
    fixture.app.select_session(parent.clone()).unwrap();
    fixture
        .app
        .submit_prompt(
            "Plan it",
            vec![crate::app::Attachment {
                path: notes.clone(),
            }],
        )
        .unwrap();
    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: parent.clone(),
            content: vec![
                PromptContent::Text("Plan it".into()),
                PromptContent::FileLink { path: notes },
            ],
        })
    );

    finish(&mut fixture, &child, "done");
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    let outcome = ChildOutcome::Ended(StopReason::EndTurn);
    assert_eq!(
        prompts(&fixture.backend, &parent),
        [
            "Plan it".to_owned(),
            result_text(&child, TurnOrigin::Orchestrator, &outcome, "done"),
        ]
    );
}

#[test]
fn a_child_turn_end_prompts_its_idle_parent_with_the_result() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);

    send(&mut fixture, &child, Update::AgentText("half".into()));
    send(&mut fixture, &child, Update::AgentText(" done".into()));
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    let outcome = ChildOutcome::Ended(StopReason::EndTurn);
    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: parent.clone(),
            content: vec![PromptContent::Text(result_text(
                &child,
                TurnOrigin::Orchestrator,
                &outcome,
                "half done"
            ))],
        })
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildResult {
            child: child.clone(),
            repository: repository("api"),
            title: "API endpoint".into(),
            origin: TurnOrigin::Orchestrator,
            outcome,
            text: "half done".into(),
        })
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(status(&fixture.app, &child), Status::Done);
}

#[test]
fn a_user_started_child_turn_is_marked_as_such() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(child.clone()).unwrap();
    fixture
        .app
        .submit_prompt("Also add a test", Vec::new())
        .unwrap();

    send(&mut fixture, &child, Update::AgentText("added".into()));
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    let text = last_prompt(&fixture.backend, &parent);
    assert!(text.contains("(turn started by the user)"), "{text}");
    assert_eq!(
        text,
        result_text(
            &child,
            TurnOrigin::User,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "added"
        )
    );
}

#[test]
fn a_failed_child_turn_is_marked_failed_with_the_error() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);

    send(&mut fixture, &child, Update::Failed("boom".into()));

    let text = last_prompt(&fixture.backend, &parent);
    assert!(text.contains("(failed: boom)"), "{text}");
    assert_eq!(status(&fixture.app, &child), Status::Failed);
}

#[test]
fn a_cancelled_child_turn_is_marked_with_its_stop_reason() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);

    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );

    let text = last_prompt(&fixture.backend, &parent);
    assert!(text.contains("(turn ended: cancelled)"), "{text}");
}

#[test]
fn a_child_blocked_on_a_permission_produces_no_result() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);

    send(
        &mut fixture,
        &child,
        Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Run cargo test".into(),
            kind: ToolKind::Execute,
            options: vec![PermissionOption {
                id: PermissionOptionId::new("allow"),
                name: "Allow".into(),
                kind: PermissionKind::AllowOnce,
            }],
            name: None,
            locations: Vec::new(),
        },
    );

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    assert_eq!(status(&fixture.app, &parent), Status::Idle);
}

#[test]
fn results_queue_while_the_parent_is_mid_turn_and_go_out_one_per_turn() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    for (session, answer) in [(&child, "api done"), (&web, "web done")] {
        orchestrate(&mut fixture, session);
        send(&mut fixture, session, Update::AgentText(answer.into()));
        send(
            &mut fixture,
            session,
            Update::TurnEnded(StopReason::EndTurn),
        );
    }

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);

    let ended = ChildOutcome::Ended(StopReason::EndTurn);
    let web_result = child_result_prompt(
        &web,
        &repository("web"),
        "Web page",
        TurnOrigin::Orchestrator,
        &ended,
        "web done",
    );
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(last_prompt(&fixture.backend, &parent), web_result);
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(prompts(&fixture.backend, &parent).len(), 3);
    assert_eq!(status(&fixture.app, &parent), Status::Done);
}

#[test]
fn a_child_result_for_a_detached_parent_waits_for_the_reattach() {
    let Family {
        dir: _dir,
        fixture,
        parent,
        child,
        ..
    } = family();
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    restarted.app.select_session(child.clone()).unwrap();
    restarted
        .app
        .submit_prompt("Add the endpoint", Vec::new())
        .unwrap();
    let request = load_request(&restarted.backend);
    restarted
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: child.clone(),
            config: Vec::new(),
        })
        .unwrap();
    send(&mut restarted, &child, Update::AgentText("done".into()));

    send(
        &mut restarted,
        &child,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert!(matches!(
        restarted.backend.calls().last(),
        Some(Call::LoadSession {
            session,
            orchestrator: true,
            ..
        }) if session == &parent
    ));
    let request = load_request(&restarted.backend);
    assert!(prompts(&restarted.backend, &parent).is_empty());

    restarted
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: parent.clone(),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(
        prompts(&restarted.backend, &parent),
        [result_text(
            &child,
            TurnOrigin::User,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "done"
        )]
    );
    assert_eq!(status(&restarted.app, &parent), Status::Running);
}

#[test]
fn a_child_without_a_final_message_reports_so() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(
        &mut fixture,
        &child,
        Update::ToolCall {
            id: ToolCallId::new("t-1"),
            title: "Read main.rs".into(),
            kind: ToolKind::Read,
            status: ToolStatus::Completed,
            content: Vec::new(),
            locations: Vec::new(),
        },
    );

    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "(no final message)"
        )
    );
}

#[test]
fn a_session_without_a_parent_reports_to_nobody() {
    let Family {
        mut fixture,
        project,
        ..
    } = family();
    let lone = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
    fixture
        .app
        .submit_prompt("Fix the build", Vec::new())
        .unwrap();
    send(&mut fixture, &lone, Update::AgentText("fixed".into()));
    let calls = fixture.backend.calls();

    send(&mut fixture, &lone, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(status(&fixture.app, &lone), Status::Done);
}

#[test]
fn a_child_whose_transcript_is_not_loaded_is_read_from_the_store() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(
        &mut fixture,
        &child,
        Update::AgentText("from the store".into()),
    );
    fixture.app.transcripts.remove(&child);
    assert!(
        !fixture
            .store
            .load_updates(child.clone())
            .unwrap()
            .is_empty()
    );

    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "from the store"
        )
    );
}

#[test]
fn a_closed_project_keeps_results_without_starting_turns() {
    let Family {
        dir,
        mut fixture,
        project,
        parent,
        child,
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("half".into()));
    fixture.app.close_project(project).unwrap();

    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Idle);

    fixture.app.open_project(dir.path()).unwrap();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Status?", Vec::new()).unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [
            "Status?".to_owned(),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::Cancelled),
                "half"
            )
        ]
    );
}

#[test]
fn a_turn_that_ends_in_a_closed_project_sends_nothing_deferred() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    fixture.app.close_project(project).unwrap();

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Done);
}

#[test]
fn closing_a_project_with_a_waiting_child_prompt_starts_no_turn() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);

    fixture.app.close_project(project).unwrap();

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &child), Status::Idle);
}

#[test]
fn a_cancelled_turn_reports_but_does_not_drain() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::Cancelled),
    );

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 2);

    fixture.app.submit_prompt("Go on", Vec::new()).unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "api done"
        )
    );
    assert_eq!(deferred(&fixture.app, &parent), 1);
}

#[test]
fn a_child_whose_connection_closes_mid_turn_reports_the_failure() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("half".into()));

    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: child.clone(),
            error: "agent crashed".into(),
        })
        .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Failed("connection to the agent closed: agent crashed".into()),
            "half"
        )
    );
    assert_eq!(status(&fixture.app, &child), Status::Failed);
}

#[test]
fn a_child_whose_create_fails_reports_the_failure() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, request) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: child.clone(),
            error: "agent not found".into(),
        })
        .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Failed("agent not found".into()),
            "(no final message)"
        )
    );
    assert_eq!(status(&fixture.app, &child), Status::Failed);
}

#[test]
fn a_failed_create_outside_a_turn_reports_nothing() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, request) = unattached_child(&mut fixture, &project, &parent);

    fixture
        .app
        .apply_event(BackendEvent::RequestFailed {
            request,
            session: child.clone(),
            error: "agent not found".into(),
        })
        .unwrap();

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(status(&fixture.app, &child), Status::Failed);
}

#[test]
fn a_stopped_queued_child_prompt_reports_a_cancellation() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);
    fixture.app.select_session(child.clone()).unwrap();

    fixture.app.cancel().unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::Cancelled),
            "(no final message)"
        )
    );
    assert_eq!(status(&fixture.app, &child), Status::Idle);
}

#[test]
fn a_parent_whose_connection_closed_drains_through_a_reattach() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    assert_eq!(deferred(&fixture.app, &parent), 1);

    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: parent.clone(),
            error: "agent crashed".into(),
        })
        .unwrap();

    assert!(matches!(
        fixture.backend.calls().last(),
        Some(Call::LoadSession {
            session,
            orchestrator: true,
            ..
        }) if session == &parent
    ));
    let request = load_request(&fixture.backend);
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);

    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: parent.clone(),
            config: Vec::new(),
        })
        .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "api done"
        )
    );
    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn a_generated_prompt_never_jumps_deferred_ones() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(status(&fixture.app, &parent), Status::Done);

    finish(&mut fixture, &child, "api done again");

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 3);

    fixture.app.submit_prompt("Go on", Vec::new()).unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "api done"
        )
    );
    assert_eq!(deferred(&fixture.app, &parent), 2);
}

#[test]
fn a_proposal_is_recorded_and_answered_at_once() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let children = vec![
        proposed("api", "Add the endpoint", "API endpoint"),
        proposed("web", "Add the page", "Web page"),
    ];

    let answer = call_propose(&mut fixture, &parent, children.clone());

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    assert_eq!(
        answer,
        Ok("proposal recorded, awaiting approval".to_owned())
    );
    let id = proposal_id(&fixture.app, &parent);
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::Proposal {
            id: id.clone(),
            children: children.clone(),
            decision: None,
        })
    );
    assert_eq!(
        fixture.store.load_updates(parent.clone()).unwrap().last(),
        Some(&Update::ChildrenProposed {
            id: id.clone(),
            children: children.clone(),
        })
    );
    assert_eq!(
        fixture.app.proposals.get(&(parent.clone(), id)),
        Some(&children)
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn a_proposal_for_an_unknown_repository_is_refused_and_names_the_repositories() {
    let Family {
        dir,
        mut fixture,
        parent,
        ..
    } = family();

    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![
            proposed("api", "Add the endpoint", "API endpoint"),
            proposed("apu", "Add the page", "Web page"),
        ],
    );

    assert_eq!(
        answer,
        Err("unknown repository \"apu\"; the repositories are: api, web".to_owned())
    );
    assert!(fixture.app.transcripts[&parent].entries().is_empty());
    assert!(fixture.app.proposals.is_empty());

    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fixture.app.open_project(dir.path()).unwrap();
    let root = fixture.app.projects[0].repositories[0].clone();
    assert!(root.is_root);

    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![proposed(&root.name, "Fix it", "Root")],
    );

    assert_eq!(
        answer,
        Err(format!(
            "unknown repository \"{}\"; the repositories are: ",
            root.name
        ))
    );
    assert!(fixture.app.transcripts[&parent].entries().is_empty());
}

#[test]
fn an_empty_proposal_and_a_blank_prompt_are_refused() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();

    let empty = call_propose(&mut fixture, &parent, Vec::new());
    let blank = call_propose(
        &mut fixture,
        &parent,
        vec![
            proposed("api", "Add the endpoint", "API endpoint"),
            proposed("web", " \n ", "Web page"),
        ],
    );

    assert_eq!(
        empty,
        Err("propose_children needs at least one child".to_owned())
    );
    assert_eq!(blank, Err("child 2 has an empty prompt".to_owned()));
    assert!(fixture.app.transcripts[&parent].entries().is_empty());
    assert!(fixture.app.proposals.is_empty());
}

#[test]
fn a_blank_title_is_derived_from_the_prompt() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();

    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![proposed("api", "\nAdd the endpoint\nwith tests", "  ")],
    );

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    assert_eq!(
        fixture.app.transcripts[&parent].entries(),
        [Entry::Proposal {
            id: proposal_id(&fixture.app, &parent),
            children: vec![proposed(
                "api",
                "\nAdd the endpoint\nwith tests",
                "Add the endpoint"
            )],
            decision: None,
        }]
    );
}

#[test]
fn tool_calls_from_non_orchestrators_and_unknown_sessions_are_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let children = vec![proposed("api", "Add the endpoint", "API endpoint")];

    let repository_session = call_propose(&mut fixture, &child, children.clone());
    let unknown = call_propose(&mut fixture, &SessionId::new(), children.clone());
    fixture
        .app
        .apply_event(BackendEvent::SessionClosed {
            session: parent.clone(),
            error: "agent crashed".into(),
        })
        .unwrap();
    let detached = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

    assert_eq!(
        repository_session,
        Err("this session is not an orchestrator".to_owned())
    );
    assert_eq!(unknown, Err("unknown session".to_owned()));
    assert_eq!(detached, Err("unknown session".to_owned()));
    assert!(fixture.app.transcripts[&child].entries().is_empty());
    assert!(fixture.app.proposals.is_empty());
    assert!(prompts(&fixture.backend, &child).is_empty());
}

#[test]
fn an_orchestrator_with_an_undecided_proposal_waits_for_you_when_its_turn_ends() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let children = vec![proposed("web", "Add the page", "Web page")];
    call_propose(&mut fixture, &parent, children).unwrap();
    finish(&mut fixture, &child, "api done");

    assert_eq!(deferred(&fixture.app, &parent), 1);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    // The card does not hold the orchestrator's turn: the result waiting behind it goes out.
    let result = result_text(
        &child,
        TurnOrigin::Orchestrator,
        &ChildOutcome::Ended(StopReason::EndTurn),
        "api done",
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(
        prompts(&fixture.backend, &parent),
        ["Plan it".to_owned(), result]
    );
    assert_eq!(deferred(&fixture.app, &parent), 0);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
    assert_eq!(prompts(&fixture.backend, &parent).len(), 2);
}

#[test]
fn an_orchestrator_waiting_only_on_its_card_takes_a_prompt() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = waiting_on_the_card(&mut fixture, &parent, two_children());
    assert!(!fixture.app.session_view().unwrap().running());

    fixture.app.submit_prompt("Go on", Vec::new()).unwrap();

    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: parent.clone(),
            content: vec![PromptContent::Text("Go on".into())],
        })
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("api", "Add the endpoint", "API endpoint", false)],
        )
        .map(started_ids)
        .unwrap();

    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(
        prompts(&fixture.backend, &parent),
        [
            "Plan it".to_owned(),
            "Go on".to_owned(),
            children_started_prompt(&approved(&fixture.app, &parent, &id))
        ]
    );
}

#[test]
fn a_waiting_permission_still_blocks_prompts() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    propose_now(&mut fixture, &parent, two_children());
    send(&mut fixture, &parent, read_request());
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
    assert!(fixture.app.session_view().unwrap().running());

    let refused = fixture.app.submit_prompt("Go on", Vec::new());
    let switched = fixture
        .app
        .switch_backend(parent.clone(), backend_id("claude"));

    assert!(
        matches!(refused, Err(AppError::TurnInProgress)),
        "{refused:?}"
    );
    assert!(
        matches!(switched, Err(AppError::TurnInProgress)),
        "{switched:?}"
    );
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
}

/// A request to read `main.rs`, which an orchestrator asks the user about.
fn read_request() -> Update {
    Update::PermissionRequested {
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
    }
}

/// A request of `kind` about the files `locations`.
fn request_at(kind: ToolKind, locations: &[PathBuf]) -> Update {
    let Update::PermissionRequested {
        id,
        tool_call_id,
        title,
        options,
        name,
        ..
    } = read_request()
    else {
        unreachable!("read_request is a permission request")
    };
    Update::PermissionRequested {
        id,
        tool_call_id,
        title,
        kind,
        options,
        name,
        locations: locations.to_vec(),
    }
}

/// Who works in the worktree `web` of [`Disk`], besides the child in `api`.
#[derive(Clone, Copy)]
enum WebWorktree {
    /// Nobody.
    Nobody,
    /// A session that is nobody's child.
    Stranger,
    /// A child of another orchestrator.
    OtherOrchestratorsChild,
}

/// Real folders under one temp dir: the worktree `api` of the orchestrator's child, the
/// worktree `web` of someone else, `outside`, and a link `api/link` leading to `outside`.
struct Disk {
    root: TempDir,
}

impl Disk {
    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }
}

fn disk() -> Disk {
    let root = TempDir::new().unwrap();
    for folder in ["api/src", "web", "outside"] {
        fs::create_dir_all(root.path().join(folder)).unwrap();
    }
    for file in ["api/src/main.rs", "web/a.rs", "outside/secret"] {
        fs::write(root.path().join(file), "x").unwrap();
    }
    std::os::unix::fs::symlink(root.path().join("outside"), root.path().join("api/link")).unwrap();
    Disk { root }
}

fn in_worktree(fixture: &mut Fixture, session: &SessionId, path: PathBuf) {
    fixture
        .app
        .change_session(session.clone(), |worktree| {
            worktree.workspace = Workspace::Worktree {
                branch: "feat/x".into(),
                base: "main".into(),
                fetched: false,
                path,
            };
        })
        .unwrap();
}

/// An orchestrator mid-turn whose child works in the worktree `api` of a [`Disk`], after
/// `request` was sent to it: its status and the last call to the backend.
fn ask_with_worktree_child(
    web: WebWorktree,
    request: impl FnOnce(&Disk) -> Update,
) -> (Status, Call) {
    let disk = disk();
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    in_worktree(&mut fixture, &child, disk.path("api"));
    match web {
        WebWorktree::Nobody => {}
        WebWorktree::Stranger => {
            let stranger =
                attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
            in_worktree(&mut fixture, &stranger, disk.path("web"));
        }
        WebWorktree::OtherOrchestratorsChild => {
            let other = attached_session(&mut fixture, &project, Scope::Project);
            let theirs = attached_child(&mut fixture, &project, &other, "web", "Theirs");
            in_worktree(&mut fixture, &theirs, disk.path("web"));
        }
    }
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    send(&mut fixture, &parent, request(&disk));
    (
        status(&fixture.app, &parent),
        fixture.backend.calls().last().cloned().unwrap(),
    )
}

fn asks(outcome: &(Status, Call)) -> bool {
    outcome.0 == Status::WaitingForYou && !matches!(outcome.1, Call::RespondPermission { .. })
}

#[test]
fn an_orchestrator_reads_in_a_childs_worktree_without_asking() {
    let (status, call) = ask_with_worktree_child(WebWorktree::Nobody, |disk| {
        request_at(
            ToolKind::Read,
            &[
                disk.path("api/src/main.rs"),
                disk.path("api/src"),
                disk.path("api/src/../src/main.rs"),
            ],
        )
    });

    assert_eq!(status, Status::Running);
    assert!(
        matches!(
            call,
            Call::RespondPermission {
                option: Some(ref option),
                ..
            } if option == &PermissionOptionId::new("allow")
        ),
        "{call:?}"
    );
}

#[test]
fn a_read_outside_the_childrens_worktrees_still_asks() {
    type Locations = fn(&Disk) -> Vec<PathBuf>;
    let cases: [Locations; 7] = [
        |disk| vec![disk.path("api/src/main.rs"), disk.path("outside/secret")],
        |disk| vec![disk.path("outside/secret")],
        // The folder holding the worktree is not inside it.
        |disk| vec![disk.path("api/..")],
        |disk| vec![disk.path("api/../outside/secret")],
        // A sibling that merely starts with the worktree's name.
        |disk| {
            fs::create_dir_all(disk.path("api-x")).unwrap();
            fs::write(disk.path("api-x/a.rs"), "x").unwrap();
            vec![disk.path("api-x/a.rs")]
        },
        // A file that does not exist cannot be resolved.
        |disk| vec![disk.path("api/src/missing.rs")],
        |_| vec![PathBuf::from("api/src/main.rs")],
    ];
    for (index, locations) in cases.into_iter().enumerate() {
        let outcome = ask_with_worktree_child(WebWorktree::Nobody, |disk| {
            request_at(ToolKind::Read, &locations(disk))
        });

        assert!(asks(&outcome), "case {index}: {outcome:?}");
    }
}

#[test]
fn a_link_leading_out_of_a_worktree_does_not_count_as_inside() {
    for relative in ["api/link/secret", "api/link/../secret", "api/link"] {
        let outcome = ask_with_worktree_child(WebWorktree::Nobody, |disk| {
            request_at(ToolKind::Read, &[disk.path(relative)])
        });

        assert!(asks(&outcome), "{relative}: {outcome:?}");
    }
}

#[test]
fn a_search_in_a_childs_worktree_still_asks() {
    let outcome = ask_with_worktree_child(WebWorktree::Nobody, |disk| {
        request_at(ToolKind::Search, &[disk.path("api/src")])
    });

    assert!(asks(&outcome), "{outcome:?}");
}

#[test]
fn another_sessions_worktree_does_not_count() {
    for web in [WebWorktree::Stranger, WebWorktree::OtherOrchestratorsChild] {
        let outcome = ask_with_worktree_child(web, |disk| {
            request_at(ToolKind::Read, &[disk.path("web/a.rs")])
        });

        assert!(asks(&outcome), "{outcome:?}");
    }
}

#[test]
fn a_read_in_a_checked_out_childs_folder_still_asks() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let cwd = fixture.app.session(&child).unwrap().cwd.clone();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();

    send(
        &mut fixture,
        &parent,
        request_at(ToolKind::Read, &[cwd.join("main.rs")]),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
}

#[test]
fn an_edit_in_a_childs_worktree_is_still_denied() {
    for kind in [
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Execute,
    ] {
        let (_, call) = ask_with_worktree_child(WebWorktree::Nobody, |disk| {
            request_at(kind, &[disk.path("api/src/main.rs")])
        });

        // The request offers nothing to reject with, so the denial cancels it.
        assert!(
            matches!(call, Call::RespondPermission { option: None, .. }),
            "{kind:?}: {call:?}"
        );
    }
}

#[test]
fn a_read_without_locations_still_asks() {
    let outcome = ask_with_worktree_child(WebWorktree::Nobody, |_| request_at(ToolKind::Read, &[]));

    assert!(asks(&outcome), "{outcome:?}");
}

#[test]
fn send_to_child_prompts_an_idle_child_now() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(prompts(&fixture.backend, &child), ["Also add tests"]);
    assert_eq!(
        fixture.app.transcripts[&child].entries().last(),
        Some(&Entry::OrchestratorPrompt("Also add tests".into()))
    );
    assert_eq!(status(&fixture.app, &child), Status::Running);
    // The orchestrator's own transcript tells which child it prompted with what.
    assert_eq!(
        fixture.app.transcripts[&parent].entries(),
        [Entry::ChildPrompted {
            child,
            prompt: "Also add tests".into(),
        }]
    );
}

#[test]
fn a_prompt_sent_to_a_child_is_still_in_the_orchestrators_transcript_after_a_restart() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    call_send_to_child(&mut fixture, &parent, &child, "Also add tests").unwrap();
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);

    restarted.app.select_session(parent).unwrap();

    assert_eq!(
        restarted.app.session_view().unwrap().entries,
        [Entry::ChildPrompted {
            child,
            prompt: "Also add tests".into(),
        }]
    );
}

#[test]
fn send_to_child_queues_for_a_busy_child_until_its_turn_ends() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("added".into()));

    let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

    assert_eq!(
        answer,
        Ok(format!(
            "prompt queued for child {child}; it goes out when the child is free"
        ))
    );
    assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);
    // A queued prompt is recorded when the orchestrator sends it, not when it goes out.
    assert_eq!(
        fixture.app.transcripts[&parent].entries(),
        [Entry::ChildPrompted {
            child: child.clone(),
            prompt: "Also add tests".into(),
        }]
    );

    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "added"
        )]
    );
    assert_eq!(
        prompts(&fixture.backend, &child),
        ["Add the endpoint", "Also add tests"]
    );
    assert_eq!(
        fixture.app.transcripts[&child].entries().last(),
        Some(&Entry::OrchestratorPrompt("Also add tests".into()))
    );
    assert_eq!(status(&fixture.app, &child), Status::Running);
}

#[test]
fn send_to_child_refuses_a_blank_prompt() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let unknown = SessionId::new();

    let blank = call_send_to_child(&mut fixture, &parent, &child, " \n ");
    let blank_to_unknown = call_send_to_child(&mut fixture, &parent, &unknown, "");

    assert_eq!(blank, Err("send_to_child needs a prompt".to_owned()));
    assert_eq!(
        blank_to_unknown,
        Err("send_to_child needs a prompt".to_owned())
    );
    assert!(prompts(&fixture.backend, &child).is_empty());
    assert!(fixture.app.transcripts[&child].entries().is_empty());
    assert!(fixture.app.transcripts[&parent].entries().is_empty());
    assert_eq!(status(&fixture.app, &child), Status::Idle);
    assert_eq!(deferred(&fixture.app, &child), 0);
}

#[test]
fn send_to_child_refuses_strangers() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let stranger = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
    let unknown = SessionId::new();

    let to_unknown = call_send_to_child(&mut fixture, &parent, &unknown, "Also add tests");
    let to_stranger = call_send_to_child(&mut fixture, &parent, &stranger, "Also add tests");

    assert_eq!(to_unknown, Err(format!("unknown child {unknown}")));
    assert_eq!(
        to_stranger,
        Err(format!("{stranger} is not a child of this session"))
    );
    assert!(prompts(&fixture.backend, &stranger).is_empty());
    assert_eq!(status(&fixture.app, &stranger), Status::Idle);
    assert!(fixture.app.transcripts[&parent].entries().is_empty());
}

/// The calls `backend` recorded from the call at position `from` on.
fn calls_since(backend: &RecordingBackend, from: usize) -> Vec<Call> {
    backend.calls().into_iter().skip(from).collect()
}

#[test]
fn stopping_the_children_stops_every_child_in_a_turn_and_nothing_else() {
    let Family {
        dir: _dir,
        mut fixture,
        project,
        parent,
        child: running,
    } = family();
    let waiting = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    let done = attached_child(&mut fixture, &project, &parent, "web", "Docs");
    let idle = attached_child(&mut fixture, &project, &parent, "api", "Later");
    let below = attached_child(&mut fixture, &project, &running, "api", "Below");
    let stranger = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
    for session in [&running, &waiting, &done, &below, &stranger] {
        orchestrate(&mut fixture, session);
    }
    send(&mut fixture, &waiting, read_request());
    // The finished child's result starts a turn of the orchestrator itself.
    send(&mut fixture, &done, Update::TurnEnded(StopReason::EndTurn));
    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(status(&fixture.app, &waiting), Status::WaitingForYou);
    let before = fixture.backend.calls().len();

    fixture.app.stop_children(&parent).unwrap();

    assert_eq!(
        calls_since(&fixture.backend, before),
        [
            Call::Cancel {
                session: running.clone()
            },
            Call::RespondPermission {
                session: waiting.clone(),
                permission: PermissionId(1),
                option: None,
            },
            Call::Cancel {
                session: waiting.clone()
            },
        ]
    );
    // A cancelled turn stays running until the backend reports its end.
    assert_eq!(status(&fixture.app, &running), Status::Running);
    assert_eq!(status(&fixture.app, &waiting), Status::Running);
    assert_eq!(status(&fixture.app, &done), Status::Done);
    assert_eq!(status(&fixture.app, &idle), Status::Idle);
    assert_eq!(status(&fixture.app, &below), Status::Running);
    assert_eq!(status(&fixture.app, &stranger), Status::Running);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn stopping_the_children_drops_a_prompt_that_has_not_gone_out() {
    let Family {
        dir: _dir,
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);
    assert_eq!(status(&fixture.app, &child), Status::Running);
    let before = fixture.backend.calls().len();

    fixture.app.stop_children(&parent).unwrap();

    assert_eq!(status(&fixture.app, &child), Status::Idle);
    // Nothing reached the child's agent, so there is no turn to cancel; the orchestrator
    // learns that the child was stopped.
    assert_eq!(
        calls_since(&fixture.backend, before),
        [Call::Prompt {
            session: parent.clone(),
            content: vec![PromptContent::Text(result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::Cancelled),
                "(no final message)"
            ))],
        }]
    );
}

#[test]
fn stopping_the_children_of_an_unknown_session_fails_and_changes_nothing() {
    let Family {
        dir: _dir,
        mut fixture,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    let before = fixture.backend.calls().len();
    let unknown = SessionId::new();

    let result = fixture.app.stop_children(&unknown);

    assert!(matches!(result, Err(AppError::UnknownSession(id)) if id == unknown));
    assert!(calls_since(&fixture.backend, before).is_empty());
    assert_eq!(status(&fixture.app, &child), Status::Running);
}

#[test]
fn stopping_the_children_stops_at_the_first_child_the_backend_refuses() {
    let Family {
        dir: _dir,
        mut fixture,
        project,
        parent,
        child: first,
    } = family();
    let second = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    orchestrate(&mut fixture, &first);
    orchestrate(&mut fixture, &second);
    fixture.backend.fail_next_call();
    let before = fixture.backend.calls().len();

    let result = fixture.app.stop_children(&parent);

    assert!(matches!(result, Err(AppError::Backend(_))));
    // The refused cancel is the last command: the second child keeps running.
    assert_eq!(
        calls_since(&fixture.backend, before),
        [Call::Cancel { session: first }]
    );
    assert_eq!(status(&fixture.app, &second), Status::Running);
}

fn choice(name: &str, prompt: &str, title: &str, auto_allow: bool) -> ChildChoice {
    ChildChoice {
        repository: repository(name),
        prompt: prompt.into(),
        title: title.into(),
        auto_allow,
        branch: BranchChoice::default(),
    }
}

/// The sessions an approval started at once; panics when it waits for git.
fn started_ids(approval: Approval) -> Vec<SessionId> {
    match approval {
        Approval::Started(ids) => ids,
        Approval::Pending => panic!("the approval waits for git"),
    }
}

/// Every create request the backend received: its id, session, working directory and
/// whether the session is an orchestrator.
fn creates(backend: &RecordingBackend) -> Vec<(RequestId, SessionId, PathBuf, bool)> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::NewSession {
                request,
                session,
                cwd,
                orchestrator,
                ..
            } => Some((request, session, cwd, orchestrator)),
            _ => None,
        })
        .collect()
}

/// Lets the orchestrator `parent` propose `children` and returns the proposal's id.
fn propose_now(
    fixture: &mut Fixture,
    parent: &SessionId,
    children: Vec<ProposedChild>,
) -> ProposalId {
    call_propose(fixture, parent, children).unwrap();
    proposal_id(&fixture.app, parent)
}

/// The two children the tests propose by default.
fn two_children() -> Vec<ProposedChild> {
    vec![
        proposed("api", "Add the endpoint", "API endpoint"),
        proposed("web", "Add the page", "Web page"),
    ]
}

/// The decision recorded on the proposal `id` in the session's transcript.
fn decision(app: &App, session: &SessionId, id: &ProposalId) -> Option<Decision> {
    app.transcripts[session]
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

/// The children recorded as approved on the proposal `id`; none when it is not approved.
fn approved(app: &App, session: &SessionId, id: &ProposalId) -> Vec<ApprovedChild> {
    app.transcripts[session]
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Proposal {
                id: existing,
                decision: Some(Decision::Approved(children)),
                ..
            } if existing == id => Some(children.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The message of an [`AppError::InvalidChildren`], `None` for any other result.
fn invalid(result: Result<Approval, AppError>) -> Option<String> {
    result
        .err()
        .filter(|error| matches!(error, AppError::InvalidChildren(_)))
        .map(|error| error.to_string())
}

/// Lets the orchestrator `parent` start a turn, propose `children` and end the turn, so it
/// waits for the decision; returns the proposal's id.
fn waiting_on_the_card(
    fixture: &mut Fixture,
    parent: &SessionId,
    children: Vec<ProposedChild>,
) -> ProposalId {
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let id = propose_now(fixture, parent, children);
    send(fixture, parent, Update::TurnEnded(StopReason::EndTurn));
    assert_eq!(status(&fixture.app, parent), Status::WaitingForYou);
    id
}

/// Answers the create request `request` for `session`.
fn create(fixture: &mut Fixture, request: RequestId, session: &SessionId) {
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new(format!("b-{session}")),
            config: Vec::new(),
        })
        .unwrap();
}

/// Asserts that `child` was started as approved, as a child of `parent`, and that its
/// prompt waits for its backend session.
fn assert_started(fixture: &Fixture, parent: &SessionId, child: &ApprovedChild) {
    let session = fixture.app.session(&child.session).unwrap();
    assert_eq!(session.parent.as_ref(), Some(parent));
    assert_eq!(session.scope, Scope::Repository(child.repository.clone()));
    assert_eq!(session.title, child.title);
    assert_eq!(session.auto_allow, child.auto_allow);
    assert_eq!(session.status, Status::Running);
    assert_eq!(
        fixture.app.transcripts[&child.session].entries(),
        [Entry::OrchestratorPrompt(child.prompt.clone())]
    );
    assert!(prompts(&fixture.backend, &child.session).is_empty());
}

/// Asserts that `child` was sent its prompt after its repository's instruction chain.
fn assert_prompted_with_instructions(
    fixture: &Fixture,
    project: &ProjectId,
    child: &ApprovedChild,
) {
    let project = fixture.app.project(project).unwrap();
    let repository = project
        .repositories
        .iter()
        .find(|repository| repository.id == child.repository)
        .unwrap();
    assert_eq!(
        prompts(&fixture.backend, &child.session),
        [inject(
            &instruction_chain(project, Some(repository)),
            &child.prompt
        )]
    );
}

/// A child started as `session` in the checkout of the repository `name` of `project` with
/// its `(prompt, title)`.
fn checked_out_child(
    app: &App,
    project: &ProjectId,
    session: &SessionId,
    name: &str,
    (prompt, title): (&str, &str),
    auto_allow: bool,
) -> ApprovedChild {
    let scope = Scope::Repository(repository(name));
    ApprovedChild {
        session: session.clone(),
        repository: repository(name),
        prompt: prompt.into(),
        title: title.into(),
        auto_allow,
        mode: BranchMode::CheckedOut,
        branch: None,
        cwd: app.working_directory(project, &scope).unwrap(),
    }
}

#[test]
fn approving_starts_one_child_per_entry_and_wakes_the_orchestrator() {
    let Family {
        dir,
        mut fixture,
        project,
        parent,
        ..
    } = family();
    fs::write(
        dir.path().join("api/AGENTS.md"),
        "Keep the API compatible.\n",
    )
    .unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());
    let before = creates(&fixture.backend).len();
    let current = fixture.app.current_session();

    let children = fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![
                choice("api", "Add the endpoint with tests", "API endpoint", false),
                choice("web", "Add the page", "Web page", true),
            ],
        )
        .map(started_ids)
        .unwrap();

    let created = creates(&fixture.backend).split_off(before);
    assert_eq!(
        created
            .iter()
            .map(|(_, session, _, _)| session.clone())
            .collect::<Vec<_>>(),
        children
    );
    for ((_, _, cwd, orchestrator), name) in created.iter().zip(["api", "web"]) {
        let scope = Scope::Repository(repository(name));
        assert_eq!(
            cwd,
            &fixture.app.working_directory(&project, &scope).unwrap()
        );
        assert!(!orchestrator);
    }
    let expected = vec![
        checked_out_child(
            &fixture.app,
            &project,
            &children[0],
            "api",
            ("Add the endpoint with tests", "API endpoint"),
            false,
        ),
        checked_out_child(
            &fixture.app,
            &project,
            &children[1],
            "web",
            ("Add the page", "Web page"),
            true,
        ),
    ];
    for child in &expected {
        assert_started(&fixture, &parent, child);
    }
    let wake = children_started_prompt(&expected);
    assert_eq!(
        fixture.app.transcripts[&parent].entries(),
        [
            Entry::Proposal {
                id: id.clone(),
                children: two_children(),
                decision: Some(Decision::Approved(expected.clone())),
            },
            Entry::PartiturPrompt(wake.clone()),
        ]
    );
    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: parent.clone(),
            content: vec![PromptContent::Text(wake)],
        })
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert!(fixture.app.proposals.is_empty());
    assert_eq!(fixture.app.current_session(), current);

    for (request, child, _, _) in &created {
        create(&mut fixture, *request, child);
    }

    for child in &expected {
        assert_prompted_with_instructions(&fixture, &project, child);
    }
    assert!(
        prompts(&fixture.backend, &children[0])[0].contains("Keep the API compatible."),
        "the api child's prompt lacks its instructions"
    );
}

#[test]
fn the_children_take_the_card_values_not_the_proposal() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());

    let children = fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![
                choice("web", "Add the page", "Web page", false),
                choice("web", "\nStyle the page\nwith care", "  ", false),
            ],
        )
        .map(started_ids)
        .unwrap();

    let approved = approved(&fixture.app, &parent, &id);
    assert_eq!(
        approved
            .iter()
            .map(|child| (child.repository.as_str(), child.title.as_str()))
            .collect::<Vec<_>>(),
        [("web", "Web page"), ("web", "Style the page")]
    );
    assert_eq!(
        approved
            .iter()
            .map(|child| child.session.clone())
            .collect::<Vec<_>>(),
        children
    );
    let child = fixture.app.session(&children[1]).unwrap();
    assert_eq!(child.title, "Style the page");
    assert_eq!(child.scope, Scope::Repository(repository("web")));
    assert_eq!(
        fixture.app.transcripts[&children[1]].entries(),
        [Entry::OrchestratorPrompt(
            "\nStyle the page\nwith care".into()
        )]
    );
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

/// The backend each of `sessions` was asked to be created on, in order.
fn created_on(backend: &RecordingBackend, sessions: &[SessionId]) -> Vec<BackendId> {
    sessions
        .iter()
        .map(|wanted| {
            backend
                .calls()
                .into_iter()
                .find_map(|call| match call {
                    Call::NewSession {
                        session, backend, ..
                    } if session == *wanted => Some(backend),
                    _ => None,
                })
                .unwrap()
        })
        .collect()
}

/// The card that starts both of [`two_children`] as proposed.
fn two_choices() -> Vec<ChildChoice> {
    vec![
        choice("api", "Add the endpoint", "API endpoint", false),
        choice("web", "Add the page", "Web page", false),
    ]
}

#[test]
fn children_run_on_their_orchestrators_backend() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    fixture
        .app
        .switch_backend(parent.clone(), backend_id("b"))
        .unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
    create(&mut fixture, request, &parent);
    let id = propose_now(&mut fixture, &parent, two_children());

    let children = fixture
        .app
        .approve_proposal(parent.clone(), id, two_choices())
        .map(started_ids)
        .unwrap();

    assert_eq!(children.len(), 2);
    assert_eq!(
        created_on(&fixture.backend, &children),
        [backend_id("b"), backend_id("b")]
    );
    for child in &children {
        assert_eq!(fixture.app.session(child).unwrap().backend, backend_id("b"));
    }
}

#[test]
fn children_of_an_orchestrator_on_a_removed_backend_run_on_the_default_without_its_model() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_on(MemoryStore::new(), backends_a_and_b(), backend_id("a"));
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    fixture
        .app
        .switch_backend(parent.clone(), backend_id("b"))
        .unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
    create(&mut fixture, request, &parent);
    fixture
        .app
        .change_session(parent.clone(), |session| {
            session.model = Some(ConfigValueId::new("opus"));
            session.effort = Some(ConfigValueId::new("high"));
        })
        .unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());
    let only_a = backends_a_and_b().into_iter().take(1).collect();
    let mut restarted = fixture_on(fixture.store.clone(), only_a, backend_id("a"));
    drop(fixture);
    restarted.app.select_session(parent.clone()).unwrap();
    assert_eq!(
        restarted.app.session(&parent).unwrap().backend,
        backend_id("b")
    );

    let children = restarted
        .app
        .approve_proposal(parent.clone(), id, two_choices())
        .map(started_ids)
        .unwrap();

    assert_eq!(
        created_on(&restarted.backend, &children),
        [backend_id("a"), backend_id("a")]
    );
    for child in &children {
        assert_eq!(
            restarted.app.session(child).unwrap().backend,
            backend_id("a")
        );
        assert_eq!(restarted.app.session(child).unwrap().model, None);
        assert_eq!(restarted.app.session(child).unwrap().effort, None);
    }
}

#[test]
fn children_inherit_their_orchestrators_model_and_effort() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    fixture
        .app
        .change_session(parent.clone(), |session| {
            session.model = Some(ConfigValueId::new("opus"));
            session.effort = Some(ConfigValueId::new("xhigh"));
        })
        .unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());

    let children = fixture
        .app
        .approve_proposal(parent, id, two_choices())
        .map(started_ids)
        .unwrap();

    for child in &children {
        let session = fixture.app.session(child).unwrap();
        assert_eq!(session.model, Some(ConfigValueId::new("opus")));
        assert_eq!(session.effort, Some(ConfigValueId::new("xhigh")));
    }
    let child = children.first().unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls
        .iter()
        .filter_map(Call::as_new_session)
        .find(|(_, session, _)| *session == child)
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: child.clone(),
            backend_session: BackendSessionId::new(format!("b-{child}")),
            config: agent_options("sonnet"),
        })
        .unwrap();
    let set = |option: &str, value: &str| Call::SetConfig {
        session: child.clone(),
        option: ConfigId::new(option),
        value: ConfigValueId::new(value),
    };
    let before = fixture.backend.calls().len();
    assert_eq!(fixture.backend.calls().last(), Some(&set("model", "opus")));
    assert_eq!(
        fixture.app.session(child).unwrap().effort,
        Some(ConfigValueId::new("xhigh"))
    );

    send(
        &mut fixture,
        child,
        Update::ConfigOptions(agent_options("opus")),
    );

    let calls = fixture.backend.calls();
    assert_eq!(calls[before], set("effort", "xhigh"));
    assert_eq!(
        fixture.app.session(child).unwrap().effort,
        Some(ConfigValueId::new("xhigh"))
    );
    let position = |wanted: &Call| calls.iter().position(|call| call == wanted).unwrap();
    let model = position(&set("model", "opus"));
    let effort = position(&set("effort", "xhigh"));
    let prompt = calls
        .iter()
        .position(|call| matches!(call, Call::Prompt { session, .. } if session == child))
        .unwrap();
    assert!(model < prompt);
    assert!(effort < prompt);
}

#[test]
fn a_child_whose_inherited_model_is_rejected_still_sends_its_first_prompt() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    fixture
        .app
        .change_session(parent.clone(), |session| {
            session.model = Some(ConfigValueId::new("opus"));
            session.effort = Some(ConfigValueId::new("xhigh"));
        })
        .unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());
    let children = fixture
        .app
        .approve_proposal(parent, id, two_choices())
        .map(started_ids)
        .unwrap();
    let child = children.first().unwrap();
    let calls = fixture.backend.calls();
    let (request, _, _) = calls
        .iter()
        .filter_map(Call::as_new_session)
        .find(|(_, session, _)| *session == child)
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: child.clone(),
            backend_session: BackendSessionId::new(format!("b-{child}")),
            config: agent_options("sonnet"),
        })
        .unwrap();
    let is_prompt = |call: &Call| matches!(call, Call::Prompt { session, .. } if session == child);
    assert!(!fixture.backend.calls().iter().any(is_prompt));

    send(
        &mut fixture,
        child,
        Update::ConfigRejected {
            option: ConfigId::new("model"),
            value: ConfigValueId::new("opus"),
            error: "refused".into(),
        },
    );

    assert!(fixture.backend.calls().iter().any(is_prompt));
    let session = fixture.app.session(child).unwrap();
    assert_eq!(session.model, Some(ConfigValueId::new("sonnet")));
    assert_eq!(session.effort, Some(ConfigValueId::new("high")));
}

#[test]
fn an_orchestrator_waiting_on_the_card_runs_again_after_the_decision() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = waiting_on_the_card(&mut fixture, &parent, two_children());

    fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("api", "Add the endpoint", "API endpoint", false)],
        )
        .map(started_ids)
        .unwrap();

    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(
        prompts(&fixture.backend, &parent),
        [
            "Plan it".to_owned(),
            children_started_prompt(&approved(&fixture.app, &parent, &id))
        ]
    );
    assert_eq!(deferred(&fixture.app, &parent), 0);
}

#[test]
fn an_orchestrator_mid_turn_gets_the_wake_after_its_turn() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());

    fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("api", "Add the endpoint", "API endpoint", false)],
        )
        .map(started_ids)
        .unwrap();

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    let wake = children_started_prompt(&approved(&fixture.app, &parent, &id));
    assert_eq!(last_prompt(&fixture.backend, &parent), wake);
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::PartiturPrompt(wake))
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn invalid_children_change_nothing() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let calls = fixture.backend.calls();
    let sessions = fixture.app.sessions.len();

    let empty = invalid(
        fixture
            .app
            .approve_proposal(parent.clone(), id.clone(), Vec::new()),
    );
    let unknown = invalid(fixture.app.approve_proposal(
        parent.clone(),
        id.clone(),
        vec![choice("apu", "Add the endpoint", "API endpoint", false)],
    ));
    let blank = invalid(fixture.app.approve_proposal(
        parent.clone(),
        id.clone(),
        vec![
            choice("api", "Add the endpoint", "API endpoint", false),
            choice("web", " \n ", "Web page", true),
        ],
    ));

    assert_eq!(
        empty.as_deref(),
        Some("cannot start these children: propose_children needs at least one child")
    );
    assert_eq!(
        unknown.as_deref(),
        Some(
            "cannot start these children: unknown repository \"apu\"; the repositories are: api, web"
        )
    );
    assert_eq!(
        blank.as_deref(),
        Some("cannot start these children: child 2 has an empty prompt")
    );
    assert_eq!(fixture.backend.calls(), calls);
    assert_eq!(fixture.app.sessions.len(), sessions);
    assert_eq!(
        fixture.app.transcripts[&parent].pending_proposal(&id),
        Some(two_children().as_slice())
    );
    assert!(fixture.app.proposals.contains_key(&(parent.clone(), id)));
}

#[test]
fn a_backend_failure_while_approving_leaves_the_proposal_undecided() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let sessions = fixture.app.sessions.len();
    fixture.backend.fail_next_call();

    let result = fixture.app.approve_proposal(
        parent.clone(),
        id.clone(),
        vec![choice("api", "Add the endpoint", "API endpoint", false)],
    );

    assert!(matches!(result, Err(AppError::Backend(_))), "{result:?}");
    assert_eq!(fixture.app.sessions.len(), sessions);
    assert_eq!(
        fixture.app.transcripts[&parent].pending_proposal(&id),
        Some(two_children().as_slice())
    );
    assert!(fixture.app.proposals.contains_key(&(parent.clone(), id)));
    assert!(prompts(&fixture.backend, &parent).is_empty());
}

#[test]
fn rejecting_wakes_the_orchestrator_with_the_note() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let created = creates(&fixture.backend).len();

    fixture
        .app
        .reject_proposal(parent.clone(), id.clone(), Some("not now".into()))
        .unwrap();

    assert_eq!(
        decision(&fixture.app, &parent, &id),
        Some(Decision::Rejected {
            note: Some("not now".into())
        })
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        "proposal rejected by the user\nnote: not now"
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::PartiturPrompt(
            "proposal rejected by the user\nnote: not now".into()
        ))
    );
    assert!(fixture.app.proposals.is_empty());
    assert_eq!(creates(&fixture.backend).len(), created);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    let blank = propose_now(&mut fixture, &parent, two_children());

    fixture
        .app
        .reject_proposal(parent.clone(), blank.clone(), Some(" \n".into()))
        .unwrap();

    assert_eq!(
        decision(&fixture.app, &parent, &blank),
        Some(Decision::Rejected { note: None })
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        "proposal rejected by the user"
    );
}

#[test]
fn a_decided_proposal_cannot_be_approved_again() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let card = vec![choice("api", "Add the endpoint", "API endpoint", false)];
    fixture
        .app
        .approve_proposal(parent.clone(), id.clone(), card.clone())
        .map(started_ids)
        .unwrap();
    let created = creates(&fixture.backend).len();

    let again = fixture
        .app
        .approve_proposal(parent.clone(), id.clone(), card.clone());
    let rejected = fixture
        .app
        .reject_proposal(parent.clone(), id.clone(), None);
    let stranger = fixture
        .app
        .approve_proposal(SessionId::new(), id.clone(), card.clone());

    assert!(matches!(&again, Err(AppError::UnknownProposal(refused)) if *refused == id));
    assert_eq!(
        again.unwrap_err().to_string(),
        format!("no undecided proposal {id}")
    );
    assert!(matches!(rejected, Err(AppError::UnknownProposal(refused)) if refused == id));
    assert!(matches!(stranger, Err(AppError::UnknownSession(_))));
    assert_eq!(creates(&fixture.backend).len(), created);

    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    restarted.app.select_session(parent.clone()).unwrap();

    let after_restart = restarted
        .app
        .approve_proposal(parent.clone(), id.clone(), card);

    assert!(matches!(after_restart, Err(AppError::UnknownProposal(refused)) if refused == id));
    assert!(creates(&restarted.backend).is_empty());
}

#[test]
fn a_pending_proposal_survives_a_restart() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        ..
    } = family();
    let id = propose_now(&mut fixture, &parent, two_children());
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    assert!(restarted.app.proposals.is_empty());
    restarted.app.select_session(parent.clone()).unwrap();

    let children = restarted
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("web", "Add the page", "Web page", false)],
        )
        .map(started_ids)
        .unwrap();

    let created = creates(&restarted.backend);
    assert_eq!(created.len(), 1);
    assert_eq!(vec![created[0].1.clone()], children);
    assert_eq!(
        restarted.app.session(&children[0]).unwrap().parent,
        Some(parent.clone())
    );
    assert_eq!(approved(&restarted.app, &parent, &id).len(), 1);
    assert_eq!(
        restarted.app.transcripts[&parent].entries().last(),
        Some(&Entry::PartiturPrompt(children_started_prompt(&approved(
            &restarted.app,
            &parent,
            &id
        ))))
    );
    assert_eq!(status(&restarted.app, &parent), Status::Running);
}

#[test]
fn undecided_proposals_from_before_a_restart_still_hold_the_orchestrator() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        ..
    } = family();
    let older = propose_now(&mut fixture, &parent, two_children());
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    restarted.app.select_session(parent.clone()).unwrap();
    restarted.app.submit_prompt("Plan it", Vec::new()).unwrap();
    assert!(matches!(
        restarted.backend.calls().last(),
        Some(Call::LoadSession {
            request: RequestId(1),
            ..
        })
    ));
    restarted
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request: RequestId(1),
            session: parent.clone(),
            config: Vec::new(),
        })
        .unwrap();
    call_propose(
        &mut restarted,
        &parent,
        vec![proposed("web", "Add the page", "Web page")],
    )
    .unwrap();
    let newer = restarted
        .app
        .proposals
        .keys()
        .map(|(_, id)| id.clone())
        .find(|id| *id != older)
        .unwrap();
    send(
        &mut restarted,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);
    let card = vec![choice("web", "Add the page", "Web page", false)];

    restarted
        .app
        .approve_proposal(parent.clone(), newer.clone(), card.clone())
        .map(started_ids)
        .unwrap();

    assert_eq!(status(&restarted.app, &parent), Status::Running);
    assert_eq!(
        last_prompt(&restarted.backend, &parent),
        children_started_prompt(&approved(&restarted.app, &parent, &newer))
    );

    // The proposal from before the restart is still undecided, so the turn the wake started
    // ends waiting for you instead of done.
    send(
        &mut restarted,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);

    restarted
        .app
        .approve_proposal(parent.clone(), older.clone(), card)
        .map(started_ids)
        .unwrap();
    send(
        &mut restarted,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&restarted.backend, &parent),
        children_started_prompt(&approved(&restarted.app, &parent, &older))
    );
    assert_eq!(status(&restarted.app, &parent), Status::Done);
    assert_eq!(deferred(&restarted.app, &parent), 0);
}

/// The request that asked the backend to load `session`'s stored backend session.
fn load_request_of(backend: &RecordingBackend, session: &SessionId) -> RequestId {
    backend
        .calls()
        .into_iter()
        .find_map(|call| match call {
            Call::LoadSession {
                request,
                session: loaded,
                ..
            } if loaded == *session => Some(request),
            _ => None,
        })
        .unwrap()
}

/// Answers the load request of `session`.
fn load(fixture: &mut Fixture, session: &SessionId) {
    let request = load_request_of(&fixture.backend, session);
    fixture
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: session.clone(),
            config: Vec::new(),
        })
        .unwrap();
}

#[test]
fn a_child_result_after_a_restart_keeps_an_undecided_card_waiting() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    propose_now(&mut fixture, &parent, two_children());
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    restarted.app.select_session(child.clone()).unwrap();
    restarted
        .app
        .submit_prompt("Add the endpoint", Vec::new())
        .unwrap();
    load(&mut restarted, &child);
    send(&mut restarted, &child, Update::AgentText("api done".into()));

    send(
        &mut restarted,
        &child,
        Update::TurnEnded(StopReason::EndTurn),
    );
    load(&mut restarted, &parent);

    assert_eq!(
        prompts(&restarted.backend, &parent),
        [result_text(
            &child,
            TurnOrigin::User,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "api done"
        )]
    );
    assert_eq!(status(&restarted.app, &parent), Status::Running);

    send(
        &mut restarted,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);
}

#[test]
fn new_session_still_selects_and_titles_as_before() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();

    let session = fixture
        .app
        .new_session(project.clone(), Scope::Repository(repository("api")))
        .unwrap();

    assert_eq!(fixture.app.current_session(), Some(session.clone()));
    let created = fixture.app.session(&session).unwrap();
    assert_eq!(created.title, "New session");
    assert_eq!(created.parent, None);
    assert!(!created.auto_allow);
    assert!(!created.orchestrator);
    assert_eq!(created.status, Status::Idle);
    assert_eq!(
        fixture.store.load_sessions(project).unwrap(),
        std::slice::from_ref(created)
    );
    assert_eq!(fixture.app.transcripts[&session].entries(), []);
}

#[test]
fn a_decision_starts_the_deferred_results_before_the_wake() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    let id = waiting_on_the_card(&mut fixture, &parent, two_children());
    fixture.app.submit_prompt("Go on", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");
    // A stopped turn sends nothing deferred, so the results wait for the decision.
    fixture.app.cancel().unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
    assert_eq!(deferred(&fixture.app, &parent), 2);

    fixture
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("api", "Add the endpoint", "API endpoint", false)],
        )
        .map(started_ids)
        .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "api done"
        )
    );
    assert_eq!(deferred(&fixture.app, &parent), 2);
    assert_eq!(
        fixture.app.deferred[&parent].back(),
        Some(&Generated::Wake(children_started_prompt(&approved(
            &fixture.app,
            &parent,
            &id
        ))))
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn another_undecided_proposal_keeps_the_orchestrator_waiting() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let first = propose_now(&mut fixture, &parent, two_children());
    call_propose(
        &mut fixture,
        &parent,
        vec![proposed("web", "Add the page", "Web page")],
    )
    .unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    fixture
        .app
        .reject_proposal(parent.clone(), first, None)
        .unwrap();

    assert_eq!(status(&fixture.app, &parent), Status::Running);
    assert_eq!(
        prompts(&fixture.backend, &parent),
        ["Plan it", "proposal rejected by the user"]
    );

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    let second = proposal_id(&fixture.app, &parent);
    fixture
        .app
        .reject_proposal(parent.clone(), second, Some("later".into()))
        .unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [
            "Plan it",
            "proposal rejected by the user",
            "proposal rejected by the user\nnote: later"
        ]
    );
    assert_eq!(status(&fixture.app, &parent), Status::Done);
    assert_eq!(deferred(&fixture.app, &parent), 0);
}

#[test]
fn a_waiting_permission_keeps_the_orchestrator_waiting() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let id = propose_now(&mut fixture, &parent, two_children());
    send(&mut fixture, &parent, read_request());
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    fixture
        .app
        .reject_proposal(parent.clone(), id, None)
        .unwrap();

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 1);
}

#[test]
fn a_tool_call_that_fails_inside_is_answered_with_the_error() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.backend.fail_next_call();

    let result = fixture.app.apply_event(BackendEvent::ToolCalled {
        call: ToolRequestId(7),
        session: parent.clone(),
        tool: OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: "Also add tests".into(),
        },
    });

    assert!(matches!(result, Err(AppError::Backend(_))), "{result:?}");
    let error = result.unwrap_err();
    let calls = fixture.backend.calls();
    assert!(matches!(
        &calls[calls.len() - 2],
        Call::Prompt { session, .. } if *session == child
    ));
    assert_eq!(
        calls.last(),
        Some(&Call::AnswerTool {
            call: ToolRequestId(7),
            result: Err(error.to_string()),
        })
    );
}

#[test]
fn child_reports_agent_turn_to_parent() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "started");
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    send(&mut fixture, &child, Update::TurnStarted);
    send(&mut fixture, &child, Update::AgentText("all green".into()));
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Agent,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "all green"
        )
    );
    assert_eq!(status(&fixture.app, &child), Status::Done);
}

#[test]
fn generated_prompt_waits_for_agent_turn() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "started");
    send(&mut fixture, &child, Update::TurnStarted);

    let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

    assert_eq!(
        answer,
        Ok(format!(
            "prompt queued for child {child}; it goes out when the child is free"
        ))
    );
    assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);

    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    assert_eq!(
        prompts(&fixture.backend, &child),
        ["Add the endpoint", "Also add tests"]
    );
}

#[test]
fn agent_turn_of_orchestrator_awaiting_decision_ends_waiting() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    let children = vec![proposed("web", "Add the page", "Web page")];
    call_propose(&mut fixture, &parent, children).unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    send(&mut fixture, &parent, Update::TurnStarted);

    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
}

// Archiving an orchestrator with its children.

#[test]
fn archiving_an_orchestrator_archives_and_stops_its_children() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let grandchild = attached_child(&mut fixture, &project, &child, "web", "Web page");
    orchestrate(&mut fixture, &child);

    fixture.app.archive(parent.clone()).unwrap();

    let mark = fixture.app.session(&parent).unwrap().archived;
    assert!(mark.is_some());
    assert_eq!(fixture.app.session(&child).unwrap().archived, mark);
    assert_eq!(fixture.app.session(&grandchild).unwrap().archived, mark);
    assert_eq!(
        fixture.backend.calls().last(),
        Some(&Call::Cancel {
            session: child.clone()
        })
    );
    let stored = fixture.store.load_sessions(project).unwrap();
    assert!(stored.iter().all(|session| session.archived == mark));

    fixture.app.restore(parent.clone()).unwrap();

    assert_eq!(fixture.app.session(&parent).unwrap().archived, None);
    assert_eq!(fixture.app.session(&child).unwrap().archived, None);
    assert_eq!(fixture.app.session(&grandchild).unwrap().archived, None);
}

#[test]
fn a_child_cannot_be_archived_or_restored_alone() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    assert!(matches!(
        fixture.app.archive(child.clone()),
        Err(AppError::ArchiveChild(id)) if id == child
    ));
    assert_eq!(fixture.app.session(&child).unwrap().archived, None);

    fixture.app.archive(parent).unwrap();

    assert!(matches!(
        fixture.app.restore(child.clone()),
        Err(AppError::ArchiveChild(id)) if id == child
    ));
    assert!(fixture.app.session(&child).unwrap().archived.is_some());
}

#[test]
fn a_child_result_waits_while_its_orchestrator_is_archived() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("half".into()));
    fixture.app.archive(parent.clone()).unwrap();

    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Idle);
}

#[test]
fn restoring_sends_the_result_that_waited() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("half".into()));
    fixture.app.archive(parent.clone()).unwrap();
    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );

    fixture.app.restore(parent.clone()).unwrap();

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::Cancelled),
            "half"
        )]
    );
    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn restoring_in_a_closed_project_sends_nothing_deferred() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    fixture.app.archive(parent.clone()).unwrap();
    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );
    fixture.app.close_project(project).unwrap();

    fixture.app.restore(parent.clone()).unwrap();

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
}

#[test]
fn an_archived_orchestrator_cannot_approve_or_reject() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let id = waiting_on_the_card(&mut fixture, &parent, two_children());
    fixture.app.archive(parent.clone()).unwrap();
    let before = fixture.backend.calls().len();

    let approved = fixture
        .app
        .approve_proposal(parent.clone(), id.clone(), two_choices());
    let rejected = fixture.app.reject_proposal(parent.clone(), id, None);

    assert!(matches!(approved, Err(AppError::Archived(session)) if session == parent));
    assert!(matches!(rejected, Err(AppError::Archived(session)) if session == parent));
    assert_eq!(fixture.backend.calls().len(), before);
}

#[test]
fn the_archive_summary_counts_children_and_shows_them_running() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    attached_child(&mut fixture, &project, &child, "web", "Web page");
    fixture
        .app
        .change_session(parent.clone(), |done| done.status = Status::Done)
        .unwrap();
    orchestrate(&mut fixture, &child);

    let summary = fixture.app.archive_summary(&parent).unwrap();

    assert!(summary.orchestrator);
    assert_eq!(summary.children, 2);
    assert!(summary.stops);
    assert_eq!(summary.status, Status::Running);
    assert!(matches!(
        fixture.app.archive_summary(&child),
        Err(AppError::ArchiveChild(id)) if id == child
    ));
}

#[test]
fn the_archive_summary_of_a_done_orchestrator_with_idle_children() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture
        .app
        .change_session(parent.clone(), |done| done.status = Status::Done)
        .unwrap();

    let summary = fixture.app.archive_summary(&parent).unwrap();

    assert_eq!(summary.children, 1);
    assert!(!summary.stops);
    assert_eq!(summary.status, Status::Done);
}

#[test]
fn restoring_before_the_stopped_turn_ends_sends_nothing_into_it() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    assert_eq!(deferred(&fixture.app, &parent), 1);
    fixture.app.archive(parent.clone()).unwrap();

    fixture.app.restore(parent.clone()).unwrap();

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn a_failed_stop_saves_the_family_live_and_sends_what_it_held() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (waiting, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &waiting);
    let running = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    orchestrate(&mut fixture, &running);

    fixture.backend.fail_next_call();
    let refused = fixture.app.archive(parent.clone());

    assert!(matches!(
        refused,
        Err(AppError::Backend(
            crate::backend::BackendError::Unavailable(_)
        ))
    ));
    let stored = fixture.store.load_sessions(project).unwrap();
    assert!(stored.iter().all(|session| session.archived.is_none()));
    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert_eq!(prompts(&fixture.backend, &parent).len(), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

// Deleting an orchestrator with its children.

/// The sessions the backend was asked to close, in order.
fn closed(backend: &RecordingBackend) -> Vec<SessionId> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::CloseSession { session } => Some(session),
            _ => None,
        })
        .collect()
}

#[test]
fn deleting_an_orchestrator_deletes_and_closes_its_children() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let grandchild = attached_child(&mut fixture, &project, &child, "web", "Web page");
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("half".into()));

    fixture.app.delete(parent.clone(), &[]).unwrap();

    assert_eq!(
        closed(&fixture.backend),
        [parent.clone(), child.clone(), grandchild.clone()]
    );
    assert!(fixture.store.load_sessions(project).unwrap().is_empty());
    for member in [&parent, &child, &grandchild] {
        assert!(fixture.app.session(member).is_err());
        assert!(
            fixture
                .store
                .load_updates(member.clone())
                .unwrap()
                .is_empty()
        );
    }
    let calls = fixture.backend.calls().len();
    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(fixture.app.deferred.is_empty());
}

#[test]
fn a_child_cannot_be_deleted_alone() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();

    let refused = fixture.app.delete(child.clone(), &[]);

    assert!(matches!(&refused, Err(AppError::DeleteChild(id)) if *id == child));
    assert_eq!(
        refused.unwrap_err().to_string(),
        "delete the orchestrator: a child session is deleted with it"
    );
    assert!(fixture.app.session(&child).is_ok());
    assert_eq!(fixture.store.load_sessions(project).unwrap().len(), 2);
    assert!(closed(&fixture.backend).is_empty());
    assert!(matches!(
        fixture.app.delete_summary(&child),
        Err(AppError::DeleteChild(id)) if id == child
    ));
    assert!(fixture.app.session(&parent).is_ok());
}

#[test]
fn held_results_and_proposals_go_with_the_orchestrator_and_spare_others() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let other = attached_session(&mut fixture, &project, Scope::Project);
    let other_child = attached_child(&mut fixture, &project, &other, "web", "Web page");
    let other_proposal = waiting_on_the_card(&mut fixture, &other, two_children());
    fixture.app.submit_prompt("Again", Vec::new()).unwrap();
    finish(&mut fixture, &other_child, "web done");
    waiting_on_the_card(&mut fixture, &parent, two_children());
    fixture.app.submit_prompt("Plan more", Vec::new()).unwrap();
    finish(&mut fixture, &child, "api done");
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(deferred(&fixture.app, &other), 1);

    fixture.app.delete(parent.clone(), &[]).unwrap();

    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert!(!fixture.app.awaits_decision(&parent));
    assert_eq!(deferred(&fixture.app, &other), 1);
    assert_eq!(proposal_id(&fixture.app, &other), other_proposal);
    assert_eq!(fixture.app.current_session(), None);
    send(&mut fixture, &other, Update::TurnEnded(StopReason::EndTurn));
    assert!(last_prompt(&fixture.backend, &other).contains("web done"));
}

#[test]
fn a_tool_call_from_a_deleted_orchestrator_is_answered_with_the_deletion() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.delete(parent.clone(), &[]).unwrap();

    let answer = call_propose(&mut fixture, &parent, two_children());

    assert_eq!(answer, Err("the session was deleted".into()));
    assert!(!fixture.app.awaits_decision(&parent));
}

#[test]
fn a_tool_call_nothing_waits_for_any_more_is_dropped_quietly() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.delete(parent.clone(), &[]).unwrap();
    let called = |call| BackendEvent::ToolCalled {
        call,
        session: parent.clone(),
        tool: OrchestratorTool::ProposeChildren(two_children()),
    };

    fixture
        .backend
        .fail_next_call_with(BackendError::UnknownToolCall(ToolRequestId(8)));
    let gone = fixture.app.apply_event(called(ToolRequestId(8)));
    fixture.backend.fail_next_call();
    let failed = fixture.app.apply_event(called(ToolRequestId(9)));

    assert!(gone.is_ok());
    assert!(matches!(
        failed,
        Err(AppError::Backend(BackendError::Unavailable(_)))
    ));
}

#[test]
fn the_delete_summary_counts_the_children_of_an_archived_orchestrator() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    attached_child(&mut fixture, &project, &child, "web", "Web page");
    fixture.app.archive(parent.clone()).unwrap();

    let summary = fixture.app.delete_summary(&parent).unwrap();

    assert!(summary.orchestrator);
    assert_eq!(summary.children, 2);
    assert!(!summary.stops);
}

/// An open project with the repositories `names`, which git knows on `main` (with `develop`
/// and `origin/main`), an attached, idle orchestrator and its undecided proposal of a child
/// in `api` and one in `web`.
struct GitSplit {
    _dir: TempDir,
    fixture: Fixture,
    project: ProjectId,
    parent: SessionId,
    id: ProposalId,
}

fn git_split(names: &[&str]) -> GitSplit {
    let dir = project_dir(names);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    for repository in fixture.app.project(&project).unwrap().repositories.clone() {
        let info = repo_info(
            &repository.path,
            "main",
            &["main", "develop"],
            &["origin/main"],
        );
        fixture.git.add_repository(repository.path, info);
    }
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    let id = propose_now(&mut fixture, &parent, two_children());
    GitSplit {
        _dir: dir,
        fixture,
        project,
        parent,
        id,
    }
}

impl GitSplit {
    /// The folder of the repository `name`.
    fn path(&self, name: &str) -> PathBuf {
        let scope = Scope::Repository(repository(name));
        self.fixture
            .app
            .working_directory(&self.project, &scope)
            .unwrap()
    }

    /// Where the default pattern puts the worktree of `branch` in the repository `name`.
    fn worktree(&self, name: &str, branch: &str) -> PathBuf {
        self.path(name)
            .parent()
            .unwrap()
            .join(format!("{name}-wt"))
            .join(branch)
    }

    fn check(&mut self, children: &[ChildChoice]) -> Vec<ChildCheck> {
        self.fixture
            .app
            .check_split(&self.parent, &self.id, children)
            .unwrap()
    }

    /// Checks `children` again without the user asking, as after a git answer.
    fn recheck(&mut self, children: &[ChildChoice]) -> Vec<ChildCheck> {
        self.fixture
            .app
            .recheck_split(&self.parent, &self.id, children)
            .unwrap()
    }

    /// Checks `children`, lets git answer the reading and checks them again with it.
    fn checked(&mut self, children: &[ChildChoice]) -> Vec<ChildCheck> {
        self.check(children);
        assert!(settle(&mut self.fixture.app, &self.fixture.git).is_empty());
        self.recheck(children)
    }

    fn approve(&mut self, children: &[ChildChoice]) -> Result<Approval, AppError> {
        self.fixture
            .app
            .approve_proposal(self.parent.clone(), self.id.clone(), children.to_vec())
    }

    fn settle(&mut self) -> Vec<AppError> {
        settle(&mut self.fixture.app, &self.fixture.git)
    }

    fn approval(&mut self) -> Option<ApprovalView> {
        self.fixture
            .app
            .select_session(self.parent.clone())
            .unwrap();
        self.fixture.app.session_view().unwrap().approval
    }

    /// The git calls since the `from`th.
    fn git_since(&self, from: usize) -> Vec<GitCall> {
        self.fixture.git.calls().split_off(from)
    }

    /// The sessions created as children of the orchestrator.
    fn children(&self) -> Vec<SessionId> {
        self.fixture
            .app
            .sessions
            .iter()
            .filter(|session| session.parent.as_ref() == Some(&self.parent))
            .map(|session| session.id.clone())
            .collect()
    }

    fn undecided(&self) -> bool {
        self.fixture
            .app
            .proposals
            .contains_key(&(self.parent.clone(), self.id.clone()))
    }
}

/// A child of `name` titled `<name> child` in `mode` with the branch `branch`, from no base,
/// without fetching.
fn wants(name: &str, mode: BranchMode, branch: &str) -> ChildChoice {
    ChildChoice {
        branch: BranchChoice {
            mode,
            branch: branch.into(),
            base: None,
            fetch: false,
        },
        ..choice(name, "Do it", &format!("{name} child"), false)
    }
}

fn worktree_in(name: &str, branch: &str) -> ChildChoice {
    wants(name, BranchMode::Worktree, branch)
}

fn problems(checks: &[ChildCheck]) -> Vec<Option<String>> {
    checks.iter().map(|check| check.problem.clone()).collect()
}

/// The text of an [`AppError::InvalidChildren`] without its prefix.
fn refusal(result: Result<Approval, AppError>) -> String {
    match result {
        Err(AppError::InvalidChildren(text)) => text,
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_child_starts_from_the_orchestrators_defaults_and_is_checked_in_its_own_repository() {
    let mut split = git_split(&["api", "web"]);
    let (api, web) = (split.path("api"), split.path("web"));
    let mut info = repo_info(&api, "main", &["main", "develop"], &["origin/develop"]);
    info.branches[1].upstream_remote = Some("up".into());
    info.remote_branches.push("up/develop".into());
    split.fixture.git.add_repository(api.clone(), info);
    split
        .fixture
        .git
        .add_repository(web.clone(), repo_info(&web, "main", &["main"], &[]));
    let defaults = BranchChoice {
        mode: BranchMode::Worktree,
        branch: "feat/x".into(),
        base: Some("develop".into()),
        fetch: true,
    };
    let parent = split.parent.clone();
    split
        .fixture
        .app
        .change_session(parent, |orchestrator| {
            orchestrator.child_defaults = Some(defaults);
        })
        .unwrap();
    split
        .fixture
        .app
        .select_session(split.parent.clone())
        .unwrap();
    let defaults = split
        .fixture
        .app
        .session_view()
        .unwrap()
        .child_defaults
        .unwrap();
    let children: Vec<ChildChoice> = two_choices()
        .into_iter()
        .map(|child| ChildChoice {
            branch: defaults.clone(),
            ..child
        })
        .collect();

    let checks = split.checked(&children);

    assert_eq!(
        checks,
        [
            ChildCheck {
                loading: false,
                bases: vec!["develop".into(), "main".into()],
                base: Some("develop".into()),
                fetch: true,
                fetch_remote: Some("up".into()),
                worktree: Some(split.worktree("api", "feat/x")),
                problem: None,
            },
            ChildCheck {
                loading: false,
                bases: vec!["main".into()],
                base: Some("main".into()),
                fetch: false,
                fetch_remote: None,
                worktree: Some(split.worktree("web", "feat/x")),
                problem: None,
            },
        ]
    );
}

#[test]
fn the_check_reads_each_repository_once_in_the_background() {
    let mut split = git_split(&["api", "web", "docs"]);
    let children = [
        worktree_in("api", "feat/x"),
        worktree_in("web", "feat/x"),
        wants("docs", BranchMode::CheckedOut, ""),
    ];

    let first = split.check(&children);
    let again = split.check(&children);

    let loading: Vec<bool> = first.iter().map(|check| check.loading).collect();
    assert_eq!(loading, [true, true, false]);
    assert_eq!(first, again);
    assert_eq!(
        split.fixture.git.calls(),
        [
            GitCall::Inspect(split.path("api")),
            GitCall::Inspect(split.path("web"))
        ]
    );
    assert_eq!(split.fixture.git.waiting(), 1);
    assert!(split.settle().is_empty());
    let settled = split.check(&children);
    assert!(settled.iter().all(|check| !check.loading), "{settled:?}");
    assert_eq!(problems(&settled), [None, None, None]);
    assert_eq!(split.fixture.git.calls().len(), 2);
}

#[test]
fn a_repository_wanted_later_is_read_then() {
    let mut split = git_split(&["api", "web"]);
    split.checked(&[
        worktree_in("api", "feat/x"),
        wants("web", BranchMode::CheckedOut, ""),
    ]);

    let checks = split.check(&[worktree_in("api", "feat/x"), worktree_in("web", "feat/x")]);

    assert!(!checks[0].loading);
    assert!(checks[1].loading);
    assert_eq!(split.git_since(1), [GitCall::Inspect(split.path("web"))]);
}

#[test]
fn two_children_with_the_same_name_in_one_repository_are_refused() {
    let mut split = git_split(&["api", "web"]);

    let checks = split.checked(&[
        worktree_in("api", "feat/x"),
        wants("api", BranchMode::Worktree, " feat/x "),
        worktree_in("web", "feat/x"),
    ]);

    assert_eq!(
        problems(&checks),
        [
            None,
            Some("branch feat/x is also used by another child in api".into()),
            None
        ]
    );
}

#[test]
fn two_children_cannot_share_a_worktree_folder() {
    let mut split = git_split(&["api", "web"]);
    let settings = ProjectSettings {
        project: {
            let mut layer = SettingsLayer::default();
            layer
                .set(Level::Project, "worktree_path", "/wt/{branch}")
                .unwrap();
            layer
        },
        ..ProjectSettings::default()
    };
    split
        .fixture
        .store
        .set_settings(split.project.clone(), settings);

    let checks = split.checked(&[worktree_in("api", "feat/x"), worktree_in("web", "feat/x")]);

    assert_eq!(checks[1].worktree, Some(PathBuf::from("/wt/feat/x")));
    assert_eq!(
        problems(&checks),
        [
            None,
            Some("worktree /wt/feat/x is also used by another child".into())
        ]
    );
}

#[test]
fn a_new_branch_child_cannot_share_the_checkout() {
    let mut split = git_split(&["api", "web"]);
    let new_branch = |name: &str, branch: &str| wants(name, BranchMode::NewBranch, branch);
    let in_use = Some("api is in use by another child — use a worktree".to_owned());

    let beside_checked_out = split.checked(&[
        wants("api", BranchMode::CheckedOut, ""),
        new_branch("api", "feat/x"),
        new_branch("web", "feat/x"),
    ]);
    let two_new = split.check(&[new_branch("api", "feat/x"), new_branch("api", "feat/y")]);
    let beside_a_worktree =
        split.check(&[worktree_in("api", "feat/x"), new_branch("api", "feat/y")]);

    assert_eq!(problems(&beside_checked_out), [None, in_use.clone(), None]);
    assert_eq!(problems(&two_new), [in_use.clone(), in_use]);
    assert_eq!(problems(&beside_a_worktree), [None, None]);

    let live = attached_session(
        &mut split.fixture,
        &split.project,
        Scope::Repository(repository("web")),
    );
    let parent = split.parent.clone();
    split
        .fixture
        .app
        .change_session(live, |session| {
            session.title = "Web work".into();
            session.parent = Some(parent);
        })
        .unwrap();
    let beside_a_session = split.check(&[new_branch("web", "feat/x")]);
    assert_eq!(
        problems(&beside_a_session),
        [Some(
            "web is in use by \"Web work\" — use a worktree".into()
        )]
    );
}

#[test]
fn a_child_whose_repository_cannot_be_read_says_why() {
    let mut split = git_split(&["api", "web"]);
    split
        .fixture
        .git
        .fail_next(GitOp::Inspect, &split.path("web"));

    let checks = split.checked(&[worktree_in("api", "feat/x"), worktree_in("web", "feat/x")]);

    assert_eq!(
        problems(&checks),
        [
            None,
            Some(
                "cannot read the git state of web: git worktree list failed: simulated failure"
                    .into()
            )
        ]
    );
}

#[test]
fn a_reading_git_cannot_queue_fails_its_children() {
    let mut split = git_split(&["api", "web"]);
    split.fixture.git.refuse_next_request();

    let checks = split.check(&[worktree_in("api", "feat/x")]);

    assert_eq!(
        problems(&checks),
        [Some(
            "cannot read the git state of api: git is unavailable: memory git refused the request"
                .into()
        )]
    );
}

#[test]
fn a_child_in_an_unknown_repository_or_without_a_main_checkout_is_refused() {
    let mut split = git_split(&["api", "web"]);
    let mut headless = repo_info(&split.path("web"), "main", &["main"], &[]);
    headless.worktrees.clear();
    split
        .fixture
        .git
        .add_repository(split.path("web"), headless);
    let mut from_main = worktree_in("web", "feat/x");
    from_main.branch.base = Some("main".into());

    let checks = split.checked(&[worktree_in("apu", "feat/x"), from_main]);

    assert_eq!(
        problems(&checks),
        [
            Some("unknown repository \"apu\"".into()),
            Some("web has no main checkout".into())
        ]
    );
}

#[test]
fn a_check_needs_an_undecided_proposal_and_its_settings() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    let (parent, id) = (split.parent.clone(), split.id.clone());

    let stranger = split
        .fixture
        .app
        .check_split(&SessionId::new(), &id, &children);
    let unknown = split
        .fixture
        .app
        .check_split(&parent, &ProposalId::new(), &children);
    split.fixture.store.fail_next_settings_load();
    let unreadable = split.fixture.app.check_split(&parent, &id, &children);

    assert!(
        matches!(stranger, Err(AppError::UnknownSession(_))),
        "{stranger:?}"
    );
    assert!(
        matches!(unknown, Err(AppError::UnknownProposal(_))),
        "{unknown:?}"
    );
    assert!(
        matches!(unreadable, Err(AppError::Store(_))),
        "{unreadable:?}"
    );
    assert!(split.fixture.git.calls().is_empty());
}

#[test]
fn approval_waits_for_git_and_then_starts_the_children() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    let before = creates(&split.fixture.backend).len();

    let approval = split.approve(&children).unwrap();

    assert_eq!(approval, Approval::Pending);
    assert_eq!(creates(&split.fixture.backend).len(), before);
    assert_eq!(
        split.approval(),
        Some(ApprovalView {
            proposal: split.id.clone(),
            pending: true,
            error: None
        })
    );
    assert!(split.settle().is_empty());
    let (api_tree, web_tree) = (
        split.worktree("api", "feat/x"),
        split.worktree("web", "feat/x"),
    );
    for (name, tree) in [("api", &api_tree), ("web", &web_tree)] {
        let info = split.fixture.git.repository(&split.path(name)).unwrap();
        assert_eq!(info.checked_out_at("feat/x"), Some(tree.as_path()));
    }
    let created: Vec<PathBuf> = creates(&split.fixture.backend)
        .split_off(before)
        .into_iter()
        .map(|(_, _, cwd, _)| cwd)
        .collect();
    assert_eq!(created, [api_tree.clone(), web_tree.clone()]);
    let started = split.children();
    let session = split.fixture.app.session(&started[0]).unwrap();
    assert_eq!(session.cwd, api_tree);
    assert_eq!(
        session.workspace,
        Workspace::Worktree {
            branch: "feat/x".into(),
            base: "main".into(),
            fetched: false,
            path: api_tree.clone()
        }
    );
    let approved = approved(&split.fixture.app, &split.parent, &split.id);
    assert_eq!(approved[1].mode, BranchMode::Worktree);
    assert_eq!(approved[1].branch.as_deref(), Some("feat/x"));
    assert_eq!(approved[1].cwd, web_tree);
    let wake = last_prompt(&split.fixture.backend, &split.parent);
    assert!(
        wake.contains(&format!("· worktree feat/x · in {}", web_tree.display())),
        "{wake}"
    );
    assert_eq!(split.approval(), None);
    assert!(!split.undecided());
    assert!(split.fixture.app.splits.is_empty());
}

#[test]
fn checked_out_children_start_at_once() {
    let mut split = git_split(&["api", "web"]);
    let children = two_choices();

    let checks = split.check(&children);
    let approval = split.approve(&children).unwrap();

    assert_eq!(checks, [ChildCheck::default(), ChildCheck::default()]);
    assert_eq!(approval, Approval::Started(split.children()));
    assert_eq!(split.children().len(), 2);
    assert!(split.fixture.git.calls().is_empty());
}

#[test]
fn approve_or_reject_while_approving_is_refused() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    let waiting = split.fixture.git.waiting();

    let again = split.approve(&children);
    let rejected = split
        .fixture
        .app
        .reject_proposal(split.parent.clone(), split.id.clone(), None);

    for refused in [again.map(drop), rejected] {
        assert!(
            matches!(&refused, Err(AppError::Workspace(text)) if text == "the split is being approved"),
            "{refused:?}"
        );
    }
    assert_eq!(split.fixture.git.waiting(), waiting);
    assert!(split.settle().is_empty());
    let made = split
        .fixture
        .git
        .calls()
        .into_iter()
        .filter(|call| matches!(call, GitCall::AddWorktree { .. }))
        .count();
    assert_eq!(made, 2);
    assert_eq!(split.children().len(), 2);
}

#[test]
fn a_child_still_loading_refuses_the_approval() {
    let mut split = git_split(&["api", "web"]);

    let refused = split.approve(&[worktree_in("api", "feat/x")]);

    assert_eq!(refusal(refused), "the branches are still loading");
    assert_eq!(
        split.fixture.git.calls(),
        [GitCall::Inspect(split.path("api"))]
    );
    assert_eq!(split.approval(), None);
}

#[test]
fn a_child_with_a_problem_refuses_the_approval_before_any_git_step() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", ""), worktree_in("web", "feat/x")];
    split.checked(&children);

    let refused = split.approve(&children);

    assert_eq!(refusal(refused), "enter a branch name");
    assert_eq!(split.fixture.git.waiting(), 0);
    assert_eq!(split.fixture.git.calls().len(), 2);
    assert!(split.children().is_empty());
}

#[test]
fn one_failed_child_undoes_the_others_and_starts_none() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("web"));
    split.approve(&children).unwrap();
    let from = split.fixture.git.calls().len();

    let errors = split.settle();

    assert!(errors.is_empty(), "{errors:?}");
    let (api, web) = (split.path("api"), split.path("web"));
    let api_tree = split.worktree("api", "feat/x");
    assert_eq!(
        split.git_since(from),
        [
            GitCall::AddWorktree {
                repository: api.clone(),
                path: api_tree.clone(),
                branch: "feat/x".into(),
                start: "refs/heads/main".into()
            },
            GitCall::AddWorktree {
                repository: web,
                path: split.worktree("web", "feat/x"),
                branch: "feat/x".into(),
                start: "refs/heads/main".into()
            },
            GitCall::RemoveWorktree {
                repository: api.clone(),
                path: api_tree
            },
            GitCall::DeleteBranch {
                repository: api.clone(),
                branch: "feat/x".into()
            },
        ]
    );
    assert!(
        !split
            .fixture
            .git
            .repository(&api)
            .unwrap()
            .has_branch("feat/x")
    );
    assert!(split.children().is_empty());
    assert!(split.undecided());
    assert_eq!(
        split.approval(),
        Some(ApprovalView {
            proposal: split.id.clone(),
            pending: false,
            error: Some("git worktree add failed: simulated failure".into())
        })
    );
}

#[test]
fn a_failed_approval_reads_the_repositories_again() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("api"));
    split.approve(&children).unwrap();
    assert!(split.settle().is_empty());
    let from = split.fixture.git.calls().len();

    let checks = split.check(&children);

    assert!(checks.iter().all(|check| check.loading), "{checks:?}");
    assert_eq!(
        split.git_since(from),
        [
            GitCall::Inspect(split.path("api")),
            GitCall::Inspect(split.path("web"))
        ]
    );
    assert!(split.settle().is_empty());
    assert_eq!(problems(&split.check(&children)), [None, None]);
    assert_eq!(split.approve(&children).unwrap(), Approval::Pending);
    assert_eq!(
        split
            .approval()
            .map(|approval| (approval.pending, approval.error)),
        Some((true, None))
    );
}

#[test]
fn the_undo_switches_a_new_branch_checkout_back() {
    for head in [Head::Branch("main".into()), Head::Detached("abc123".into())] {
        let mut split = git_split(&["api", "web"]);
        let api = split.path("api");
        let mut info = repo_info(&api, "main", &["main"], &[]);
        info.worktrees[0].head = head.clone();
        split.fixture.git.add_repository(api.clone(), info);
        let mut new_branch = wants("api", BranchMode::NewBranch, "feat/x");
        new_branch.branch.base = Some("main".into());
        let children = [new_branch, worktree_in("web", "feat/x")];
        split.checked(&children);
        split
            .fixture
            .git
            .fail_next(GitOp::AddWorktree, &split.path("web"));
        split.approve(&children).unwrap();

        assert!(split.settle().is_empty());

        let calls = split.fixture.git.calls();
        assert_eq!(
            calls[calls.len() - 2..],
            [
                GitCall::RestoreHead {
                    repository: api.clone(),
                    head: head.clone()
                },
                GitCall::DeleteBranch {
                    repository: api.clone(),
                    branch: "feat/x".into()
                },
            ]
        );
        let restored = split.fixture.git.repository(&api).unwrap();
        assert_eq!(restored.head(), Some(&head));
        assert!(!restored.has_branch("feat/x"));
    }
}

#[test]
fn fetch_runs_once_per_repository_and_remote() {
    let mut split = git_split(&["api", "web"]);
    let fetching = |name: &str, branch: &str| {
        let mut child = worktree_in(name, branch);
        child.branch.fetch = true;
        child
    };
    let children = [
        fetching("api", "feat/x"),
        fetching("api", "feat/y"),
        fetching("web", "feat/x"),
        wants("web", BranchMode::Worktree, "feat/z"),
        wants("api", BranchMode::CheckedOut, ""),
    ];
    let checks = split.checked(&children);
    assert_eq!(
        checks.iter().map(|check| check.fetch).collect::<Vec<_>>(),
        [true, true, true, false, false]
    );
    split.approve(&children).unwrap();
    let from = split.fixture.git.calls().len();

    assert!(split.settle().is_empty());

    let (api, web) = (split.path("api"), split.path("web"));
    let add = |repository: &PathBuf, name: &str, branch: &str, start: &str| GitCall::AddWorktree {
        repository: repository.clone(),
        path: split.worktree(name, branch),
        branch: branch.into(),
        start: start.into(),
    };
    assert_eq!(
        split.git_since(from),
        [
            GitCall::Fetch {
                repository: api.clone(),
                remote: "origin".into()
            },
            GitCall::Fetch {
                repository: web.clone(),
                remote: "origin".into()
            },
            add(&api, "api", "feat/x", "refs/remotes/origin/main"),
            add(&api, "api", "feat/y", "refs/remotes/origin/main"),
            add(&web, "web", "feat/x", "refs/remotes/origin/main"),
            add(&web, "web", "feat/z", "refs/heads/main"),
        ]
    );
    let approved = approved(&split.fixture.app, &split.parent, &split.id);
    assert_eq!(approved.len(), 5);
    assert_eq!(
        (
            approved[4].mode,
            approved[4].branch.clone(),
            approved[4].cwd.clone()
        ),
        (BranchMode::CheckedOut, None, api)
    );
}

#[test]
fn a_reading_that_arrives_after_the_decision_is_ignored() {
    let mut split = git_split(&["api", "web"]);
    split.check(&[worktree_in("api", "feat/x")]);
    split
        .fixture
        .app
        .reject_proposal(split.parent.clone(), split.id.clone(), None)
        .unwrap();

    assert!(split.settle().is_empty());

    assert!(split.fixture.app.splits.is_empty());
}

#[test]
fn a_failed_fetch_makes_nothing_to_undo() {
    let mut split = git_split(&["api", "web"]);
    let mut child = worktree_in("api", "feat/x");
    child.branch.fetch = true;
    let children = [child];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::Fetch, &split.path("api"));
    split.approve(&children).unwrap();

    assert!(split.settle().is_empty());

    assert_eq!(split.fixture.git.waiting(), 0);
    assert!(
        !split
            .fixture
            .git
            .calls()
            .iter()
            .any(|call| matches!(call, GitCall::RemoveWorktree { .. }))
    );
    assert_eq!(
        split.approval().and_then(|approval| approval.error),
        Some("git fetch failed: simulated failure".into())
    );
}

#[test]
fn a_branch_made_outside_meanwhile_stops_the_approval() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    let api = split.path("api");
    let made = repo_info(
        &api,
        "main",
        &["main", "develop", "feat/x"],
        &["origin/main"],
    );
    split.fixture.git.add_repository(api, made);

    assert!(split.settle().is_empty());

    assert!(
        !split
            .fixture
            .git
            .calls()
            .iter()
            .any(|call| matches!(call, GitCall::AddWorktree { .. }))
    );
    assert_eq!(
        split.approval().and_then(|approval| approval.error),
        Some("branch feat/x already exists in api".into())
    );
    assert!(split.undecided());
}

#[test]
fn a_base_gone_meanwhile_stops_the_approval() {
    let mut split = git_split(&["api", "web"]);
    let mut from_develop = worktree_in("api", "feat/x");
    from_develop.branch.base = Some("develop".into());
    let children = [worktree_in("web", "feat/x"), from_develop];
    split.checked(&children);
    split.approve(&children).unwrap();
    let api = split.path("api");
    split
        .fixture
        .git
        .add_repository(api.clone(), repo_info(&api, "main", &["main"], &[]));

    assert!(split.settle().is_empty());

    assert_eq!(
        split.approval().and_then(|approval| approval.error),
        Some("api changed meanwhile; check the branch settings and approve again".into())
    );
    assert!(split.children().is_empty());
}

#[test]
fn settings_or_a_run_that_fail_after_the_reading_stop_the_approval() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    split.fixture.store.fail_next_settings_load();

    assert!(split.settle().is_empty());

    let unreadable = split
        .approval()
        .and_then(|approval| approval.error)
        .unwrap();
    assert!(unreadable.contains("settings.toml"), "{unreadable}");

    split.checked(&children);
    split.approve(&children).unwrap();
    let events = split.fixture.git.answer();
    split.fixture.git.refuse_next_request();
    for event in events {
        split.fixture.app.apply_git_event(event).unwrap();
    }

    assert_eq!(
        split.approval().and_then(|approval| approval.error),
        Some("git is unavailable: memory git refused the request".into())
    );
    assert_eq!(split.fixture.git.waiting(), 0);
}

#[test]
fn a_reading_git_cannot_queue_refuses_the_approval() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split.fixture.git.refuse_next_request();

    let refused = split.approve(&children);

    assert!(matches!(refused, Err(AppError::Git(_))), "{refused:?}");
    assert_eq!(split.approval(), None);
    assert_eq!(split.approve(&children).unwrap(), Approval::Pending);
}

#[test]
fn an_orchestrator_deleted_while_approving_gets_its_childrens_work_undone() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    for event in split.fixture.git.answer() {
        split.fixture.app.apply_git_event(event).unwrap();
    }
    let before = creates(&split.fixture.backend).len();
    split.fixture.app.delete(split.parent.clone(), &[]).unwrap();

    assert!(split.settle().is_empty());

    for name in ["api", "web"] {
        let info = split.fixture.git.repository(&split.path(name)).unwrap();
        assert!(!info.has_branch("feat/x"), "{name}");
        assert_eq!(info.worktrees.len(), 1, "{name}");
    }
    assert_eq!(creates(&split.fixture.backend).len(), before);
    assert!(split.fixture.app.splits.is_empty());
}

#[test]
fn an_orchestrator_deleted_while_its_repositories_are_read_again_starts_nothing() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    split.fixture.app.delete(split.parent.clone(), &[]).unwrap();
    let from = split.fixture.git.calls().len();

    assert!(split.settle().is_empty());

    assert!(split.git_since(from).is_empty());
}

#[test]
fn an_orchestrator_archived_while_approving_starts_nothing() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();
    split.fixture.app.archive(split.parent.clone()).unwrap();
    assert!(split.settle().is_empty());
    assert!(split.approval().is_none());
    assert!(
        !split
            .fixture
            .git
            .calls()
            .iter()
            .any(|call| matches!(call, GitCall::AddWorktree { .. }))
    );

    split.fixture.app.restore(split.parent.clone()).unwrap();
    split.checked(&children);
    split.approve(&children).unwrap();
    for event in split.fixture.git.answer() {
        split.fixture.app.apply_git_event(event).unwrap();
    }
    split.fixture.app.archive(split.parent.clone()).unwrap();
    assert!(split.settle().is_empty());

    let info = split.fixture.git.repository(&split.path("api")).unwrap();
    assert!(!info.has_branch("feat/x"));
    assert!(split.children().is_empty());
    assert!(split.undecided());
}

#[test]
fn a_child_that_cannot_start_leaves_the_started_ones_and_undoes_the_rest() {
    let mut split = git_split(&["api", "web", "docs"]);
    let children = [
        worktree_in("api", "feat/x"),
        worktree_in("web", "feat/x"),
        worktree_in("docs", "feat/x"),
    ];
    split.checked(&children);
    split.approve(&children).unwrap();
    for event in split.fixture.git.answer() {
        split.fixture.app.apply_git_event(event).unwrap();
    }
    let events = split.fixture.git.answer();
    split.fixture.backend.fail_call_after(1);

    let failed: Vec<AppError> = events
        .into_iter()
        .filter_map(|event| split.fixture.app.apply_git_event(event).err())
        .collect();

    assert!(
        matches!(failed.as_slice(), [AppError::Backend(_)]),
        "{failed:?}"
    );
    assert!(split.settle().is_empty());
    let kept = |name: &str| {
        let info = split.fixture.git.repository(&split.path(name)).unwrap();
        info.has_branch("feat/x")
    };
    assert_eq!(
        [kept("api"), kept("web"), kept("docs")],
        [true, false, false]
    );
    assert!(split.undecided());
    assert_eq!(split.approval(), None);
}

#[test]
fn an_undo_that_fails_names_the_repository() {
    let mut split = git_split(&["api", "web", "docs"]);
    let children = [
        worktree_in("api", "feat/x"),
        worktree_in("web", "feat/x"),
        worktree_in("docs", "feat/x"),
    ];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("docs"));
    split
        .fixture
        .git
        .fail_next(GitOp::RemoveWorktree, &split.path("api"));
    split.approve(&children).unwrap();

    let errors = split.settle();

    let [error @ AppError::Undo { repository, .. }] = errors.as_slice() else {
        panic!("{errors:?}");
    };
    assert_eq!(repository, "api");
    assert_eq!(
        error.to_string(),
        "could not undo the branch work in api: git worktree remove failed: simulated failure"
    );
    let web = split.fixture.git.repository(&split.path("web")).unwrap();
    assert!(!web.has_branch("feat/x"));
}

#[test]
fn an_undo_git_cannot_queue_names_every_repository() {
    let mut split = git_split(&["api", "web", "docs"]);
    let children = [
        worktree_in("api", "feat/x"),
        worktree_in("web", "feat/x"),
        worktree_in("api", "feat/y"),
        worktree_in("docs", "feat/x"),
    ];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("docs"));
    split.approve(&children).unwrap();
    for event in split.fixture.git.answer() {
        split.fixture.app.apply_git_event(event).unwrap();
    }
    let events = split.fixture.git.answer();
    split.fixture.git.refuse_next_request();

    let failed: Vec<AppError> = events
        .into_iter()
        .filter_map(|event| split.fixture.app.apply_git_event(event).err())
        .collect();

    let [AppError::Undo { repository, .. }] = failed.as_slice() else {
        panic!("{failed:?}");
    };
    assert_eq!(repository, "api, web");
}

#[test]
fn an_answer_the_proposal_no_longer_waits_for_is_ignored() {
    let mut split = git_split(&["api", "web"]);
    let api_only = [worktree_in("api", "feat/x")];
    let both = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&api_only);
    split.approve(&api_only).unwrap();
    let api = split.path("api");
    let made = repo_info(&api, "main", &["main", "feat/x"], &[]);
    split.fixture.git.add_repository(api, made);
    split.check(&both);
    let mut events = split.fixture.git.answer().into_iter();
    let (approval_check, stale) = (events.next().unwrap(), events.next().unwrap());
    split.fixture.app.apply_git_event(approval_check).unwrap();
    assert!(split.check(&both).iter().all(|check| check.loading));

    split.fixture.app.apply_git_event(stale).unwrap();

    assert!(split.check(&both).iter().all(|check| check.loading));
    assert!(split.settle().is_empty());
    let checks = split.check(&both);
    assert!(checks.iter().all(|check| !check.loading));
}

#[test]
fn the_approval_in_flight_shows_before_a_failed_one() {
    let mut split = git_split(&["api", "web"]);
    let failing = split.id.clone();
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("api"));
    split.approve(&children).unwrap();
    assert!(split.settle().is_empty());
    let parent = split.parent.clone();
    call_propose(&mut split.fixture, &parent, two_children()).unwrap();
    split.id = split
        .fixture
        .app
        .proposals
        .keys()
        .map(|(_, id)| id.clone())
        .find(|id| *id != failing)
        .unwrap();
    let web = [worktree_in("web", "feat/y")];
    split.checked(&web);

    split.approve(&web).unwrap();

    let shown = split.approval().unwrap();
    assert_eq!((shown.proposal, shown.pending), (split.id.clone(), true));
    assert!(split.settle().is_empty());
    let left = split.approval().unwrap();
    assert_eq!(left.proposal, failing);
    assert!(left.error.is_some());

    split
        .fixture
        .app
        .reject_proposal(parent, failing, None)
        .unwrap();
    assert_eq!(split.approval(), None);
    assert!(split.fixture.app.splits.is_empty());
}

#[test]
fn the_undo_returns_to_the_head_the_fresh_reading_saw() {
    let mut split = git_split(&["api", "web"]);
    let api = split.path("api");
    let mut new_branch = wants("api", BranchMode::NewBranch, "feat/x");
    new_branch.branch.base = Some("main".into());
    let children = [new_branch, worktree_in("web", "feat/x")];
    split.checked(&children);
    let mut switched = repo_info(&api, "develop", &["main", "develop"], &["origin/main"]);
    switched.worktrees[0].head = Head::Branch("develop".into());
    split.fixture.git.add_repository(api.clone(), switched);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("web"));
    split.approve(&children).unwrap();

    assert!(split.settle().is_empty());

    let restored = split.fixture.git.repository(&api).unwrap();
    assert_eq!(restored.head(), Some(&Head::Branch("develop".into())));
}

#[test]
fn a_reading_waits_while_the_repository_is_busy_and_shows_what_the_undo_left() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    split
        .fixture
        .git
        .fail_next(GitOp::AddWorktree, &split.path("web"));
    split.approve(&children).unwrap();
    for _ in 0..2 {
        for event in split.fixture.git.answer() {
            split.fixture.app.apply_git_event(event).unwrap();
        }
    }
    let from = split.fixture.git.calls().len();

    let while_undoing = split.check(&children);

    assert!(while_undoing[0].loading);
    assert_eq!(split.git_since(from), [GitCall::Inspect(split.path("web"))]);
    assert!(split.settle().is_empty());
    let after_undo = split.check(&children);
    assert!(after_undo[0].loading);
    assert_eq!(
        split.git_since(from + 1).last(),
        Some(&GitCall::Inspect(split.path("api")))
    );
    assert!(split.settle().is_empty());
    assert_eq!(problems(&split.check(&children)), [None, None]);
}

#[test]
fn a_failed_reading_is_read_again_by_the_next_check() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("web", "feat/x")];
    split
        .fixture
        .git
        .fail_next(GitOp::Inspect, &split.path("web"));
    let failed = split.checked(&children);
    assert!(failed[0].problem.is_some());
    let from = split.fixture.git.calls().len();

    let retried = split.check(&children);

    assert!(retried[0].loading);
    assert_eq!(split.git_since(from), [GitCall::Inspect(split.path("web"))]);
    assert!(split.settle().is_empty());
    assert_eq!(problems(&split.check(&children)), [None]);
}

#[test]
fn failed_readings_of_two_proposals_stay_until_the_user_checks_again() {
    let mut split = git_split(&["api", "web"]);
    let parent = split.parent.clone();
    call_propose(&mut split.fixture, &parent, two_children()).unwrap();
    let second = split
        .fixture
        .app
        .proposals
        .keys()
        .map(|(_, id)| id.clone())
        .find(|id| *id != split.id)
        .unwrap();
    let (api, web) = (split.path("api"), split.path("web"));
    for _ in 0..10 {
        split.fixture.git.fail_next(GitOp::Inspect, &api);
        split.fixture.git.fail_next(GitOp::Inspect, &web);
    }
    let first_children = [worktree_in("api", "feat/x")];
    let second_children = [worktree_in("web", "feat/y")];
    split.check(&first_children);
    split
        .fixture
        .app
        .check_split(&parent, &second, &second_children)
        .unwrap();

    for _ in 0..10 {
        for event in split.fixture.git.answer() {
            split.fixture.app.apply_git_event(event).unwrap();
            split.recheck(&first_children);
            split
                .fixture
                .app
                .recheck_split(&parent, &second, &second_children)
                .unwrap();
        }
    }

    assert_eq!(split.fixture.git.calls().len(), 2);
    assert!(split.recheck(&first_children)[0].problem.is_some());
    let from = split.fixture.git.calls().len();
    let retried = split.check(&first_children);
    assert!(retried[0].loading);
    assert_eq!(split.git_since(from), [GitCall::Inspect(api)]);
}

#[test]
fn a_failed_reading_is_read_again_once_its_repository_is_free() {
    let mut split = git_split(&["api", "web"]);
    let web = split.path("web");
    let children = [worktree_in("web", "feat/x")];
    split.fixture.git.fail_next(GitOp::Inspect, &web);
    assert!(split.checked(&children)[0].problem.is_some());
    split
        .fixture
        .app
        .prepare_session(split.project.clone(), Scope::Repository(repository("web")))
        .unwrap();
    assert!(split.settle().is_empty());
    split.fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    split.fixture.app.draft_branch("feat/y".into()).unwrap();
    split.fixture.app.create_from_draft().unwrap();
    assert!(split.settle().is_empty());
    let from = split.fixture.git.calls().len();

    let after_create = split.recheck(&children);

    assert!(after_create[0].loading);
    assert_eq!(split.git_since(from), [GitCall::Inspect(web)]);
    assert!(split.settle().is_empty());
    assert_eq!(problems(&split.recheck(&children)), [None]);
}

#[test]
fn a_split_in_a_repository_a_create_is_reading_again_is_busy() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split
        .fixture
        .app
        .prepare_session(split.project.clone(), Scope::Repository(repository("api")))
        .unwrap();
    assert!(split.settle().is_empty());
    split.fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    split.fixture.app.draft_branch("feat/y".into()).unwrap();
    split.fixture.app.create_from_draft().unwrap();

    let while_reading = split.recheck(&children);

    assert_eq!(
        problems(&while_reading),
        [Some("api is busy with another branch change".into())]
    );
    assert_eq!(
        refusal(split.approve(&children)),
        "api is busy with another branch change"
    );
}

#[test]
fn a_create_in_a_repository_of_a_pending_approval_is_busy() {
    let mut split = git_split(&["api", "web"]);
    split
        .fixture
        .app
        .prepare_session(split.project.clone(), Scope::Repository(repository("api")))
        .unwrap();
    split.fixture.app.draft_mode(BranchMode::Worktree).unwrap();
    split.fixture.app.draft_branch("feat/y".into()).unwrap();
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    split.approve(&children).unwrap();

    let refused = split.fixture.app.create_from_draft();

    assert!(
        matches!(&refused, Err(AppError::Workspace(text)) if text == "api is busy with another branch change"),
        "{refused:?}"
    );
    assert!(split.settle().is_empty());
    assert_eq!(split.approval(), None);
    assert_eq!(split.children().len(), 1);
}
