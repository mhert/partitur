#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use crate::app::scheduler::{Generated, final_message};
use crate::app::{App, AppError};
use crate::backend::{AgentUpdate, ChildState, ChildWait};
use crate::transcript::Transcript;

use crate::backend::{
    BackendEvent, ChildOutcome, OrchestratorTool, PermissionKind, PermissionOption, PromptContent,
    StopReason, TitleProposal, ToolKind, ToolStatus, TurnOrigin,
};
use crate::ids::{PermissionId, PermissionOptionId, SessionId, ToolCallId};
use crate::orchestrator::{child_result_prompt, title_proposal_prompt};
use crate::record::TranscriptRecord;
use crate::session::{Scope, Status};
use crate::store::Store as _;
use crate::testing::{Call, MemoryStore};
use crate::tool_call::ToolRole;
use crate::transcript::Entry;
use crate::workflow::bmad_v6_12;

use super::super::tests::load_request;
use crate::app::test_support::{
    CHECKPOINT, Family, Fixture, GATE_FOOTER, api_proposal, ask_merge_or_pr, at_task_four,
    attached_child, attached_session, build_plan_gate, call_decide, call_propose_title,
    call_send_to_child, call_tool, checkpoint_wait, closed, deferred, end_with,
    ended_at_checkpoint, ended_with_question, family, finish, fixture_with, last_prompt,
    merge_or_pr, orchestrate, prompts, proposal_id, proposal_text, queued_waits, reach_checkpoint,
    read_request, repository, result_text, send, status, tree_node, turn_at_build_plan,
    two_children, unattached_child, waiting_on_the_card,
};

#[test]
fn generated_prompts_are_recorded_by_their_kind_and_sent_as_written() {
    let wake = Generated::Wake("children started".into());
    let prompt = Generated::FromOrchestrator("Add the endpoint".into());

    assert_eq!(
        wake.record(),
        TranscriptRecord::PartiturPrompt("children started".into())
    );
    assert_eq!(wake.text(), "children started");
    assert_eq!(
        prompt.record(),
        TranscriptRecord::OrchestratorPrompt("Add the endpoint".into())
    );
    assert_eq!(prompt.text(), "Add the endpoint");
}

#[test]
fn the_final_message_is_the_last_answer_of_the_latest_turn() {
    let answered = Transcript::from_updates(&[
        TranscriptRecord::UserText("go".into()),
        TranscriptRecord::AgentText("first".into()),
        TranscriptRecord::Notice("note".into()),
        TranscriptRecord::AgentText("last".into()),
        TranscriptRecord::TurnEnded(StopReason::EndTurn),
    ]);
    let silent = Transcript::from_updates(&[
        TranscriptRecord::UserText("go".into()),
        TranscriptRecord::AgentText("earlier turn".into()),
        TranscriptRecord::TurnEnded(StopReason::EndTurn),
        TranscriptRecord::OrchestratorPrompt("again".into()),
        TranscriptRecord::TurnEnded(StopReason::EndTurn),
    ]);

    let resumed = Transcript::from_updates(&[
        TranscriptRecord::UserText("go".into()),
        TranscriptRecord::AgentText("earlier turn".into()),
        TranscriptRecord::TurnEnded(StopReason::EndTurn),
        TranscriptRecord::TurnStarted,
        TranscriptRecord::TurnEnded(StopReason::EndTurn),
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
        .submit_prepared(
            &parent,
            "Plan it",
            vec![crate::app::Prepared {
                content: PromptContent::FileLink {
                    path: notes.clone(),
                },
                sent: crate::backend::SentAttachment::File {
                    name: "notes.txt".into(),
                    path: notes.clone(),
                },
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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

    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));
    send(&mut fixture, &child, AgentUpdate::AgentText(" done".into()));
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
        fixture.app.loaded(&parent).entries().last(),
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
    fixture.app.submit_prompt("Also add a test").unwrap();

    send(&mut fixture, &child, AgentUpdate::AgentText("added".into()));
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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

    send(&mut fixture, &child, AgentUpdate::Failed("boom".into()));

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
        AgentUpdate::TurnEnded(StopReason::Cancelled),
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
        AgentUpdate::PermissionRequested {
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
    fixture.app.submit_prompt("Plan it").unwrap();
    for (session, answer) in [(&child, "api done"), (&web, "web done")] {
        orchestrate(&mut fixture, session);
        send(&mut fixture, session, AgentUpdate::AgentText(answer.into()));
        send(
            &mut fixture,
            session,
            AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
    );
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(last_prompt(&fixture.backend, &parent), web_result);
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    restarted.app.submit_prompt("Add the endpoint").unwrap();
    let request = load_request(&restarted.backend);
    restarted
        .app
        .apply_event(BackendEvent::SessionLoaded {
            request,
            session: child.clone(),
            config: Vec::new(),
        })
        .unwrap();
    send(
        &mut restarted,
        &child,
        AgentUpdate::AgentText("done".into()),
    );

    send(
        &mut restarted,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::ToolCall {
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

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
    fixture.app.submit_prompt("Fix the build").unwrap();
    send(&mut fixture, &lone, AgentUpdate::AgentText("fixed".into()));
    let calls = fixture.backend.calls();

    send(
        &mut fixture,
        &lone,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
        AgentUpdate::AgentText("from the store".into()),
    );
    fixture.app.unload(&child);
    assert!(
        !fixture
            .store
            .load_records(child.clone())
            .unwrap()
            .is_empty()
    );

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
    send(&mut fixture, &child, AgentUpdate::Notice("heads up".into()));
    fixture.app.unload(&child);

    orchestrate(&mut fixture, &child);

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.first_prompt, "Add the endpoint");
}

#[test]
fn a_conversation_read_from_the_store_keeps_its_first_prompt_empty() {
    let Family {
        mut fixture, child, ..
    } = family();
    send(
        &mut fixture,
        &child,
        AgentUpdate::AgentText("earlier".into()),
    );
    fixture.app.unload(&child);

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
    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));
    fixture.app.close_project(project).unwrap();

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
    );

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Idle);

    fixture.app.open_project(dir.path()).unwrap();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Status?").unwrap();
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    fixture.app.submit_prompt("Plan it").unwrap();
    finish(&mut fixture, &child, "api done");
    fixture.app.close_project(project).unwrap();

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    fixture.app.submit_prompt("Plan it").unwrap();
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
    );

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 2);

    fixture.app.submit_prompt("Go on").unwrap();
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));

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
    fixture.app.submit_prompt("Plan it").unwrap();
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
    fixture.app.submit_prompt("Plan it").unwrap();
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(status(&fixture.app, &parent), Status::Done);

    finish(&mut fixture, &child, "api done again");

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 3);

    fixture.app.submit_prompt("Go on").unwrap();
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    fixture.app.approve_gate(child.clone()).unwrap();
    assert!(last_prompt(&fixture.backend, &child).ends_with("Plan approved. Build it."));
    send(&mut fixture, &child, AgentUpdate::AgentText("Built".into()));
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    send(&mut fixture, &child, AgentUpdate::TurnStarted);
    send(
        &mut fixture,
        &child,
        AgentUpdate::AgentText("all green".into()),
    );
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

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
    send(&mut fixture, &child, AgentUpdate::TurnStarted);

    let answer = call_send_to_child(&mut fixture, &parent, &child, "Also add tests");

    assert_eq!(
        answer,
        Ok(format!(
            "prompt queued for child {child}; it goes out when the child is free"
        ))
    );
    assert_eq!(prompts(&fixture.backend, &child), ["Add the endpoint"]);

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        prompts(&fixture.backend, &child),
        ["Add the endpoint", "Also add tests"]
    );
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
    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));
    fixture.app.archive(parent.clone()).unwrap();

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
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
    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));
    fixture.app.archive(parent.clone()).unwrap();
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
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
        AgentUpdate::TurnEnded(StopReason::Cancelled),
    );
    fixture.app.close_project(project).unwrap();

    fixture.app.restore(parent.clone()).unwrap();

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert_eq!(deferred(&fixture.app, &parent), 1);
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
    fixture.app.submit_prompt("Plan it").unwrap();
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
    send(&mut fixture, &child, AgentUpdate::AgentText("half".into()));

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
                .load_records(member.clone())
                .unwrap()
                .is_empty()
        );
    }
    let calls = fixture.backend.calls().len();
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
    );
    assert_eq!(fixture.backend.calls().len(), calls);
    assert!(fixture.app.nothing_deferred());
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
    fixture.app.submit_prompt("Again").unwrap();
    finish(&mut fixture, &other_child, "web done");
    waiting_on_the_card(&mut fixture, &parent, two_children());
    fixture.app.submit_prompt("Plan more").unwrap();
    finish(&mut fixture, &child, "api done");
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(deferred(&fixture.app, &other), 1);

    fixture.app.delete(parent.clone(), &[]).unwrap();

    assert_eq!(deferred(&fixture.app, &parent), 0);
    assert!(!fixture.app.awaits_decision(&parent));
    assert_eq!(deferred(&fixture.app, &other), 1);
    assert_eq!(proposal_id(&fixture.app, &other), other_proposal);
    assert_eq!(fixture.app.current_session(), None);
    send(
        &mut fixture,
        &other,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert!(last_prompt(&fixture.backend, &other).contains("web done"));
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

#[test]
fn a_title_proposal_waits_behind_a_child_result() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it").unwrap();
    orchestrate(&mut fixture, &child);
    send(
        &mut fixture,
        &child,
        AgentUpdate::AgentText("api done".into()),
    );
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    let answer = call_propose_title(&mut fixture, &child, "Auth endpoint");

    assert_eq!(answer, Ok("sent to the orchestrator".to_owned()));
    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    let ended = ChildOutcome::Ended(StopReason::EndTurn);
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
    );
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", false)
    );
}

/// Starts a turn of the family's orchestrator, so what reaches it waits.
fn busy(fixture: &mut Fixture, parent: &SessionId) {
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it").unwrap();
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Second", false)
    );
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        fixture.app.loaded(&parent).entries().last(),
        Some(&Entry::TitleProposal(expected))
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        proposal_text(&child, "Auth endpoint", false)
    );
}

/// The step the tree shows for a superpowers child that works on task 4 inline.
const TASK_FOUR: &str = "execute · 4";

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
        fixture.app.loaded(&parent).entries().last(),
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
    fixture.app.unload(&child);

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
    send(&mut fixture, &child, AgentUpdate::AgentText("Half".into()));

    send(&mut fixture, &child, AgentUpdate::Failed("boom".into()));

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

/// How many `child waits` prompts started a turn of `parent`, as its stored history holds.
fn waits_recorded(store: &MemoryStore, parent: &SessionId) -> usize {
    store
        .load_records(parent.clone())
        .unwrap()
        .iter()
        .filter(
            |update| matches!(update, TranscriptRecord::ChildResult { state, .. } if state.waits.is_some()),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::Cancelled),
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
    let sent = restarted.store.load_records(parent.clone()).unwrap();
    assert!(sent.iter().any(
        |update| matches!(update, TranscriptRecord::ChildResult { child: waiting, .. } if waiting == &web)
    ));
    assert_eq!(status(&restarted.app, &child), Status::WaitingForYou);
    assert_eq!(restarted.app.session(&child).unwrap().wait_reported, None);
    assert!(queued_waits(&restarted.app, &parent).is_empty());
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
fn asking(id: u64, title: &str) -> AgentUpdate {
    AgentUpdate::PermissionRequested {
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
    assert!(fixture.app.permissions.asked.is_empty());
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
    send(
        &mut fixture,
        &web,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
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
    assert_eq!(fixture.app.permissions.asked.len(), 1);
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
    assert!(fixture.app.permissions.asked.is_empty());
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
