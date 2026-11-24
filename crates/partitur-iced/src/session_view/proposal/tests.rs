#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use futures::channel::mpsc;
use iced::event;
use iced::keyboard::key;
use iced::widget::Id;
use iced_test::simulator;
use partitur_core::testing::{
    Call, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, single_claude,
};
use partitur_core::{
    App, BackendEvent, BackendSessionId, BranchMode, Created, Decision, GlobalSettings, ProposalId,
    ProposedChild, RepositoryId, Scope, SessionId, SessionNode, TranscriptRecord,
};
use tempfile::TempDir;

use super::*;
use crate::app::{BareKey, Message, State, Tab, boot, update, view};
use crate::jump::JumpMessage;
use crate::projects_view::ProjectsMessage;
use crate::session_view::SessionMessage;
use crate::test_support::{feed, settle_git, shown};

/// A state whose current session is the orchestrator of a project with the repositories
/// `api`, `docs` and `web`, plus the backend and the git that recorded the calls, the
/// session's id and the project's folder. Git knows each repository with `main` checked
/// out, a branch `dev` besides and `origin/main` on the remote.
struct Fixture {
    state: State,
    backend: RecordingBackend,
    git: MemoryGit,
    session: SessionId,
    root: PathBuf,
    _dir: TempDir,
}

fn repository(id: &str) -> RepositoryId {
    RepositoryId::try_from(id).unwrap()
}

/// The orchestrator started without the new-session sheet, so it keeps no branch
/// settings for its children.
fn fixture() -> Fixture {
    fixture_with(None)
}

/// The orchestrator; with `defaults`, created through the new-session sheet in that mode
/// with that branch name, which its children start from.
fn fixture_with(defaults: Option<(BranchMode, &str)>) -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = partitur_core::canonical(dir.path()).unwrap();
    let git = MemoryGit::new();
    for name in ["api", "docs", "web"] {
        let path = root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        git.add_repository(
            path.clone(),
            repo_info(&path, "main", &["dev", "main"], &["origin/main"]),
        );
    }
    let backend = RecordingBackend::new();
    let (backends, default) = single_claude();
    let memory = MemoryStore::new();
    let files = partitur_core::testing::MemoryFiles::on_disk().with_settings_from(memory.clone());
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(memory.clone()),
        Box::new(memory),
        Box::new(git.clone()),
        Box::new(files.clone()),
        backends,
        default,
        GlobalSettings::default(),
        Box::new(partitur_core::testing::FixedClock::default()),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = partitur_core::testing::open_project(&mut state.app, &files, dir.path()).unwrap();
    if defaults.is_some() {
        state.app.prepare_session(project, Scope::Project).unwrap();
    } else {
        state.app.new_session(project, Scope::Project).unwrap();
    }
    let (request, session) = backend
        .calls()
        .first()
        .and_then(Call::as_new_session)
        .map(|(request, session, _)| (request, session.clone()))
        .unwrap();
    let created = BackendEvent::SessionCreated {
        request,
        session: session.clone(),
        backend_session: BackendSessionId::new("b-1"),
        config: Vec::new(),
    };
    apply(&mut state, [Message::Backend(created)]);
    let session = match defaults {
        None => session,
        Some((mode, branch)) => {
            let _task = settle_git(&mut state, &git);
            state.app.draft_mode(mode).unwrap();
            state.app.draft_branch(branch.into()).unwrap();
            let Created::Session(created) = state.app.create_from_draft().unwrap() else {
                panic!("an orchestrator makes no branch of its own");
            };
            created
        }
    };
    state.tab = Tab::Session;
    assert!(state.app.session_view().unwrap().orchestrator);
    assert_eq!(state.error, None);
    Fixture {
        state,
        backend,
        git,
        session,
        root,
        _dir: dir,
    }
}

/// An orchestrator whose children start in Worktree mode on the branch `feat/x`.
fn worktree_fixture() -> Fixture {
    fixture_with(Some((BranchMode::Worktree, "feat/x")))
}

impl Fixture {
    /// The orchestrator proposes an `api` child and a `web` child; returns the proposal.
    fn propose(&mut self) -> ProposalId {
        let id = ProposalId::new();
        let proposed = TranscriptRecord::ChildrenProposed {
            id: id.clone(),
            children: vec![
                ProposedChild {
                    repository: repository("api"),
                    prompt: "Add the endpoint".into(),
                    title: "API endpoint".into(),
                    title_source: partitur_core::TitleSource::Given,
                    base: None,
                },
                ProposedChild {
                    repository: repository("web"),
                    prompt: "Show the endpoint's data".into(),
                    title: "Web page".into(),
                    title_source: partitur_core::TitleSource::Given,
                    base: None,
                },
            ],
        };
        feed(&mut self.state, &self.session, proposed);
        id
    }

    fn draft(&self, id: &ProposalId) -> Option<&ProposalDraft> {
        self.state
            .session
            .proposals
            .get(&(self.session.clone(), id.clone()))
    }

    fn new_sessions(&self) -> usize {
        self.backend
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::NewSession { .. }))
            .count()
    }

    /// The titles of the orchestrator's children in the tree, sorted.
    fn child_titles(&self) -> Vec<String> {
        let mut titles: Vec<String> = self.children().into_iter().map(|node| node.title).collect();
        titles.sort_unstable();
        titles
    }

    /// Whether the orchestrator's child titled `title` grants permission requests without
    /// asking.
    fn child_auto_allows(&mut self, title: &str) -> bool {
        let child = self
            .children()
            .into_iter()
            .find(|node| node.title == title)
            .unwrap();
        self.auto_allows(&child.id)
    }

    /// The orchestrator's children in the tree.
    fn children(&self) -> Vec<SessionNode> {
        let tree = self.state.app.tree();
        let project = tree.projects.first().unwrap();
        let orchestrator = project
            .sessions
            .iter()
            .find(|node| node.id == self.session)
            .unwrap();
        orchestrator.children.clone()
    }

    /// Whether the session `id` grants permission requests without asking.
    fn auto_allows(&mut self, id: &SessionId) -> bool {
        self.state.app.select_session(id.clone()).unwrap();
        let auto_allow = self.state.app.session_view().unwrap().auto_allow;
        self.state.app.select_session(self.session.clone()).unwrap();
        auto_allow
    }

    /// Clicks the widget `target` and applies the messages the click sends, which it
    /// returns.
    fn click(&mut self, target: impl Into<Id>) -> Vec<Message> {
        let mut ui = simulator(view(&self.state));
        ui.click(target.into()).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        apply(&mut self.state, messages.clone());
        messages
    }

    /// Clicks the text `label` and applies the messages the click sends, which it
    /// returns.
    fn click_text(&mut self, label: &str) -> Vec<Message> {
        let mut ui = simulator(view(&self.state));
        ui.click(label).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        apply(&mut self.state, messages.clone());
        messages
    }

    /// The branch settings the orchestrator keeps for its children.
    fn defaults(&self) -> BranchChoice {
        self.state
            .app
            .session_view()
            .unwrap()
            .child_defaults
            .unwrap()
    }

    /// Lets git answer everything it was asked, and what those answers ask in turn.
    fn settle(&mut self) {
        let _task = settle_git(&mut self.state, &self.git);
    }

    /// How many texts on screen read `label`.
    fn count(&self, label: &str) -> usize {
        shown(&mut simulator(view(&self.state)), label)
    }

    /// What the card checked about the child at `index`.
    fn check(&self, id: &ProposalId, index: usize) -> ChildCheck {
        self.draft(id).unwrap().split.checks.as_ref().unwrap()[index].clone()
    }

    /// What git was asked so far: the calls it answered and the requests waiting.
    fn git_asked(&self) -> (usize, usize) {
        (self.git.calls().len(), self.git.waiting())
    }

    /// Which of the forms of the children at the positions 0, 1 and 2 are on screen,
    /// told by their title fields.
    fn forms(&self, id: &ProposalId) -> [bool; 3] {
        let mut ui = simulator(view(&self.state));
        [0, 1, 2].map(|index| ui.find(widget(id, &format!("title-{index}"))).is_ok())
    }
}

/// Feeds every message through `update`, dropping the tasks: the simulator cannot run them.
fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
    for message in messages {
        let _task = update(state, message);
    }
}

fn proposal(id: &ProposalId, message: ProposalMessage) -> Message {
    Message::Session(SessionMessage::Proposal(id.clone(), message))
}

fn widget(id: &ProposalId, name: &str) -> Id {
    Id::from(format!("proposal-{id}-{name}"))
}

#[test]
fn a_pending_proposal_gets_a_draft_and_lists_its_children_and_the_skipped_repository() {
    let mut fixture = fixture();
    let id = fixture.propose();

    let draft = fixture.draft(&id).unwrap();
    let repositories: Vec<_> = draft.split.children.iter().map(|c| &c.repository).collect();
    assert_eq!(repositories, [&repository("api"), &repository("web")]);
    assert_eq!(draft.split.children[0].title, "API endpoint");
    assert_eq!(draft.split.children[0].prompt.trim(), "Add the endpoint");
    assert!(!draft.split.children[0].auto_allow);
    assert_eq!(draft.split.note, "");
    assert_eq!(draft.editing, Editing::Nothing);

    let mut ui = simulator(view(&fixture.state));
    ui.find("Proposed split").unwrap();
    ui.find("2 child sessions · 1 repo untouched").unwrap();
    ui.find(HINT).unwrap();
    assert_eq!(HINT, "click a row to edit its brief");
    for column in ["Repo", "Brief", "Starts"] {
        ui.find(column).unwrap();
    }
    // The children's rows, each with its repository, its title over its prompt.
    ui.find(widget(&id, "row-0")).unwrap();
    ui.find(widget(&id, "row-1")).unwrap();
    ui.find("api").unwrap();
    ui.find("web").unwrap();
    ui.find("API endpoint").unwrap();
    ui.find("Add the endpoint").unwrap();
    ui.find("Show the endpoint's data").unwrap();
    assert_eq!(shown(&mut ui, "starts now"), 2);
    // The repository without a child.
    ui.find(widget(&id, "skipped-docs")).unwrap();
    ui.find("docs").unwrap();
    assert_eq!(shown(&mut ui, "—"), 1);
    assert_eq!(shown(&mut ui, "skipped"), 1);
    ui.find("Approve split").unwrap();
    ui.find("Edit split").unwrap();
    ui.find("Reject").unwrap();
    // The key on the approve button, and the one that sends the prompt.
    assert_eq!(shown(&mut ui, "Enter"), 2);
    ui.find(widget(&id, "note")).unwrap();
    ui.find(REJECTION_NOTE).unwrap();
    assert_eq!(REJECTION_NOTE, "rejection note (optional)");
    // No form is open, and the picker belongs to editing the whole split.
    drop(ui);
    assert_eq!(fixture.forms(&id), [false, false, false]);
    let mut ui = simulator(view(&fixture.state));
    assert!(ui.find(widget(&id, "add")).is_err());
    assert!(ui.find("Done editing").is_err());
}

#[test]
fn the_header_counts_the_children_and_the_untouched_repositories() {
    assert_eq!(summary(1, 0), "1 child session");
    assert_eq!(summary(2, 0), "2 child sessions");
    assert_eq!(summary(1, 1), "1 child session · 1 repo untouched");
    assert_eq!(summary(0, 3), "0 child sessions · 3 repos untouched");
}

#[test]
fn a_click_on_a_childs_row_turns_it_into_its_form_one_row_at_a_time() {
    let mut fixture = fixture();
    let id = fixture.propose();

    let messages = fixture.click(widget(&id, "row-1"));
    assert_eq!(messages, [proposal(&id, ProposalMessage::OpenRow(1))]);
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::Row(1));
    assert_eq!(fixture.forms(&id), [false, true, false]);
    let mut ui = simulator(view(&fixture.state));
    for part in ["title-1", "prompt-1", "auto-1", "remove-1", "done-1"] {
        ui.find(widget(&id, part)).unwrap();
    }
    ui.find("remove").unwrap();
    // The form stands for the row, the other child keeps its row.
    assert!(ui.find(widget(&id, "row-1")).is_err());
    assert_eq!(shown(&mut ui, "starts now"), 1);
    drop(ui);

    let messages = fixture.click(widget(&id, "row-0"));
    assert_eq!(messages, [proposal(&id, ProposalMessage::OpenRow(0))]);
    assert_eq!(fixture.forms(&id), [true, false, false]);

    let messages = fixture.click_text("done");
    assert_eq!(messages, [proposal(&id, ProposalMessage::CloseRow)]);
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::Nothing);
    assert_eq!(fixture.forms(&id), [false, false, false]);

    // A row that does not exist opens nothing.
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::OpenRow(2))],
    );
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::Nothing);
    assert_eq!(fixture.state.error, None);
}

#[test]
fn a_click_on_a_skipped_row_adds_a_child_and_opens_its_form() {
    let mut fixture = fixture();
    let id = fixture.propose();

    let messages = fixture.click(widget(&id, "skipped-docs"));

    assert_eq!(
        messages,
        [proposal(&id, ProposalMessage::Add(repository("docs")))]
    );
    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children.len(), 3);
    assert_eq!(draft.split.children[2].repository, repository("docs"));
    assert_eq!(draft.split.children[2].title, "");
    assert_eq!(draft.split.children[2].prompt.trim(), "");
    assert_eq!(draft.editing, Editing::Row(2));
    assert_eq!(fixture.forms(&id), [false, false, true]);
    let mut ui = simulator(view(&fixture.state));
    ui.find("3 child sessions").unwrap();
    assert!(ui.find("skipped").is_err());
    drop(ui);

    // Left as it is, the new child's row says that its prompt is missing.
    fixture.click_text("done");
    let mut ui = simulator(view(&fixture.state));
    ui.find(NO_BRIEF).unwrap();
    assert_eq!(NO_BRIEF, "no brief yet");
}

#[test]
fn edit_split_opens_every_form_and_the_picker_until_it_is_pressed_again() {
    let mut fixture = fixture();
    let id = fixture.propose();
    fixture.click(widget(&id, "row-1"));

    let messages = fixture.click_text("Edit split");
    assert_eq!(messages, [proposal(&id, ProposalMessage::ToggleEditAll)]);
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::All);
    assert_eq!(fixture.forms(&id), [true, true, false]);
    let mut ui = simulator(view(&fixture.state));
    ui.find("Done editing").unwrap();
    assert!(ui.find("Edit split").is_err());
    // Leaving one form makes no sense while all are open.
    assert!(ui.find("done").is_err());
    assert_eq!(shown(&mut ui, "remove"), 2);
    // A pick list exposes neither its placeholder nor its menu entries to selectors, so
    // the picker is found by its id and its entries are checked directly.
    ui.find(widget(&id, "add")).unwrap();
    assert_eq!(ADD_CHILD, "add child");
    let model = fixture.state.app.session_view().unwrap();
    let names: Vec<String> = add_choices(&model)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, ["api", "docs", "web"]);
    drop((ui, model));

    // A child added now is one more open form, and one closed alone changes nothing.
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Add(repository("web"))),
            proposal(&id, ProposalMessage::CloseRow),
        ],
    );
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::All);
    assert_eq!(fixture.forms(&id), [true, true, true]);
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::Remove(2))],
    );
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::All);

    let messages = fixture.click_text("Done editing");
    assert_eq!(messages, [proposal(&id, ProposalMessage::ToggleEditAll)]);
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::Nothing);
    assert_eq!(fixture.forms(&id), [false, false, false]);
    let mut ui = simulator(view(&fixture.state));
    ui.find("Edit split").unwrap();
    assert!(ui.find(widget(&id, "add")).is_err());
    assert_eq!(fixture.state.error, None);
}

#[test]
fn removing_and_adding_children_edits_the_draft() {
    let mut fixture = fixture();
    let id = fixture.propose();
    fixture.click(widget(&id, "row-0"));

    let messages = fixture.click(widget(&id, "remove-0"));
    assert_eq!(messages, [proposal(&id, ProposalMessage::Remove(0))]);
    // The form that was open belonged to the removed child.
    assert_eq!(fixture.draft(&id).unwrap().editing, Editing::Nothing);
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Add(repository("web"))),
            // A child that does not exist is not removed.
            proposal(&id, ProposalMessage::Remove(2)),
        ],
    );

    let draft = fixture.draft(&id).unwrap();
    let repositories: Vec<_> = draft.split.children.iter().map(|c| &c.repository).collect();
    assert_eq!(repositories, [&repository("web"), &repository("web")]);
    assert_eq!(draft.split.children[0].title, "Web page");
    assert_eq!(draft.split.children[1].title, "");
    assert_eq!(draft.split.children[1].prompt.trim(), "");
    assert_eq!(draft.editing, Editing::Row(1));
    let mut ui = simulator(view(&fixture.state));
    ui.find("2 child sessions · 2 repos untouched").unwrap();
    assert!(ui.find("API endpoint").is_err());
    assert_eq!(shown(&mut ui, "skipped"), 2);
    assert_eq!(fixture.state.error, None);
}

#[test]
fn approving_sends_the_draft_to_the_app() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();
    fixture.click(widget(&id, "row-1"));

    let messages = fixture.click(widget(&id, "auto-1"));
    assert_eq!(
        messages,
        [proposal(&id, ProposalMessage::SetAutoAllow(1, true))]
    );
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::EditTitle(0, "  Endpoint v2 ".into())),
            proposal(&id, ProposalMessage::EditNote("ship it".into())),
        ],
    );
    let messages = fixture.click_text("Approve split");
    assert_eq!(messages, [proposal(&id, ProposalMessage::Approve)]);

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before + 2);
    assert_eq!(fixture.child_titles(), ["Endpoint v2", "Web page"]);
    assert!(!fixture.child_auto_allows("Endpoint v2"));
    assert!(fixture.child_auto_allows("Web page"));
    assert!(fixture.draft(&id).is_none());
    let mut ui = simulator(view(&fixture.state));
    ui.find("You approved the split. Started").unwrap();
    assert!(ui.find("Approve split").is_err());
    assert!(ui.find("Proposed split").is_err());
}

#[test]
fn approving_without_edits_uses_the_proposal_and_the_orchestrators_toggle() {
    let mut fixture = fixture();
    apply(
        &mut fixture.state,
        [Message::Session(SessionMessage::SetAutoAllow(true))],
    );
    let id = fixture.propose();
    assert!(fixture.draft(&id).unwrap().split.children[1].auto_allow);

    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::Approve)],
    );

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.child_titles(), ["API endpoint", "Web page"]);
    assert!(fixture.child_auto_allows("API endpoint"));
    assert!(fixture.child_auto_allows("Web page"));
    assert!(fixture.draft(&id).is_none());
}

#[test]
fn rejecting_with_a_note_records_it() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::EditNote("not now".into()))],
    );

    let messages = fixture.click_text("Reject");
    assert_eq!(messages, [proposal(&id, ProposalMessage::Reject)]);

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_none());
    let mut ui = simulator(view(&fixture.state));
    ui.find("You rejected the split.").unwrap();
    ui.find("not now").unwrap();
    assert!(ui.find("Reject").is_err());
}

#[test]
fn an_approve_the_app_refuses_shows_the_error_and_keeps_the_draft() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Remove(0)),
            proposal(&id, ProposalMessage::Remove(0)),
        ],
    );

    let mut ui = simulator(view(&fixture.state));
    ui.find("0 child sessions · 3 repos untouched").unwrap();
    drop(ui);
    assert_eq!(
        fixture.click_text("Approve split"),
        [],
        "Approve is disabled"
    );
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::Approve)],
    );

    let error = fixture.state.error.clone().unwrap();
    assert!(
        error.starts_with("cannot start these children: "),
        "{error}"
    );
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).unwrap().split.children.is_empty());
    let mut ui = simulator(view(&fixture.state));
    ui.find("Reject").unwrap();
}

#[test]
fn enter_approves_the_newest_undecided_proposal_on_the_session_tab() {
    let mut fixture = fixture();
    let older = fixture.propose();
    let newer = fixture.propose();
    let before = fixture.new_sessions();

    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before + 2);
    assert!(fixture.draft(&newer).is_none());
    assert!(fixture.draft(&older).is_some());

    // The next Enter decides the proposal that is the newest undecided one now.
    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
    assert_eq!(fixture.new_sessions(), before + 4);
    assert!(fixture.draft(&older).is_none());

    // With nothing left to decide, Enter does nothing.
    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
    assert_eq!(fixture.new_sessions(), before + 4);
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.state.tab, Tab::Session);
}

#[test]
fn enter_approves_nothing_on_another_tab_or_under_the_jump_panel() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();

    for tab in [Tab::Projects, Tab::Env] {
        fixture.state.tab = tab;
        apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
        assert_eq!(fixture.new_sessions(), before, "{tab:?}");
        assert!(fixture.draft(&id).is_some(), "{tab:?}");
        fixture.state.tab = Tab::Session;
    }

    // The panel takes Enter for its highlighted session.
    apply(
        &mut fixture.state,
        [
            Message::Jump(JumpMessage::Open),
            Message::Key(BareKey::Enter),
        ],
    );
    assert_eq!(fixture.state.jump, None);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_some());
    assert_eq!(fixture.state.error, None);
}

#[test]
fn enter_approves_no_split_without_children() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Remove(0)),
            proposal(&id, ProposalMessage::Remove(0)),
            Message::Key(BareKey::Enter),
        ],
    );

    // Like the disabled button, the key does not even ask the app.
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_some());
}

#[test]
fn the_cards_fields_keep_enter_to_themselves() {
    let mut fixture = fixture();
    let id = fixture.propose();
    fixture.click(widget(&id, "row-0"));

    // A key a field takes never reaches the window, where Enter approves the split.
    let mut ui = simulator(view(&fixture.state));
    assert_eq!(ui.tap_key(key::Named::Enter), event::Status::Ignored);
    for field in ["title-0", "prompt-0", "note"] {
        ui.click(widget(&id, field)).unwrap();
        assert_eq!(
            ui.tap_key(key::Named::Enter),
            event::Status::Captured,
            "{field}"
        );
    }
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(!messages.contains(&proposal(&id, ProposalMessage::Approve)));
    apply(&mut fixture.state, messages);

    // The title and the note are as they were; the prompt got its line break.
    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children[0].title, "API endpoint");
    assert_eq!(draft.split.note, "");
    assert_eq!(draft.split.children.len(), 2);
}

#[test]
fn a_proposal_without_a_draft_is_shown_read_only() {
    let mut fixture = fixture();
    let id = fixture.propose();
    fixture.state.session.proposals.clear();

    let mut ui = simulator(view(&fixture.state));
    ui.find("Proposed split").unwrap();
    ui.find("2 child sessions · 1 repo untouched").unwrap();
    ui.find("API endpoint").unwrap();
    ui.find("Show the endpoint's data").unwrap();
    ui.find("skipped").unwrap();
    assert!(ui.find(HINT).is_err());
    assert!(ui.find("Approve split").is_err());
    assert!(ui.find(widget(&id, "row-0")).is_err());
    // A click on a row sends nothing.
    ui.click("API endpoint").unwrap();
    ui.click("skipped").unwrap();
    assert_eq!(ui.into_messages().count(), 0);
}

#[test]
fn closing_the_project_drops_its_drafts() {
    let mut fixture = fixture();
    let id = fixture.propose();
    assert!(fixture.draft(&id).is_some());
    let project = fixture.state.app.tree().projects[0].id.clone();

    apply(
        &mut fixture.state,
        [Message::Projects(ProjectsMessage::Close(project))],
    );

    assert_eq!(fixture.state.error, None);
    assert!(fixture.state.session.proposals.is_empty());
}

#[test]
fn enter_does_nothing_on_an_archived_orchestrator() {
    let mut fixture = fixture();
    let id = fixture.propose();
    let before = fixture.new_sessions();
    let orchestrator = fixture.state.app.current_session().unwrap();
    fixture.state.app.archive(orchestrator).unwrap();

    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_some());
}

#[test]
fn every_child_starts_from_the_orchestrators_branch_choice() {
    let mut fixture = worktree_fixture();
    let defaults = fixture.defaults();
    assert_eq!(defaults.mode, BranchMode::Worktree);
    assert_eq!(defaults.branch, "feat/x");

    let id = fixture.propose();

    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children[0].branch, defaults);
    assert_eq!(draft.split.children[1].branch, defaults);
    let branches: Vec<BranchChoice> = draft
        .split
        .choices()
        .into_iter()
        .map(|choice| choice.branch)
        .collect();
    assert_eq!(branches, [defaults.clone(), defaults]);

    // An orchestrator that keeps no settings for its children starts them checked out.
    let mut plain = fixture_with(None);
    let id = plain.propose();
    assert_eq!(
        plain.draft(&id).unwrap().split.children[0].branch,
        BranchChoice::default()
    );
}

#[test]
fn a_value_that_still_equals_the_orchestrators_is_marked() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    fixture.click(widget(&id, "row-0"));

    // The mode, the branch name, the base and the fetch checkbox.
    assert_eq!(fixture.count("from orchestrator"), 4);
    let mut ui = simulator(view(&fixture.state));
    for part in [
        "mode-0-worktree",
        "mode-0-new-branch",
        "mode-0-checked-out",
        "branch-0",
        "base-0",
        "fetch-0",
    ] {
        ui.find(widget(&id, part)).unwrap();
    }
    let worktree = fixture.check(&id, 0).worktree.unwrap();
    let written = crate::home_path::abbreviated(&worktree, std::env::home_dir().as_deref());
    assert_eq!(shown(&mut ui, &written), 1);
    assert_eq!(shown(&mut ui, "Fetch origin and start from origin/main"), 1);
    drop(ui);

    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::EditBranch(0, "feat/y".into()),
        )],
    );
    assert_eq!(fixture.count("from orchestrator"), 3);
    assert_eq!(
        fixture.draft(&id).unwrap().split.children[0].branch.branch,
        "feat/y"
    );
    // The check follows the edit.
    assert!(
        fixture
            .check(&id, 0)
            .worktree
            .unwrap()
            .ends_with("api-wt/feat/y")
    );

    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::SetFetch(0, false)),
            proposal(&id, ProposalMessage::SetBase(0, "dev".into())),
        ],
    );
    assert_eq!(fixture.count("from orchestrator"), 1);
    assert_eq!(fixture.check(&id, 0).base.as_deref(), Some("dev"));
    assert!(!fixture.check(&id, 0).fetch);

    // Checked-out mode needs no name, base or fetch.
    let messages = fixture.click(widget(&id, "mode-0-checked-out"));
    assert_eq!(
        messages,
        [proposal(
            &id,
            ProposalMessage::SetMode(0, BranchMode::CheckedOut)
        )]
    );
    assert_eq!(fixture.count("from orchestrator"), 0);
    let mut ui = simulator(view(&fixture.state));
    assert!(ui.find(widget(&id, "branch-0")).is_err());
    assert!(ui.find(widget(&id, "base-0")).is_err());
    assert_eq!(fixture.state.error, None);
}

#[test]
fn a_child_added_from_a_skipped_row_starts_from_the_orchestrator_too() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    let defaults = fixture.defaults();

    fixture.click(widget(&id, "skipped-docs"));
    assert_eq!(
        fixture.draft(&id).unwrap().split.children[2].branch,
        defaults
    );

    // So does one added from the picker of `Edit split`.
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::ToggleEditAll),
            proposal(&id, ProposalMessage::Add(repository("web"))),
        ],
    );
    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children[3].branch, defaults);
    assert_eq!(draft.split.checks.as_ref().unwrap().len(), 4);
}

#[test]
fn a_row_with_a_problem_says_why_and_approve_is_disabled() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    assert_eq!(fixture.check(&id, 0).problem, None);

    // A second child in `api` wants the same branch there.
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Add(repository("api"))),
            proposal(&id, ProposalMessage::CloseRow),
        ],
    );
    let problem = fixture.check(&id, 2).problem.unwrap();
    assert!(problem.contains("feat/x"), "{problem}");
    assert!(fixture.count(&problem) >= 1);
    // Each row names its mode and branch.
    assert_eq!(fixture.count("Worktree"), 3);
    assert_eq!(fixture.count("feat/x"), 3);

    let asked = fixture.git_asked();
    let before = fixture.new_sessions();
    assert_eq!(fixture.click_text("Approve split"), []);
    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
    assert_eq!(fixture.git_asked(), asked);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_some());
    assert_eq!(fixture.state.error, None);

    // Without the clash the split can be approved again.
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::Remove(2))],
    );
    assert_eq!(fixture.count(&problem), 0);
    assert_eq!(fixture.click_text("Approve split").len(), 1);
}

#[test]
fn a_blocked_split_says_next_to_approve_which_child_blocks_it() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    assert_eq!(fixture.count("Cannot approve"), 0);

    // A bad branch name in `web` blocks the split, and the line names `web`.
    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::EditBranch(1, "bad name".into()),
        )],
    );
    let problem = fixture.check(&id, 1).problem.unwrap();
    let alone = format!("Cannot approve: web: {problem}");
    assert_eq!(fixture.count(&alone), 1);

    // A second `api` child that wants `feat/x` there too comes after it.
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Add(repository("api"))),
            proposal(&id, ProposalMessage::CloseRow),
        ],
    );
    assert!(fixture.check(&id, 2).problem.is_some());
    assert_eq!(fixture.count(&alone), 0);
    assert_eq!(fixture.count(&format!("{alone} (and 1 more)")), 1);

    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::Remove(2)),
            proposal(&id, ProposalMessage::EditBranch(1, "feat/x".into())),
        ],
    );
    assert_eq!(fixture.count("Cannot approve"), 0);
}

#[test]
fn approving_sends_each_childs_branch_choice() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    let defaults = fixture.defaults();
    fixture.click(widget(&id, "row-1"));
    let messages = fixture.click(widget(&id, "mode-1-new-branch"));
    assert_eq!(
        messages,
        [proposal(
            &id,
            ProposalMessage::SetMode(1, BranchMode::NewBranch)
        )]
    );
    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::EditBranch(1, "feat/web".into()),
        )],
    );
    let worktree = fixture.check(&id, 0).worktree.unwrap();
    let branches: Vec<BranchChoice> = fixture
        .draft(&id)
        .unwrap()
        .split
        .choices()
        .into_iter()
        .map(|choice| choice.branch)
        .collect();
    assert_eq!(
        branches,
        [
            defaults.clone(),
            BranchChoice {
                mode: BranchMode::NewBranch,
                branch: "feat/web".into(),
                ..defaults
            }
        ]
    );
    let before = fixture.backend.calls().len();

    assert_eq!(
        fixture.click_text("Approve split"),
        [proposal(&id, ProposalMessage::Approve)]
    );
    fixture.settle();

    assert_eq!(fixture.state.error, None);
    let cwds: Vec<PathBuf> = fixture.backend.calls()[before..]
        .iter()
        .filter_map(Call::as_new_session)
        .map(|(_, _, cwd)| cwd.to_owned())
        .collect();
    assert_eq!(cwds, [worktree, fixture.root.join("web")]);
    let model = fixture.state.app.session_view().unwrap();
    let approved = model
        .entries
        .iter()
        .find_map(|entry| match entry {
            Entry::Proposal {
                decision: Some(Decision::Approved(approved)),
                ..
            } => Some(approved.clone()),
            _ => None,
        })
        .unwrap();
    let made: Vec<(BranchMode, Option<String>)> = approved
        .into_iter()
        .map(|child| (child.mode, child.branch))
        .collect();
    assert_eq!(
        made,
        [
            (BranchMode::Worktree, Some("feat/x".into())),
            (BranchMode::NewBranch, Some("feat/web".into()))
        ]
    );
}

#[test]
fn the_branch_field_keeps_enter_to_itself() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    fixture.click(widget(&id, "row-0"));

    let mut ui = simulator(view(&fixture.state));
    for field in ["title-0", "prompt-0", "branch-0", "note"] {
        ui.click(widget(&id, field)).unwrap();
        assert_eq!(
            ui.tap_key(key::Named::Enter),
            event::Status::Captured,
            "{field}"
        );
    }
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(!messages.contains(&proposal(&id, ProposalMessage::Approve)));
    apply(&mut fixture.state, messages);

    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children[0].branch.branch, "feat/x");
    assert_eq!(fixture.state.error, None);
}

#[test]
fn the_card_says_loading_until_the_branches_arrive() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();

    // Both children make a branch, so both wait for their repositories.
    assert_eq!(fixture.count("loading…"), 2);
    assert!(fixture.check(&id, 0).loading);
    assert_eq!(fixture.click_text("Approve split"), []);
    let before = fixture.new_sessions();
    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
    assert_eq!(fixture.new_sessions(), before);
    assert!(fixture.draft(&id).is_some());
    assert_eq!(fixture.state.error, None);
    // An open form says it in its base and fetch rows.
    fixture.click(widget(&id, "row-0"));
    assert_eq!(fixture.count("loading…"), 3);

    fixture.settle();

    assert_eq!(fixture.count("loading…"), 0);
    assert!(!fixture.check(&id, 0).loading);
    assert_eq!(
        fixture.click_text("Approve split"),
        [proposal(&id, ProposalMessage::Approve)]
    );
}

#[test]
fn unreadable_repositories_settle_until_the_user_edits_the_card() {
    let mut fixture = worktree_fixture();
    for _ in 0..10 {
        for name in ["api", "web"] {
            fixture
                .git
                .fail_next(GitOp::Inspect, &fixture.root.join(name));
        }
    }
    let before = fixture.git.calls().len();
    let first = fixture.propose();
    let second = fixture.propose();

    fixture.settle();

    // Each card read both repositories once, and shows why it failed.
    assert_eq!(fixture.git.calls().len() - before, 4);
    assert!(fixture.check(&first, 0).problem.is_some());
    assert!(fixture.check(&second, 1).problem.is_some());
    apply(
        &mut fixture.state,
        [proposal(
            &first,
            ProposalMessage::EditBranch(0, "feat/y".into()),
        )],
    );
    assert!(fixture.check(&first, 0).loading);
    assert_eq!(fixture.git.calls().len() - before, 6);
}

#[test]
fn approving_shows_approving_and_locks_the_card() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    fixture.click(widget(&id, "row-0"));

    assert_eq!(
        fixture.click_text("Approve split"),
        [proposal(&id, ProposalMessage::Approve)]
    );
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.count("approving…"), 1);
    assert_eq!(fixture.count("Approve split"), 0);
    // Only the composer still offers Enter.
    assert_eq!(fixture.count("Enter"), 1);

    let asked = fixture.git_asked();
    assert_eq!(fixture.click_text("approving…"), []);
    assert_eq!(fixture.click_text("Reject"), []);
    assert_eq!(fixture.click_text("Edit split"), []);
    assert_eq!(fixture.click(widget(&id, "done-0")), []);
    assert_eq!(fixture.click(widget(&id, "row-1")), []);
    assert_eq!(fixture.click(widget(&id, "skipped-docs")), []);
    assert_eq!(fixture.click(widget(&id, "remove-0")), []);
    assert_eq!(fixture.click(widget(&id, "mode-0-checked-out")), []);
    let mut ui = simulator(view(&fixture.state));
    ui.click(widget(&id, "branch-0")).unwrap();
    ui.typewrite("y");
    ui.click(widget(&id, "note")).unwrap();
    ui.typewrite("n");
    assert_eq!(ui.into_messages().count(), 0);
    apply(&mut fixture.state, [Message::Key(BareKey::Enter)]);
    // A message sent anyway changes nothing.
    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::EditBranch(0, "feat/z".into())),
            proposal(&id, ProposalMessage::Remove(1)),
        ],
    );
    assert_eq!(fixture.git_asked(), asked);
    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children.len(), 2);
    assert_eq!(draft.split.children[0].branch.branch, "feat/x");
    assert_eq!(fixture.state.error, None);

    // The rest of the window works as usual.
    apply(&mut fixture.state, [Message::SelectTab(Tab::Projects)]);
    assert_eq!(fixture.state.tab, Tab::Projects);
    apply(&mut fixture.state, [Message::SelectTab(Tab::Session)]);

    // Once the fresh reading is in, the approval's own repositories are busy with it,
    // which the card does not count against it.
    for event in fixture.git.answer() {
        apply(&mut fixture.state, [Message::Git(event)]);
    }
    // Neither the open form nor the other row says so.
    for (index, busy) in [
        (0, "api is busy with another branch change"),
        (1, "web is busy with another branch change"),
    ] {
        assert_eq!(fixture.check(&id, index).problem.as_deref(), Some(busy));
        assert_eq!(fixture.count(busy), 0, "{busy}");
    }
    // Nor does the line by the buttons.
    assert_eq!(
        fixture.count("Cannot approve: api: api is busy with another branch change (and 1 more)"),
        0
    );
    assert_eq!(fixture.count("approving…"), 1);

    fixture.settle();

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.child_titles(), ["API endpoint", "Web page"]);
    let mut ui = simulator(view(&fixture.state));
    ui.find("You approved the split. Started").unwrap();
    assert!(ui.find("approving…").is_err());
    assert!(ui.find("Proposed split").is_err());
    // The decided proposal needs its draft no more.
    assert!(fixture.draft(&id).is_none());
}

#[test]
fn a_failed_approval_keeps_the_card_with_the_error() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    // The worktree in `api` is made, the one in `web` fails, so the first is undone.
    fixture
        .git
        .fail_next(GitOp::AddWorktree, &fixture.root.join("web"));
    let before = fixture.new_sessions();

    fixture.click_text("Approve split");
    fixture.settle();

    let error = fixture
        .state
        .app
        .session_view()
        .unwrap()
        .approval
        .unwrap()
        .error
        .unwrap();
    assert_eq!(fixture.count(&error), 1);
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.new_sessions(), before);
    assert_eq!(fixture.draft(&id).unwrap().split.children.len(), 2);
    // The card checked the repositories again once the undo was done.
    assert!(!fixture.check(&id, 0).loading);
    assert_eq!(fixture.check(&id, 0).problem, None);
    assert_eq!(
        fixture.click_text("Approve split"),
        [proposal(&id, ProposalMessage::Approve)]
    );
    assert_eq!(fixture.count("approving…"), 1);
}

#[test]
fn the_base_and_fetch_are_marked_as_they_apply() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    fixture.click(widget(&id, "row-0"));
    assert_eq!(fixture.count("from orchestrator"), 4);

    // `dev` has no remote counterpart, so the fetch the child still wants does not apply.
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::SetBase(0, "dev".into()))],
    );

    assert!(fixture.draft(&id).unwrap().split.children[0].branch.fetch);
    assert!(!fixture.check(&id, 0).fetch);
    assert_eq!(fixture.count("from orchestrator"), 2);
}

#[test]
fn archiving_the_session_in_a_childs_checkout_clears_its_problem() {
    let mut fixture = worktree_fixture();
    let project = fixture.state.app.tree().projects[0].id.clone();
    let web = fixture
        .state
        .app
        .new_session(project, Scope::Repository(repository("web")))
        .unwrap();
    fixture
        .state
        .app
        .select_session(fixture.session.clone())
        .unwrap();
    let id = fixture.propose();
    fixture.settle();
    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::SetMode(1, BranchMode::NewBranch),
        )],
    );
    let problem = fixture.check(&id, 1).problem.unwrap();
    assert!(problem.starts_with("web is in use by"), "{problem}");
    assert_eq!(fixture.count(&problem), 1);
    assert_eq!(fixture.click_text("Approve split"), []);

    apply(
        &mut fixture.state,
        [
            Message::Projects(ProjectsMessage::Archive(web)),
            Message::Confirm(crate::confirm_sheet::ConfirmMessage::Confirm),
        ],
    );

    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.check(&id, 1).problem, None);
    assert_eq!(fixture.count(&problem), 0);
    assert_eq!(
        fixture.click_text("Approve split"),
        [proposal(&id, ProposalMessage::Approve)]
    );
}

#[test]
fn a_session_made_in_a_childs_checkout_shows_on_the_card() {
    use crate::new_session_sheet::NewSessionMessage;

    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    fixture.settle();
    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::SetMode(1, BranchMode::NewBranch),
        )],
    );
    assert_eq!(fixture.check(&id, 1).problem, None);
    let project = fixture.state.app.tree().projects[0].id.clone();

    // A Checked-out session in `web`, made in the new-session sheet.
    apply(
        &mut fixture.state,
        [Message::Projects(ProjectsMessage::NewSession(
            project,
            Scope::Repository(repository("web")),
        ))],
    );
    let (request, session) = fixture
        .backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .map(|(request, session, _)| (request, session.clone()))
        .next_back()
        .unwrap();
    let warm = BackendEvent::SessionCreated {
        request,
        session,
        backend_session: BackendSessionId::new("b-web"),
        config: Vec::new(),
    };
    apply(&mut fixture.state, [Message::Backend(warm)]);
    fixture.settle();
    apply(
        &mut fixture.state,
        [
            Message::NewSession(NewSessionMessage::Mode(BranchMode::CheckedOut)),
            Message::NewSession(NewSessionMessage::Create),
        ],
    );
    assert_eq!(fixture.state.error, None);
    assert_eq!(fixture.state.app.draft(), None);

    let problem = fixture.check(&id, 1).problem.unwrap();
    assert!(problem.starts_with("web is in use by"), "{problem}");
    // Back on the orchestrator, the card says so.
    apply(
        &mut fixture.state,
        [
            Message::SelectSession(fixture.session.clone()),
            Message::SelectTab(Tab::Session),
        ],
    );
    assert_eq!(fixture.count(&problem), 1);
    assert_eq!(fixture.click_text("Approve split"), []);
}

/// A state whose current session is a plain session in the repository `api` of a
/// project with `api` and `web`, working in a worktree of its branch `feature-x` made
/// from `main`. Git knows each repository with `main` checked out and the branches `dev`
/// and `release` besides.
fn sibling_fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = partitur_core::canonical(dir.path()).unwrap();
    let git = MemoryGit::new();
    for name in ["api", "web"] {
        let path = root.join(name);
        std::fs::create_dir_all(path.join(".git")).unwrap();
        git.add_repository(
            path.clone(),
            repo_info(&path, "main", &["dev", "main", "release"], &["origin/main"]),
        );
    }
    let backend = RecordingBackend::new();
    let (backends, default) = single_claude();
    let memory = MemoryStore::new();
    let files = partitur_core::testing::MemoryFiles::on_disk().with_settings_from(memory.clone());
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(memory.clone()),
        Box::new(memory),
        Box::new(git.clone()),
        Box::new(files.clone()),
        backends,
        default,
        GlobalSettings::default(),
        Box::new(partitur_core::testing::FixedClock::default()),
    )
    .unwrap();
    let mut state = boot(app, mpsc::unbounded().1, mpsc::unbounded().1).0;
    let project = partitur_core::testing::open_project(&mut state.app, &files, dir.path()).unwrap();
    state
        .app
        .prepare_session(project, Scope::Repository(repository("api")))
        .unwrap();
    attach_last(&mut state, &backend, "b-warm");
    let _task = settle_git(&mut state, &git);
    state.app.draft_mode(BranchMode::Worktree).unwrap();
    state.app.draft_branch("feature-x".into()).unwrap();
    assert!(matches!(
        state.app.create_from_draft().unwrap(),
        Created::Pending
    ));
    let _task = settle_git(&mut state, &git);
    attach_last(&mut state, &backend, "b-proposer");
    state.tab = Tab::Session;
    let model = state.app.session_view().unwrap();
    let session = model.id.clone();
    let siblings = model.siblings.clone().unwrap();
    assert_eq!(siblings.proposer_branch.as_deref(), Some("feature-x"));
    assert_eq!(siblings.choice.base.as_deref(), Some("main"));
    drop(model);
    assert_eq!(state.error, None);
    Fixture {
        state,
        backend,
        git,
        session,
        root,
        _dir: dir,
    }
}

/// Answers the newest session the backend was asked for with `backend_session`.
fn attach_last(state: &mut State, backend: &RecordingBackend, backend_session: &str) {
    let (request, session) = backend
        .calls()
        .iter()
        .filter_map(Call::as_new_session)
        .map(|(request, session, _)| (request, session.clone()))
        .next_back()
        .unwrap();
    let created = BackendEvent::SessionCreated {
        request,
        session,
        backend_session: BackendSessionId::new(backend_session),
        config: Vec::new(),
    };
    apply(state, [Message::Backend(created)]);
}

impl Fixture {
    /// The plain session proposes a sibling in its own repository, from `base` when
    /// given; returns the proposal, once git has answered.
    fn propose_sibling(&mut self, base: Option<&str>) -> ProposalId {
        let tool = partitur_core::PeerTool::ProposeSession {
            repository: None,
            title: "Follow up".into(),
            prompt: "Do the rest".into(),
            base: base.map(str::to_owned),
        };
        crate::test_support::deliver_tool(&mut self.state, self.session.clone(), tool);
        self.settle();
        let model = self.state.app.session_view().unwrap();
        model
            .entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                Entry::Proposal { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap()
    }
}

#[test]
fn a_sibling_proposal_shows_a_one_row_proposed_session_card() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);

    let draft = fixture.draft(&id).unwrap();
    assert_eq!(draft.split.children.len(), 1);
    let mut ui = simulator(view(&fixture.state));
    ui.find("Proposed session").unwrap();
    ui.find(SIBLING_SUMMARY).unwrap();
    ui.find(widget(&id, "row-0")).unwrap();
    ui.find("Follow up").unwrap();
    ui.find("Do the rest").unwrap();
    for absent in ["Proposed split", "skipped", "Edit split", "Approve split"] {
        assert_eq!(shown(&mut ui, absent), 0, "{absent}");
    }
    ui.find("Approve").unwrap();
    ui.find("Reject").unwrap();
    assert!(ui.find(widget(&id, "skipped-web")).is_err());
    drop(ui);

    // Without the sheet's way to edit everything, no picker adds a row.
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::ToggleEditAll)],
    );
    let mut ui = simulator(view(&fixture.state));
    assert!(ui.find(widget(&id, "add")).is_err());
}

#[test]
fn a_sibling_row_has_no_remove_button_and_no_from_orchestrator_markers() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);

    fixture.click(widget(&id, "row-0"));

    assert_eq!(fixture.forms(&id), [true, false, false]);
    let mut ui = simulator(view(&fixture.state));
    ui.find(widget(&id, "title-0")).unwrap();
    ui.find(widget(&id, "done-0")).unwrap();
    assert!(ui.find(widget(&id, "remove-0")).is_err());
    assert_eq!(shown(&mut ui, "remove"), 0);
    assert_eq!(shown(&mut ui, FROM_ORCHESTRATOR), 0);
}

#[test]
fn a_sibling_form_starts_from_the_proposers_settings() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);

    let draft = fixture.draft(&id).unwrap();
    let branch = &draft.split.children[0].branch;
    assert_eq!(branch.mode, BranchMode::Worktree);
    assert_eq!(branch.base.as_deref(), Some("main"));
    assert_eq!(branch.branch, "");
    assert_eq!(fixture.check(&id, 0).base.as_deref(), Some("main"));
    assert_eq!(
        fixture.check(&id, 0).problem.as_deref(),
        Some("enter a branch name")
    );
    assert_eq!(fixture.click_text("Approve"), []);
    let before = fixture.new_sessions();
    apply(
        &mut fixture.state,
        [proposal(&id, ProposalMessage::Approve)],
    );
    assert_eq!(fixture.new_sessions(), before);

    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::EditBranch(0, "follow-up".into()),
        )],
    );
    assert_eq!(fixture.check(&id, 0).problem, None);
    assert_eq!(
        fixture.click_text("Approve"),
        [proposal(&id, ProposalMessage::Approve)]
    );
}

#[test]
fn the_base_picker_marks_the_proposers_branch() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);
    let check = fixture.check(&id, 0);
    assert!(check.bases.contains(&"feature-x".to_owned()));

    let model = fixture.state.app.session_view().unwrap();
    let draft = fixture.draft(&id).unwrap();
    let proposer = proposer_branch(&model, &draft.split.children[0].repository);
    assert_eq!(proposer, Some("feature-x"));
    assert_eq!(proposer_branch(&model, &repository("web")), None);
    let offered: Vec<String> = crate::branch_controls::base_options(&check.bases, proposer)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(offered.contains(&"feature-x  (proposer's branch)".to_owned()));
    assert!(offered.contains(&"main".to_owned()));
    drop(model);

    apply(
        &mut fixture.state,
        [
            proposal(&id, ProposalMessage::SetBase(0, "feature-x".into())),
            proposal(&id, ProposalMessage::EditBranch(0, "follow-up".into())),
        ],
    );
    let choices = fixture.draft(&id).unwrap().split.choices();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].branch.base.as_deref(), Some("feature-x"));
    assert_eq!(fixture.check(&id, 0).base.as_deref(), Some("feature-x"));
}

#[test]
fn a_split_marks_no_proposers_branch() {
    let mut fixture = worktree_fixture();
    let id = fixture.propose();
    let model = fixture.state.app.session_view().unwrap();
    assert_eq!(proposer_branch(&model, &repository("api")), None);
    drop(model);
    let offered: Vec<String> = crate::branch_controls::base_options(&["main".to_owned()], None)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(offered, ["main"]);
    assert!(fixture.draft(&id).is_some());
}

#[test]
fn an_agents_base_seeds_the_form() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(Some("release"));

    let draft = fixture.draft(&id).unwrap();
    assert_eq!(
        draft.split.children[0].branch.base.as_deref(),
        Some("release")
    );
    assert_eq!(fixture.check(&id, 0).base.as_deref(), Some("release"));
}

#[test]
fn approving_a_sibling_starts_it_and_shows_the_decided_card() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);
    apply(
        &mut fixture.state,
        [proposal(
            &id,
            ProposalMessage::EditBranch(0, "follow-up".into()),
        )],
    );
    let before: Vec<SessionId> = top_level(&fixture);

    fixture.click_text("Approve");
    fixture.settle();

    assert_eq!(fixture.state.error, None);
    assert!(fixture.draft(&id).is_none());
    let started: Vec<SessionId> = top_level(&fixture)
        .into_iter()
        .filter(|session| !before.contains(session))
        .collect();
    assert_eq!(started.len(), 1);
    let mut ui = simulator(view(&fixture.state));
    ui.find("Proposed session").unwrap();
    ui.find("Follow up").unwrap();
    ui.find(DECIDED_SIBLING_SUMMARY).unwrap();
    ui.find(OPEN_HINT).unwrap();
    ui.find(STARTED).unwrap();
    ui.find("Worktree").unwrap();
    ui.find("follow-up").unwrap();
    assert_eq!(shown(&mut ui, "You approved the split. Started"), 0);
    assert!(ui.find("Approve").is_err());
    drop(ui);
    assert_eq!(
        fixture.click(widget(&id, "row-0")),
        [Message::SelectSession(started[0].clone())]
    );
}

#[test]
fn a_rejected_sibling_keeps_the_rejection_line() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);

    fixture.click_text("Reject");

    assert!(fixture.draft(&id).is_none());
    let mut ui = simulator(view(&fixture.state));
    ui.find("You rejected the session.").unwrap();
    assert!(ui.find("You rejected the split.").is_err());
    assert!(ui.find("Proposed session").is_err());
}

#[test]
fn a_sibling_proposal_without_a_draft_is_one_read_only_row() {
    let mut fixture = sibling_fixture();
    let id = fixture.propose_sibling(None);
    fixture.state.session.proposals.clear();

    let mut ui = simulator(view(&fixture.state));
    ui.find("Proposed session").unwrap();
    ui.find("Follow up").unwrap();
    assert_eq!(shown(&mut ui, "skipped"), 0);
    assert!(ui.find("Reject").is_err());
    assert!(ui.find(widget(&id, "row-0")).is_err());
}

/// The ids of the project's top-level sessions in the tree.
fn top_level(fixture: &Fixture) -> Vec<SessionId> {
    let tree = fixture.state.app.tree();
    tree.projects
        .first()
        .unwrap()
        .sessions
        .iter()
        .map(|node| node.id.clone())
        .collect()
}
