#![allow(clippy::disallowed_methods)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::Role;
use crate::app::scheduler::Generated;
use crate::app::split::{Approval, ChildChoice};
use crate::app::{App, AppError};
use crate::ids::ProposalId;

use crate::app::test_support::disk_chain;
use crate::backend::{
    AgentUpdate, ApprovedChild, BackendEvent, ChildOutcome, Decision, PromptContent, StopReason,
    TurnOrigin,
};
use crate::git::Head;
use crate::ids::{
    BackendId, BackendSessionId, ConfigId, ConfigValueId, ProjectId, RequestId, SessionId,
};
use crate::instructions::inject;
use crate::orchestrator::{PROPOSAL_RECORDED, children_started_prompt};
use crate::record::TranscriptRecord;
use crate::session::{
    BranchChoice, BranchMode, Scope, Status, TitleSource, Workspace, title_from_prompt,
};
use crate::settings::{Level, ProjectSettings, SettingsLayer};
use crate::store::Store as _;
use crate::testing::{Call, GitCall, GitOp, MemoryStore, RecordingBackend, repo_info, settle};
use crate::tool_call::ToolRole;
use crate::transcript::Entry;
use crate::view_model::{ApprovalView, ChildCheck, ExistingOption};

use super::super::tests::agent_options;
use crate::app::test_support::{
    Family, Fixture, answer_files, attached_child, attached_session, backend_id, backends_a_and_b,
    call_propose, choice, decision, deferred, family, finish, fixture_on, fixture_with,
    last_prompt, project_dir, prompts, proposal_id, propose_now, proposed, read_request, refusal,
    repository, result_text, send, status, two_children, waiting_on_the_card,
};
use crate::app::test_support::{select, settle_store};

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
    answer_files(&mut fixture);

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
    fixture.app.live_entry(&children[0]).attached = true;
    answer_files(&mut fixture);
    (fixture, children[0].clone())
}

#[test]
fn a_spawned_child_keeps_its_proposal_prompt_as_its_first_prompt() {
    let (mut fixture, child) = approved_child("Endpoint", "Endpoint");

    let spawned = fixture.app.session(&child).unwrap().clone();
    assert_eq!(spawned.first_prompt, "Add the endpoint");
    assert_eq!(spawned.skill, None);
    send(
        &mut fixture,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    select(&mut fixture.app, &fixture.store, child.clone());
    fixture.app.submit_prompt("/brainstorming more").unwrap();
    answer_files(&mut fixture);

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
    send(
        &mut fixture,
        &child,
        AgentUpdate::Title("API endpoint".into()),
    );
    assert_eq!(fixture.app.session(&child).unwrap().title, "API endpoint");
}

#[test]
fn a_derived_proposed_title_the_user_edited_is_given() {
    let (mut fixture, child) = approved_child(" ", "My endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title, "My endpoint");
    assert_eq!(session.title_source, TitleSource::Given);
    send(
        &mut fixture,
        &child,
        AgentUpdate::Title("API endpoint".into()),
    );
    assert_eq!(fixture.app.session(&child).unwrap().title, "My endpoint");
}

#[test]
fn an_approved_child_left_as_proposed_has_a_chosen_title() {
    let (mut fixture, child) = approved_child("Endpoint", "Endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title_source, TitleSource::Chosen);
    send(
        &mut fixture,
        &child,
        AgentUpdate::Title("API endpoint".into()),
    );
    assert_eq!(fixture.app.session(&child).unwrap().title, "Endpoint");
}

#[test]
fn an_approved_child_the_user_retitled_has_a_given_title() {
    let (mut fixture, child) = approved_child("Endpoint", "My endpoint");

    let session = fixture.app.session(&child).unwrap();
    assert_eq!(session.title, "My endpoint");
    assert_eq!(session.title_source, TitleSource::Given);
    send(
        &mut fixture,
        &child,
        AgentUpdate::Title("API endpoint".into()),
    );
    assert_eq!(fixture.app.session(&child).unwrap().title, "My endpoint");
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
    answer_files(&mut fixture);

    let child = fixture.app.session(&children[0]).unwrap();
    assert_eq!(child.title, "Add the endpoint");
    assert_eq!(child.title_source, TitleSource::Chosen);
}

#[test]
fn an_orchestrator_with_an_undecided_proposal_waits_for_you_when_its_turn_ends() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
    let children = vec![proposed("web", "Add the page", "Web page")];
    call_propose(&mut fixture, &parent, children).unwrap();
    finish(&mut fixture, &child, "api done");

    assert_eq!(deferred(&fixture.app, &parent), 1);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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

    fixture.app.submit_prompt("Go on").unwrap();
    answer_files(&mut fixture);

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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    answer_files(&mut fixture);

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
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
    propose_now(&mut fixture, &parent, two_children());
    send(&mut fixture, &parent, read_request());
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
    assert!(fixture.app.session_view().unwrap().running());

    let refused = fixture.app.submit_prompt("Go on");
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

/// The children recorded as approved on the proposal `id`; none when it is not approved.
fn approved(app: &App, session: &SessionId, id: &ProposalId) -> Vec<ApprovedChild> {
    app.loaded(session)
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
    assert_eq!(session.role.parent(), Some(parent));
    assert_eq!(session.scope, Scope::Repository(child.repository.clone()));
    assert_eq!(session.title, child.title);
    assert_eq!(session.auto_allow, child.auto_allow);
    assert_eq!(session.status, Status::Running);
    assert_eq!(
        fixture.app.loaded(&child.session).entries(),
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
            &disk_chain(project, Some(repository)),
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
        fixture.app.loaded(&parent).entries(),
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
    assert!(fixture.app.no_proposals());
    assert_eq!(fixture.app.current_session(), current);

    for (request, child, _, _) in &created {
        create(&mut fixture, *request, child);
    }
    answer_files(&mut fixture);

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
    answer_files(&mut fixture);

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
        fixture.app.loaded(&children[1]).entries(),
        [Entry::OrchestratorPrompt(
            "\nStyle the page\nwith care".into()
        )]
    );
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
    let project = fixture.open(dir.path());
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
    answer_files(&mut fixture);

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
    let project = fixture.open(dir.path());
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
    select(&mut restarted.app, &restarted.store, parent.clone());
    assert_eq!(
        restarted.app.session(&parent).unwrap().backend,
        backend_id("b")
    );

    let children = restarted
        .app
        .approve_proposal(parent.clone(), id, two_choices())
        .map(started_ids)
        .unwrap();
    answer_files(&mut restarted);

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
    let project = fixture.open(dir.path());
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
    answer_files(&mut fixture);
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
        AgentUpdate::ConfigOptions(agent_options("opus")),
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
    let project = fixture.open(dir.path());
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
    answer_files(&mut fixture);
    let is_prompt = |call: &Call| matches!(call, Call::Prompt { session, .. } if session == child);
    assert!(!fixture.backend.calls().iter().any(is_prompt));

    send(
        &mut fixture,
        child,
        AgentUpdate::ConfigRejected {
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
    answer_files(&mut fixture);

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
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
    answer_files(&mut fixture);
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
    answer_files(&mut fixture);

    assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);
    assert_eq!(deferred(&fixture.app, &parent), 1);
    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    let wake = children_started_prompt(&approved(&fixture.app, &parent, &id));
    assert_eq!(last_prompt(&fixture.backend, &parent), wake);
    assert_eq!(
        fixture.app.loaded(&parent).entries().last(),
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
        fixture.app.loaded(&parent).pending_proposal(&id),
        Some(two_children().as_slice())
    );
    assert!(fixture.app.live[&parent].proposals.contains_key(&id));
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
        fixture.app.loaded(&parent).pending_proposal(&id),
        Some(two_children().as_slice())
    );
    assert!(fixture.app.live[&parent].proposals.contains_key(&id));
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
        fixture.app.loaded(&parent).entries().last(),
        Some(&Entry::PartiturPrompt(
            "proposal rejected by the user\nnote: not now".into()
        ))
    );
    assert!(fixture.app.no_proposals());
    assert_eq!(creates(&fixture.backend).len(), created);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    answer_files(&mut fixture);
    let created = creates(&fixture.backend).len();

    let again = fixture
        .app
        .approve_proposal(parent.clone(), id.clone(), card.clone());
    answer_files(&mut fixture);
    let rejected = fixture
        .app
        .reject_proposal(parent.clone(), id.clone(), None);
    answer_files(&mut fixture);
    let stranger = fixture
        .app
        .approve_proposal(SessionId::new(), id.clone(), card.clone());
    answer_files(&mut fixture);

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
    select(&mut restarted.app, &restarted.store, parent.clone());

    let after_restart = restarted
        .app
        .approve_proposal(parent.clone(), id.clone(), card);
    answer_files(&mut restarted);

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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    let restarted = fixture_with(fixture.store.clone());

    assert_eq!(status(&restarted.app, &parent), Status::Idle);
    assert!(
        !restarted
            .store
            .load_records(restarted.store.project_of(&parent).unwrap(), parent.clone())
            .unwrap()
            .iter()
            .any(|update| matches!(update, TranscriptRecord::Notice(_)))
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
    assert!(restarted.app.no_proposals());
    select(&mut restarted.app, &restarted.store, parent.clone());

    let children = restarted
        .app
        .approve_proposal(
            parent.clone(),
            id.clone(),
            vec![choice("web", "Add the page", "Web page", false)],
        )
        .map(started_ids)
        .unwrap();
    answer_files(&mut restarted);

    let created = creates(&restarted.backend);
    assert_eq!(created.len(), 1);
    assert_eq!(vec![created[0].1.clone()], children);
    assert_eq!(
        restarted.app.session(&children[0]).unwrap().role.parent(),
        Some(&parent)
    );
    assert_eq!(approved(&restarted.app, &parent, &id).len(), 1);
    assert_eq!(
        restarted.app.loaded(&parent).entries().last(),
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
    select(&mut restarted.app, &restarted.store, parent.clone());
    restarted.app.submit_prompt("Plan it").unwrap();
    answer_files(&mut restarted);
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
        .live
        .values()
        .flat_map(|live| live.proposals.keys().cloned())
        .find(|id| *id != older)
        .unwrap();
    send(
        &mut restarted,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);
    let card = vec![choice("web", "Add the page", "Web page", false)];

    restarted
        .app
        .approve_proposal(parent.clone(), newer.clone(), card.clone())
        .map(started_ids)
        .unwrap();
    answer_files(&mut restarted);

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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);

    restarted
        .app
        .approve_proposal(parent.clone(), older.clone(), card)
        .map(started_ids)
        .unwrap();
    answer_files(&mut restarted);
    send(
        &mut restarted,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    select(&mut restarted.app, &restarted.store, child.clone());
    restarted.app.submit_prompt("Add the endpoint").unwrap();
    load(&mut restarted, &child);
    send(
        &mut restarted,
        &child,
        AgentUpdate::AgentText("api done".into()),
    );

    send(
        &mut restarted,
        &child,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    settle_store(&mut restarted);
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&restarted.app, &parent), Status::WaitingForYou);
}

#[test]
fn new_session_still_selects_and_titles_as_before() {
    let dir = project_dir(&["api", "web"]);
    let mut fixture = fixture_with(MemoryStore::new());
    let project = fixture.open(dir.path());

    let session = fixture
        .app
        .new_session(project.clone(), Scope::Repository(repository("api")))
        .unwrap();

    assert_eq!(fixture.app.current_session(), Some(session.clone()));
    let created = fixture.app.session(&session).unwrap();
    assert_eq!(created.title, "untitled");
    assert_eq!(created.role.parent(), None);
    assert!(!created.auto_allow);
    assert!(!created.role.is_orchestrator());
    assert_eq!(created.status, Status::Idle);
    assert_eq!(
        fixture.store.load_sessions(project).unwrap(),
        std::slice::from_ref(created)
    );
    assert_eq!(fixture.app.loaded(&session).entries(), []);
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
    fixture.app.submit_prompt("Go on").unwrap();
    answer_files(&mut fixture);
    finish(&mut fixture, &child, "api done");
    finish(&mut fixture, &web, "web done");
    // A stopped turn sends nothing deferred, so the results wait for the decision.
    fixture.app.cancel().unwrap();
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::Cancelled),
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
    answer_files(&mut fixture);

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
        fixture.app.live[&parent].deferred.back(),
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
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
        AgentUpdate::TurnEnded(StopReason::EndTurn),
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
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
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
fn an_answered_proposal_then_the_turn_end_waits_for_you() {
    let Family {
        mut fixture,
        parent,
        child,
        ..
    } = family();
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();

    let answer = call_propose(&mut fixture, &parent, two_children());
    assert_eq!(answer, Ok(PROPOSAL_RECORDED.to_owned()));
    send(&mut fixture, &child, AgentUpdate::AgentText("busy".into()));
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
}

#[test]
fn agent_turn_of_orchestrator_awaiting_decision_ends_waiting() {
    let Family {
        mut fixture,
        parent,
        ..
    } = family();
    select(&mut fixture.app, &fixture.store, parent.clone());
    fixture.app.submit_prompt("Plan it").unwrap();
    let children = vec![proposed("web", "Add the page", "Web page")];
    call_propose(&mut fixture, &parent, children).unwrap();
    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );
    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);

    send(&mut fixture, &parent, AgentUpdate::TurnStarted);

    assert_eq!(status(&fixture.app, &parent), Status::Running);

    send(
        &mut fixture,
        &parent,
        AgentUpdate::TurnEnded(StopReason::EndTurn),
    );

    assert_eq!(status(&fixture.app, &parent), Status::WaitingForYou);
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
    git_split_on(MemoryStore::new(), names)
}

/// [`git_split`] over `store`, which the project is opened on.
fn git_split_on(store: MemoryStore, names: &[&str]) -> GitSplit {
    let dir = project_dir(names);
    let mut fixture = fixture_with(store);
    let project = fixture.open(dir.path());
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

    /// Approves `children`; the files the children's first prompts wait for are read.
    fn approve(&mut self, children: &[ChildChoice]) -> Result<Approval, AppError> {
        let approval = self.fixture.app.approve_proposal(
            self.parent.clone(),
            self.id.clone(),
            children.to_vec(),
        );
        answer_files(&mut self.fixture);
        approval
    }

    /// Lets git answer, and then the files the started children's first prompts wait for.
    fn settle(&mut self) -> Vec<AppError> {
        let errors = settle(&mut self.fixture.app, &self.fixture.git);
        answer_files(&mut self.fixture);
        errors
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
            .filter(|session| session.role.parent() == Some(&self.parent))
            .map(|session| session.id.clone())
            .collect()
    }

    fn undecided(&self) -> bool {
        self.fixture
            .app
            .live(&self.parent)
            .is_some_and(|live| live.proposals.contains_key(&self.id))
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
            existing: None,
        },
        ..choice(name, "Do it", &format!("{name} child"), false)
    }
}

fn worktree_in(name: &str, branch: &str) -> ChildChoice {
    wants(name, BranchMode::Worktree, branch)
}

/// The local branch `name`, checked out in no worktree, in the main checkout when `main`.
fn branch(name: &str, main: bool) -> ExistingOption {
    ExistingOption {
        name: name.into(),
        worktree: None,
        main,
        gone: false,
    }
}

fn problems(checks: &[ChildCheck]) -> Vec<Option<String>> {
    checks.iter().map(|check| check.problem.clone()).collect()
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
        existing: None,
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
                existing_options: vec![branch("develop", false), branch("main", true)],
                use_existing: None,
                note: None,
                problem: None,
            },
            ChildCheck {
                loading: false,
                bases: vec!["main".into()],
                base: Some("main".into()),
                fetch: false,
                fetch_remote: None,
                worktree: Some(split.worktree("web", "feat/x")),
                existing_options: vec![branch("main", true)],
                use_existing: None,
                note: None,
                problem: None,
            },
        ]
    );
}

#[test]
fn a_worktree_folder_that_exists_is_refused_once_the_files_port_says_so() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    let folder = split.worktree("api", "feat/x");
    std::fs::create_dir_all(&folder).unwrap();
    let asked = |split: &GitSplit| {
        split
            .fixture
            .files
            .queries()
            .iter()
            .filter(|query| **query == crate::files::FilesQuery::Resolve(vec![folder.clone()]))
            .count()
    };

    assert_eq!(problems(&split.checked(&children)), [None]);
    assert_eq!(asked(&split), 1);
    answer_files(&mut split.fixture);

    let exists = Some(format!("{} already exists", folder.display()));
    assert_eq!(
        problems(&split.recheck(&children)),
        std::slice::from_ref(&exists)
    );
    // Only a change to the card asks again.
    assert_eq!(asked(&split), 1);
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(problems(&split.check(&children)), [exists]);
    assert_eq!(asked(&split), 2);
    answer_files(&mut split.fixture);
    assert_eq!(problems(&split.recheck(&children)), [None]);
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
    let store = MemoryStore::new();
    store.set_default_settings(settings);
    let mut split = git_split_on(store, &["api", "web"]);

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
fn settings_are_read_once() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    let link_session = split.parent.clone();

    split.checked(&children);
    split.approve(&children).unwrap();
    for _ in 0..3 {
        split.fixture.app.link_opener(&link_session).unwrap();
    }

    assert_eq!(split.fixture.store.settings_loads(), 1);
}

#[test]
fn two_children_cannot_share_a_worktree_folder() {
    // Absolute on every platform: a bare `/wt` has no drive on Windows.
    let root = std::env::temp_dir().join("shared-worktrees");
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
    let store = MemoryStore::new();
    store.set_default_settings(settings);
    let mut split = git_split_on(store, &["api", "web"]);

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
            session.role = Role::Child { parent };
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
    let store = MemoryStore::new();
    store.fail_next_settings_load();
    let mut split = git_split_on(store, &["api", "web"]);
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

    let as_is = |repo: &str| ChildCheck {
        note: Some(format!("works in {repo} as it is now")),
        ..ChildCheck::default()
    };
    assert_eq!(checks, [as_is("api"), as_is("web")]);
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

/// Approves `children` without answering the files port, then lets git answer the fresh
/// reading before the files port answers the folders the press asked about.
fn approve_with_the_folders_answered_last(split: &mut GitSplit, children: &[ChildChoice]) {
    let approval = split
        .fixture
        .app
        .approve_proposal(split.parent.clone(), split.id.clone(), children.to_vec())
        .unwrap();
    assert_eq!(approval, Approval::Pending);
    assert!(settle(&mut split.fixture.app, &split.fixture.git).is_empty());
    assert!(split.approval().unwrap().pending);
    assert!(split.children().is_empty());
    // The repositories stay busy while the reading waits.
    let api = split.path("api");
    assert!(split.fixture.app.git_work.is_busy(&api));
    answer_files(&mut split.fixture);
    assert!(split.settle().is_empty());
}

#[test]
fn an_approval_refuses_a_worktree_folder_made_after_the_check_even_answered_last() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    answer_files(&mut split.fixture);
    let folder = split.worktree("api", "feat/x");
    std::fs::create_dir_all(&folder).unwrap();
    // The answer about another folder comes first, and decides nothing.
    let elsewhere = split.worktree("web", "elsewhere");
    split.fixture.app.ask_folder(&elsewhere, true);

    approve_with_the_folders_answered_last(&mut split, &children);

    let approval = split.approval().unwrap();
    assert!(!approval.pending);
    assert_eq!(
        approval.error,
        Some(format!("{} already exists", folder.display()))
    );
    assert!(split.children().is_empty());
    assert!(split.undecided());
    assert!(!split.fixture.app.git_work.is_busy(&split.path("api")));
}

#[test]
fn deleting_an_orchestrator_whose_approval_waits_frees_its_repositories() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
    split.checked(&children);
    answer_files(&mut split.fixture);
    let approval = split
        .fixture
        .app
        .approve_proposal(split.parent.clone(), split.id.clone(), children.to_vec())
        .unwrap();
    assert_eq!(approval, Approval::Pending);
    assert!(settle(&mut split.fixture.app, &split.fixture.git).is_empty());
    let api = split.path("api");
    assert!(split.fixture.app.git_work.is_busy(&api));
    assert!(split.fixture.app.holds_anything_for(&split.parent));

    split.fixture.app.delete(split.parent.clone(), &[]).unwrap();
    answer_files(&mut split.fixture);

    assert!(!split.fixture.app.holds_anything_for(&split.parent));
    assert!(!split.fixture.app.git_work.is_busy(&api));
    let owner = crate::app::folders::Owner::Proposal(split.id.clone());
    assert!(!split.fixture.app.folders.shown.contains_key(&owner));
}

#[test]
fn an_approval_goes_on_once_the_folders_it_asked_about_are_answered() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x"), worktree_in("web", "feat/x")];
    split.checked(&children);
    answer_files(&mut split.fixture);

    approve_with_the_folders_answered_last(&mut split, &children);

    assert_eq!(split.children().len(), 2);
    assert!(!split.undecided());
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
fn settings_that_cannot_be_read_stop_the_approval() {
    let store = MemoryStore::new();
    store.fail_next_settings_load();
    let mut split = git_split_on(store, &["api", "web"]);
    let children = [worktree_in("api", "feat/x")];

    let approved = split.approve(&children);

    assert!(matches!(approved, Err(AppError::Store(_))), "{approved:?}");
    let text = approved.unwrap_err().to_string();
    assert!(text.contains("settings.toml"), "{text}");
    assert_eq!(split.approval(), None);
}

#[test]
fn a_run_that_fails_after_the_reading_stops_the_approval() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "feat/x")];
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
        .live
        .values()
        .flat_map(|live| live.proposals.keys().cloned())
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
        .live
        .values()
        .flat_map(|live| live.proposals.keys().cloned())
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
    // The folder the press asked about is answered before the fresh reading.
    answer_files(&mut split.fixture);
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

impl GitSplit {
    /// Gives the repository `name` the local branch `branch`, checked out nowhere.
    fn with_branch(&mut self, name: &str, branch: &str) {
        let path = self.path(name);
        let mut info = self.fixture.git.repository(&path).unwrap();
        info.branches.push(crate::git::LocalBranch {
            name: branch.into(),
            upstream_remote: None,
        });
        self.fixture.git.add_repository(path, info);
    }

    /// Gives the repository `name` the local branch `branch`, checked out in a worktree of
    /// its own made outside Partitur, and returns that worktree's folder.
    fn with_worktree(&mut self, name: &str, branch: &str) -> PathBuf {
        self.with_branch(name, branch);
        let path = self.path(name);
        let tree = path.parent().unwrap().join("outside").join(branch);
        let mut info = self.fixture.git.repository(&path).unwrap();
        info.worktrees.push(crate::git::WorktreeInfo {
            path: tree.clone(),
            head: Head::Branch(branch.into()),
            prunable: false,
        });
        self.fixture.git.add_repository(path, info);
        tree
    }
}

/// A child of `name` titled `title` in the existing `mode` on the existing branch `branch`.
fn existing_in(name: &str, mode: BranchMode, branch: &str, title: &str) -> ChildChoice {
    let mut child = wants(name, mode, "");
    child.branch.existing = Some(branch.into());
    child.title = title.into();
    child
}

#[test]
fn a_child_reuses_an_existing_worktree() {
    let mut split = git_split(&["api", "web"]);
    let tree = split.with_worktree("api", "feat/x");
    let children = [existing_in(
        "api",
        BranchMode::ExistingWorktree,
        "feat/x",
        "Reuse",
    )];

    let checks = split.checked(&children);

    assert_eq!(problems(&checks), [None]);
    assert_eq!(checks[0].worktree, Some(tree.clone()));
    assert_eq!(checks[0].use_existing, None);
    assert_eq!(
        checks[0].note.as_deref(),
        Some("uses its existing worktree")
    );
    assert!(
        checks[0]
            .existing_options
            .iter()
            .any(|option| option.name == "feat/x" && option.worktree.as_ref() == Some(&tree)),
        "{:?}",
        checks[0].existing_options
    );
    let from = split.fixture.git.calls().len();
    let approval = split.approve(&children).unwrap();

    assert_eq!(approval, Approval::Started(split.children()));
    assert_eq!(split.git_since(from), []);
    let started = split.children();
    let session = split.fixture.app.session(&started[0]).unwrap();
    assert_eq!(session.cwd, tree);
    assert_eq!(
        session.workspace,
        Workspace::ExistingWorktree {
            branch: "feat/x".into(),
            path: tree.clone()
        }
    );
    let approved = approved(&split.fixture.app, &split.parent, &split.id);
    assert_eq!(approved[0].mode, BranchMode::ExistingWorktree);
    assert_eq!(approved[0].branch.as_deref(), Some("feat/x"));
    assert_eq!(approved[0].cwd, tree);
    let wake = last_prompt(&split.fixture.backend, &split.parent);
    assert!(
        wake.contains(&format!(
            "· worktree on existing branch feat/x · in {}",
            tree.display()
        )),
        "{wake}"
    );
}

#[test]
fn a_child_named_like_an_existing_branch_offers_use_existing() {
    let mut split = git_split(&["api", "web"]);

    let checks = split.checked(&[
        worktree_in("api", " develop "),
        wants("web", BranchMode::NewBranch, "feat/x"),
    ]);

    assert_eq!(
        problems(&checks),
        [Some("branch develop already exists in api".into()), None]
    );
    assert_eq!(checks[0].use_existing.as_deref(), Some("develop"));
    assert_eq!(checks[1].use_existing, None);
    assert_eq!(checks[0].note, None);
}

#[test]
fn a_busy_repository_offers_no_use_existing() {
    let mut split = git_split(&["api", "web"]);
    let children = [worktree_in("api", "develop")];
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
    assert_eq!(while_reading[0].use_existing, None);
}

#[test]
fn two_children_may_share_an_existing_worktree() {
    let mut split = git_split(&["api", "web"]);
    let tree = split.with_worktree("api", "feat/x");
    let children = [
        existing_in("api", BranchMode::ExistingWorktree, "feat/x", "First"),
        existing_in("api", BranchMode::ExistingWorktree, "feat/x", " "),
    ];

    let checks = split.checked(&children);

    assert_eq!(problems(&checks), [None, None]);
    // A blank title is named as the title the prompt gives it.
    let notes: Vec<Option<String>> = checks.iter().map(|check| check.note.clone()).collect();
    assert_eq!(
        notes,
        [
            Some("uses its existing worktree · also used by \"Do it\"".into()),
            Some("uses its existing worktree · also used by \"First\"".into()),
        ]
    );
    let elsewhere = attached_session(
        &mut split.fixture,
        &split.project,
        Scope::Repository(repository("web")),
    );
    split
        .fixture
        .app
        .change_session(elsewhere, |session| {
            session.title = "Elsewhere".into();
            session.cwd = tree.clone();
        })
        .unwrap();
    let with_a_session = split.check(&children);
    // The other children come first.
    assert_eq!(
        with_a_session[0].note.as_deref(),
        Some("uses its existing worktree · also used by \"Do it\" and 1 more")
    );

    let from = split.fixture.git.calls().len();
    let approval = split.approve(&children).unwrap();
    assert_eq!(approval, Approval::Started(split.children()));
    assert_eq!(split.git_since(from), []);
    for child in split.children() {
        assert_eq!(split.fixture.app.session(&child).unwrap().cwd, tree);
    }
}

#[test]
fn switching_to_an_existing_branch_conflicts_with_a_checked_out_child() {
    let mut split = git_split(&["api", "web"]);
    split.with_branch("api", "feat/x");
    let switch = || existing_in("api", BranchMode::ExistingBranch, "feat/x", "Switch");
    let as_is = || existing_in("api", BranchMode::ExistingBranch, "main", "As is");
    let in_use = Some("api is in use by another child — use a worktree".to_owned());

    let beside_checked_out = split.checked(&[wants("api", BranchMode::CheckedOut, ""), switch()]);
    let beside_as_is = split.check(&[as_is(), switch()]);
    let new_beside_as_is = split.check(&[as_is(), wants("api", BranchMode::NewBranch, "feat/y")]);
    let as_is_beside_checked_out =
        split.check(&[as_is(), wants("api", BranchMode::CheckedOut, "")]);
    let beside_a_worktree = split.check(&[worktree_in("api", "feat/y"), switch()]);
    let two_switches = split.check(&[switch(), switch()]);

    assert_eq!(problems(&beside_checked_out), [None, in_use.clone()]);
    assert_eq!(problems(&beside_as_is), [None, in_use.clone()]);
    assert_eq!(problems(&new_beside_as_is), [None, in_use.clone()]);
    assert_eq!(problems(&as_is_beside_checked_out), [None, None]);
    assert_eq!(problems(&beside_a_worktree), [None, None]);
    assert_eq!(problems(&two_switches), [in_use.clone(), in_use]);
    assert_eq!(
        beside_a_worktree[1].note.as_deref(),
        Some("switches api itself to feat/x")
    );
    assert_eq!(
        as_is_beside_checked_out[0].note.as_deref(),
        Some("works in api as it is now · also used by \"api child\"")
    );
    assert_eq!(
        as_is_beside_checked_out[1].note.as_deref(),
        Some("works in api as it is now · also used by \"As is\"")
    );
}

#[test]
fn switching_to_a_branch_another_child_adds_a_worktree_for_is_refused() {
    let mut split = git_split(&["api", "web"]);
    split.with_branch("api", "feat/x");
    let switch = existing_in("api", BranchMode::ExistingBranch, "feat/x", "Switch");
    let add = existing_in("api", BranchMode::ExistingWorktree, "feat/x", "Add");
    let elsewhere = existing_in("api", BranchMode::ExistingWorktree, "develop", "Other");
    let refused = Some(format!(
        "feat/x is checked out in {} — pick Worktree",
        split.worktree("api", "feat/x").display()
    ));

    let switch_first = split.checked(&[switch.clone(), add.clone()]);
    let add_first = split.check(&[add, switch.clone()]);
    let other_branch = split.check(&[switch.clone(), elsewhere]);
    let mut stale = wants("api", BranchMode::NewBranch, "feat/y");
    stale.branch.existing = Some("feat/x".into());
    let new_branch = split.check(&[
        stale,
        existing_in("api", BranchMode::ExistingWorktree, "feat/x", "Add"),
    ]);

    assert_eq!(problems(&switch_first), [refused.clone(), None]);
    assert_eq!(problems(&add_first), [None, refused]);
    assert_eq!(problems(&other_branch), [None, None]);
    assert_eq!(problems(&new_branch), [None, None]);
}

#[test]
fn an_as_is_existing_child_makes_nothing() {
    let mut split = git_split(&["api", "web"]);
    let children = [existing_in(
        "api",
        BranchMode::ExistingBranch,
        "main",
        "As is",
    )];
    split.checked(&children);
    let from = split.fixture.git.calls().len();

    let approval = split.approve(&children).unwrap();

    assert_eq!(approval, Approval::Started(split.children()));
    assert_eq!(split.git_since(from), []);
    let started = split.children();
    let session = split.fixture.app.session(&started[0]).unwrap();
    assert_eq!(session.workspace, Workspace::CheckedOut);
    assert_eq!(session.cwd, split.path("api"));
}

#[test]
fn a_failed_approval_undoes_only_what_it_made() {
    let mut split = git_split(&["api", "web", "docs"]);
    split.with_worktree("api", "feat/r");
    split.with_branch("api", "feat/a");
    split.with_branch("web", "feat/s");
    let children = [
        existing_in("api", BranchMode::ExistingWorktree, "feat/r", "Reuse"),
        existing_in("api", BranchMode::ExistingWorktree, "feat/a", "Add"),
        existing_in("web", BranchMode::ExistingBranch, "feat/s", "Switch"),
        worktree_in("docs", "feat/y"),
    ];
    assert_eq!(
        problems(&split.checked(&children)),
        [None, None, None, None]
    );
    let (api, web, docs) = (split.path("api"), split.path("web"), split.path("docs"));
    split.fixture.git.fail_next(GitOp::AddWorktree, &docs);
    assert_eq!(split.approve(&children).unwrap(), Approval::Pending);
    let from = split.fixture.git.calls().len();

    assert!(split.settle().is_empty());

    let added = split.worktree("api", "feat/a");
    assert_eq!(
        split.git_since(from),
        [
            GitCall::CheckoutWorktree {
                repository: api.clone(),
                path: added.clone(),
                branch: "feat/a".into()
            },
            GitCall::RestoreHead {
                repository: web.clone(),
                head: Head::Branch("feat/s".into())
            },
            GitCall::AddWorktree {
                repository: docs.clone(),
                path: split.worktree("docs", "feat/y"),
                branch: "feat/y".into(),
                start: "refs/heads/main".into()
            },
            GitCall::RestoreHead {
                repository: web.clone(),
                head: Head::Branch("main".into())
            },
            GitCall::RemoveWorktree {
                repository: api.clone(),
                path: added
            },
        ]
    );
    let api_info = split.fixture.git.repository(&api).unwrap();
    assert!(api_info.has_branch("feat/a") && api_info.has_branch("feat/r"));
    assert!(api_info.checked_out_at("feat/r").is_some());
    let web_info = split.fixture.git.repository(&web).unwrap();
    assert_eq!(web_info.head(), Some(&Head::Branch("main".into())));
    assert!(web_info.has_branch("feat/s"));
    assert!(split.children().is_empty());
    assert!(split.undecided());
}
