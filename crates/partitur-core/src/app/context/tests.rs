#![allow(clippy::disallowed_methods)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;

use tempfile::TempDir;

use crate::app::test_support::{
    Fixture, answer_files, fixture, project_dir, prompts, saved, select,
};
use crate::backend::{BackendEvent, PromptContent};
use crate::files::{FilesEvent, FilesQuery};
use crate::ids::{BackendSessionId, ProjectId, RepositoryId, SessionId};
use crate::instructions::InstructionScope;
use crate::session::Scope;
use crate::testing::{Call, apply_files};

/// A project with the repository `api`, whose root `AGENTS.md` holds `rules`, and a session
/// in `api` whose backend session was created; the read its attaching asked for has not been
/// answered.
struct Attached {
    fixture: Fixture,
    project: ProjectId,
    session: SessionId,
    dir: TempDir,
}

fn attached(rules: &str) -> Attached {
    let dir = project_dir(&["api"]);
    fs::write(dir.path().join("AGENTS.md"), rules).unwrap();
    let mut fixture = fixture();
    let project = fixture.open(dir.path());
    let api = Scope::Repository(RepositoryId::try_from("api").unwrap());
    let session = fixture.app.new_session(project.clone(), api).unwrap();
    let (request, _, _) = crate::app::test_support::new_session_request(&fixture.backend);
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        })
        .unwrap();
    select(&mut fixture.app, &fixture.store, session.clone());
    Attached {
        fixture,
        project,
        session,
        dir,
    }
}

/// The paths of the instruction files the Env tab shows.
fn shown_files(fixture: &Fixture) -> Vec<std::path::PathBuf> {
    fixture
        .app
        .env_view()
        .unwrap()
        .instruction_files
        .into_iter()
        .map(|file| file.path)
        .collect()
}

#[test]
fn attaching_reads_the_sessions_files() {
    let mut attached = attached("rules");

    let queries = attached.fixture.files.queries();
    assert_eq!(
        queries[queries.len() - 2..],
        [
            FilesQuery::Read(attached.dir.path().join("AGENTS.md")),
            FilesQuery::Read(attached.dir.path().join("api/AGENTS.md")),
        ]
    );
    assert_eq!(attached.fixture.files.waiting(), 1);
    assert!(shown_files(&attached.fixture).is_empty());

    answer_files(&mut attached.fixture);

    assert_eq!(
        shown_files(&attached.fixture),
        [attached.dir.path().join("AGENTS.md")]
    );
    let view = attached.fixture.app.env_view().unwrap();
    assert_eq!(view.instruction_files[0].scope, InstructionScope::Global);
}

#[test]
fn env_view_reads_no_files() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);
    let queries = attached.fixture.files.queries().len();

    for _ in 0..10 {
        attached.fixture.app.env_view().unwrap();
    }

    assert_eq!(attached.fixture.files.queries().len(), queries);
    assert_eq!(attached.fixture.files.waiting(), 0);
}

#[test]
fn showing_env_tab_refreshes_instructions() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);
    fs::write(attached.dir.path().join("api/AGENTS.md"), "api rules").unwrap();
    assert_eq!(shown_files(&attached.fixture).len(), 1);

    attached.fixture.app.set_env_open(true);

    assert_eq!(shown_files(&attached.fixture).len(), 1);
    answer_files(&mut attached.fixture);
    assert_eq!(
        shown_files(&attached.fixture),
        [
            attached.dir.path().join("AGENTS.md"),
            attached.dir.path().join("api/AGENTS.md")
        ]
    );
}

#[test]
fn closing_the_env_tab_or_showing_it_without_a_session_reads_nothing() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);

    attached.fixture.app.set_env_open(false);
    assert_eq!(attached.fixture.files.waiting(), 0);

    let mut empty = fixture();
    empty.app.set_env_open(true);
    assert_eq!(empty.files.waiting(), 0);
}

#[test]
fn an_env_tab_read_saves_nothing_when_no_prompt_waits() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);
    let saves = attached.fixture.store.session_saves(&attached.project);

    attached.fixture.app.set_env_open(true);
    answer_files(&mut attached.fixture);

    assert_eq!(
        attached.fixture.store.session_saves(&attached.project),
        saves
    );
}

#[test]
fn first_prompt_waits_for_context() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);

    attached.fixture.app.submit_prompt("Fix it").unwrap();

    assert!(prompts(&attached.fixture.backend, &attached.session).is_empty());
    assert_eq!(attached.fixture.files.waiting(), 1);
    let view = attached.fixture.app.session_view().unwrap();
    assert!(view.running());
    answer_files(&mut attached.fixture);
    let sent = prompts(&attached.fixture.backend, &attached.session);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("\nrules\n</instructions>\n\nFix it"),
        "{sent:?}"
    );
    let session = saved(
        &attached.fixture.store,
        &attached.project,
        &attached.session,
    );
    assert!(session.instructions_injected);
}

#[test]
fn the_first_prompt_takes_the_files_as_they_are_when_it_is_sent() {
    let mut attached = attached("old rules");
    answer_files(&mut attached.fixture);
    fs::write(attached.dir.path().join("AGENTS.md"), "new rules").unwrap();

    attached.fixture.app.submit_prompt("Fix it").unwrap();
    answer_files(&mut attached.fixture);

    let sent = prompts(&attached.fixture.backend, &attached.session);
    assert!(sent[0].contains("new rules"), "{sent:?}");
    assert!(!sent[0].contains("old rules"), "{sent:?}");
}

#[test]
fn a_later_prompt_waits_for_nothing() {
    let mut attached = attached("rules");
    attached.fixture.app.submit_prompt("one").unwrap();
    answer_files(&mut attached.fixture);
    crate::app::test_support::send(
        &mut attached.fixture,
        &attached.session.clone(),
        crate::backend::AgentUpdate::TurnEnded(crate::backend::StopReason::EndTurn),
    );

    attached.fixture.app.submit_prompt("two").unwrap();

    assert_eq!(attached.fixture.files.waiting(), 0);
    assert_eq!(
        attached.fixture.backend.calls().last(),
        Some(&Call::Prompt {
            session: attached.session.clone(),
            content: vec![PromptContent::Text("two".into())],
        })
    );
}

#[test]
fn a_prompt_queued_before_the_session_attaches_is_read_for_once() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.open(dir.path());
    let session = fixture.app.new_session(project, Scope::Project).unwrap();
    let (request, _, _) = crate::app::test_support::new_session_request(&fixture.backend);
    fixture.app.submit_prompt("go").unwrap();

    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        })
        .unwrap();

    // The read the prompt asked for is on its way, so attaching asks for no other.
    assert_eq!(fixture.files.waiting(), 1);
    assert!(prompts(&fixture.backend, &session).is_empty());
    answer_files(&mut fixture);
    assert_eq!(prompts(&fixture.backend, &session), ["go"]);
}

#[test]
fn only_the_latest_read_is_taken() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);
    attached.fixture.app.set_env_open(true);
    attached.fixture.app.submit_prompt("Fix it").unwrap();
    let events = attached.fixture.files.answer();
    assert_eq!(events.len(), 2);
    let mut events = events.into_iter();

    // The Env tab's read was superseded by the prompt's.
    let superseded = events.next().unwrap();
    assert_eq!(
        attached.fixture.app.apply_files_event(superseded).unwrap(),
        None
    );
    assert!(prompts(&attached.fixture.backend, &attached.session).is_empty());

    attached
        .fixture
        .app
        .apply_files_event(events.next().unwrap())
        .unwrap();
    assert_eq!(
        prompts(&attached.fixture.backend, &attached.session).len(),
        1
    );
}

#[test]
fn a_read_for_a_deleted_session_is_dropped() {
    let mut attached = attached("rules");
    attached
        .fixture
        .app
        .delete(attached.session.clone(), &[])
        .unwrap();

    let events = attached.fixture.files.answer();
    assert_eq!(events.len(), 1);
    for event in events {
        assert_eq!(attached.fixture.app.apply_files_event(event).unwrap(), None);
    }
    let errors = apply_files(&mut attached.fixture.app, &attached.fixture.files);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_prompt_that_cannot_be_sent_when_the_read_arrives_is_reported() {
    let mut attached = attached("rules");
    attached.fixture.app.submit_prompt("Fix it").unwrap();
    attached.fixture.backend.fail_next_call();

    let errors = apply_files(&mut attached.fixture.app, &attached.fixture.files);

    assert!(
        matches!(errors[..], [crate::AppError::Backend(_)]),
        "{errors:?}"
    );
}

#[test]
fn an_answer_to_no_read_is_dropped() {
    let mut attached = attached("rules");
    answer_files(&mut attached.fixture);

    let stray = FilesEvent::Answered {
        request: crate::files::FilesRequestId(999),
        answers: Vec::new(),
    };

    assert_eq!(attached.fixture.app.apply_files_event(stray).unwrap(), None);
}

#[test]
fn the_env_view_tells_that_the_instructions_were_not_read_yet() {
    let mut attached = attached("rules");
    assert!(!attached.fixture.app.env_view().unwrap().instructions_read);

    answer_files(&mut attached.fixture);

    assert!(attached.fixture.app.env_view().unwrap().instructions_read);
    // A later read on its way keeps what the first one found.
    attached.fixture.app.set_env_open(true);
    let view = attached.fixture.app.env_view().unwrap();
    assert!(view.instructions_read);
    assert_eq!(view.instruction_files.len(), 1);
    answer_files(&mut attached.fixture);
}
