#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::PathBuf;

use crate::app::test_support::{
    Family, Fixture, answer_files, family, read_request, select, send, status,
};
use crate::backend::{AgentUpdate, PermissionKind, PermissionOption, StopReason, ToolKind};
use crate::files::{FilesError, FilesEvent, FilesQuery};
use crate::ids::{PermissionId, PermissionOptionId, SessionId};
use crate::orchestrator::denial_notice;
use crate::session::{Status, Workspace};
use crate::testing::Call;
use crate::transcript::Entry;

/// A request of `kind` about the files `locations`.
fn request_at(kind: ToolKind, locations: &[PathBuf]) -> AgentUpdate {
    let AgentUpdate::PermissionRequested {
        id,
        tool_call_id,
        options,
        name,
        ..
    } = read_request()
    else {
        unreachable!("read_request is a permission request")
    };
    AgentUpdate::PermissionRequested {
        id,
        tool_call_id,
        title: "Change it".into(),
        kind,
        options,
        name,
        locations: locations.to_vec(),
        partitur_tool: false,
    }
}

/// An orchestrator mid-turn whose child works in the worktree `api-wt` of the project
/// folder, which holds `api-wt/src/main.rs`.
fn mid_turn() -> (Family, PathBuf) {
    let mut family = family();
    let worktree = family.dir.path().join("api-wt");
    fs::create_dir_all(worktree.join("src")).unwrap();
    fs::write(worktree.join("src/main.rs"), "x").unwrap();
    let child = family.child.clone();
    family
        .fixture
        .app
        .change_session(child, |session| {
            session.workspace = Workspace::Worktree {
                branch: "feat/x".into(),
                base: "main".into(),
                fetched: false,
                path: worktree.clone(),
            };
        })
        .unwrap();
    let fixture = &mut family.fixture;
    select(&mut fixture.app, &fixture.store, family.parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
    answer_files(fixture);
    (family, worktree)
}

fn cwd(fixture: &Fixture, session: &SessionId) -> PathBuf {
    fixture.app.session(session).unwrap().cwd.clone()
}

fn responses(fixture: &Fixture) -> Vec<Call> {
    fixture
        .backend
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::RespondPermission { .. }))
        .collect()
}

fn allowed(session: &SessionId) -> Call {
    Call::RespondPermission {
        session: session.clone(),
        permission: PermissionId(1),
        option: Some(PermissionOptionId::new("allow")),
    }
}

fn cancelled(session: &SessionId) -> Call {
    Call::RespondPermission {
        session: session.clone(),
        permission: PermissionId(1),
        option: None,
    }
}

fn last_entry(fixture: &Fixture, session: &SessionId) -> Option<Entry> {
    fixture
        .app
        .transcript(session)
        .and_then(|transcript| transcript.entries().last().cloned())
}

#[test]
fn orchestrator_edit_outside_is_denied_after_resolution() {
    let (mut family, _) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    let main = cwd(fixture, &parent).join("api/src/main.rs");

    send(
        fixture,
        &parent,
        request_at(ToolKind::Edit, std::slice::from_ref(&main)),
    );

    // The paths and BMAD's configuration are asked about in one request.
    assert_eq!(fixture.files.waiting(), 1);
    assert!(matches!(
        &fixture.files.queries()[..],
        [.., FilesQuery::Resolve(paths), FilesQuery::Read(config)]
            if paths.contains(&main) && config.ends_with("_bmad/config.toml")
    ));
    assert!(responses(fixture).is_empty());
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    answer_files(fixture);

    assert_eq!(responses(fixture), [cancelled(&parent)]);
    assert_eq!(
        last_entry(fixture, &parent),
        Some(Entry::Notice(denial_notice("edit", "Change it")))
    );
}

#[test]
fn permission_waits_for_resolution() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    send(
        fixture,
        &parent,
        request_at(ToolKind::Read, &[worktree.join("src/main.rs")]),
    );

    // Nothing grants the request while its paths are being resolved.
    fixture
        .app
        .answer_permission(
            parent.clone(),
            PermissionId(1),
            Some(PermissionOptionId::new("allow")),
        )
        .unwrap();
    fixture.app.set_auto_allow(parent.clone(), true).unwrap();
    assert!(responses(fixture).is_empty());
    assert!(fixture.app.has_waiting_permission(&parent));
    let resolving = |fixture: &Fixture| fixture.app.session_view().unwrap().resolving_permissions;
    assert_eq!(resolving(fixture), [PermissionId(1)]);

    answer_files(fixture);
    assert!(resolving(fixture).is_empty());

    assert_eq!(responses(fixture), [allowed(&parent)]);
    assert_eq!(status(&fixture.app, &parent), Status::Running);
}

#[test]
fn an_edit_in_a_workflow_folder_counts_bmads_configuration_as_it_is_at_the_request() {
    let (mut family, _) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    let cwd = cwd(fixture, &parent);
    // Written after the orchestrator's files were read for its first prompt.
    fs::create_dir_all(cwd.join("_bmad")).unwrap();
    fs::write(
        cwd.join("_bmad/config.toml"),
        "[core]\noutput_folder = \"out\"\n",
    )
    .unwrap();

    send(
        fixture,
        &parent,
        request_at(ToolKind::Edit, &[cwd.join("out/prd.md")]),
    );
    answer_files(fixture);

    assert!(responses(fixture).is_empty());
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
}

#[test]
fn a_failed_resolution_denies_an_edit_and_asks_for_a_read() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    let cwd = cwd(fixture, &parent);
    let unavailable = |fixture: &mut Fixture| {
        for event in fixture.files.answer() {
            let FilesEvent::Answered { request, answers } = event;
            let answers = answers
                .iter()
                .map(|_| Err(FilesError::Unavailable("gone".into())))
                .collect();
            fixture
                .app
                .apply_files_event(FilesEvent::Answered { request, answers })
                .unwrap();
        }
    };

    send(
        fixture,
        &parent,
        request_at(ToolKind::Edit, &[cwd.join("docs/superpowers/x.md")]),
    );
    unavailable(fixture);
    assert_eq!(responses(fixture), [cancelled(&parent)]);

    let AgentUpdate::PermissionRequested { options, .. } = read_request() else {
        unreachable!("read_request is a permission request")
    };
    send(
        fixture,
        &parent,
        AgentUpdate::PermissionRequested {
            id: PermissionId(2),
            tool_call_id: crate::ids::ToolCallId::new("t-2"),
            title: "Read main.rs".into(),
            kind: ToolKind::Read,
            options,
            name: None,
            locations: vec![worktree.join("src/main.rs")],
            partitur_tool: false,
        },
    );
    unavailable(fixture);
    assert_eq!(responses(fixture).len(), 1);
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
}

#[test]
fn a_request_cancelled_while_resolving_takes_no_decision() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    send(
        fixture,
        &parent,
        request_at(ToolKind::Read, &[worktree.join("src/main.rs")]),
    );

    fixture
        .app
        .answer_permission(parent.clone(), PermissionId(1), None)
        .unwrap();
    answer_files(fixture);

    assert_eq!(responses(fixture), [cancelled(&parent)]);
    assert!(!fixture.app.has_waiting_permission(&parent));
}

#[test]
fn a_rejection_while_resolving_is_sent() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    let AgentUpdate::PermissionRequested {
        id,
        tool_call_id,
        mut options,
        ..
    } = request_at(ToolKind::Read, &[])
    else {
        unreachable!("request_at is a permission request")
    };
    options.push(PermissionOption {
        id: PermissionOptionId::new("reject"),
        name: "Reject".into(),
        kind: PermissionKind::RejectOnce,
    });
    send(
        fixture,
        &parent,
        AgentUpdate::PermissionRequested {
            id,
            tool_call_id,
            title: "Read main.rs".into(),
            kind: ToolKind::Read,
            options,
            name: None,
            locations: vec![worktree.join("src/main.rs")],
            partitur_tool: false,
        },
    );

    fixture
        .app
        .answer_permission(
            parent.clone(),
            PermissionId(1),
            Some(PermissionOptionId::new("reject")),
        )
        .unwrap();
    answer_files(fixture);

    assert_eq!(
        responses(fixture),
        [Call::RespondPermission {
            session: parent.clone(),
            permission: PermissionId(1),
            option: Some(PermissionOptionId::new("reject")),
        }]
    );
}

#[test]
fn a_request_that_lapsed_with_its_turn_takes_no_decision() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    send(
        fixture,
        &parent,
        request_at(ToolKind::Read, &[worktree.join("src/main.rs")]),
    );

    send(
        fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    answer_files(fixture);

    assert!(responses(fixture).is_empty());
    assert!(!fixture.app.has_waiting_permission(&parent));
    assert_eq!(status(&fixture.app, &parent), Status::Done);
}

#[test]
fn a_resolution_for_a_deleted_session_is_dropped() {
    let (mut family, worktree) = mid_turn();
    let parent = family.parent.clone();
    let fixture = &mut family.fixture;
    send(
        fixture,
        &parent,
        request_at(ToolKind::Read, &[worktree.join("src/main.rs")]),
    );

    fixture.app.delete(parent.clone(), &[]).unwrap();
    assert!(!fixture.app.permissions.holds(&parent));
    answer_files(fixture);

    assert!(responses(fixture).is_empty());
    assert!(!fixture.app.permissions.holds(&parent));
}

#[test]
fn deleting_another_session_keeps_a_request_resolving() {
    let (mut family, worktree) = mid_turn();
    let (parent, project) = (family.parent.clone(), family.project.clone());
    let fixture = &mut family.fixture;
    let other = crate::app::test_support::attached_session(
        fixture,
        &project,
        crate::session::Scope::Repository(crate::app::test_support::repository("web")),
    );
    send(
        fixture,
        &parent,
        request_at(ToolKind::Read, &[worktree.join("src/main.rs")]),
    );

    fixture.app.delete(other, &[]).unwrap();
    answer_files(fixture);

    assert_eq!(responses(fixture), [allowed(&parent)]);
}
