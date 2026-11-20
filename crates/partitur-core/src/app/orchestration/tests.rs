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
    ApprovedChild, BackendEvent, ChildOutcome, Decision, OrchestratorTool, PermissionKind,
    PermissionOption, PromptContent, ProposedChild, StopReason, TitleProposal, ToolKind,
    ToolStatus, TurnOrigin, Update,
};
use crate::git::Head;
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, PermissionId,
    PermissionOptionId, ProjectId, RepositoryId, RequestId, SessionId, ToolCallId,
};
use crate::instructions::{inject, instruction_chain};
use crate::orchestrator::{
    PROPOSAL_RECORDED, child_result_prompt, children_started_prompt, title_proposal_prompt,
};
use crate::session::{
    BranchChoice, BranchMode, Scope, Status, TitleSource, Workspace, title_from_prompt,
};
use crate::settings::{GlobalSettings, Level, ProjectSettings, SettingsLayer};
use crate::store::Store as _;
use crate::testing::{
    Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, RecordingReply, repo_info,
    settle, single_claude,
};
use crate::tool_call::{SessionTool, ToolCall, ToolRole};
use crate::transcript::Entry;
use crate::view_model::{ApprovalView, ChildCheck};
use crate::workflow::{RECORDED, bmad_v6_12};

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
    // Canonical, because the app stores canonical paths and macOS resolves the temp dir through /private.
    let dir = TempDir::new_in(crate::canonical(std::env::temp_dir()).unwrap()).unwrap();
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
        &ChildState::default(),
        text,
    )
}

fn proposed(name: &str, prompt: &str, title: &str) -> ProposedChild {
    ProposedChild {
        repository: repository(name),
        prompt: prompt.into(),
        title: title.into(),
        title_source: TitleSource::Given,
        base: None,
    }
}

/// Lets `session` call `tool` and returns the single answer its reply was given.
fn call_tool(
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
            decision: None,
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
            state: ChildState::default(),
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
            partitur_tool: false,
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
        &ChildState::default(),
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
            role: ToolRole::Orchestrator,
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
            raw_input: None,
            call_result: None,
            widget: None,
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
fn a_first_turn_read_from_the_store_records_its_prompt() {
    let Family {
        mut fixture, child, ..
    } = family();
    send(&mut fixture, &child, Update::Notice("heads up".into()));
    fixture.app.transcripts.remove(&child);

    orchestrate(&mut fixture, &child);

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.first_prompt, "Add the endpoint");
}

#[test]
fn a_conversation_read_from_the_store_keeps_its_first_prompt_empty() {
    let Family {
        mut fixture, child, ..
    } = family();
    send(&mut fixture, &child, Update::AgentText("earlier".into()));
    fixture.app.transcripts.remove(&child);

    orchestrate(&mut fixture, &child);

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.first_prompt, "");
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
            role: ToolRole::Orchestrator,
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
}

#[test]
fn a_root_only_orchestrator_proposes_children_in_the_root() {
    let dir = project_dir(&["."]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.app.open_project(dir.path()).unwrap();
    let root = fixture.app.projects[0].repositories[0].clone();
    assert!(root.is_root);
    let parent = attached_session(&mut fixture, &project, Scope::Project);
    fixture
        .app
        .change_session(parent.clone(), |session| session.orchestrator = true)
        .unwrap();

    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![
            proposed(&root.name, "Plan the PRD", "PRD"),
            proposed(&root.name, "Draft story 1", "Story 1"),
        ],
    );

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    fixture.app.select_session(parent).unwrap();
    let view = fixture.app.session_view().unwrap();
    let names: Vec<&str> = view
        .repositories
        .iter()
        .map(|repository| repository.name.as_str())
        .collect();
    assert_eq!(names, [root.name.as_str()]);
}

#[test]
fn a_proposal_for_the_root_beside_nested_repositories_is_refused() {
    let Family {
        dir,
        mut fixture,
        parent,
        ..
    } = family();
    // Discovery stops at a root repository, so the root is added by hand, as a project
    // whose root and children are all repositories.
    let path = crate::canonical(dir.path()).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    fixture.app.projects[0].repositories.insert(
        0,
        Repository {
            id: RepositoryId::try_from(name.as_str()).unwrap(),
            path,
            name: name.clone(),
            has_instructions: false,
            has_claude_md: false,
            is_root: true,
        },
    );

    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![proposed(&name, "Fix it", "Root")],
    );

    assert_eq!(
        answer,
        Err(format!(
            "unknown repository \"{name}\"; the repositories are: api, web"
        ))
    );
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
            children: vec![ProposedChild {
                title_source: TitleSource::Derived,
                ..proposed("api", "\nAdd the endpoint\nwith tests", "Add the endpoint")
            }],
            decision: None,
        }]
    );
}

#[test]
fn a_proposed_child_with_a_title_keeps_it_fixed() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![
            proposed("api", "Add the endpoint", "Endpoint"),
            proposed("web", "Add the page\nwith tests", " "),
        ],
    );
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let proposal = proposal_id(&fixture.app, &parent);

    let children = started_ids(
        fixture
            .app
            .approve_proposal(
                parent.clone(),
                proposal,
                vec![
                    choice("api", "Add the endpoint", "Endpoint", false),
                    choice("web", "Add the page\nwith tests", " ", false),
                ],
            )
            .unwrap(),
    );

    let titled = fixture.app.session(&children[0]).unwrap();
    assert_eq!(titled.title, "Endpoint");
    assert_eq!(titled.title_source, TitleSource::Chosen);
    let derived = fixture.app.session(&children[1]).unwrap();
    assert_eq!(derived.title, "Add the page");
    assert_eq!(derived.title_source, TitleSource::Derived);
}

/// Proposes one child with `title`, approves it with the title `approved` the card left, and
/// returns the session started.
fn approved_child(title: &str, approved: &str) -> (Fixture, SessionId) {
    approved_child_with("Add the endpoint", title, approved)
}

/// As [`approved_child`], with the child proposed and approved with `prompt`.
fn approved_child_with(prompt: &str, title: &str, approved: &str) -> (Fixture, SessionId) {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let answer = call_propose(&mut fixture, &parent, vec![proposed("api", prompt, title)]);
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let proposal = proposal_id(&fixture.app, &parent);
    let children = started_ids(
        fixture
            .app
            .approve_proposal(
                parent,
                proposal,
                vec![choice("api", prompt, approved, false)],
            )
            .unwrap(),
    );
    // The started child's connection is open.
    fixture.app.attached.insert(children[0].clone());
    (fixture, children[0].clone())
}

#[test]
fn a_spawned_child_keeps_its_proposal_prompt_as_its_first_prompt() {
    let (mut fixture, child) = approved_child("Endpoint", "Endpoint");

    let spawned = fixture.app.session(&child).unwrap().clone();
    assert_eq!(spawned.first_prompt, "Add the endpoint");
    assert_eq!(spawned.skill, None);
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));
    fixture.app.select_session(child.clone()).unwrap();
    fixture
        .app
        .submit_prompt("/brainstorming more", Vec::new())
        .unwrap();

    let after = fixture.app.session(&child).unwrap();
    assert_eq!(after.first_prompt, "Add the endpoint");
    assert_eq!(after.skill, None);
}

#[test]
fn a_spawned_child_takes_the_skill_its_proposal_prompt_starts_with() {
    let (fixture, child) = approved_child_with("/tdd Add the endpoint", "Endpoint", "Endpoint");

    let spawned = fixture.app.session(&child).unwrap();
    assert_eq!(spawned.first_prompt, "/tdd Add the endpoint");
    assert_eq!(spawned.skill.as_deref(), Some("/tdd"));
}

#[test]
fn a_derived_proposed_title_the_card_leaves_stays_derived_and_takes_the_agent_title() {
    let (mut fixture, child) = approved_child("", &title_from_prompt("Add the endpoint"));

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title, "Add the endpoint");
    assert_eq!(session.title_source, TitleSource::Derived);
    send(&mut fixture, &child, Update::Title("API endpoint".into()));
    assert_eq!(fixture.app.session(&child).unwrap().title, "API endpoint");
}

#[test]
fn a_derived_proposed_title_the_user_edited_is_given() {
    let (mut fixture, child) = approved_child(" ", "My endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title, "My endpoint");
    assert_eq!(session.title_source, TitleSource::Given);
    send(&mut fixture, &child, Update::Title("API endpoint".into()));
    assert_eq!(fixture.app.session(&child).unwrap().title, "My endpoint");
}

#[test]
fn an_approved_child_left_as_proposed_has_a_chosen_title() {
    let (mut fixture, child) = approved_child("Endpoint", "Endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title_source, TitleSource::Chosen);
    send(&mut fixture, &child, Update::Title("API endpoint".into()));
    assert_eq!(fixture.app.session(&child).unwrap().title, "Endpoint");
}

#[test]
fn an_approved_child_the_user_retitled_has_a_given_title() {
    let (mut fixture, child) = approved_child("Endpoint", "My endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title, "My endpoint");
    assert_eq!(session.title_source, TitleSource::Given);
    send(&mut fixture, &child, Update::Title("API endpoint".into()));
    assert_eq!(fixture.app.session(&child).unwrap().title, "My endpoint");
}

#[test]
fn a_proposal_records_where_each_title_comes_from() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![
            proposed("api", "Add the endpoint", "Endpoint"),
            proposed("web", "Add the page", " "),
        ],
    );
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let proposal = proposal_id(&fixture.app, &parent);

    let recorded = fixture.app.transcripts[&parent]
        .pending_proposal(&proposal)
        .unwrap()
        .to_vec();

    assert_eq!(recorded[0].title_source, TitleSource::Given);
    assert_eq!(recorded[1].title_source, TitleSource::Derived);
    assert_eq!(recorded[1].title, "Add the page");
}

#[test]
fn a_proposed_title_equal_to_the_derived_one_is_still_chosen() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let prompt = "Add the endpoint";
    let answer = call_propose(
        &mut fixture,
        &parent,
        vec![proposed("api", prompt, &title_from_prompt(prompt))],
    );
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let proposal = proposal_id(&fixture.app, &parent);

    let children = started_ids(
        fixture
            .app
            .approve_proposal(
                parent.clone(),
                proposal,
                vec![choice("api", prompt, &title_from_prompt(prompt), false)],
            )
            .unwrap(),
    );

    let child = fixture.app.session(&children[0]).unwrap();
    assert_eq!(child.title, "Add the endpoint");
    assert_eq!(child.title_source, TitleSource::Chosen);
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

/// Lets `session` report `event` about its workflow and returns the answer.
fn call_report(
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

#[test]
fn a_plain_session_cannot_report_a_run() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let start_run = || bmad_v6_12::Event::StartRun {
        topic: "Invoices".into(),
        path: bmad_v6_12::RunPath::Epic,
    };

    assert_eq!(
        call_report(&mut fixture, &child, start_run()),
        Err("this session is not an orchestrator".to_owned())
    );
    assert!(fixture.app.transcripts[&child].entries().is_empty());
    // The workflow tools stay open to it.
    assert_eq!(
        call_report(
            &mut fixture,
            &child,
            bmad_v6_12::Event::FinishWorkflow { outcome: None }
        ),
        Ok(RECORDED.to_owned())
    );
    assert_eq!(
        call_report(&mut fixture, &parent, start_run()),
        Ok(RECORDED.to_owned())
    );
}

/// The BMAD build plan of story `1.2` at `plans/1.2.md`, waiting for the user's approval.
fn build_plan_gate() -> bmad_v6_12::Event {
    bmad_v6_12::Event::RequestBuildPlanApproval {
        story: "1.2".into(),
        path: "plans/1.2.md".into(),
    }
}

/// Lets `child` run a turn its orchestrator started in which it asks for approval of its
/// build plan and answers `answer`.
fn turn_at_build_plan(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    orchestrate(fixture, child);
    assert_eq!(
        call_report(fixture, child, build_plan_gate()).map(|_| ()),
        Ok(())
    );
    send(fixture, child, Update::AgentText(answer.into()));
    send(fixture, child, Update::TurnEnded(StopReason::EndTurn));
}

#[test]
fn a_bmad_child_at_its_plan_gate_reports_that_it_waits() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    turn_at_build_plan(&mut fixture, &child, "The plan is written");

    let reported = prompts(&fixture.backend, &parent);
    assert_eq!(reported.len(), 1);
    assert!(
        reported[0].starts_with(&format!(
            "child waits · api · API endpoint\nsession {child}\n"
        )),
        "{}",
        reported[0]
    );
    assert!(reported[0].contains(" · waits at its build plan\n"));
    assert!(reported[0].contains("The plan is written"));
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    fixture.app.approve_gate(child.clone()).unwrap();
    assert!(last_prompt(&fixture.backend, &child).ends_with("Plan approved. Build it."));
    send(&mut fixture, &child, Update::AgentText("Built".into()));
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    let reported = prompts(&fixture.backend, &parent);
    assert_eq!(reported.len(), 2);
    assert!(
        reported[1].starts_with("child result · "),
        "{}",
        reported[1]
    );
    assert!(reported[1].contains("Built"));
    assert!(!reported[1].contains("The plan is written"));
}

#[test]
fn send_to_a_bmad_child_at_a_gate_names_the_gate_word() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    turn_at_build_plan(&mut fixture, &child, "The plan is written");
    let sent = prompts(&fixture.backend, &child).len();

    let refusal = call_send_to_child(&mut fixture, &parent, &child, "Go on");

    assert_eq!(
        refusal,
        Err(format!(
            r#"child {child} waits at its build plan; send decision "approve" or "changes""#
        ))
    );
    assert_eq!(prompts(&fixture.backend, &child).len(), sent);
    assert_eq!(deferred(&fixture.app, &child), 0);
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
}

#[test]
fn next_gate_finds_the_oldest_bmad_gate_across_sessions() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let other = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    let gate_at = |fixture: &mut Fixture, session: &SessionId, event, since| {
        fixture
            .app
            .change_session(session.clone(), |gated| {
                gated.workflow = Some(crate::workflow::apply(None, &event, since));
            })
            .unwrap();
    };
    assert_eq!(fixture.app.next_gate(), None);

    gate_at(&mut fixture, &child, build_plan_gate().into(), 30);
    assert_eq!(fixture.app.next_gate(), Some(child.clone()));
    let review = bmad_v6_12::Event::RequestReviewVerdict {
        story: "1.2".into(),
        verdict: bmad_v6_12::ReviewVerdict::Approved,
        path: "reviews/1.2.md".into(),
    };
    gate_at(&mut fixture, &other, review.into(), 10);
    let design = crate::SuperpowersV7Event::RequestDesignApproval {
        path: "design.md".into(),
    };
    gate_at(&mut fixture, &parent, design.into(), 20);

    assert_eq!(fixture.app.next_gate(), Some(other));
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
        partitur_tool: false,
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
        partitur_tool: false,
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
    // Windows symlinks need developer mode: only the Unix-only link test reads `api/link`.
    #[cfg(unix)]
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

// Windows symlinks need developer mode, which a runner or user may not have.
#[cfg(unix)]
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

/// An orchestrator mid-turn, with auto-allow set to `auto_allow`, after it asked to edit the
/// file `relative` of its working folder: its status and the last call to the backend.
fn ask_to_edit(auto_allow: bool, relative: &str) -> (Status, Call) {
    let Family {
        mut fixture,
        parent,
        dir: _dir,
        ..
    } = family();
    let cwd = fixture.app.session(&parent).unwrap().cwd.clone();
    fixture
        .app
        .set_auto_allow(parent.clone(), auto_allow)
        .unwrap();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    send(
        &mut fixture,
        &parent,
        request_at(ToolKind::Edit, &[cwd.join(relative)]),
    );
    (
        status(&fixture.app, &parent),
        fixture.backend.calls().last().cloned().unwrap(),
    )
}

#[test]
fn an_orchestrators_edit_in_a_workflow_folder_is_asked() {
    for relative in [
        "docs/superpowers/plans/2026-11-15-x.md",
        ".superpowers/notes.md",
        "_bmad-output/planning-artifacts/prd.md",
    ] {
        let outcome = ask_to_edit(false, relative);

        assert!(asks(&outcome), "{relative}: {outcome:?}");
    }
}

#[test]
fn an_orchestrators_edit_in_a_workflow_folder_is_granted_with_auto_allow() {
    let (status, call) = ask_to_edit(true, "docs/superpowers/specs/x-design.md");

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
fn an_orchestrators_edit_outside_the_workflow_folders_is_still_denied() {
    for auto_allow in [false, true] {
        let (_, call) = ask_to_edit(auto_allow, "api/src/main.rs");

        // The request offers nothing to reject with, so the denial cancels it.
        assert!(
            matches!(call, Call::RespondPermission { option: None, .. }),
            "{call:?}"
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
            decision: None,
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
            decision: None,
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
            decision: None,
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

fn call_list_children(fixture: &mut Fixture, session: &SessionId) -> Result<String, String> {
    call_tool(fixture, session, OrchestratorTool::ListChildren)
}

#[test]
fn list_children_lists_direct_children_with_status() {
    let Family {
        mut fixture,
        project,
        parent,
        child: running,
        ..
    } = family();
    let finished = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    let grandchild = attached_child(&mut fixture, &project, &running, "web", "Nested");
    fixture
        .app
        .change_session(running.clone(), |child| child.status = Status::Running)
        .unwrap();
    fixture
        .app
        .change_session(finished.clone(), |child| child.status = Status::Done)
        .unwrap();

    let answer = call_list_children(&mut fixture, &parent);

    assert_eq!(
        answer,
        Ok(format!(
            "children:\n- API endpoint · api · session {running} · running\n- Web page · web · session {finished} · done"
        ))
    );
    assert!(!answer.as_ref().unwrap().contains("set by you"));
    assert!(!answer.unwrap().contains(&grandchild.to_string()));
}

#[test]
fn list_children_names_every_status() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let mut expected = vec!["children:".to_owned()];
    for (status, word) in [
        (Status::Idle, "idle"),
        (Status::Running, "running"),
        (Status::WaitingForYou, "waiting for you"),
        (Status::Interrupted, "interrupted"),
        (Status::Done, "done"),
        (Status::Failed, "failed"),
    ] {
        let listed = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        fixture
            .app
            .change_session(listed.clone(), |session| session.status = status)
            .unwrap();
        expected.push(format!("- Web page · web · session {listed} · {word}"));
    }
    expected.insert(1, format!("- API endpoint · api · session {child} · idle"));

    assert_eq!(
        call_list_children(&mut fixture, &parent),
        Ok(expected.join("\n"))
    );
}

#[test]
fn list_children_without_children_says_so() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture
        .app
        .change_session(child, |orphan| orphan.parent = None)
        .unwrap();

    assert_eq!(
        call_list_children(&mut fixture, &parent),
        Ok("no children yet".to_owned())
    );
}

#[test]
fn list_children_is_refused_for_non_orchestrators() {
    let Family {
        mut fixture, child, ..
    } = family();

    assert_eq!(
        call_list_children(&mut fixture, &child),
        Err("this session is not an orchestrator".to_owned())
    );
}

fn call_cancel_child(
    fixture: &mut Fixture,
    session: &SessionId,
    child: &SessionId,
) -> Result<String, String> {
    call_tool(
        fixture,
        session,
        OrchestratorTool::CancelChild {
            child: child.clone(),
        },
    )
}

#[test]
fn cancel_child_stops_a_running_child() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    let before = fixture.backend.calls().len();

    let answer = call_cancel_child(&mut fixture, &parent, &child);

    assert_eq!(
        answer,
        Ok(format!(
            "cancel requested for child {child}; its result follows as a child result"
        ))
    );
    assert_eq!(
        calls_since(&fixture.backend, before),
        [Call::Cancel {
            session: child.clone()
        }]
    );

    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::Cancelled),
            "(no final message)"
        )]
    );
}

#[test]
fn cancel_child_drops_a_queued_prompt() {
    let Family {
        mut fixture,
        project,
        parent,
        ..
    } = family();
    let (child, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);
    let before = fixture.backend.calls().len();

    let answer = call_cancel_child(&mut fixture, &parent, &child);

    assert_eq!(
        answer,
        Ok(format!(
            "cancel requested for child {child}; its result follows as a child result"
        ))
    );
    assert_eq!(status(&fixture.app, &child), Status::Idle);
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
fn cancel_child_leaves_an_idle_child_alone() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let before = fixture.backend.calls().len();

    let answer = call_cancel_child(&mut fixture, &parent, &child);

    assert_eq!(answer, Ok(format!("child {child} is not running (idle)")));
    assert!(calls_since(&fixture.backend, before).is_empty());
    assert_eq!(status(&fixture.app, &child), Status::Idle);
}

#[test]
fn cancel_child_refuses_strangers() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let stranger = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
    let grandchild = attached_child(&mut fixture, &project, &child, "web", "Nested");
    let unknown = SessionId::new();
    orchestrate(&mut fixture, &stranger);
    orchestrate(&mut fixture, &grandchild);
    let before = fixture.backend.calls().len();

    let to_unknown = call_cancel_child(&mut fixture, &parent, &unknown);
    let to_stranger = call_cancel_child(&mut fixture, &parent, &stranger);
    let to_grandchild = call_cancel_child(&mut fixture, &parent, &grandchild);

    assert_eq!(to_unknown, Err(format!("unknown child {unknown}")));
    assert_eq!(
        to_stranger,
        Err(format!("{stranger} is not a child of this session"))
    );
    assert_eq!(
        to_grandchild,
        Err(format!("{grandchild} is not a child of this session"))
    );
    assert!(calls_since(&fixture.backend, before).is_empty());
    assert_eq!(status(&fixture.app, &stranger), Status::Running);
    assert_eq!(status(&fixture.app, &grandchild), Status::Running);
}

fn call_get_child_result(
    fixture: &mut Fixture,
    session: &SessionId,
    child: &SessionId,
) -> Result<String, String> {
    call_tool(
        fixture,
        session,
        OrchestratorTool::GetChildResult {
            child: child.clone(),
        },
    )
}

#[test]
fn get_child_result_returns_the_last_final_message() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "All done");
    fixture.app.transcripts.remove(&child);

    let answer = call_get_child_result(&mut fixture, &parent, &child);

    assert_eq!(answer, Ok(format!("child {child} · done\n\nAll done")));
}

#[test]
fn get_child_result_while_working_says_so() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let (queued, _) = unattached_child(&mut fixture, &project, &parent);
    orchestrate(&mut fixture, &child);
    orchestrate(&mut fixture, &queued);

    assert_eq!(
        call_get_child_result(&mut fixture, &parent, &child),
        Ok(format!(
            "child {child} is still working (running); its result follows as a child result"
        ))
    );
    assert_eq!(
        call_get_child_result(&mut fixture, &parent, &queued),
        Ok(format!(
            "child {queued} is still working (running); its result follows as a child result"
        ))
    );
}

#[test]
fn get_child_result_before_any_answer_says_none_yet() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_get_child_result(&mut fixture, &parent, &child);

    assert_eq!(
        answer,
        Ok(format!("child {child} has no result yet (idle)"))
    );
}

#[test]
fn get_child_result_of_a_failed_child_says_it_failed() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::Failed("boom".into()));

    let answer = call_get_child_result(&mut fixture, &parent, &child);

    assert_eq!(
        answer,
        Ok(format!("child {child} failed without a final message"))
    );
}

#[test]
fn get_child_result_after_a_cancelled_turn_returns_its_final_message() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("Half way".into()));
    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );
    let status = status_word(status(&fixture.app, &child));

    let answer = call_get_child_result(&mut fixture, &parent, &child);

    assert_eq!(answer, Ok(format!("child {child} · {status}\n\nHalf way")));
}

#[test]
fn get_child_result_refuses_strangers() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let stranger = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
    let grandchild = attached_child(&mut fixture, &project, &child, "web", "Nested");
    let unknown = SessionId::new();

    assert_eq!(
        call_get_child_result(&mut fixture, &parent, &unknown),
        Err(format!("unknown child {unknown}"))
    );
    assert_eq!(
        call_get_child_result(&mut fixture, &parent, &stranger),
        Err(format!("{stranger} is not a child of this session"))
    );
    assert_eq!(
        call_get_child_result(&mut fixture, &parent, &grandchild),
        Err(format!("{grandchild} is not a child of this session"))
    );
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
/// tool role.
fn creates(backend: &RecordingBackend) -> Vec<(RequestId, SessionId, PathBuf, ToolRole)> {
    backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::NewSession {
                request,
                session,
                cwd,
                role,
                ..
            } => Some((request, session, cwd, role)),
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

/// Asserts that `child` was sent its prompt after its repository's instruction chain, the
/// note that tells it its title and the note that tells it its orchestrator decides.
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
            &format!(
                "Your session is titled \"{}\". If your work drifts from that title, propose a new one with propose_title.\n\nYou work for an orchestrator. When your workflow opens a gate or you ask with ask_user, end your turn: the orchestrator decides and answers in your next prompt.\n\n{}",
                child.title, child.prompt
            )
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
    for ((_, _, cwd, role), name) in created.iter().zip(["api", "web"]) {
        let scope = Scope::Repository(repository(name));
        assert_eq!(
            cwd,
            &fixture.app.working_directory(&project, &scope).unwrap()
        );
        assert_eq!(*role, ToolRole::Child);
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
fn an_orchestrator_whose_turn_ended_on_an_undecided_proposal_is_not_interrupted() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        ..
    } = family();
    propose_now(&mut fixture, &parent, two_children());
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    let restarted = fixture_with(fixture.store.clone());

    assert_eq!(status(&restarted.app, &parent), Status::Idle);
    assert!(
        !restarted
            .store
            .load_updates(parent.clone())
            .unwrap()
            .iter()
            .any(|update| matches!(update, Update::Notice(_)))
    );
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
fn loading_a_child_opens_it_on_the_child_role() {
    let Family {
        dir: _dir,
        fixture,
        child,
        ..
    } = family();
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);

    restarted.app.select_session(child.clone()).unwrap();
    restarted.app.submit_prompt("Go on", Vec::new()).unwrap();

    assert!(matches!(
        restarted.backend.calls().last(),
        Some(Call::LoadSession {
            session,
            role: ToolRole::Child,
            ..
        }) if session == &child
    ));
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
    assert_eq!(created.title, "untitled");
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
    let reply = RecordingReply::new();

    let result = fixture.app.call_tool(ToolCall {
        session: parent.clone(),
        tool: OrchestratorTool::SendToChild {
            child: child.clone(),
            prompt: "Also add tests".into(),
            decision: None,
        }
        .into(),
        reply: reply.clone(),
    });

    assert!(matches!(result, Err(AppError::Backend(_))), "{result:?}");
    let error = result.unwrap_err();
    let calls = fixture.backend.calls();
    assert!(matches!(
        calls.last(),
        Some(Call::Prompt { session, .. }) if *session == child
    ));
    assert_eq!(reply.answers(), [Err(error.to_string())]);
}

#[test]
fn a_call_nobody_waits_for_changes_nothing() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    let calls_before = fixture.backend.calls();
    let entries_before = fixture.app.transcripts[&parent].entries().len();
    let reply = RecordingReply::new();
    reply.give_up();

    fixture
        .app
        .call_tool(ToolCall {
            session: parent.clone(),
            tool: OrchestratorTool::ProposeChildren(vec![proposed("api", "Add it", "API")]).into(),
            reply: reply.clone(),
        })
        .unwrap();

    assert!(reply.answers().is_empty());
    assert_eq!(
        fixture.app.transcripts[&parent].entries().len(),
        entries_before
    );
    assert_eq!(fixture.backend.calls(), calls_before);
}

#[test]
fn an_answered_proposal_then_the_turn_end_waits_for_you() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();

    let answer = call_propose(&mut fixture, &parent, two_children());
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    send(&mut fixture, &child, Update::AgentText("busy".into()));
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
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

    /// Where the default pattern puts the worktree of `branch` in the repository `name`, each
    /// `/` of `branch` spelt as the platform's separator.
    fn worktree(&self, name: &str, branch: &str) -> PathBuf {
        let tree = self.path(name).parent().unwrap().join(format!("{name}-wt"));
        branch.split('/').fold(tree, |path, part| path.join(part))
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
fn a_worktree_pattern_with_a_tilde_and_no_home_folder_is_a_childs_problem() {
    let mut split = git_split(&["api", "web"]);
    let settings = ProjectSettings {
        project: {
            let mut layer = SettingsLayer::default();
            layer
                .set(Level::Project, "worktree_path", "~/wt/{branch}")
                .unwrap();
            layer
        },
        ..ProjectSettings::default()
    };
    split
        .fixture
        .store
        .set_settings(split.project.clone(), settings);

    let checks = split.checked(&[
        worktree_in("api", "feat/x"),
        wants("web", BranchMode::NewBranch, "feat/x"),
    ]);

    assert_eq!(
        problems(&checks),
        [
            Some("worktree_path starts with ~, but there is no home folder to put there".into()),
            None
        ]
    );
}

#[test]
fn two_children_cannot_share_a_worktree_folder() {
    let mut split = git_split(&["api", "web"]);
    // Absolute on every platform: a bare `/wt` has no drive on Windows.
    let root = split.path("api").parent().unwrap().to_path_buf();
    let pattern = format!("{}/wt/{{branch}}", root.display());
    let settings = ProjectSettings {
        project: {
            let mut layer = SettingsLayer::default();
            layer
                .set(Level::Project, "worktree_path", &pattern)
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

    let shared = root.join("wt").join("feat").join("x");
    assert_eq!(checks[1].worktree, Some(shared.clone()));
    assert_eq!(
        problems(&checks),
        [
            None,
            Some(format!(
                "worktree {} is also used by another child",
                shared.display()
            ))
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

/// Lets `session` call the session tool `tool` and returns the single answer its reply was
/// given.
fn call_session_tool(
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

fn call_rename(
    fixture: &mut Fixture,
    session: &SessionId,
    target: Option<&SessionId>,
    title: &str,
) -> Result<String, String> {
    call_session_tool(
        fixture,
        session,
        SessionTool::Rename {
            title: title.into(),
            session: target.cloned(),
        },
    )
}

/// A session of the family's project that is neither an orchestrator nor a child.
fn standalone(fixture: &mut Fixture, project: &ProjectId) -> SessionId {
    let session = attached_session(fixture, project, Scope::Repository(repository("web")));
    assert_eq!(
        fixture.app.session(&session).unwrap().tool_role(),
        ToolRole::Plain
    );
    session
}

fn title_of(app: &App, session: &SessionId) -> (String, TitleSource) {
    let session = app.session(session).unwrap();
    (session.title.clone(), session.title_source)
}

#[test]
fn a_standalone_session_renames_itself() {
    let Family {
        mut fixture,
        project,
        ..
    } = family();
    let session = standalone(&mut fixture, &project);
    let old = fixture.app.session(&session).unwrap().title.clone();

    let answer = call_rename(&mut fixture, &session, None, "Auth flow");

    assert_eq!(answer, Ok(format!("renamed \"{old}\" to \"Auth flow\"")));
    assert_eq!(
        title_of(&fixture.app, &session),
        ("Auth flow".to_owned(), TitleSource::Chosen)
    );
    let stored = fixture.store.load_sessions(project).unwrap();
    let stored = stored.iter().find(|stored| stored.id == session).unwrap();
    assert_eq!(stored.title, "Auth flow");
    assert_eq!(stored.title_source, TitleSource::Chosen);
}

#[test]
fn an_orchestrator_renames_itself() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();

    let answer = call_rename(&mut fixture, &parent, None, "Login across the stack");

    assert!(answer.is_ok(), "answer {answer:?}");
    assert_eq!(
        title_of(&fixture.app, &parent),
        ("Login across the stack".to_owned(), TitleSource::Chosen)
    );
}

#[test]
fn an_orchestrator_renames_its_child() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_rename(&mut fixture, &parent, Some(&child), "Auth endpoint");

    assert_eq!(
        answer,
        Ok("renamed \"API endpoint\" to \"Auth endpoint\"".to_owned())
    );
    assert_eq!(
        title_of(&fixture.app, &child),
        ("Auth endpoint".to_owned(), TitleSource::Chosen)
    );
    assert_ne!(fixture.app.session(&parent).unwrap().title, "Auth endpoint");
}

#[test]
fn rename_replaces_a_title_the_user_typed() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.rename_session(child.clone(), "Mine").unwrap();
    assert_eq!(title_of(&fixture.app, &child).1, TitleSource::Given);

    let answer = call_rename(&mut fixture, &parent, Some(&child), "Theirs");

    assert!(answer.is_ok(), "answer {answer:?}");
    assert_eq!(
        title_of(&fixture.app, &child),
        ("Theirs".to_owned(), TitleSource::Chosen)
    );
}

#[test]
fn rename_trims_the_title() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_rename(&mut fixture, &parent, Some(&child), "  Auth  ");

    assert_eq!(
        answer,
        Ok("renamed \"API endpoint\" to \"Auth\"".to_owned())
    );
    assert_eq!(title_of(&fixture.app, &child).0, "Auth");
}

#[test]
fn a_blank_rename_changes_nothing() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let before = title_of(&fixture.app, &child);

    let answer = call_rename(&mut fixture, &parent, Some(&child), "   ");

    assert_eq!(answer, Err("rename_session needs a title".to_owned()));
    assert_eq!(title_of(&fixture.app, &child), before);
}

#[test]
fn an_orchestrator_cannot_rename_a_foreign_session() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let nested = attached_child(&mut fixture, &project, &child, "web", "Nested");
    let unknown = SessionId::new();

    let foreign = call_rename(&mut fixture, &parent, Some(&nested), "Mine now");
    let missing = call_rename(&mut fixture, &parent, Some(&unknown), "Mine now");

    assert_eq!(
        foreign,
        Err(format!("{nested} is not a child of this session"))
    );
    assert_eq!(missing, Err(format!("unknown child {unknown}")));
    assert_eq!(title_of(&fixture.app, &nested).0, "Nested");
}

#[test]
fn a_standalone_session_cannot_name_another() {
    let Family {
        mut fixture,
        project,
        child,
        ..
    } = family();
    let session = standalone(&mut fixture, &project);

    let answer = call_rename(&mut fixture, &session, Some(&child), "Mine now");

    assert_eq!(
        answer,
        Err("this session is not an orchestrator".to_owned())
    );
    assert_eq!(title_of(&fixture.app, &child).0, "API endpoint");
}

#[test]
fn a_child_cannot_rename_itself() {
    let Family {
        mut fixture, child, ..
    } = family();

    let answer = call_rename(&mut fixture, &child, None, "Mine now");

    assert_eq!(
        answer,
        Err("a child cannot rename itself; propose a title with propose_title".to_owned())
    );
    assert_eq!(title_of(&fixture.app, &child).0, "API endpoint");
}

#[test]
fn rename_of_a_running_child_keeps_its_status() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture
        .app
        .change_session(child.clone(), |running| running.status = Status::Running)
        .unwrap();
    let calls = fixture.backend.calls().len();

    let answer = call_rename(&mut fixture, &parent, Some(&child), "Auth endpoint");

    assert!(answer.is_ok(), "answer {answer:?}");
    assert_eq!(status(&fixture.app, &child), Status::Running);
    assert_eq!(fixture.backend.calls().len(), calls);
}

#[test]
fn an_agent_title_does_not_replace_a_chosen_title() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    call_rename(&mut fixture, &parent, Some(&child), "Auth endpoint").unwrap();

    send(&mut fixture, &child, Update::Title("Other".into()));

    assert_eq!(title_of(&fixture.app, &child).0, "Auth endpoint");
}

#[test]
fn list_children_marks_titles_the_user_set() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let chosen = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    fixture.app.rename_session(child.clone(), "Mine").unwrap();
    call_rename(&mut fixture, &parent, Some(&chosen), "Their page").unwrap();

    let answer = call_list_children(&mut fixture, &parent).unwrap();

    assert_eq!(
        answer,
        format!(
            "children:\n- Mine · api · session {child} · idle · set by you\n- Their page · web · session {chosen} · idle"
        )
    );
}

fn call_propose_title(
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

fn api_proposal(child: &SessionId, proposed: &str, by_user: bool) -> TitleProposal {
    TitleProposal {
        child: child.clone(),
        repository: repository("api"),
        title: "API endpoint".into(),
        proposed: proposed.into(),
        by_user,
    }
}

fn proposal_text(child: &SessionId, proposed: &str, by_user: bool) -> String {
    title_proposal_prompt(&api_proposal(child, proposed, by_user))
}

#[test]
fn a_child_proposal_prompts_its_idle_orchestrator() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_propose_title(&mut fixture, &child, "Auth endpoint");

    assert_eq!(answer, Ok("sent to the orchestrator".to_owned()));
    assert_eq!(title_of(&fixture.app, &child).0, "API endpoint");
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", false)
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::TitleProposal(api_proposal(
            &child,
            "Auth endpoint",
            false
        )))
    );
    assert_eq!(
        fixture.store.load_updates(parent.clone()).unwrap().last(),
        Some(&Update::TitleProposed(api_proposal(
            &child,
            "Auth endpoint",
            false
        )))
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn a_proposal_trims_the_title() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    call_propose_title(&mut fixture, &child, "  Auth  ").unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth", false)
    );
}

#[test]
fn a_blank_proposal_sends_nothing() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    let answer = call_propose_title(&mut fixture, &child, "  ");

    assert_eq!(answer, Err("propose_title needs a title".to_owned()));
    assert!(prompts(&fixture.backend, &parent).is_empty());
}

#[test]
fn a_proposal_marks_a_title_the_user_set() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture
        .app
        .rename_session(child.clone(), "API endpoint")
        .unwrap();

    call_propose_title(&mut fixture, &child, "Auth endpoint").unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", true)
    );
}

#[test]
fn a_title_proposal_waits_behind_a_child_result() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
    orchestrate(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("api done".into()));
    send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

    let answer = call_propose_title(&mut fixture, &child, "Auth endpoint");

    assert_eq!(answer, Ok("sent to the orchestrator".to_owned()));
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    let ended = ChildOutcome::Ended(StopReason::EndTurn);
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
    );
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", false)
    );
}

#[test]
fn a_standalone_session_cannot_propose_a_title() {
    let Family {
        mut fixture,
        project,
        ..
    } = family();
    let session = standalone(&mut fixture, &project);

    let answer = call_propose_title(&mut fixture, &session, "Auth");

    assert_eq!(answer, Err("this session is not a child".to_owned()));
}

#[test]
fn an_orchestrator_cannot_propose_a_title() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();

    let answer = call_propose_title(&mut fixture, &parent, "Auth");

    assert_eq!(answer, Err("this session is not a child".to_owned()));
    assert!(prompts(&fixture.backend, &parent).is_empty());
}

#[test]
fn a_child_outside_a_repository_proposes_nothing() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture
        .app
        .change_session(child.clone(), |moved| moved.scope = Scope::Project)
        .unwrap();

    let answer = call_propose_title(&mut fixture, &child, "Auth");

    assert_eq!(
        answer,
        Err("this session works in no repository".to_owned())
    );
    assert!(prompts(&fixture.backend, &parent).is_empty());
}

#[test]
fn rename_collapses_a_multi_line_title_to_one_line() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    call_rename(&mut fixture, &parent, Some(&child), "Auth\n   flow\tfix").unwrap();

    assert_eq!(title_of(&fixture.app, &child).0, "Auth flow fix");
}

#[test]
fn rename_caps_a_long_title() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    call_rename(&mut fixture, &parent, Some(&child), &"a".repeat(100)).unwrap();

    assert_eq!(
        title_of(&fixture.app, &child).0,
        format!("{}…", "a".repeat(60))
    );
}

#[test]
fn a_proposal_collapses_and_caps_the_title() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    call_propose_title(
        &mut fixture,
        &child,
        &format!("Auth\nflow {}", "b".repeat(80)),
    )
    .unwrap();

    let expected = format!("Auth flow {}…", "b".repeat(50));
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, &expected, false)
    );
}

#[test]
fn a_plain_session_is_not_told_its_title() {
    let Family {
        mut fixture,
        project,
        ..
    } = family();
    let session = fixture
        .app
        .new_session(project, Scope::Repository(repository("web")))
        .unwrap();
    let request = fixture
        .backend
        .calls()
        .last()
        .unwrap()
        .as_new_session()
        .unwrap()
        .0;
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-plain"),
            config: Vec::new(),
        })
        .unwrap();
    fixture.app.select_session(session.clone()).unwrap();

    fixture.app.submit_prompt("Hello", Vec::new()).unwrap();

    assert_eq!(prompts(&fixture.backend, &session), ["Hello"]);
}

/// Starts a turn of the family's orchestrator, so what reaches it waits.
fn busy(fixture: &mut Fixture, parent: &SessionId) {
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
}

#[test]
fn a_newer_proposal_replaces_one_that_waits() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);

    call_propose_title(&mut fixture, &child, "First").unwrap();
    call_propose_title(&mut fixture, &child, "Second").unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Second", false)
    );
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(prompts(&fixture.backend, &parent).len(), 2);
}

#[test]
fn proposals_of_two_children_both_wait() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    busy(&mut fixture, &parent);

    call_propose_title(&mut fixture, &child, "Auth endpoint").unwrap();
    call_propose_title(&mut fixture, &web, "Login page").unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    let sent = prompts(&fixture.backend, &parent);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[1], proposal_text(&child, "Auth endpoint", false));
    assert!(
        sent[2].starts_with("title proposal · web · Web page"),
        "{}",
        sent[2]
    );
}

#[test]
fn a_waiting_proposal_reads_the_title_when_it_goes_out() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);
    call_propose_title(&mut fixture, &child, "Auth endpoint").unwrap();

    fixture.app.rename_session(child.clone(), "Mine").unwrap();
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    let expected = TitleProposal {
        title: "Mine".into(),
        ..api_proposal(&child, "Auth endpoint", true)
    };
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        title_proposal_prompt(&expected)
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::TitleProposal(expected))
    );
}

#[test]
fn an_orchestrator_naming_itself_renames_itself() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();

    let answer = call_rename(&mut fixture, &parent, Some(&parent.clone()), "Login");

    assert!(answer.is_ok(), "answer {answer:?}");
    assert_eq!(
        title_of(&fixture.app, &parent),
        ("Login".to_owned(), TitleSource::Chosen)
    );
}

#[test]
fn a_waiting_proposal_whose_child_is_gone_goes_out_as_made() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);
    call_propose_title(&mut fixture, &child, "Auth endpoint").unwrap();

    fixture.app.sessions.retain(|session| session.id != child);
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", false)
    );
}

/// The superpowers report `event`, as a workflow report.
fn superpowers(event: crate::SuperpowersV7Event) -> crate::workflow::ModuleEvent {
    event.into()
}

/// Lets `child` report that it executes its plan inline and works on task 4.
fn at_task_four(fixture: &mut Fixture, child: &SessionId) {
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
fn reach_checkpoint(fixture: &mut Fixture, child: &SessionId) {
    call_report(
        fixture,
        child,
        superpowers(crate::SuperpowersV7Event::ReachCheckpoint { after_task: 4 }),
    )
    .unwrap();
}

/// Lets `child` ask the user "Merge or PR?", with the options Merge and PR.
fn ask_merge_or_pr(fixture: &mut Fixture, child: &SessionId) {
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

fn merge_or_pr() -> crate::question::Question {
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
fn end_with(fixture: &mut Fixture, child: &SessionId, answer: &str) {
    send(fixture, child, Update::AgentText(answer.into()));
    send(fixture, child, Update::TurnEnded(StopReason::EndTurn));
}

/// The step the tree shows for a superpowers child that works on task 4 inline.
const TASK_FOUR: &str = "execute · 4";

/// The step the tree shows for a superpowers child at its checkpoint after task 4.
const CHECKPOINT: &str = "execute · checkpoint";

const GATE_FOOTER: &str = "Decide with send_to_child(session_id, decision: \"approve\" or \"changes\", prompt). Ask the user with ask_user first when the user should make this decision.";

const QUESTION_FOOTER: &str = "Answer with send_to_child(session_id, prompt); the prompt is the child's answer. Ask the user with ask_user first when the user should answer.";

#[test]
fn checkpoint_in_a_child_reports_child_waits() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);

    end_with(&mut fixture, &child, "Task 4 is done.");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        format!(
            "child waits · api · API endpoint\nsession {child}\n{CHECKPOINT} · waits at its checkpoint\n\nTask 4 is done.\n\n{GATE_FOOTER}"
        )
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildResult {
            child: child.clone(),
            repository: repository("api"),
            title: "API endpoint".into(),
            origin: TurnOrigin::Orchestrator,
            outcome: ChildOutcome::Ended(StopReason::EndTurn),
            text: "Task 4 is done.".into(),
            state: ChildState {
                step: Some(CHECKPOINT.into()),
                waits: Some(ChildWait::Gate {
                    word: "checkpoint".into(),
                }),
            },
        })
    );
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn question_in_a_child_reports_child_waits() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);

    end_with(&mut fixture, &child, "Which one?");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        format!(
            "child waits · api · API endpoint\nsession {child}\nasks: Merge or PR?\noptions: Merge · PR\n\nWhich one?\n\n{QUESTION_FOOTER}"
        )
    );
    assert_eq!(
        call_tool(
            &mut fixture,
            &parent,
            OrchestratorTool::GetChildResult {
                child: child.clone()
            }
        ),
        Ok(format!(
            "child {child} · waiting for you\nasks: Merge or PR?\noptions: Merge · PR\n\nWhich one?"
        ))
    );
}

#[test]
fn a_question_of_a_child_in_a_workflow_names_its_step() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);

    end_with(&mut fixture, &child, "Which one?");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        format!(
            "child waits · api · API endpoint\nsession {child}\n{TASK_FOUR} · asks: Merge or PR?\noptions: Merge · PR\n\nWhich one?\n\n{QUESTION_FOOTER}"
        )
    );
}

#[test]
fn gate_and_question_report_as_question() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);
    // The question is read from the child's transcript, loaded for it when it is not.
    fixture.app.transcripts.remove(&child);

    assert_eq!(
        fixture.app.child_state(&child).unwrap(),
        ChildState {
            step: Some(CHECKPOINT.into()),
            waits: Some(ChildWait::Question(merge_or_pr())),
        }
    );
}

#[test]
fn a_child_without_workflow_or_wait_has_an_empty_state() {
    let Family {
        mut fixture, child, ..
    } = family();

    assert_eq!(
        fixture.app.child_state(&child).unwrap(),
        ChildState::default()
    );
    assert!(matches!(
        fixture.app.child_state(&SessionId::new()),
        Err(AppError::UnknownSession(_))
    ));
}

#[test]
fn child_without_live_orchestrator_still_withholds() {
    let parents: [fn(&mut crate::Session); 2] = [
        |parent| parent.archived = Some(crate::Archived { at: 1 }),
        |parent| parent.orchestrator = false,
    ];
    for change in parents {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        assert!(fixture.app.delegates(&child));
        fixture.app.change_session(parent.clone(), change).unwrap();
        assert!(!fixture.app.delegates(&child));
        orchestrate(&mut fixture, &child);
        reach_checkpoint(&mut fixture, &child);

        end_with(&mut fixture, &child, "Task 4 is done.");

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(deferred(&fixture.app, &parent), 0);
        assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    }
}

#[test]
fn only_a_child_of_an_existing_orchestrator_delegates() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();

    assert!(!fixture.app.delegates(&parent));
    assert!(!fixture.app.delegates(&SessionId::new()));
    fixture
        .app
        .change_session(child.clone(), |orphan| {
            orphan.parent = Some(SessionId::new());
        })
        .unwrap();
    assert!(!fixture.app.delegates(&child));
}

#[test]
fn a_failed_turn_at_a_gate_reports_a_child_result_with_its_step() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    send(&mut fixture, &child, Update::AgentText("Half".into()));

    send(&mut fixture, &child, Update::Failed("boom".into()));

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        format!(
            "child result · api · API endpoint\nsession {child}\n{CHECKPOINT}\n(failed: boom)\n\nHalf"
        )
    );
}

#[test]
fn child_result_carries_the_step_line() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);

    end_with(&mut fixture, &child, "Working on it.");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        format!(
            "child result · api · API endpoint\nsession {child}\n{TASK_FOUR}\n\nWorking on it."
        )
    );
}

#[test]
fn list_children_and_get_child_result_name_the_state() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let plain = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    fixture
        .app
        .change_session(child.clone(), |given| {
            given.title_source = TitleSource::Given;
        })
        .unwrap();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    end_with(&mut fixture, &child, "Task 4 is done.");

    assert_eq!(
        call_list_children(&mut fixture, &parent),
        Ok(format!(
            "children:\n- API endpoint · api · session {child} · waiting for you · {CHECKPOINT} · waits at its checkpoint · set by you\n- Web page · web · session {plain} · idle"
        ))
    );
    assert_eq!(
        call_tool(
            &mut fixture,
            &parent,
            OrchestratorTool::GetChildResult {
                child: child.clone()
            }
        ),
        Ok(format!(
            "child {child} · waiting for you\n{CHECKPOINT} · waits at its checkpoint\n\nTask 4 is done."
        ))
    );
}

/// Lets the orchestrator `parent` call `send_to_child` for `child` with `prompt` and
/// `decision`, and returns the answer.
fn call_decide(
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
fn ended_at_checkpoint(fixture: &mut Fixture, child: &SessionId) {
    orchestrate(fixture, child);
    at_task_four(fixture, child);
    reach_checkpoint(fixture, child);
    end_with(fixture, child, "Task 4 is done.");
}

/// Lets `child` run a turn its orchestrator started that ends with "Merge or PR?" open.
fn ended_with_question(fixture: &mut Fixture, child: &SessionId) {
    orchestrate(fixture, child);
    ask_merge_or_pr(fixture, child);
    end_with(fixture, child, "Tests pass.");
}

/// How each gate of `child` was closed, and by whom, oldest first.
fn gate_closings(
    fixture: &Fixture,
    child: &SessionId,
) -> Vec<(crate::workflow::GateCloser, crate::workflow::Decider)> {
    fixture
        .store
        .load_updates(child.clone())
        .unwrap()
        .into_iter()
        .filter_map(|update| match update {
            Update::Workflow {
                event: crate::workflow::WorkflowEvent::GateClosed { by, decider, .. },
                ..
            } => Some((by, decider)),
            _ => None,
        })
        .collect()
}

/// How each question of `child` was closed, and by whom, oldest first.
fn question_closings(
    fixture: &Fixture,
    child: &SessionId,
) -> Vec<(crate::question::QuestionOutcome, crate::workflow::Decider)> {
    fixture
        .store
        .load_updates(child.clone())
        .unwrap()
        .into_iter()
        .filter_map(|update| match update {
            Update::QuestionClosed {
                outcome, decider, ..
            } => Some((outcome, decider)),
            _ => None,
        })
        .collect()
}

/// The note the child's transcript shows for the last gate it closed, at 14:02.
fn last_gate_note(fixture: &Fixture, child: &SessionId) -> Option<String> {
    fixture.app.transcripts[child]
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            Entry::Workflow {
                event: event @ crate::workflow::WorkflowEvent::GateClosed { .. },
                ..
            } => crate::workflow::step_note(event, "14:02"),
            _ => None,
        })
}

/// Asserts that `answer` refuses the call with `refusal`, and that `child` was not prompted
/// again and the orchestrator recorded no prompt for it.
fn assert_refused(
    fixture: &Fixture,
    parent: &SessionId,
    child: &SessionId,
    answer: &Result<String, String>,
    refusal: &str,
) {
    assert_eq!(answer, &Err(refusal.to_owned()));
    assert_eq!(prompts(&fixture.backend, child).len(), 1);
    assert!(
        !fixture.app.transcripts[parent]
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::ChildPrompted { .. }))
    );
}

#[test]
fn approve_passes_a_childs_checkpoint() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);

    let answer = call_decide(
        &mut fixture,
        &parent,
        &child,
        "",
        Some(crate::GateDecision::Approve),
    );

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(last_prompt(&fixture.backend, &child), "Continue.");
    assert_eq!(
        fixture.app.transcripts[&child].entries().last(),
        Some(&Entry::OrchestratorPrompt("Continue.".into()))
    );
    assert_eq!(
        gate_closings(&fixture, &child),
        [(
            crate::workflow::GateCloser::Approve,
            crate::workflow::Decider::Orchestrator
        )]
    );
    assert_eq!(fixture.app.gate_open(&child), None);
    assert_eq!(
        last_gate_note(&fixture, &child).as_deref(),
        Some("Checkpoint passed by orchestrator at 14:02")
    );
    assert_eq!(status(&fixture.app, &child), Status::Running);
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildPrompted {
            child: child.clone(),
            prompt: String::new(),
            decision: Some(crate::GateDecision::Approve),
        })
    );

    // The decided turn counts as the orchestrator's.
    end_with(&mut fixture, &child, "Task 5 is done.");
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );
    let reported = last_prompt(&fixture.backend, &parent);
    assert!(reported.starts_with("child result · "), "{reported}");
    assert!(
        !reported.contains("(turn started by the user)"),
        "{reported}"
    );
}

#[test]
fn approve_with_prompt_appends_it() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);

    let answer = call_decide(
        &mut fixture,
        &parent,
        &child,
        "Run the web tests first.",
        Some(crate::GateDecision::Approve),
    );

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(
        last_prompt(&fixture.backend, &child),
        "Continue.\n\nRun the web tests first."
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildPrompted {
            child: child.clone(),
            prompt: "Run the web tests first.".into(),
            decision: Some(crate::GateDecision::Approve),
        })
    );
}

#[test]
fn approve_with_a_blank_prompt_sends_the_approval_alone() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);

    call_decide(
        &mut fixture,
        &parent,
        &child,
        " \n ",
        Some(crate::GateDecision::Approve),
    )
    .unwrap();

    assert_eq!(last_prompt(&fixture.backend, &child), "Continue.");
}

#[test]
fn approve_marks_a_bmad_document_approved() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    turn_at_build_plan(&mut fixture, &child, "The plan is written");

    call_decide(
        &mut fixture,
        &parent,
        &child,
        "",
        Some(crate::GateDecision::Approve),
    )
    .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &child),
        "Plan approved. Build it."
    );
    let status = fixture
        .app
        .session(&child)
        .unwrap()
        .workflow
        .clone()
        .unwrap();
    assert_eq!(status.gate, None);
    let crate::workflow::ModuleState::BmadV6_12(state) = &status.state else {
        panic!("not a BMAD state: {:?}", status.state);
    };
    let run = state.workflow.as_ref().unwrap();
    let plan = run
        .documents
        .iter()
        .find(|doc| doc.label == "build plan")
        .unwrap();
    assert!(plan.approved_at.is_some(), "{plan:?}");
    assert_eq!(
        last_gate_note(&fixture, &child).as_deref(),
        Some("Build plan approved by orchestrator at 14:02")
    );
}

#[test]
fn changes_need_a_prompt_and_carry_the_prefix() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);

    let blank = call_decide(
        &mut fixture,
        &parent,
        &child,
        " ",
        Some(crate::GateDecision::Changes),
    );
    assert_refused(
        &fixture,
        &parent,
        &child,
        &blank,
        "send_to_child needs a prompt",
    );
    assert!(fixture.app.gate_open(&child).is_some());

    let answer = call_decide(
        &mut fixture,
        &parent,
        &child,
        "fix x",
        Some(crate::GateDecision::Changes),
    );

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(
        last_prompt(&fixture.backend, &child),
        "Before continuing: fix x"
    );
    assert_eq!(
        gate_closings(&fixture, &child),
        [(
            crate::workflow::GateCloser::Prompt,
            crate::workflow::Decider::Orchestrator
        )]
    );
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildPrompted {
            child: child.clone(),
            prompt: "fix x".into(),
            decision: Some(crate::GateDecision::Changes),
        })
    );
}

#[test]
fn changes_to_a_bmad_plan_carry_its_prefix() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    turn_at_build_plan(&mut fixture, &child, "The plan is written");

    call_decide(
        &mut fixture,
        &parent,
        &child,
        "split step 2",
        Some(crate::GateDecision::Changes),
    )
    .unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &child),
        "Changes to the plan: split step 2"
    );
    assert_eq!(
        last_gate_note(&fixture, &child).as_deref(),
        Some("Changes to the build plan requested by orchestrator at 14:02")
    );
}

#[test]
fn gate_without_decision_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);

    let answer = call_decide(&mut fixture, &parent, &child, "Go on", None);

    assert_refused(
        &fixture,
        &parent,
        &child,
        &answer,
        &format!(r#"child {child} waits at its checkpoint; send decision "approve" or "changes""#),
    );
    assert!(fixture.app.gate_open(&child).is_some());
    assert_eq!(deferred(&fixture.app, &child), 0);
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
}

#[test]
fn question_is_answered_by_a_plain_prompt() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_with_question(&mut fixture, &child);

    let answer = call_decide(&mut fixture, &parent, &child, "PR", None);

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(last_prompt(&fixture.backend, &child), "PR");
    assert_eq!(
        question_closings(&fixture, &child),
        [(
            crate::question::QuestionOutcome::Answered("PR".into()),
            crate::workflow::Decider::Orchestrator
        )]
    );
    assert!(!fixture.app.question_open(&child));
    assert_eq!(status(&fixture.app, &child), Status::Running);
    assert_eq!(
        fixture.app.transcripts[&parent].entries().last(),
        Some(&Entry::ChildPrompted {
            child: child.clone(),
            prompt: "PR".into(),
            decision: None,
        })
    );
}

#[test]
fn answer_closes_gate_and_question() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);
    end_with(&mut fixture, &child, "Task 4 is done.");

    let answer = call_decide(&mut fixture, &parent, &child, "PR", None);

    assert_eq!(answer, Ok(format!("prompt sent to child {child}")));
    assert_eq!(last_prompt(&fixture.backend, &child), "PR");
    assert_eq!(
        gate_closings(&fixture, &child),
        [(
            crate::workflow::GateCloser::Prompt,
            crate::workflow::Decider::Orchestrator
        )]
    );
    assert_eq!(
        question_closings(&fixture, &child),
        [(
            crate::question::QuestionOutcome::Answered("PR".into()),
            crate::workflow::Decider::Orchestrator
        )]
    );
}

#[test]
fn question_with_decision_is_refused() {
    for decision in [crate::GateDecision::Approve, crate::GateDecision::Changes] {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        at_task_four(&mut fixture, &child);
        reach_checkpoint(&mut fixture, &child);
        ask_merge_or_pr(&mut fixture, &child);
        end_with(&mut fixture, &child, "Task 4 is done.");

        let answer = call_decide(&mut fixture, &parent, &child, "PR", Some(decision));

        assert_refused(
            &fixture,
            &parent,
            &child,
            &answer,
            &format!("child {child} asks a question; answer it with a prompt and no decision"),
        );
        assert!(fixture.app.question_open(&child));
        assert!(fixture.app.gate_open(&child).is_some());
    }
}

#[test]
fn decision_without_gate_is_refused() {
    for decision in [crate::GateDecision::Approve, crate::GateDecision::Changes] {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        finish(&mut fixture, &child, "added");

        let answer = call_decide(&mut fixture, &parent, &child, "Go on", Some(decision));

        assert_refused(
            &fixture,
            &parent,
            &child,
            &answer,
            &format!("child {child} has no open gate"),
        );
        assert_eq!(deferred(&fixture.app, &child), 0);
    }
}

#[test]
fn orchestrator_decision_after_user_decided_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);
    fixture.app.approve_gate(child.clone()).unwrap();
    end_with(&mut fixture, &child, "Task 5 is done.");
    let sent = prompts(&fixture.backend, &child).len();

    let answer = call_decide(
        &mut fixture,
        &parent,
        &child,
        "",
        Some(crate::GateDecision::Approve),
    );

    assert_eq!(answer, Err(format!("child {child} has no open gate")));
    assert_eq!(prompts(&fixture.backend, &child).len(), sent);
    assert_eq!(
        gate_closings(&fixture, &child),
        [(
            crate::workflow::GateCloser::Approve,
            crate::workflow::Decider::User
        )]
    );
}

#[test]
fn decision_while_child_in_turn_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);

    let decided = call_decide(
        &mut fixture,
        &parent,
        &child,
        "",
        Some(crate::GateDecision::Approve),
    );
    let plain = call_decide(&mut fixture, &parent, &child, "Go on", None);

    let refusal = format!("child {child} is still in its turn");
    assert_refused(&fixture, &parent, &child, &decided, &refusal);
    assert_refused(&fixture, &parent, &child, &plain, &refusal);
    assert_eq!(deferred(&fixture.app, &child), 0);
    assert!(fixture.app.gate_open(&child).is_some());
}

#[test]
fn a_question_of_a_child_in_its_turn_is_not_answered() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);

    let answer = call_decide(&mut fixture, &parent, &child, "PR", None);

    assert_refused(
        &fixture,
        &parent,
        &child,
        &answer,
        &format!("child {child} is still in its turn"),
    );
    assert!(fixture.app.question_open(&child));
}

#[test]
fn a_decision_for_an_archived_child_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);
    fixture
        .app
        .change_session(child.clone(), |archived| {
            archived.archived = Some(crate::Archived { at: 1 });
        })
        .unwrap();

    let answer = call_decide(
        &mut fixture,
        &parent,
        &child,
        "",
        Some(crate::GateDecision::Approve),
    );

    assert_refused(
        &fixture,
        &parent,
        &child,
        &answer,
        &format!("session {child} is archived; restore it first"),
    );
    assert!(fixture.app.gate_open(&child).is_some());
}

/// The children whose `child waits` prompts the orchestrator `parent` has queued, in order.
fn queued_waits<'a>(app: &'a App, parent: &SessionId) -> Vec<&'a SessionId> {
    app.deferred
        .get(parent)
        .into_iter()
        .flatten()
        .filter_map(|generated| match generated {
            Generated::ChildResult { child, state, .. } if state.waits.is_some() => Some(child),
            _ => None,
        })
        .collect()
}

/// How many `child waits` prompts started a turn of `parent`, as its stored history holds.
fn waits_recorded(store: &MemoryStore, parent: &SessionId) -> usize {
    store
        .load_updates(parent.clone())
        .unwrap()
        .iter()
        .filter(
            |update| matches!(update, Update::ChildResult { state, .. } if state.waits.is_some()),
        )
        .count()
}

/// Lets the orchestrator `parent` run a turn that asks the user "Merge or PR?" and ends.
fn orchestrator_asks(fixture: &mut Fixture, parent: &SessionId) {
    busy(fixture, parent);
    ask_merge_or_pr(fixture, parent);
    end_with(fixture, parent, "Which one?");
    assert_eq!(status(&fixture.app, parent), Status::WaitingForYou);
}

/// The `child waits` prompt of the API child at its checkpoint after task 4.
fn checkpoint_wait(child: &SessionId) -> String {
    format!(
        "child waits · api · API endpoint\nsession {child}\n{CHECKPOINT} · waits at its checkpoint\n\nTask 4 is done.\n\n{GATE_FOOTER}"
    )
}

#[test]
fn orchestrator_question_parks_generated_prompts() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrator_asks(&mut fixture, &parent);

    ended_at_checkpoint(&mut fixture, &child);

    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
    assert_eq!(prompts(&fixture.backend, &parent).len(), 1);
    assert!(fixture.app.question_open(&parent));

    fixture.app.answer_question(parent.clone(), "PR").unwrap();

    assert_eq!(last_prompt(&fixture.backend, &parent), "PR");
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);

    end_with(&mut fixture, &parent, "Noted.");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        checkpoint_wait(&child)
    );
    assert!(queued_waits(&fixture.app, &parent).is_empty());
}

#[test]
fn cancelling_the_orchestrators_question_delivers_the_queue() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrator_asks(&mut fixture, &parent);
    ended_at_checkpoint(&mut fixture, &child);
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);

    fixture.app.cancel_question(parent.clone()).unwrap();

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        checkpoint_wait(&child)
    );
    assert!(queued_waits(&fixture.app, &parent).is_empty());
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn cancelling_a_question_mid_turn_delivers_nothing_before_the_turn_ends() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);
    ask_merge_or_pr(&mut fixture, &parent);
    ended_at_checkpoint(&mut fixture, &child);

    fixture.app.cancel_question(parent.clone()).unwrap();

    assert_eq!(prompts(&fixture.backend, &parent).len(), 1);
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);

    end_with(&mut fixture, &parent, "Planned.");

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        checkpoint_wait(&child)
    );
}

#[test]
fn user_deciding_in_the_child_drops_its_queued_wait() {
    let Family {
        mut fixture,
        project,
        parent,
        child,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    busy(&mut fixture, &parent);
    finish(&mut fixture, &child, "Started.");
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    end_with(&mut fixture, &child, "Task 4 is done.");
    ended_at_checkpoint(&mut fixture, &web);
    assert_eq!(deferred(&fixture.app, &parent), 3);

    // A wait already queued is not queued again.
    fixture.app.report_waiting_children(&parent).unwrap();
    assert_eq!(queued_waits(&fixture.app, &parent), [&child, &web]);

    fixture.app.approve_gate(child.clone()).unwrap();

    assert_eq!(queued_waits(&fixture.app, &parent), [&web]);
    assert_eq!(deferred(&fixture.app, &parent), 2);
}

#[test]
fn user_answering_in_the_child_drops_its_queued_wait() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);
    ended_with_question(&mut fixture, &child);
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);

    fixture.app.answer_question(child.clone(), "PR").unwrap();

    assert_eq!(deferred(&fixture.app, &parent), 0);
}

#[test]
fn archived_orchestrator_hands_waits_back_to_the_user() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    fixture.app.archive(parent.clone()).unwrap();

    end_with(&mut fixture, &child, "Task 4 is done.");

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);

    fixture.app.restore(parent.clone()).unwrap();

    assert_eq!(
        prompts(&fixture.backend, &parent),
        [checkpoint_wait(&child)]
    );
    let since = fixture
        .app
        .session(&child)
        .unwrap()
        .workflow
        .as_ref()
        .unwrap();
    let since = since.gate.as_ref().unwrap().since;
    assert_eq!(
        fixture.app.session(&child).unwrap().wait_reported,
        Some(crate::WaitMark::Gate { since })
    );

    // The wait that went out is not reported again.
    fixture.app.report_waiting_children(&parent).unwrap();

    assert_eq!(prompts(&fixture.backend, &parent).len(), 1);
    assert_eq!(deferred(&fixture.app, &parent), 0);
}

#[test]
fn a_reported_question_is_marked_before_the_gate_and_a_result_marks_nothing() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "Started.");
    assert_eq!(fixture.app.session(&child).unwrap().wait_reported, None);
    send(
        &mut fixture,
        &parent,
        Update::TurnEnded(StopReason::EndTurn),
    );

    orchestrate(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);
    end_with(&mut fixture, &child, "Which one?");

    let asked = fixture
        .app
        .session(&child)
        .unwrap()
        .question
        .clone()
        .unwrap();
    assert_eq!(
        fixture.app.session(&child).unwrap().wait_reported,
        Some(crate::WaitMark::Question(asked))
    );
}

#[test]
fn a_child_in_its_turn_is_not_reported() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    fixture
        .app
        .change_session(parent.clone(), |archived| {
            archived.archived = Some(crate::Archived { at: 1 });
        })
        .unwrap();
    fixture
        .app
        .change_session(parent.clone(), |restored| restored.archived = None)
        .unwrap();

    // Running at its gate.
    fixture.app.report_waiting_children(&parent).unwrap();
    assert!(prompts(&fixture.backend, &parent).is_empty());

    // Waiting for a permission answer in its turn.
    send(&mut fixture, &child, read_request());
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    fixture.app.report_waiting_children(&parent).unwrap();

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 0);
}

/// The app loaded again from `store`, after it reported the waiting children as the UI does
/// at start.
fn started(store: &MemoryStore) -> Fixture {
    let mut restarted = fixture_with(store.clone());
    restarted.app.report_waiting_at_start().unwrap();
    restarted
}

#[test]
fn wait_survives_restart_once() {
    // Queued while the orchestrator was busy: delivered on load, once.
    let Family {
        dir: _queued_dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    busy(&mut fixture, &parent);
    ended_at_checkpoint(&mut fixture, &child);
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
    assert_eq!(waits_recorded(&fixture.store, &parent), 0);

    let restarted = started(&fixture.store);
    drop(fixture);

    assert_eq!(waits_recorded(&restarted.store, &parent), 1);
    assert_eq!(status(&restarted.app, &parent), Status::Running);
    let again = started(&restarted.store);
    drop(restarted);
    assert_eq!(waits_recorded(&again.store, &parent), 1);
    assert!(queued_waits(&again.app, &parent).is_empty());

    // Delivered at once: not delivered again on load.
    let Family {
        dir: _delivered_dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);
    assert_eq!(waits_recorded(&fixture.store, &parent), 1);

    let restarted = started(&fixture.store);

    assert_eq!(waits_recorded(&restarted.store, &parent), 1);
    assert!(queued_waits(&restarted.app, &parent).is_empty());
    assert_eq!(status(&restarted.app, &parent), Status::Interrupted);

    // Waiting from an older version, which held the wait for the user: delivered on load.
    let Family {
        dir: _older_dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture
        .app
        .change_session(parent.clone(), |older| older.orchestrator = false)
        .unwrap();
    ended_at_checkpoint(&mut fixture, &child);
    fixture
        .app
        .change_session(parent.clone(), |older| older.orchestrator = true)
        .unwrap();
    assert_eq!(waits_recorded(&fixture.store, &parent), 0);

    let restarted = started(&fixture.store);

    assert_eq!(waits_recorded(&restarted.store, &parent), 1);
    assert_eq!(status(&restarted.app, &parent), Status::Running);
}

#[test]
fn a_child_interrupted_at_its_gate_is_not_reported_on_load() {
    let Family {
        dir: _dir,
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);

    let restarted = started(&fixture.store);

    assert_eq!(status(&restarted.app, &child), Status::Interrupted);
    assert_eq!(waits_recorded(&restarted.store, &parent), 0);
    assert!(queued_waits(&restarted.app, &parent).is_empty());
}

#[test]
fn dismissing_a_stopped_childs_question_sends_what_it_holds() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);
    fixture
        .app
        .deliver(&child, Generated::FromOrchestrator("Also add tests".into()))
        .unwrap();
    fixture.app.select_session(child.clone()).unwrap();
    fixture.app.cancel().unwrap();
    send(
        &mut fixture,
        &child,
        Update::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(deferred(&fixture.app, &child), 1);

    fixture.app.cancel_question(child.clone()).unwrap();

    assert_eq!(
        prompts(&fixture.backend, &child),
        ["Add the endpoint", "Also add tests"]
    );
    assert_eq!(deferred(&fixture.app, &child), 0);
}

#[test]
fn a_child_whose_report_fails_at_start_keeps_waiting_and_the_others_go_on() {
    let Family {
        dir: _dir,
        mut fixture,
        project,
        parent,
        child,
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    busy(&mut fixture, &parent);
    ended_at_checkpoint(&mut fixture, &child);
    ended_at_checkpoint(&mut fixture, &web);
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    assert_eq!(waits_recorded(&restarted.store, &parent), 0);
    restarted.store.set_corrupt_updates_of(child.clone());

    let reported = restarted.app.report_waiting_at_start();

    assert!(
        matches!(
            reported,
            Err(AppError::Store(crate::store::StoreError::Corrupt { .. }))
        ),
        "{reported:?}"
    );
    assert_eq!(waits_recorded(&restarted.store, &parent), 1);
    let sent = restarted.store.load_updates(parent.clone()).unwrap();
    assert!(sent.iter().any(
        |update| matches!(update, Update::ChildResult { child: waiting, .. } if waiting == &web)
    ));
    assert_eq!(status(&restarted.app, &child), Status::WaitingForYou);
    assert_eq!(restarted.app.session(&child).unwrap().wait_reported, None);
    assert!(queued_waits(&restarted.app, &parent).is_empty());
}

/// The node of `session` in the tree, at any depth.
fn tree_node(app: &App, session: &SessionId) -> crate::SessionNode {
    fn find(nodes: &[crate::SessionNode], session: &SessionId) -> Option<crate::SessionNode> {
        nodes.iter().find_map(|node| {
            if &node.id == session {
                Some(node.clone())
            } else {
                find(&node.children, session)
            }
        })
    }
    app.tree()
        .projects
        .iter()
        .find_map(|project| find(&project.sessions, session))
        .unwrap()
}

#[test]
fn delegated_only_with_live_orchestrator() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    assert!(!fixture.app.waits_for_orchestrator(&SessionId::new()));
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    // At its gate in its turn, the child does not wait yet.
    assert!(!fixture.app.waits_for_orchestrator(&child));

    end_with(&mut fixture, &child, "Task 4 is done.");
    assert!(fixture.app.waits_for_orchestrator(&child));
    assert!(!fixture.app.waits_for_orchestrator(&parent));

    fixture.app.archive(parent.clone()).unwrap();
    assert!(!fixture.app.waits_for_orchestrator(&child));
}

#[test]
fn a_child_asking_a_permission_at_its_gate_waits_for_the_user() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);

    send(&mut fixture, &child, read_request());

    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
    assert!(fixture.app.gate_open(&child).is_some());
    assert!(!fixture.app.waits_for_orchestrator(&child));
    let node = tree_node(&fixture.app, &child);
    assert!(!node.delegated);
    assert!(node.attention);
}

#[test]
fn a_child_with_a_question_waits_for_its_orchestrator_and_one_without_a_wait_does_not() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    ask_merge_or_pr(&mut fixture, &child);
    end_with(&mut fixture, &child, "Merge or PR?");
    assert!(fixture.app.gate_open(&child).is_none());
    assert!(fixture.app.waits_for_orchestrator(&child));

    let Family {
        mut fixture, child, ..
    } = family();
    fixture
        .app
        .change_session(child.clone(), |waiting| {
            waiting.status = Status::WaitingForYou;
        })
        .unwrap();
    assert!(!fixture.app.waits_for_orchestrator(&child));
}

#[test]
fn delegated_child_shows_in_the_tree_and_its_view() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    end_with(&mut fixture, &child, "Task 4 is done.");

    let node = tree_node(&fixture.app, &child);
    assert!(node.delegated);
    assert!(!node.attention);
    assert!(!node.step.as_ref().unwrap().gate_waiting);
    assert!(!tree_node(&fixture.app, &parent).attention);
    fixture.app.select_session(parent.clone()).unwrap();
    let view = fixture.app.session_view().unwrap();
    assert!(!view.delegated);
    assert!(!view.attention);
    assert!(view.children[0].delegated);
    fixture.app.select_session(child.clone()).unwrap();
    assert!(fixture.app.session_view().unwrap().delegated);

    // A parent that is no longer an orchestrator hands the gate back to the user.
    fixture
        .app
        .change_session(parent.clone(), |plain| plain.orchestrator = false)
        .unwrap();
    let node = tree_node(&fixture.app, &child);
    assert!(!node.delegated);
    assert!(node.attention);
    assert!(node.step.as_ref().unwrap().gate_waiting);
    assert!(tree_node(&fixture.app, &parent).attention);
    assert!(!fixture.app.session_view().unwrap().delegated);
}

#[test]
fn next_gate_skips_delegated_children() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    at_task_four(&mut fixture, &child);
    reach_checkpoint(&mut fixture, &child);
    end_with(&mut fixture, &child, "Task 4 is done.");

    assert_eq!(fixture.app.next_gate(), None);

    fixture
        .app
        .change_session(parent, |plain| plain.orchestrator = false)
        .unwrap();
    assert_eq!(fixture.app.next_gate(), Some(child));
}

/// A request `id` titled `title` to read a file, offering `Allow` and `Reject`.
fn asking(id: u64, title: &str) -> Update {
    Update::PermissionRequested {
        id: PermissionId(id),
        tool_call_id: ToolCallId::new(format!("t-{id}")),
        title: title.into(),
        kind: ToolKind::Read,
        options: asked_options(),
        name: None,
        locations: Vec::new(),
        partitur_tool: false,
    }
}

/// The options [`asking`] offers.
fn asked_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: PermissionOptionId::new("allow"),
            name: "Allow".into(),
            kind: PermissionKind::AllowOnce,
        },
        PermissionOption {
            id: PermissionOptionId::new("reject"),
            name: "Reject".into(),
            kind: PermissionKind::RejectOnce,
        },
    ]
}

/// The waiting requests of `parent`'s children as `(child, permission, title)`.
fn child_asks(app: &App, parent: &SessionId) -> Vec<(SessionId, u64, String)> {
    app.child_permissions(parent)
        .into_iter()
        .map(|asked| (asked.child, asked.permission.0, asked.title))
        .collect()
}

#[test]
fn child_permissions_lists_waiting_requests_of_children() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    assert!(fixture.app.child_permissions(&parent).is_empty());

    send(&mut fixture, &child, asking(1, "Read src/x.rs"));

    assert_eq!(
        fixture.app.child_permissions(&parent),
        vec![crate::ChildPermission {
            child: child.clone(),
            repository: "api".into(),
            permission: PermissionId(1),
            title: "Read src/x.rs".into(),
            options: asked_options(),
        }]
    );
    // Only the requests of its own children.
    assert!(fixture.app.child_permissions(&child).is_empty());
    assert!(fixture.app.child_permissions(&SessionId::new()).is_empty());
    // A child asking still marks its orchestrator.
    assert!(!tree_node(&fixture.app, &child).delegated);
    assert!(tree_node(&fixture.app, &parent).attention);
    // The orchestrator's view carries them.
    fixture.app.select_session(parent.clone()).unwrap();
    assert_eq!(
        fixture.app.session_view().unwrap().child_permissions,
        fixture.app.child_permissions(&parent)
    );

    fixture
        .app
        .answer_permission(
            child.clone(),
            PermissionId(1),
            Some(PermissionOptionId::new("allow")),
        )
        .unwrap();
    assert!(fixture.app.child_permissions(&parent).is_empty());
    assert!(fixture.app.asked.is_empty());
}

#[test]
fn child_permissions_come_oldest_first_and_leave_once_settled() {
    let Family {
        mut fixture,
        project,
        parent,
        child: api,
        ..
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    orchestrate(&mut fixture, &api);
    orchestrate(&mut fixture, &web);
    send(&mut fixture, &web, asking(1, "Read web.rs"));
    send(&mut fixture, &api, asking(1, "Read api.rs"));
    send(&mut fixture, &api, asking(2, "Read lib.rs"));
    let ask = |session: &SessionId, id: u64, title: &str| (session.clone(), id, title.to_owned());
    assert_eq!(
        child_asks(&fixture.app, &parent),
        [
            ask(&web, 1, "Read web.rs"),
            ask(&api, 1, "Read api.rs"),
            ask(&api, 2, "Read lib.rs"),
        ]
    );
    assert_eq!(fixture.app.child_permissions(&parent)[0].repository, "web");

    // Answering one leaves the others, and the same request asked again comes last.
    fixture
        .app
        .answer_permission(api.clone(), PermissionId(1), None)
        .unwrap();
    send(&mut fixture, &api, asking(1, "Write api.rs"));
    assert_eq!(
        child_asks(&fixture.app, &parent),
        [
            ask(&web, 1, "Read web.rs"),
            ask(&api, 2, "Read lib.rs"),
            ask(&api, 1, "Write api.rs"),
        ]
    );

    // A request that lapses with its turn leaves too.
    send(&mut fixture, &web, Update::TurnEnded(StopReason::EndTurn));
    orchestrate(&mut fixture, &web);
    send(&mut fixture, &web, asking(1, "Edit web.rs"));
    assert_eq!(
        child_asks(&fixture.app, &parent),
        [
            ask(&api, 2, "Read lib.rs"),
            ask(&api, 1, "Write api.rs"),
            ask(&web, 1, "Edit web.rs"),
        ]
    );

    // A forgotten child takes its requests along.
    fixture.app.forget(std::slice::from_ref(&api));
    assert_eq!(
        child_asks(&fixture.app, &parent),
        [ask(&web, 1, "Edit web.rs")]
    );
    assert_eq!(fixture.app.asked.len(), 1);
}

#[test]
fn a_request_answered_at_once_is_never_listed() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.set_auto_allow(child.clone(), true).unwrap();
    orchestrate(&mut fixture, &child);

    send(&mut fixture, &child, asking(1, "Read src/x.rs"));

    assert!(fixture.app.child_permissions(&parent).is_empty());
    assert!(fixture.app.asked.is_empty());
}

/// Lets the orchestrator `parent`, woken by a `child waits`, ask the user "Merge or PR?" and
/// end its turn, so the next `child waits` is queued; returns once it waits for the answer.
fn parent_asks_after_child_waits(fixture: &mut Fixture, parent: &SessionId) {
    ask_merge_or_pr(fixture, parent);
    end_with(fixture, parent, "Which one?");
    assert!(fixture.app.question_open(parent));
}

#[test]
fn a_decision_on_a_newer_gate_than_the_one_reported_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_at_checkpoint(&mut fixture, &child);
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        checkpoint_wait(&child)
    );
    parent_asks_after_child_waits(&mut fixture, &parent);
    // The user passes the checkpoint in the child, which stops at the next one later on.
    fixture.app.approve_gate(child.clone()).unwrap();
    call_report(
        &mut fixture,
        &child,
        superpowers(crate::SuperpowersV7Event::StartTask { n: 5, note: None }),
    )
    .unwrap();
    call_report(
        &mut fixture,
        &child,
        superpowers(crate::SuperpowersV7Event::ReachCheckpoint { after_task: 5 }),
    )
    .unwrap();
    fixture
        .app
        .change_session(child.clone(), |later| {
            if let Some(gate) = later
                .workflow
                .as_mut()
                .and_then(|status| status.gate.as_mut())
            {
                gate.since += 60;
            }
        })
        .unwrap();
    end_with(&mut fixture, &child, "Task 5 is done.");
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
    fixture.app.answer_question(parent.clone(), "PR").unwrap();
    let sent = prompts(&fixture.backend, &child).len();

    for decision in [
        Some(crate::GateDecision::Approve),
        Some(crate::GateDecision::Changes),
    ] {
        let answer = call_decide(&mut fixture, &parent, &child, "Go on", decision);

        assert_eq!(
            answer,
            Err(format!(
                "child {child} now waits at its checkpoint; that wait reaches you as a child waits prompt"
            ))
        );
    }
    assert_eq!(prompts(&fixture.backend, &child).len(), sent);
    assert!(fixture.app.gate_open(&child).is_some());
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
    assert_eq!(
        gate_closings(&fixture, &child),
        [(
            crate::workflow::GateCloser::Approve,
            crate::workflow::Decider::User
        )]
    );
}

#[test]
fn an_answer_to_a_newer_question_than_the_one_reported_is_refused() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_with_question(&mut fixture, &child);
    parent_asks_after_child_waits(&mut fixture, &parent);
    // The user answers in the child, which asks again.
    fixture.app.answer_question(child.clone(), "PR").unwrap();
    ask_merge_or_pr(&mut fixture, &child);
    end_with(&mut fixture, &child, "Which branch?");
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
    fixture.app.answer_question(parent.clone(), "PR").unwrap();
    let sent = prompts(&fixture.backend, &child).len();

    let answer = call_decide(&mut fixture, &parent, &child, "main", None);

    assert_eq!(
        answer,
        Err(format!(
            "child {child} now asks another question; it reaches you as a child waits prompt"
        ))
    );
    assert_eq!(prompts(&fixture.backend, &child).len(), sent);
    assert!(fixture.app.question_open(&child));
    assert_eq!(queued_waits(&fixture.app, &parent), [&child]);
}

#[test]
fn a_child_whose_wait_never_reached_its_orchestrator_asks_the_user() {
    let Family {
        dir: _dir,
        mut fixture,
        project,
        parent,
        child,
    } = family();
    let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
    busy(&mut fixture, &parent);
    ended_at_checkpoint(&mut fixture, &child);
    ended_at_checkpoint(&mut fixture, &web);
    // Queued for the busy orchestrator, both wait for it.
    assert!(fixture.app.waits_for_orchestrator(&child));
    assert!(fixture.app.waits_for_orchestrator(&web));
    let mut restarted = fixture_with(fixture.store.clone());
    drop(fixture);
    restarted.store.set_corrupt_updates_of(child.clone());

    let _failed = restarted.app.report_waiting_at_start();

    assert!(!restarted.app.waits_for_orchestrator(&child));
    assert!(restarted.app.waits_for_orchestrator(&web));
    let node = tree_node(&restarted.app, &child);
    assert!(!node.delegated);
    assert!(node.attention);
    assert_eq!(restarted.app.next_gate(), Some(child));
}

#[test]
fn a_follow_up_waits_while_the_childs_question_is_open() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    assert_eq!(
        call_decide(&mut fixture, &parent, &child, "Also add tests", None),
        Ok(format!(
            "prompt queued for child {child}; it goes out when the child is free"
        ))
    );
    ask_merge_or_pr(&mut fixture, &child);

    end_with(&mut fixture, &child, "Which one?");

    assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);
    assert_eq!(deferred(&fixture.app, &child), 1);

    // The orchestrator's answer goes out first, the follow-up once that turn ended.
    call_decide(&mut fixture, &parent, &child, "PR", None).unwrap();
    assert_eq!(last_prompt(&fixture.backend, &child), "PR");
    assert_eq!(deferred(&fixture.app, &child), 1);
    end_with(&mut fixture, &child, "Opened the PR.");
    assert_eq!(last_prompt(&fixture.backend, &child), "Also add tests");
    assert_eq!(deferred(&fixture.app, &child), 0);
}

#[test]
fn dismissing_the_question_a_child_waited_with_sends_its_follow_up() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    fixture
        .app
        .deliver(&child, Generated::FromOrchestrator("Also add tests".into()))
        .unwrap();
    ask_merge_or_pr(&mut fixture, &child);
    end_with(&mut fixture, &child, "Which one?");
    assert_eq!(deferred(&fixture.app, &child), 1);

    fixture.app.cancel_question(child.clone()).unwrap();

    assert_eq!(last_prompt(&fixture.backend, &child), "Also add tests");
    assert_eq!(deferred(&fixture.app, &child), 0);
    assert_eq!(status(&fixture.app, &child), Status::Running);
}

#[test]
fn dismissing_a_question_while_a_permission_waits_sends_nothing() {
    let Family {
        mut fixture, child, ..
    } = family();
    orchestrate(&mut fixture, &child);
    fixture
        .app
        .deliver(&child, Generated::FromOrchestrator("Also add tests".into()))
        .unwrap();
    ask_merge_or_pr(&mut fixture, &child);
    send(&mut fixture, &child, read_request());
    assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);

    fixture.app.cancel_question(child.clone()).unwrap();

    assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);
    assert_eq!(deferred(&fixture.app, &child), 1);
}
