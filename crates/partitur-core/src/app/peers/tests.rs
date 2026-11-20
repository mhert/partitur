#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::archive::Archived;
use crate::backend::BackendEvent;
use crate::ids::{BackendSessionId, ProjectId, RepositoryId};
use crate::session::Scope;
use crate::settings::GlobalSettings;
use crate::testing::{MemoryGit, MemoryStore, RecordingBackend, RecordingReply, single_claude};
use crate::tool_call::ToolCall;

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
