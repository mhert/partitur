#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::app::{Approval, ChildChoice};
use crate::archive::Archived;
use crate::backend::{
    ApprovedChild, BackendEvent, PromptContent, ProposedChild, StopReason, Update,
};
use crate::ids::{BackendSessionId, ConfigValueId, ProjectId, ProposalId, RepositoryId};
use crate::orchestrator::PROPOSAL_RECORDED;
use crate::peers::{session_started_prompt, sibling_note};
use crate::session::{BranchChoice, BranchMode, Scope, Status, TitleSource};
use crate::settings::GlobalSettings;
use crate::testing::{
    Call, MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude,
};
use crate::tool_call::ToolCall;
use crate::transcript::Entry;
use crate::workflow::{self, superpowers_v7};

struct Fixture {
    app: App,
    backend: RecordingBackend,
}

fn fixture() -> Fixture {
    let (backends, default) = single_claude();
    let backend = RecordingBackend::new();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(MemoryStore::new()),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    Fixture { app, backend }
}

fn project_dir(repositories: &[&str]) -> TempDir {
    let dir = TempDir::new_in(crate::canonical(std::env::temp_dir()).unwrap()).unwrap();
    for name in repositories {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    dir
}

fn attached(fixture: &mut Fixture, project: &ProjectId, scope: Scope, created: u64) -> SessionId {
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
        .change_session(session.clone(), |changed| {
            changed.created_at = created;
            changed.title = format!("s{created}");
        })
        .unwrap();
    session
}

fn api() -> Scope {
    Scope::Repository(RepositoryId::try_from("api").unwrap())
}

fn call(fixture: &mut Fixture, caller: &SessionId, tool: PeerTool) -> Result<String, String> {
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: caller.clone(),
            tool: tool.into(),
            reply: reply.clone(),
        })
        .unwrap();
    let mut answers = reply.answers();
    assert_eq!(answers.len(), 1);
    answers.pop().unwrap()
}

#[test]
fn list_sessions_lists_live_newest_first_then_archived() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let old = attached(&mut fixture, &project, api(), 1);
    let middle = attached(&mut fixture, &project, api(), 2);
    let new = attached(&mut fixture, &project, api(), 3);
    let archived = attached(&mut fixture, &project, api(), 4);
    fixture
        .app
        .change_session(archived.clone(), |changed| {
            changed.archived = Some(Archived { at: 9 });
        })
        .unwrap();

    let answer = call(&mut fixture, &old, PeerTool::ListSessions).unwrap();

    let ids: Vec<String> = answer
        .lines()
        .skip(1)
        .map(|line| line.split(" · ").nth(2).unwrap().to_owned())
        .collect();
    assert_eq!(answer.lines().next(), Some("sessions:"));
    assert_eq!(
        ids,
        [new, middle, old, archived]
            .map(|id| format!("session {id}"))
            .to_vec()
    );
    assert!(answer.lines().nth(3).unwrap().ends_with(" · this session"));
    assert!(answer.lines().last().unwrap().ends_with(" · archived"));
}

#[test]
fn list_sessions_is_offered_to_every_role() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let orchestrator = attached(&mut fixture, &project, Scope::Project, 1);
    assert!(fixture.app.session(&orchestrator).unwrap().orchestrator);
    let child = attached(&mut fixture, &project, api(), 2);
    fixture
        .app
        .change_session(child.clone(), |changed| {
            changed.parent = Some(orchestrator.clone());
        })
        .unwrap();
    let plain = attached(&mut fixture, &project, api(), 3);

    for caller in [&plain, &orchestrator, &child] {
        assert!(call(&mut fixture, caller, PeerTool::ListSessions).is_ok());
    }
}

#[test]
fn list_sessions_covers_only_the_callers_project() {
    let one = project_dir(&["api"]);
    let other = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(one.path()).unwrap();
    let elsewhere = fixture.app.open_project(other.path()).unwrap();
    let mine = attached(&mut fixture, &project, api(), 1);
    let foreign = attached(&mut fixture, &elsewhere, api(), 2);

    let answer = call(&mut fixture, &mine, PeerTool::ListSessions).unwrap();

    assert!(answer.contains(&mine.to_string()));
    assert!(!answer.contains(&foreign.to_string()));
}

#[test]
fn propose_session_is_refused_for_an_orchestrator_and_a_child() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let orchestrator = attached(&mut fixture, &project, Scope::Project, 1);
    let child = attached(&mut fixture, &project, api(), 2);
    fixture
        .app
        .change_session(child.clone(), |changed| {
            changed.parent = Some(orchestrator.clone());
        })
        .unwrap();
    let propose = || PeerTool::ProposeSession {
        repository: None,
        title: "t".into(),
        prompt: "p".into(),
        base: None,
    };

    assert_eq!(
        call(&mut fixture, &orchestrator, propose()),
        Err("an orchestrator proposes children with propose_children".to_owned())
    );
    assert_eq!(
        call(&mut fixture, &child, propose()),
        Err("a child cannot start sessions; ask your orchestrator".to_owned())
    );
}

#[test]
fn list_sessions_shows_the_step_and_the_gate_a_session_waits_at() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let asker = attached(&mut fixture, &project, api(), 1);
    let waiting = attached(&mut fixture, &project, api(), 2);
    let reply = RecordingReply::new();
    fixture
        .app
        .call_tool(ToolCall {
            session: waiting.clone(),
            tool: superpowers_v7::Event::RequestDesignApproval {
                path: "design.md".into(),
            }
            .into(),
            reply: reply.clone(),
        })
        .unwrap();
    let status = fixture
        .app
        .session(&waiting)
        .unwrap()
        .workflow
        .clone()
        .unwrap();
    let step = workflow::step_cell(&status).label;
    let (module, kind) = fixture.app.gate_open(&waiting).unwrap();
    let word = workflow::gate_texts(module, &kind).word;

    let answer = call(&mut fixture, &asker, PeerTool::ListSessions).unwrap();

    let line = answer
        .lines()
        .find(|line| line.contains(&waiting.to_string()))
        .unwrap();
    assert!(
        line.ends_with(&format!(" · {step} · waits at its {word}")),
        "{line}"
    );
}

fn propose_session(repository: Option<&str>, title: &str, prompt: &str) -> PeerTool {
    PeerTool::ProposeSession {
        repository: repository.map(|name| RepositoryId::try_from(name).unwrap()),
        title: title.into(),
        prompt: prompt.into(),
        base: None,
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

/// The id and children of the session's only undecided proposal.
fn proposal(app: &App, session: &SessionId) -> (ProposalId, Vec<ProposedChild>) {
    let found: Vec<_> = app
        .proposals
        .iter()
        .filter(|((proposer, _), _)| proposer == session)
        .map(|((_, id), children)| (id.clone(), children.clone()))
        .collect();
    assert_eq!(found.len(), 1);
    found[0].clone()
}

fn choice(name: &str, prompt: &str, title: &str) -> ChildChoice {
    ChildChoice {
        repository: RepositoryId::try_from(name).unwrap(),
        prompt: prompt.into(),
        title: title.into(),
        auto_allow: false,
        branch: BranchChoice::default(),
    }
}

/// A plain session in `api` that ran a turn in which it proposed a sibling in `api`, with
/// the fixture, the project folder and the proposal's id.
struct Proposer {
    dir: TempDir,
    fixture: Fixture,
    session: SessionId,
    proposal: ProposalId,
}

fn proposer() -> Proposer {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let session = attached(&mut fixture, &project, api(), 1);
    fixture
        .app
        .change_session(session.clone(), |changed| {
            changed.model = Some(ConfigValueId::new("opus"));
            changed.effort = Some(ConfigValueId::new("high"));
        })
        .unwrap();
    fixture.app.select_session(session.clone()).unwrap();
    fixture
        .app
        .submit_prompt("Hand it off", Vec::new())
        .unwrap();
    let answer = call(
        &mut fixture,
        &session,
        propose_session(Some("api"), "Follow-up", "Do the rest"),
    );
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    send(
        &mut fixture,
        &session,
        Update::TurnEnded(StopReason::EndTurn),
    );
    let (proposal, _) = proposal(&fixture.app, &session);
    Proposer {
        dir,
        fixture,
        session,
        proposal,
    }
}

/// Approves the proposer's sibling as proposed and returns the session it started.
fn approve(proposer: &mut Proposer) -> SessionId {
    let approval = proposer
        .fixture
        .app
        .approve_proposal(
            proposer.session.clone(),
            proposer.proposal.clone(),
            vec![choice("api", "Do the rest", "Follow-up")],
        )
        .unwrap();
    let Approval::Started(started) = approval else {
        panic!("the approval waits for git");
    };
    assert_eq!(started.len(), 1);
    started[0].clone()
}

#[test]
fn a_plain_session_proposes_a_sibling() {
    let Proposer {
        fixture, session, ..
    } = proposer();

    let (id, children) = proposal(&fixture.app, &session);
    let expected = vec![ProposedChild {
        repository: RepositoryId::try_from("api").unwrap(),
        prompt: "Do the rest".into(),
        title: "Follow-up".into(),
        title_source: TitleSource::Given,
        base: None,
    }];
    assert_eq!(children, expected);
    let recorded = Entry::Proposal {
        id,
        children: expected,
        decision: None,
    };
    assert!(
        fixture.app.transcripts[&session]
            .entries()
            .contains(&recorded)
    );
    // An undecided sibling holds the proposer as a split holds an orchestrator: it asks you.
    assert_eq!(
        fixture.app.session(&session).unwrap().status,
        Status::WaitingForYou
    );
}

#[test]
fn a_proposed_sibling_keeps_its_base_and_a_cleaned_title() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let caller = attached(&mut fixture, &project, api(), 1);

    let answer = call(
        &mut fixture,
        &caller,
        PeerTool::ProposeSession {
            repository: None,
            title: "Follow\n  up".into(),
            prompt: "Do the rest".into(),
            base: Some("feature".into()),
        },
    );

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let (_, children) = proposal(&fixture.app, &caller);
    assert_eq!(children[0].title, "Follow up");
    assert_eq!(children[0].base.as_deref(), Some("feature"));
}

#[test]
fn a_sibling_without_repository_goes_to_the_callers_repository() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let web = Scope::Repository(RepositoryId::try_from("web").unwrap());
    let caller = attached(&mut fixture, &project, web, 1);

    let answer = call(&mut fixture, &caller, propose_session(None, "", "Do it"));

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let (_, children) = proposal(&fixture.app, &caller);
    assert_eq!(
        children[0].repository,
        RepositoryId::try_from("web").unwrap()
    );
    assert_eq!(children[0].title, "Do it");
    assert_eq!(children[0].title_source, TitleSource::Derived);
}

#[test]
fn a_sibling_without_repository_in_a_folder_repository_goes_there() {
    let dir = project_dir(&[""]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let caller = attached(&mut fixture, &project, Scope::Project, 1);
    assert!(!fixture.app.session(&caller).unwrap().orchestrator);
    let root = fixture.app.project(&project).unwrap().repositories[0]
        .id
        .clone();

    let answer = call(&mut fixture, &caller, propose_session(None, "t", "Do it"));

    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    let (_, children) = proposal(&fixture.app, &caller);
    assert_eq!(children[0].repository, root);
}

#[test]
fn a_sibling_without_repository_in_a_folder_without_one_is_refused() {
    let dir = project_dir(&[]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let caller = attached(&mut fixture, &project, Scope::Project, 1);

    let answer = call(&mut fixture, &caller, propose_session(None, "t", "Do it"));

    assert_eq!(
        answer,
        Err("this project has no repository to start a session in".to_owned())
    );
    assert!(fixture.app.proposals.is_empty());
}

#[test]
fn propose_session_refuses_an_unknown_repository_and_a_blank_prompt() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let caller = attached(&mut fixture, &project, api(), 1);

    assert_eq!(
        call(&mut fixture, &caller, propose_session(Some("db"), "t", "p")),
        Err("unknown repository \"db\"; the repositories are: api, web".to_owned())
    );
    assert_eq!(
        call(
            &mut fixture,
            &caller,
            propose_session(Some("api"), "t", "  ")
        ),
        Err("propose_session needs a prompt".to_owned())
    );
    assert!(fixture.app.proposals.is_empty());
}

#[test]
fn approving_a_sibling_starts_a_top_level_session_with_the_note() {
    let mut proposer = proposer();
    let id = proposer.session.clone();

    let sibling = approve(&mut proposer);

    let app = &proposer.fixture.app;
    let from = app.session(&id).unwrap().clone();
    let started = app.session(&sibling).unwrap();
    assert_eq!(started.parent, None);
    assert!(!started.orchestrator);
    assert_eq!(started.project, from.project);
    assert_eq!(started.scope, api());
    assert_eq!(started.title, "Follow-up");
    assert_eq!(started.backend, from.backend);
    assert_eq!(started.model, Some(ConfigValueId::new("opus")));
    assert_eq!(started.effort, Some(ConfigValueId::new("high")));
    assert!(proposer.dir.path().exists());
    let mut fixture = proposer.fixture;
    // The started session's connection opens.
    let request = fixture
        .backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .find(|(_, session, _)| *session == &sibling)
        .map(|(request, _, _)| request)
        .unwrap();
    fixture
        .app
        .apply_event(BackendEvent::SessionCreated {
            request,
            session: sibling.clone(),
            backend_session: BackendSessionId::new(format!("b-{sibling}")),
            config: Vec::new(),
        })
        .unwrap();
    let note = sibling_note(&from.title, &id);
    assert_eq!(
        note,
        format!(
            "Started by session \"s1\" ({id}). Read it with read_session when you need its context."
        )
    );
    let sent = prompts(&fixture.backend, &sibling);
    assert_eq!(sent, [format!("{note}\n\nDo the rest")]);
    assert!(!sent[0].contains(crate::orchestrator::CHILD_ROLE));
    assert!(!sent[0].contains("Your session is titled"));
}

#[test]
fn approving_a_sibling_wakes_the_proposer_with_session_started() {
    let mut proposer = proposer();

    let sibling = approve(&mut proposer);

    let app = &proposer.fixture.app;
    let started = ApprovedChild {
        session: sibling.clone(),
        repository: RepositoryId::try_from("api").unwrap(),
        prompt: "Do the rest".into(),
        title: "Follow-up".into(),
        auto_allow: false,
        mode: BranchMode::CheckedOut,
        branch: None,
        cwd: app.session(&sibling).unwrap().cwd.clone(),
    };
    let wake = session_started_prompt(&started);
    assert_eq!(
        wake,
        format!(
            "session started:\n- Follow-up · api · session {sibling} · checked-out · in {}\nIt runs on its own and reports nothing back; read it with read_session(session_id).",
            started.cwd.display()
        )
    );
    assert_eq!(
        prompts(&proposer.fixture.backend, &proposer.session),
        ["Hand it off".to_owned(), wake]
    );
    assert!(app.proposals.is_empty());
}

#[test]
fn a_started_session_line_names_the_branch_it_made() {
    let started = ApprovedChild {
        session: SessionId::new(),
        repository: RepositoryId::try_from("api").unwrap(),
        prompt: "p".into(),
        title: "t".into(),
        auto_allow: false,
        mode: BranchMode::Worktree,
        branch: Some("feature".into()),
        cwd: "/work/api".into(),
    };

    let wake = session_started_prompt(&started);

    assert_eq!(
        wake.lines().nth(1),
        Some(
            format!(
                "- t · api · session {} · worktree feature · in /work/api",
                started.session
            )
            .as_str()
        )
    );
}

#[test]
fn rejecting_a_sibling_wakes_the_proposer_with_the_note() {
    let mut proposer = proposer();

    proposer
        .fixture
        .app
        .reject_proposal(
            proposer.session.clone(),
            proposer.proposal.clone(),
            Some("not now".into()),
        )
        .unwrap();

    assert_eq!(
        prompts(&proposer.fixture.backend, &proposer.session),
        [
            "Hand it off".to_owned(),
            "proposal rejected by the user\nnote: not now".to_owned()
        ]
    );
}

#[test]
fn approving_a_sibling_of_an_archived_session_is_refused() {
    let mut proposer = proposer();
    proposer
        .fixture
        .app
        .change_session(proposer.session.clone(), |changed| {
            changed.archived = Some(Archived { at: 9 });
        })
        .unwrap();

    let approval = proposer.fixture.app.approve_proposal(
        proposer.session.clone(),
        proposer.proposal.clone(),
        vec![choice("api", "Do the rest", "Follow-up")],
    );

    assert!(matches!(approval, Err(AppError::Archived(id)) if id == proposer.session));
}

#[test]
fn a_sibling_never_reports_back() {
    let mut proposer = proposer();
    let sibling = approve(&mut proposer);
    let before = prompts(&proposer.fixture.backend, &proposer.session);

    send(
        &mut proposer.fixture,
        &sibling,
        Update::AgentText("done".into()),
    );
    send(
        &mut proposer.fixture,
        &sibling,
        Update::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(
        prompts(&proposer.fixture.backend, &proposer.session),
        before
    );
    assert!(
        proposer
            .fixture
            .app
            .deferred
            .get(&proposer.session)
            .is_none_or(std::collections::VecDeque::is_empty)
    );
}

#[test]
fn read_session_is_not_available_yet() {
    let dir = project_dir(&["api"]);
    let mut fixture = fixture();
    let project = fixture.app.open_project(dir.path()).unwrap();
    let caller = attached(&mut fixture, &project, api(), 1);

    let answer = call(
        &mut fixture,
        &caller,
        PeerTool::ReadSession {
            session: caller.clone(),
            page: None,
        },
    );

    assert_eq!(answer, Err("not available yet".to_owned()));
}
