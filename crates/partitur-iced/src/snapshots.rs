//! Renders the tabs (the Session tab also for an orchestrator, before and after its split
//! is decided, with the split's branch settings open and while its approval waits for git),
//! the jump panel, the delete sheet of an orchestrator whose children work in worktrees
//! (before and after git has answered), and archiving (the tree with archived sessions, the archive
//! sheet, an archived session and the jump panel listing archived sessions) and the
//! new-session sheet (for an orchestrator, and for a repository in each branch mode, while
//! its agent and git start, once they have answered and while Create waits for git) to PNG
//! files in both themes, for checking the interface by eye. Nothing is asserted about the pixels, so the
//! test is ignored by default:
//!
//! ```text
//! ICED_TEST_BACKEND=tiny-skia cargo test -p partitur-iced snapshots -- --ignored
//! ```
//!
//! The files land in `target/snapshots`, at twice the window's 1100 by 720 logical pixels.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};

use futures::channel::mpsc;
use iced::Size;
use iced::theme::Mode;
use iced_test::Simulator;
use partitur_core::testing::{
    Call, MemoryGit, MemoryStore, RecordingBackend, repo_info, single_claude,
};
use partitur_core::{
    App, Approval, BackendEvent, BackendSessionId, BranchChoice, BranchMode, ChildChoice,
    ConfigCategory, ConfigChoice, ConfigId, ConfigOption, ConfigValueId, Created, GlobalSettings,
    OrchestratorTool, PermissionId, PermissionKind, PermissionOption, PermissionOptionId,
    ProjectId, ProposalId, ProposedChild, RepositoryId, Scope, SessionId, StopReason, ToolCallId,
    ToolKind, ToolStatus, Update,
};
use tempfile::TempDir;

use crate::app::{Message, State, Tab, boot, update, view};
use crate::confirm_sheet::ConfirmMessage;
use crate::jump::JumpMessage;
use crate::new_session_sheet::NewSessionMessage;
use crate::projects_view::ProjectsMessage;
use crate::session_view::{ProposalMessage, SessionMessage};
use crate::test_support::{attach, deliver, deliver_tool, settle_git};
use crate::{settings, theme};

/// The size of the rendered window, in logical pixels.
const WINDOW: Size = Size::new(1100.0, 720.0);

/// Writes `state`'s window as `<name>-dark` and `<name>-light`, telling `state` the
/// desktop's preference first: some views take colours from it rather than from the theme
/// they are drawn with.
fn render(state: &mut State, name: &str) {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&folder).unwrap();
    for (mode, suffix) in [(Mode::Dark, "dark"), (Mode::Light, "light")] {
        let path = folder.join(format!("{name}-{suffix}"));
        // A snapshot is only written where no file is yet; an existing one is compared.
        for stale in std::fs::read_dir(&folder).unwrap().flatten() {
            if stale
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{name}-{suffix}-"))
            {
                std::fs::remove_file(stale.path()).unwrap();
            }
        }
        let _task = update(state, Message::SystemTheme(mode));
        let mut ui = Simulator::with_size(settings(), WINDOW, view(state));
        let snapshot = ui.snapshot(&theme::theme(mode)).unwrap();
        assert!(snapshot.matches_image(&path).unwrap());
    }
}

/// Feeds `update` for `session` through the application, as its backend would report it.
fn report(state: &mut State, session: &SessionId, update: Update) {
    deliver(
        state,
        BackendEvent::Update {
            session: session.clone(),
            update,
        },
    );
}

/// Ends the running turn of `session` the regular way.
fn end_turn(state: &mut State, session: &SessionId) {
    report(state, session, Update::TurnEnded(StopReason::EndTurn));
}

/// A new session of the whole `project` that runs the turn `prompt` started; the prompt
/// names it.
fn prompted(
    state: &mut State,
    backend: &RecordingBackend,
    project: &ProjectId,
    prompt: &str,
) -> SessionId {
    let session = state
        .app
        .new_session(project.clone(), Scope::Project)
        .unwrap();
    attach(state, backend, &session, prompt);
    state.app.submit_prompt(prompt, Vec::new()).unwrap();
    session
}

/// Lets `orchestrator` propose one child per `(repository, title)` of `children`, approves
/// the proposal and returns the started children in the order of `children`. The tree lists
/// them the other way round, newest first.
fn split(
    state: &mut State,
    backend: &RecordingBackend,
    orchestrator: &SessionId,
    children: &[(&str, &str)],
) -> Vec<SessionId> {
    let id = ProposalId::new();
    let prompt = "Do your part";
    let proposed = children
        .iter()
        .map(|(repository, title)| ProposedChild {
            repository: RepositoryId::try_from(*repository).unwrap(),
            prompt: prompt.into(),
            title: (*title).into(),
        })
        .collect();
    report(
        state,
        orchestrator,
        Update::ChildrenProposed {
            id: id.clone(),
            children: proposed,
        },
    );
    let choices = children
        .iter()
        .map(|(repository, title)| ChildChoice {
            repository: RepositoryId::try_from(*repository).unwrap(),
            prompt: prompt.into(),
            title: (*title).into(),
            auto_allow: false,
            branch: BranchChoice::default(),
        })
        .collect();
    let Approval::Started(started) = state
        .app
        .approve_proposal(orchestrator.clone(), id, choices)
        .unwrap()
    else {
        panic!("checked-out children start at once");
    };
    for child in &started {
        attach(state, backend, child, &child.to_string());
    }
    started
}

/// The Projects tab with every kind of row, and the folder its projects live in, which must
/// outlive the state.
///
/// The project `shop-app` has three repositories and two orchestrators: `Add gift cards`
/// waits for the user while one of its children runs (the cursor is on it, and its context
/// usage is known) and two are done; `Fix checkout 500` is done and collapsed. The project
/// `taffle` is a single repository with a finished and a running session. The git behind
/// them knows every repository, each with `main` checked out, a branch `develop` besides and
/// `origin/main` on the remote.
fn two_projects() -> (State, RecordingBackend, MemoryGit, TempDir) {
    let root = TempDir::new().unwrap();
    let folder = fs::canonicalize(root.path()).unwrap();
    let shop = folder.join("shop-app");
    let taffle = folder.join("taffle");
    let git = MemoryGit::new();
    let repositories = ["backend", "frontend", "infra"].map(|name| shop.join(name));
    for repository in repositories.iter().chain([&taffle]) {
        fs::create_dir_all(repository.join(".git")).unwrap();
        let info = repo_info(repository, "main", &["develop", "main"], &["origin/main"]);
        git.add_repository(repository.clone(), info);
    }
    fs::write(shop.join("AGENTS.md"), "rules").unwrap();

    let backend = RecordingBackend::new();
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(MemoryStore::new()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let shop = state.app.open_project(&shop).unwrap();
    let taffle = state.app.open_project(&taffle).unwrap();

    // The older orchestrator: its child reported back and both are done.
    let checkout = prompted(&mut state, &backend, &shop, "Fix checkout 500");
    let fixes = split(
        &mut state,
        &backend,
        &checkout,
        &[("backend", "Order totals")],
    );
    end_turn(&mut state, &checkout);
    for fix in &fixes {
        end_turn(&mut state, fix);
    }
    // The news that its child started, and then the child's result, each started another
    // turn of the orchestrator.
    end_turn(&mut state, &checkout);
    end_turn(&mut state, &checkout);

    let gift_cards = prompted(&mut state, &backend, &shop, "Add gift cards");
    let parts = split(
        &mut state,
        &backend,
        &gift_cards,
        &[
            ("infra", "Feature flag"),
            ("frontend", "Redeem form"),
            ("backend", "Gift card API"),
        ],
    );
    end_turn(&mut state, &parts[0]);
    end_turn(&mut state, &parts[1]);
    report(
        &mut state,
        &parts[2],
        Update::Usage {
            used: 41_260,
            size: 200_000,
        },
    );
    report(
        &mut state,
        &gift_cards,
        Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Read the deploy notes".into(),
            kind: ToolKind::Read,
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

    let renamed = prompted(&mut state, &backend, &taffle, "Rename config keys");
    end_turn(&mut state, &renamed);
    let upgrade = prompted(&mut state, &backend, &taffle, "Upgrade to Symfony 8");
    report(
        &mut state,
        &upgrade,
        Update::Usage {
            used: 112_400,
            size: 200_000,
        },
    );

    for message in [
        ProjectsMessage::Toggle(checkout),
        ProjectsMessage::Select(parts[2].clone()),
    ] {
        let _task = update(&mut state, Message::Projects(message));
    }
    let _task = update(&mut state, Message::SelectTab(Tab::Projects));
    assert_eq!(state.error, None);
    (state, backend, git, root)
}

/// A child session on the Session tab with every kind of entry an agent's turn leaves, and
/// the folder its project lives in, which must outlive the state.
///
/// The orchestrator `Add gift cards` of the project `shop-app` started the child `Gift
/// card API` in the repository `backend`; the project and that repository each have an
/// `AGENTS.md`. The child got the orchestrator's prompt, thought
/// at length, read one file and wrote two, answered in two paragraphs and now asks for
/// permission to run the tests. Its backend offers a model and an effort, and its context
/// usage is known.
fn child_session() -> (State, TempDir) {
    let root = TempDir::new().unwrap();
    let shop = root.path().join("shop-app");
    for repository in ["backend", "frontend"] {
        fs::create_dir_all(shop.join(repository).join(".git")).unwrap();
    }
    fs::write(shop.join("AGENTS.md"), "rules").unwrap();
    fs::write(shop.join("backend").join("AGENTS.md"), "backend rules").unwrap();
    let backend = RecordingBackend::new();
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(MemoryStore::new()),
        Box::new(MemoryGit::new()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = state.app.open_project(&shop).unwrap();
    let orchestrator = prompted(&mut state, &backend, &project, "Add gift cards");

    let repository = RepositoryId::try_from("backend").unwrap();
    let prompt = "Add a GiftCard entity, repository and REST endpoint. Codes are 16 chars, \
                 balance in cents. Follow AGENTS.md. Report the final API shape back.";
    let id = ProposalId::new();
    report(
        &mut state,
        &orchestrator,
        Update::ChildrenProposed {
            id: id.clone(),
            children: vec![ProposedChild {
                repository: repository.clone(),
                prompt: prompt.into(),
                title: "Gift card API".into(),
            }],
        },
    );
    let choice = ChildChoice {
        repository,
        prompt: prompt.into(),
        title: "Gift card API".into(),
        auto_allow: false,
        branch: BranchChoice::default(),
    };
    let Approval::Started(started) = state
        .app
        .approve_proposal(orchestrator, id, vec![choice])
        .unwrap()
    else {
        panic!("a checked-out child starts at once");
    };
    let child = started[0].clone();
    attach(&mut state, &backend, &child, "b-child");
    let _task = update(
        &mut state,
        Message::Projects(ProjectsMessage::Select(child.clone())),
    );

    let folder = state.app.session_view().unwrap().cwd.clone();
    for update in child_turn(&folder) {
        report(&mut state, &child, update);
    }
    assert_eq!(state.error, None);
    assert_eq!(state.tab, Tab::Session);
    (state, root)
}

/// The options a backend offers: the model `opus` and the effort `high`, each with one
/// other value to pick.
fn picker_options() -> Vec<ConfigOption> {
    let picker = |id: &str, category, current: &str, other: &str| ConfigOption {
        id: ConfigId::new(id),
        name: id.into(),
        category,
        current: ConfigValueId::new(current),
        choices: [current, other]
            .map(|value| ConfigChoice {
                id: ConfigValueId::new(value),
                name: value.into(),
            })
            .into(),
    };
    vec![
        picker("model", ConfigCategory::Model, "opus", "sonnet"),
        picker("effort", ConfigCategory::Effort, "high", "medium"),
    ]
}

/// [`picker_options`] as the update a running agent reports.
fn pickers() -> Update {
    Update::ConfigOptions(picker_options())
}

/// What the agent of [`child_session`]'s child reported in its turn, the child working in
/// `folder`: the options of its backend, a long thought, three calls on files, an answer of
/// two paragraphs with inline code, a permission request and its context usage.
fn child_turn(folder: &Path) -> Vec<Update> {
    let call = |id: &str, kind, title: &str, file: &str| Update::ToolCall {
        id: ToolCallId::new(id),
        title: title.into(),
        kind,
        status: ToolStatus::Completed,
        content: Vec::new(),
        locations: vec![folder.join(file)],
    };
    let option = |id: &str, name: &str, kind| PermissionOption {
        id: PermissionOptionId::new(id),
        name: name.into(),
        kind,
    };
    vec![
        pickers(),
        Update::AgentThought(
            "The repo uses API Platform, so a state processor fits better than a controller. \
             GiftCard as an aggregate root keeps the balance invariant in one place, and the \
             redeem route stays a thin state processor."
                .into(),
        ),
        call(
            "t-1",
            ToolKind::Read,
            "Read Order.php",
            "src/Entity/Order.php",
        ),
        call(
            "t-2",
            ToolKind::Edit,
            "Write GiftCard.php",
            "src/Entity/GiftCard.php",
        ),
        call(
            "t-3",
            ToolKind::Edit,
            "Write RedeemProcessor.php",
            "src/State/RedeemProcessor.php",
        ),
        Update::AgentText(
            "Entity, repository and the `POST /gift-cards/{code}/redeem` processor are in \
             place. The suite has to run before I report the API shape.\n\nNext: migration, \
             then report the API shape to the orchestrator."
                .into(),
        ),
        Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-4"),
            title: "bin/phpunit --testsuite gift-cards".into(),
            kind: ToolKind::Execute,
            options: vec![
                option("allow", "Allow once", PermissionKind::AllowOnce),
                option("always", "Always allow", PermissionKind::AllowAlways),
                option("reject", "Reject", PermissionKind::RejectOnce),
            ],
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        },
        Update::Usage {
            used: 41_260,
            size: 200_000,
        },
    ]
}

/// An orchestrator on the Session tab whose proposed split waits for the user, the backend
/// and the git that record their calls, the orchestrator and the folder its project lives
/// in, which must outlive the state.
///
/// The project `shop-app` has the repositories `backend`, `frontend` and `infra`, each with
/// `main` checked out, a branch `develop` besides and `origin/main` on the remote. The
/// orchestrator `Add gift cards` was made in the new-session sheet, so its children start in
/// Worktree mode on the branch `feat/checkout-fix`. It got the user's prompt, thought at
/// length and proposed a child for `backend` and one for `frontend`; its turn is over, its
/// backend offers a model and an effort, and git has not described the children's
/// repositories yet.
fn proposed_split() -> (State, RecordingBackend, MemoryGit, SessionId, TempDir) {
    let root = TempDir::new().unwrap();
    let shop = fs::canonicalize(root.path()).unwrap().join("shop-app");
    let git = MemoryGit::new();
    for repository in ["backend", "frontend", "infra"].map(|name| shop.join(name)) {
        fs::create_dir_all(repository.join(".git")).unwrap();
        let info = repo_info(&repository, "main", &["develop", "main"], &["origin/main"]);
        git.add_repository(repository, info);
    }
    let backend = RecordingBackend::new();
    let (backends, default) = single_claude();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(MemoryStore::new()),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = state.app.open_project(&shop).unwrap();
    state.app.prepare_session(project, Scope::Project).unwrap();
    warm_up(&mut state, &backend);
    let _task = settle_git(&mut state, &git);
    state.app.draft_mode(BranchMode::Worktree).unwrap();
    state.app.draft_branch("feat/checkout-fix".into()).unwrap();
    let Created::Session(orchestrator) = state.app.create_from_draft().unwrap() else {
        panic!("an orchestrator makes no branch of its own");
    };
    state
        .app
        .submit_prompt(
            "Add gift cards\n\nCustomers buy a code, redeem it at checkout, balance is \
             tracked. Each repo follows its own AGENTS.md.",
            Vec::new(),
        )
        .unwrap();
    let child = |repository: &str, title: &str, prompt: &str| ProposedChild {
        repository: RepositoryId::try_from(repository).unwrap(),
        prompt: prompt.into(),
        title: title.into(),
    };
    let updates = [
        pickers(),
        Update::AgentThought(
            "Read all three AGENTS.md. Infra ships images from main, so no infra change. \
             Frontend depends on backend's endpoint shape, so backend reports it back first."
                .into(),
        ),
        Update::Usage {
            used: 12_300,
            size: 200_000,
        },
    ];
    for update in updates {
        report(&mut state, &orchestrator, update);
    }
    // The proposal comes through the orchestrator's tool, so the session waits for the
    // user once its turn is over.
    let proposal = OrchestratorTool::ProposeChildren(vec![
        child(
            "backend",
            "Gift card API",
            "GiftCard entity, repository, REST endpoint to redeem. Codes 16 chars, balance in \
             cents. Report the final API shape back.",
        ),
        child(
            "frontend",
            "Redeem form",
            "Redeem field in checkout, gift-card product page. Use the endpoint shape backend \
             reports.",
        ),
    ]);
    deliver_tool(&mut state, orchestrator.clone(), proposal);
    end_turn(&mut state, &orchestrator);
    let _task = update(&mut state, Message::SelectTab(Tab::Session));
    assert_eq!(state.error, None);
    (state, backend, git, orchestrator, root)
}

/// Renders the card of [`proposed_split`] once git has described the children's
/// repositories (`orchestrator-split`), with every child's form open
/// (`split-branches`) and while the approval of the split as proposed waits for git
/// (`split-approving`); then lets git make the children's worktrees, which starts them.
fn split_screens(state: &mut State, git: &MemoryGit) {
    let _task = settle_git(state, git);
    render(state, "orchestrator-split");
    let proposal = state
        .session
        .proposals
        .keys()
        .map(|(_, proposal)| proposal.clone())
        .next()
        .unwrap();
    let on_card = |state: &mut State, message: ProposalMessage| {
        let message = SessionMessage::Proposal(proposal.clone(), message);
        let _task = update(state, Message::Session(message));
    };
    on_card(state, ProposalMessage::ToggleEditAll);
    render(state, "split-branches");
    on_card(state, ProposalMessage::Approve);
    render(state, "split-approving");
    let _task = settle_git(state, git);
    assert_eq!(state.error, None);
}

/// Carries [`split_screens`] on: the child in `backend` reports its API back and is done,
/// and the orchestrator says what it does next and prompts the child in `frontend`, which
/// still runs, before its own turn ends.
fn carry_on(state: &mut State, backend: &RecordingBackend, orchestrator: &SessionId) {
    // The view model lists the newest child first: `frontend`, then `backend`.
    let children: Vec<SessionId> = state
        .app
        .session_view()
        .unwrap()
        .children
        .iter()
        .map(|child| child.id.clone())
        .collect();
    let [frontend, api] = children.as_slice() else {
        panic!("the split starts two children");
    };
    for child in [api, frontend] {
        attach(state, backend, child, &child.to_string());
    }
    // The news that its children started was a turn of the orchestrator.
    end_turn(state, orchestrator);

    report(
        state,
        api,
        Update::AgentText(
            "POST /gift-cards/{code}/redeem → 200 {balanceCents, currency}\n\
             GET /gift-cards/{code} → 200 | 404\n\
             Tests green · migration Version20261109 added"
                .into(),
        ),
    );
    report(
        state,
        api,
        Update::Usage {
            used: 31_800,
            size: 200_000,
        },
    );
    end_turn(state, api);

    // The result started the orchestrator's next turn.
    report(
        state,
        orchestrator,
        Update::AgentText(
            "Filled the API shape into frontend's brief and started it. No question for you."
                .into(),
        ),
    );
    deliver_tool(
        state,
        orchestrator.clone(),
        OrchestratorTool::SendToChild {
            child: frontend.clone(),
            prompt: "Redeem field in checkout calls the redeem endpoint; show balance \
                     formatted in the shop's currency and the remaining amount after the \
                     order."
                .into(),
        },
    );
    report(
        state,
        orchestrator,
        Update::Usage {
            used: 19_400,
            size: 200_000,
        },
    );
    end_turn(state, orchestrator);
    report(
        state,
        frontend,
        Update::Usage {
            used: 18_200,
            size: 200_000,
        },
    );
    assert_eq!(state.error, None);
}

/// The live or archived session of `state` titled `title`.
fn titled(state: &State, title: &str) -> SessionId {
    fn find(sessions: &[partitur_core::SessionNode], title: &str) -> Option<SessionId> {
        sessions.iter().find_map(|session| {
            (session.title == title)
                .then(|| session.id.clone())
                .or_else(|| find(&session.children, title))
        })
    }
    state
        .app
        .tree()
        .projects
        .iter()
        .find_map(|project| {
            find(&project.sessions, title).or_else(|| find(&project.archived, title))
        })
        .unwrap()
}

/// Renders archiving and deleting over the state of [`two_projects`]: `Rename config keys`
/// is archived in `taffle`, whose archived sessions are shown, and the cursor is on the done
/// orchestrator `Fix checkout 500`, whose row shows the action strip, while the pointer is
/// over the archived row, which shows its own (`archive-tree`); then
/// the archive sheet for that orchestrator (`archive-sheet`), the archived session on the
/// Session tab (`archived-session`) and the jump panel listing archived sessions too
/// (`jump-archived`).
fn archive_screens(state: &mut State) {
    let renamed = titled(state, "Rename config keys");
    let checkout = titled(state, "Fix checkout 500");
    state.app.archive(renamed.clone()).unwrap();
    let taffle = state.app.tree().projects[1].id.clone();
    state.projects.archived_open.insert(taffle);
    state.projects.cursor = Some(checkout.clone());
    state.projects.hovered = Some(renamed.clone());
    render(state, "archive-tree");
    state.projects.hovered = None;

    let _task = update(
        state,
        Message::Projects(ProjectsMessage::Archive(checkout.clone())),
    );
    render(state, "archive-sheet");
    let _task = update(state, Message::Confirm(ConfirmMessage::Cancel));

    let _task = update(state, Message::Projects(ProjectsMessage::Select(renamed)));
    render(state, "archived-session");

    let _task = update(state, Message::SelectTab(Tab::Projects));
    for message in [JumpMessage::Open, JumpMessage::IncludeArchived(true)] {
        let _task = update(state, Message::Jump(message));
    }
    render(state, "jump-archived");
    let _task = update(state, Message::Jump(JumpMessage::Close));
    assert_eq!(state.error, None);
}

/// Renders the delete sheet for the orchestrator of [`carry_on`], whose children work in
/// worktrees, before git has answered (`delete-sheet-checking`) and once it has
/// (`delete-sheet`); the worktree of the child in `backend` has three uncommitted files and
/// neither branch is merged. Cancelling leaves everything in place.
fn delete_screens(state: &mut State, git: &MemoryGit, orchestrator: &SessionId) {
    let backend = state.app.tree().projects[0].root.join("backend");
    let worktree = git.repository(&backend).unwrap().worktrees[1].path.clone();
    git.set_changes(&worktree, 3);
    let _task = update(
        state,
        Message::Projects(ProjectsMessage::Delete(orchestrator.clone())),
    );
    render(state, "delete-sheet-checking");
    let _task = settle_git(state, git);
    render(state, "delete-sheet");
    let _task = update(state, Message::Confirm(ConfirmMessage::Cancel));
    assert_eq!(state.error, None);
}

/// Answers the latest warm-up `backend` was asked for with [`picker_options`].
fn warm_up(state: &mut State, backend: &RecordingBackend) {
    let (request, session) = backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .map(|(request, session, _)| (request, session.clone()))
        .next_back()
        .unwrap();
    deliver(
        state,
        BackendEvent::SessionCreated {
            request,
            session,
            backend_session: BackendSessionId::new("b-warm"),
            config: picker_options(),
        },
    );
}

/// Feeds each of `messages` to the new-session sheet.
fn on_sheet(state: &mut State, messages: impl IntoIterator<Item = NewSessionMessage>) {
    for message in messages {
        let _task = update(state, Message::NewSession(message));
    }
}

/// Renders the new-session sheet over the state of [`two_projects`]. For the repository
/// `backend` of `shop-app`: in Worktree mode with `feat/login-retry` typed while its agent
/// starts and git has not described the repository yet (`new-session-loading`), once both
/// have answered (`new-session-worktree`) and after Create while git makes the worktree
/// (`new-session-creating`); then, opened afresh, in Checked-out mode
/// (`new-session-checked-out`). Last, for the whole project, an orchestrator, once its agent
/// and git have answered (`new-session-sheet`). Cancelling each leaves nothing behind.
fn new_session_screens(state: &mut State, backend: &RecordingBackend, git: &MemoryGit) {
    let shop = state.app.tree().projects[0].id.clone();
    let repository = Scope::Repository(RepositoryId::try_from("backend").unwrap());
    let open = |state: &mut State, scope: Scope| {
        let _task = update(
            state,
            Message::Projects(ProjectsMessage::NewSession(shop.clone(), scope)),
        );
    };

    open(state, repository.clone());
    on_sheet(
        state,
        [
            NewSessionMessage::Mode(BranchMode::Worktree),
            NewSessionMessage::Branch("feat/login-retry".into()),
        ],
    );
    render(state, "new-session-loading");

    warm_up(state, backend);
    let _task = settle_git(state, git);
    render(state, "new-session-worktree");

    on_sheet(state, [NewSessionMessage::Create]);
    render(state, "new-session-creating");
    on_sheet(state, [NewSessionMessage::Cancel]);
    let _task = settle_git(state, git);

    open(state, repository);
    warm_up(state, backend);
    let _task = settle_git(state, git);
    render(state, "new-session-checked-out");
    on_sheet(state, [NewSessionMessage::Cancel]);

    open(state, Scope::Project);
    warm_up(state, backend);
    let _task = settle_git(state, git);
    render(state, "new-session-sheet");

    on_sheet(state, [NewSessionMessage::Cancel]);
    assert_eq!(state.app.draft(), None);
    assert_eq!(state.error, None);
}

#[test]
#[ignore = "writes PNG files for checking the interface by eye"]
fn the_tabs_in_both_themes() {
    let (mut projects, backend, git, _folder) = two_projects();
    render(&mut projects, "projects");

    // The jump panel over the Projects tab, with a query a few sessions match.
    for message in [JumpMessage::Open, JumpMessage::Query("gift".into())] {
        let _task = update(&mut projects, Message::Jump(message));
    }
    render(&mut projects, "jump");
    let _task = update(&mut projects, Message::Jump(JumpMessage::Close));
    archive_screens(&mut projects);
    new_session_screens(&mut projects, &backend, &git);

    let (mut session, _folder) = child_session();
    render(&mut session, "session");

    // The Env tab of the same child: it has instruction files, a brief and a usage.
    let _task = update(&mut session, Message::SelectTab(Tab::Env));
    render(&mut session, "env");

    let (mut split, backend, git, orchestrator, _folder) = proposed_split();
    split_screens(&mut split, &git);

    carry_on(&mut split, &backend, &orchestrator);
    render(&mut split, "orchestrator-running");
    delete_screens(&mut split, &git, &orchestrator);
}
