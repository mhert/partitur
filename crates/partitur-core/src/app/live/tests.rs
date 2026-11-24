#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::slice;
use std::sync::Arc;

use tempfile::TempDir;

use crate::app::test_support::{
    Family, Fixture, attached_session, call_propose_title, ended_with_question, family, family_on,
    finish, fixture_with, last_prompt, orchestrate, project_dir, prompts, read_request,
    result_text, send, settle_store, stored_session,
};
use crate::backend::{AgentUpdate, BackendEvent, ChildOutcome, StopReason, TurnOrigin};
use crate::ids::{PermissionId, SessionId};
use crate::project::discover_project;
use crate::record::TranscriptRecord;
use crate::session::Scope;
use crate::store::{Store as _, StoreError};
use crate::testing::{Call, MemoryStore, RecordingReply, apply_store};
use crate::tool_call::{PartiturTool, ToolCall};
use crate::transcript::Entry;
use crate::{AppError, OrchestratorTool};

/// An app over `store` with one stored project session, not opened yet, whose history holds
/// the prompt `hi`.
struct Stored {
    fixture: Fixture,
    session: SessionId,
    _dir: TempDir,
}

fn stored(store: MemoryStore) -> Stored {
    let dir = project_dir(&[]);
    let project = discover_project(dir.path()).unwrap();
    let mut old = stored_session(&project, "old", 1);
    old.instructions_injected = true;
    // With its first prompt known, a prompt reads nothing from the store.
    old.first_prompt = "hi".into();
    store.save_projects(slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id.clone(), slice::from_ref(&old))
        .unwrap();
    store
        .append_record(
            project.id,
            old.id.clone(),
            &TranscriptRecord::UserText("hi".into()),
        )
        .unwrap();
    Stored {
        fixture: fixture_with(store),
        session: old.id,
        _dir: dir,
    }
}

impl Stored {
    fn select(&mut self) {
        self.fixture
            .app
            .select_session(self.session.clone())
            .unwrap();
    }

    fn entries(&self) -> Vec<Entry> {
        self.fixture.app.loaded(&self.session).entries().to_vec()
    }

    /// The prompts the session was sent, oldest first.
    fn prompts(&self) -> Vec<String> {
        prompts(&self.fixture.backend, &self.session)
    }

    /// Answers the backend's load of the session's stored backend session.
    fn attach(&mut self) {
        let request = self
            .fixture
            .backend
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::LoadSession { request, .. } => Some(request),
                _ => None,
            })
            .unwrap();
        self.fixture
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: self.session.clone(),
                config: Vec::new(),
            })
            .unwrap();
    }
}

/// Records the notice `text` in the stored session, as Partitur does.
fn notice(stored: &mut Stored, text: &str) {
    stored
        .fixture
        .app
        .record(&stored.session, &TranscriptRecord::Notice(text.into()))
        .unwrap();
}

/// Lets `caller` call `tool` and returns the reply, answered or not.
fn call(fixture: &mut Fixture, caller: &SessionId, tool: OrchestratorTool) -> Arc<RecordingReply> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: caller.clone(),
            tool: PartiturTool::Orchestrator(tool),
            reply: reply.clone(),
        })
        .unwrap();
    reply
}

#[test]
fn selecting_an_unloaded_session_shows_loading() {
    let mut stored = stored(MemoryStore::new());

    stored.select();

    let view = stored.fixture.app.session_view().unwrap();
    assert!(view.loading);
    assert!(view.entries.is_empty());
    assert!(stored.fixture.app.env_view().is_some());
    assert!(stored.fixture.app.loading(&stored.session));
    settle_store(&mut stored.fixture);
    let view = stored.fixture.app.session_view().unwrap();
    assert!(!view.loading);
    assert_eq!(view.entries, [Entry::user("hi")]);
}

#[test]
fn update_during_load_appears_once() {
    let mut stored = stored(MemoryStore::held());
    let session = stored.session.clone();
    // Queued before the load: the load holds it.
    notice(&mut stored, "before");
    stored.select();
    // Queued behind the load: kept aside and applied after it.
    notice(&mut stored, "during");
    assert!(stored.fixture.app.transcript(&session).is_none());

    stored.fixture.store.release();
    settle_store(&mut stored.fixture);

    assert_eq!(
        stored.entries(),
        [
            Entry::user("hi"),
            Entry::Notice("before".into()),
            Entry::Notice("during".into())
        ]
    );
    notice(&mut stored, "after");
    assert_eq!(stored.entries().len(), 4);
}

#[test]
fn selecting_twice_or_coming_back_while_it_loads_loads_once() {
    let mut stored = stored(MemoryStore::held());
    let project = stored.fixture.store.project_of(&stored.session).unwrap();
    let other = attached_session(&mut stored.fixture, &project, Scope::Project);
    let held = stored.fixture.store.held_commands();

    stored.select();
    stored.select();
    stored.fixture.app.select_session(other).unwrap();
    stored.select();

    assert_eq!(stored.fixture.store.held_commands(), held + 1);
    stored.fixture.store.release();
    settle_store(&mut stored.fixture);
    assert_eq!(stored.entries(), [Entry::user("hi")]);
}

#[test]
fn a_load_a_newer_one_replaced_is_dropped() {
    let mut stored = stored(MemoryStore::held());
    let session = stored.session.clone();
    stored.select();
    stored.fixture.app.unload(&session);
    // Only the newer load sees this.
    notice(&mut stored, "between");
    stored.select();
    notice(&mut stored, "later");

    stored.fixture.store.release();
    settle_store(&mut stored.fixture);

    assert_eq!(
        stored.entries(),
        [
            Entry::user("hi"),
            Entry::Notice("between".into()),
            Entry::Notice("later".into())
        ]
    );
}

#[test]
fn late_read_for_deleted_session_is_ignored() {
    let mut stored = stored(MemoryStore::held());
    let session = stored.session.clone();
    stored.select();

    stored.fixture.app.delete(session.clone(), &[]).unwrap();
    stored.fixture.store.release();
    let errors = apply_store(&mut stored.fixture.app, &stored.fixture.store);

    assert!(errors.is_empty(), "{errors:?}");
    assert!(!stored.fixture.app.holds_anything_for(&session));
    assert!(stored.fixture.store.take_events().is_empty());
    assert_eq!(stored.fixture.store.held_commands(), 0);
}

#[test]
fn prompt_sent_while_loading_is_queued_then_sent() {
    let mut stored = stored(MemoryStore::held());
    stored.select();

    stored.fixture.app.submit_prompt("again").unwrap();
    // The agent answers before the history: the prompt still waits for it.
    stored.attach();

    assert!(stored.prompts().is_empty());
    stored.fixture.store.release();
    settle_store(&mut stored.fixture);
    assert_eq!(stored.prompts(), ["again"]);
    assert_eq!(stored.entries(), [Entry::user("hi"), Entry::user("again")]);
}

#[test]
fn a_prompt_to_an_attached_session_waits_while_it_loads() {
    let mut stored = stored(MemoryStore::new());
    let project = stored.fixture.store.project_of(&stored.session).unwrap();
    let attached = attached_session(&mut stored.fixture, &project, Scope::Project);
    stored.fixture.app.unload(&attached);
    stored.fixture.app.select_session(attached.clone()).unwrap();

    stored.fixture.app.submit_prompt("go").unwrap();

    assert!(prompts(&stored.fixture.backend, &attached).is_empty());
    settle_store(&mut stored.fixture);
    assert_eq!(prompts(&stored.fixture.backend, &attached), ["go"]);
    assert_eq!(
        stored.fixture.app.loaded(&attached).entries(),
        [Entry::user("go")]
    );
}

#[test]
fn a_failed_load_is_reported_and_the_prompt_sent_meanwhile_still_goes_out() {
    let mut stored = stored(MemoryStore::held());
    stored
        .fixture
        .store
        .set_corrupt_updates_of(stored.session.clone());
    stored.select();
    stored.fixture.app.submit_prompt("again").unwrap();
    stored.attach();

    stored.fixture.store.release();
    let errors = apply_store(&mut stored.fixture.app, &stored.fixture.store);

    assert!(matches!(
        errors.as_slice(),
        [AppError::Store(StoreError::Corrupt { .. })]
    ));
    assert_eq!(stored.prompts(), ["again"]);
    assert!(stored.fixture.app.transcript(&stored.session).is_none());
    assert!(!stored.fixture.app.loading(&stored.session));
}

#[test]
fn child_result_waits_for_the_parent_transcript() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.unload(&parent);

    finish(&mut fixture, &child, "All done");

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert!(fixture.app.loading(&parent));
    settle_store(&mut fixture);
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "All done"
        )
    );
}

#[test]
fn a_child_result_waits_for_the_childs_transcript_and_reads_its_last_answer() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    orchestrate(&mut fixture, &child);
    fixture.app.unload(&child);
    send(&mut fixture, &child, AgentUpdate::AgentText("All".into()));
    send(&mut fixture, &child, AgentUpdate::AgentText(" done".into()));

    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert!(prompts(&fixture.backend, &parent).is_empty());
    assert!(fixture.app.loading(&child));
    settle_store(&mut fixture);
    assert_eq!(
        last_prompt(&fixture.backend, &parent),
        result_text(
            &child,
            TurnOrigin::Orchestrator,
            &ChildOutcome::Ended(StopReason::EndTurn),
            "All done"
        )
    );
}

#[test]
fn what_waits_runs_after_the_loads_own_writes() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    let project = fixture.store.project_of(&parent).unwrap();
    // A permission request a quit left unanswered.
    fixture
        .store
        .append_record(project.clone(), parent.clone(), &read_request().into())
        .unwrap();
    fixture.app.unload(&parent);

    finish(&mut fixture, &child, "All done");
    settle_store(&mut fixture);

    let records = fixture.store.load_records(project, parent).unwrap();
    let lapsed = records.iter().position(|record| {
        *record
            == TranscriptRecord::PermissionAnswered {
                id: PermissionId(1),
                option: None,
            }
    });
    let result = records
        .iter()
        .position(|record| matches!(record, TranscriptRecord::ChildResult { .. }));
    assert!(lapsed.unwrap() < result.unwrap(), "{records:?}");
}

#[test]
fn get_child_result_waits_for_the_childs_transcript() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "All done");
    fixture.app.unload(&child);

    let reply = call(
        &mut fixture,
        &parent,
        OrchestratorTool::GetChildResult {
            child: child.clone(),
        },
    );

    assert!(reply.answers().is_empty());
    settle_store(&mut fixture);
    assert_eq!(
        reply.answers(),
        [Ok(format!("child {child} · done\n\nAll done"))]
    );
}

#[test]
fn list_children_waits_for_a_child_with_a_question_open() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    ended_with_question(&mut fixture, &child);
    let loaded = call(&mut fixture, &parent, OrchestratorTool::ListChildren).answers();
    fixture.app.unload(&child);

    let reply = call(&mut fixture, &parent, OrchestratorTool::ListChildren);

    assert!(reply.answers().is_empty());
    settle_store(&mut fixture);
    assert_eq!(reply.answers(), loaded);
}

#[test]
fn a_title_proposal_waits_for_the_parents_transcript() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.unload(&parent);

    let answer = call_propose_title(&mut fixture, &child, "Endpoint");

    assert!(answer.is_ok());
    assert!(fixture.app.transcript(&parent).is_some());
    assert!(
        fixture
            .app
            .loaded(&parent)
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::TitleProposal(_)))
    );
}

#[test]
fn a_call_waiting_for_a_deleted_sessions_transcript_is_answered() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family_on(MemoryStore::held());
    finish(&mut fixture, &child, "All done");
    fixture.app.unload(&child);
    let reply = call(
        &mut fixture,
        &parent,
        OrchestratorTool::GetChildResult {
            child: child.clone(),
        },
    );

    fixture.app.delete(parent, &[]).unwrap();

    assert_eq!(reply.answers(), [Err("the session was deleted".to_owned())]);
    fixture.store.release();
    settle_store(&mut fixture);
    assert!(!fixture.app.holds_anything_for(&child));
}

#[test]
fn a_failed_load_answers_the_calls_that_waited_with_the_failure() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    finish(&mut fixture, &child, "All done");
    fixture.app.unload(&child);
    fixture.store.set_corrupt_updates_of(child.clone());
    let expected = fixture
        .store
        .load_records(fixture.store.project_of(&child).unwrap(), child.clone())
        .unwrap_err()
        .to_string();
    let reply = call(
        &mut fixture,
        &parent,
        OrchestratorTool::GetChildResult {
            child: child.clone(),
        },
    );
    // A result that waits for the same load is dropped with it.
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    let sent = prompts(&fixture.backend, &parent).len();

    let errors = apply_store(&mut fixture.app, &fixture.store);

    assert!(matches!(
        errors.as_slice(),
        [AppError::Store(StoreError::Corrupt { .. })]
    ));
    assert_eq!(reply.answers(), [Err(expected)]);
    assert_eq!(prompts(&fixture.backend, &parent).len(), sent);
}

#[test]
fn a_prompt_sent_while_loading_waits_for_the_agent_when_the_history_comes_first() {
    let mut stored = stored(MemoryStore::held());
    stored.select();
    stored.fixture.app.submit_prompt("again").unwrap();

    stored.fixture.store.release();
    settle_store(&mut stored.fixture);

    assert!(stored.prompts().is_empty());
    stored.attach();
    assert_eq!(stored.prompts(), ["again"]);
    assert_eq!(stored.entries(), [Entry::user("hi"), Entry::user("again")]);
}

#[test]
fn list_children_answers_at_once_when_no_child_has_a_question_open() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    fixture.app.unload(&child);

    let reply = call(&mut fixture, &parent, OrchestratorTool::ListChildren);

    assert_eq!(reply.answers().len(), 1);
    assert!(!fixture.app.loading(&child));
    assert!(fixture.store.take_events().is_empty());
}

#[test]
fn a_session_without_a_parent_loads_nothing_when_its_turn_ends() {
    let Family {
        mut fixture,
        project,
        ..
    } = family();
    let plain = attached_session(
        &mut fixture,
        &project,
        Scope::Repository(crate::app::test_support::repository("web")),
    );
    fixture.app.unload(&plain);

    send(
        &mut fixture,
        &plain,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert!(!fixture.app.loading(&plain));
    assert!(fixture.store.take_events().is_empty());
}
