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
use crate::app::{App, AppError};
use crate::project::Repository;

use crate::backend::{
    BackendEvent, ChildOutcome, OrchestratorTool, PromptContent, ProposedChild, StopReason,
    ToolKind, TurnOrigin, Update,
};
use crate::ids::{
    BackendSessionId, PermissionId, PermissionOptionId, ProjectId, RepositoryId, SessionId,
};
use crate::orchestrator::PROPOSAL_RECORDED;
use crate::session::{Scope, Status, TitleSource, Workspace};
use crate::store::Store as _;
use crate::testing::{Call, MemoryStore, RecordingBackend, RecordingReply};
use crate::tool_call::{SessionTool, ToolCall, ToolRole};
use crate::transcript::Entry;
use crate::workflow::{RECORDED, bmad_v6_12};

use crate::app::test_support::{
    CHECKPOINT, Family, Fixture, api_proposal, ask_merge_or_pr, at_task_four, attached_child,
    attached_session, call_decide, call_propose, call_propose_title, call_report,
    call_send_to_child, call_session_tool, call_tool, checkpoint_wait, deferred, end_with,
    ended_at_checkpoint, ended_with_question, family, finish, fixture_with, last_prompt,
    orchestrate, project_dir, prompts, proposal_id, proposal_text, proposed, queued_waits,
    reach_checkpoint, read_request, repository, result_text, send, status, superpowers,
    turn_at_build_plan, two_children, unattached_child,
};

#[test]
fn a_proposal_is_recorded_and_answered_at_once() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    fixture.app.select_session(parent.clone()).unwrap();
    fixture.app.submit_prompt("Plan it").unwrap();
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
    fixture.app.submit_prompt("Plan it").unwrap();
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
    fixture.app.submit_prompt("Plan it").unwrap();

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
    fixture.app.submit_prompt("Plan it").unwrap();
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
    restarted.app.submit_prompt("Go on").unwrap();

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

    fixture.app.submit_prompt("Hello").unwrap();

    assert_eq!(prompts(&fixture.backend, &session), ["Hello"]);
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
