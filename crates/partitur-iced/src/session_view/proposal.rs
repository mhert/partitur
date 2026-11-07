//! The approval card: an undecided proposal of children, editable before it is decided.
//!
//! Every undecided proposal of the current session gets a [`ProposalDraft`] as soon as it
//! shows up (see [`sync_drafts`]). The card edits the draft: each child's title, prompt and
//! auto-allow switch, removing children and adding new ones in the project's repositories,
//! and a rejection note for the orchestrator. Approving starts the draft's children;
//! rejecting sends the note.

use std::fmt;

use iced::widget::{
    Column, button, column, container, pick_list, row, text, text_editor, text_input, toggler,
};
use iced::{Alignment, Element, Length, Task};
use partitur_core::{
    ChildChoice, Entry, ProposalId, ProposedChild, RepositoryId, SessionViewModel,
};

use super::entries::{card as bordered, children_count};
use super::{SessionMessage, sync_markdown};
use crate::app::{self, Message, State};
use crate::projects_view::{bold, chip};

/// The placeholder of the picker that adds a child.
const ADD_CHILD: &str = "+ add child";

/// The placeholder of the note that goes to the orchestrator with a rejection.
const REJECTION_NOTE: &str = "rejection note (optional)";

/// An undecided proposal as the user is editing it on its card.
#[derive(Debug)]
pub struct ProposalDraft {
    /// The children to start, in order.
    pub(crate) children: Vec<DraftChild>,
    /// The note that goes to the orchestrator with a rejection.
    pub(crate) note: String,
}

/// One child on the approval card.
#[derive(Debug)]
pub struct DraftChild {
    /// The repository the child works in.
    pub(crate) repository: RepositoryId,
    /// The child's display title; a blank one is derived from the prompt.
    pub(crate) title: String,
    /// The child's first prompt.
    pub(crate) prompt: text_editor::Content,
    /// Whether the child grants permission requests without asking.
    pub(crate) auto_allow: bool,
}

/// An interaction on an approval card; the child it concerns is given by its position.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalMessage {
    /// The user edited a child's prompt.
    EditPrompt(usize, text_editor::Action),
    /// The user edited a child's title.
    EditTitle(usize, String),
    /// The user switched a child's auto-allow on or off.
    SetAutoAllow(usize, bool),
    /// The user removed a child.
    Remove(usize),
    /// The user added a child in this repository.
    Add(RepositoryId),
    /// The user edited the rejection note.
    EditNote(String),
    /// The user approved the proposal with the draft's children.
    Approve,
    /// The user rejected the proposal with the draft's note.
    Reject,
}

impl ProposalDraft {
    /// A draft of the proposed `children` as the orchestrator proposed them, each granting
    /// permission requests without asking when `auto_allow`, without a note.
    pub(crate) fn from_entry(children: &[ProposedChild], auto_allow: bool) -> Self {
        let children = children
            .iter()
            .map(|child| DraftChild {
                repository: child.repository.clone(),
                title: child.title.clone(),
                prompt: text_editor::Content::with_text(&child.prompt),
                auto_allow,
            })
            .collect();
        Self {
            children,
            note: String::new(),
        }
    }

    /// The children to approve, with their prompts and titles trimmed.
    pub(crate) fn choices(&self) -> Vec<ChildChoice> {
        self.children
            .iter()
            .map(|child| ChildChoice {
                repository: child.repository.clone(),
                prompt: child.prompt.text().trim().to_owned(),
                title: child.title.trim().to_owned(),
                auto_allow: child.auto_allow,
            })
            .collect()
    }

    /// The note to send with a rejection; none when it is blank.
    fn rejection_note(&self) -> Option<String> {
        let note = self.note.trim();
        (!note.is_empty()).then(|| note.to_owned())
    }
}

/// Creates a draft for every undecided proposal of the current session that has none yet,
/// its children granting permission requests without asking when the session does.
///
/// Called from [`sync_markdown`], which runs after every backend event, selection and new
/// session, so the view finds a draft for every pending proposal.
pub(crate) fn sync_drafts(state: &mut State) {
    let Some(model) = state.app.session_view() else {
        return;
    };
    for entry in model.entries {
        if let Entry::Proposal {
            id,
            children,
            decision: None,
        } = entry
        {
            state
                .session
                .proposals
                .entry((model.id.clone(), id.clone()))
                .or_insert_with(|| ProposalDraft::from_entry(children, model.auto_allow));
        }
    }
}

/// The approval card of the proposal `id` of the session `model`, editing `draft`.
pub(crate) fn card<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    draft: &'a ProposalDraft,
) -> Element<'a, Message> {
    let children = draft
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| draft_child(model, id, index, child));
    let header = text(format!(
        "proposal · {}",
        children_count(draft.children.len())
    ))
    .font(bold());
    bordered(
        Column::new()
            .spacing(12)
            .push(header)
            .extend(children)
            .push(footer(model, id, draft)),
    )
}

/// One child's row (repository, title, auto-allow switch, remove button) and its prompt.
fn draft_child<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    index: usize,
    child: &'a DraftChild,
) -> Element<'a, Message> {
    let on_title = id.clone();
    let title = text_input("title", &child.title)
        .id(widget(id, &format!("title-{index}")))
        .on_input(move |title| message(&on_title, ProposalMessage::EditTitle(index, title)))
        .width(Length::Fill);
    let on_toggle = id.clone();
    let auto_allow = toggler(child.auto_allow)
        .label("auto-allow")
        .text_size(13)
        .on_toggle(move |enabled| {
            message(&on_toggle, ProposalMessage::SetAutoAllow(index, enabled))
        });
    let remove = button(text("remove").size(13))
        .style(button::secondary)
        .on_press(message(id, ProposalMessage::Remove(index)));
    let heading = row![
        chip(repository_name(model, &child.repository)),
        title,
        container(auto_allow).id(widget(id, &format!("auto-{index}"))),
        container(remove).id(widget(id, &format!("remove-{index}"))),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let on_action = id.clone();
    let prompt = text_editor(&child.prompt)
        .id(widget(id, &format!("prompt-{index}")))
        .on_action(move |action| message(&on_action, ProposalMessage::EditPrompt(index, action)))
        .padding(8);
    column![heading, prompt].spacing(4).into()
}

/// The picker that adds a child, the rejection note and the decision buttons.
fn footer<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    draft: &'a ProposalDraft,
) -> Element<'a, Message> {
    let on_add = id.clone();
    let add = pick_list(
        add_choices(model),
        None::<RepositoryChoice>,
        move |picked| message(&on_add, ProposalMessage::Add(picked.id)),
    )
    .placeholder(ADD_CHILD)
    .text_size(13);
    let on_note = id.clone();
    let note = text_input(REJECTION_NOTE, &draft.note)
        .id(widget(id, "note"))
        .on_input(move |note| message(&on_note, ProposalMessage::EditNote(note)));
    let approve = button(text("Approve"))
        .style(button::primary)
        .on_press_maybe(
            (!draft.children.is_empty()).then(|| message(id, ProposalMessage::Approve)),
        );
    let reject = button(text("Reject"))
        .style(button::secondary)
        .on_press(message(id, ProposalMessage::Reject));
    column![
        container(add).id(widget(id, "add")),
        note,
        row![approve, reject].spacing(8),
    ]
    .spacing(8)
    .into()
}

/// Applies `message` to the draft of the current session's proposal `id` and returns the
/// follow-up work; a message for a proposal without a draft is ignored.
///
/// Approving and rejecting go through the core [`App`](partitur_core::App), which records the
/// decision in the transcript; the draft is dropped once it succeeded, and kept with the
/// reason in the error bar when the app refuses.
pub(crate) fn update(state: &mut State, id: ProposalId, message: ProposalMessage) -> Task<Message> {
    let Some(current) = state.app.current_session() else {
        return Task::none();
    };
    let key = (current.clone(), id.clone());
    let Some(draft) = state.session.proposals.get_mut(&key) else {
        return Task::none();
    };
    let decided = match message {
        ProposalMessage::EditPrompt(index, action) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.prompt.perform(action);
            }
            return Task::none();
        }
        ProposalMessage::EditTitle(index, title) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.title = title;
            }
            return Task::none();
        }
        ProposalMessage::SetAutoAllow(index, enabled) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.auto_allow = enabled;
            }
            return Task::none();
        }
        ProposalMessage::Remove(index) => {
            if index < draft.children.len() {
                draft.children.remove(index);
            }
            return Task::none();
        }
        ProposalMessage::Add(repository) => {
            let auto_allow = state
                .app
                .session_view()
                .is_some_and(|model| model.auto_allow);
            draft.children.push(DraftChild {
                repository,
                title: String::new(),
                prompt: text_editor::Content::new(),
                auto_allow,
            });
            return Task::none();
        }
        ProposalMessage::EditNote(note) => {
            draft.note = note;
            return Task::none();
        }
        ProposalMessage::Approve => {
            let choices = draft.choices();
            state.app.approve_proposal(current, id, choices).map(drop)
        }
        ProposalMessage::Reject => {
            let note = draft.rejection_note();
            state.app.reject_proposal(current, id, note)
        }
    };
    if app::report(&mut state.error, decided).is_some() {
        state.session.proposals.remove(&key);
    }
    // The decision is now in the transcript.
    sync_markdown(state);
    Task::none()
}

/// The project's repositories the picker offers for a new child, in project order.
fn add_choices(model: &SessionViewModel<'_>) -> Vec<RepositoryChoice> {
    model
        .repositories
        .iter()
        .map(|repository| RepositoryChoice {
            id: repository.id.clone(),
            name: repository.name.clone(),
        })
        .collect()
}

/// The display name of `repository` in the session's project, or its id when the project
/// does not list it.
fn repository_name(model: &SessionViewModel<'_>, repository: &RepositoryId) -> String {
    model
        .repositories
        .iter()
        .find(|node| &node.id == repository)
        .map_or_else(|| repository.to_string(), |node| node.name.clone())
}

/// The id of the widget `name` on the card of the proposal `id`.
fn widget(id: &ProposalId, name: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("proposal-{id}-{name}"))
}

/// `message` for the card of the proposal `id`.
fn message(id: &ProposalId, message: ProposalMessage) -> Message {
    Message::Session(SessionMessage::Proposal(id.clone(), message))
}

/// One repository in the picker that adds a child, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct RepositoryChoice {
    id: RepositoryId,
    name: String,
}

impl fmt::Display for RepositoryChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use futures::channel::mpsc;
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendSessionId, ProposalId, ProposedChild, RepositoryId, Scope,
        SessionId, SessionNode, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Message, State, Tab, boot, update, view};
    use crate::projects_view::ProjectsMessage;
    use crate::session_view::SessionMessage;

    /// A state whose current session is the orchestrator of a project with the repositories
    /// `api` and `web`, plus the backend that recorded the calls and the session's id.
    struct Fixture {
        state: State,
        backend: RecordingBackend,
        session: SessionId,
        _dir: TempDir,
    }

    fn repository(id: &str) -> RepositoryId {
        RepositoryId::try_from(id).unwrap()
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        for name in ["api", "web"] {
            std::fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        let backend = RecordingBackend::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project, Scope::Project).unwrap();
        let request = backend
            .calls()
            .first()
            .and_then(Call::as_new_session)
            .map(|(request, ..)| request)
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        };
        apply(&mut state, [Message::Backend(created)]);
        state.tab = Tab::Session;
        assert!(state.app.session_view().unwrap().orchestrator);
        assert_eq!(state.error, None);
        Fixture {
            state,
            backend,
            session,
            _dir: dir,
        }
    }

    impl Fixture {
        /// The orchestrator proposes an `api` child and a `web` child; returns the proposal.
        fn propose(&mut self) -> ProposalId {
            let id = ProposalId::new();
            let proposed = Update::ChildrenProposed {
                id: id.clone(),
                children: vec![
                    ProposedChild {
                        repository: repository("api"),
                        prompt: "Add the endpoint".into(),
                        title: "API endpoint".into(),
                    },
                    ProposedChild {
                        repository: repository("web"),
                        prompt: "Show the endpoint's data".into(),
                        title: "Web page".into(),
                    },
                ],
            };
            let event = BackendEvent::Update {
                session: self.session.clone(),
                update: proposed,
            };
            apply(&mut self.state, [Message::Backend(event)]);
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
            let mut titles: Vec<String> =
                self.children().into_iter().map(|node| node.title).collect();
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
    fn a_pending_proposal_gets_a_draft_and_shows_its_children_with_approve_and_reject() {
        let mut fixture = fixture();
        let id = fixture.propose();

        let draft = fixture.draft(&id).unwrap();
        let repositories: Vec<_> = draft.children.iter().map(|c| &c.repository).collect();
        assert_eq!(repositories, [&repository("api"), &repository("web")]);
        assert_eq!(draft.children[0].title, "API endpoint");
        assert_eq!(draft.children[0].prompt.text().trim(), "Add the endpoint");
        assert!(!draft.children[0].auto_allow);
        assert_eq!(draft.note, "");

        let mut ui = simulator(view(&fixture.state));
        ui.find("proposal · 2 children").unwrap();
        ui.find("api").unwrap();
        ui.find("web").unwrap();
        ui.find("API endpoint").unwrap();
        ui.find("Approve").unwrap();
        ui.find("Reject").unwrap();
        ui.find(widget(&id, "prompt-1")).unwrap();
        ui.find(widget(&id, "note")).unwrap();
        ui.find(REJECTION_NOTE).unwrap();
        assert_eq!(REJECTION_NOTE, "rejection note (optional)");
        // A pick list exposes neither its placeholder nor its menu entries to selectors, so
        // the picker is found by its id and its entries are checked directly.
        ui.find(widget(&id, "add")).unwrap();
        assert_eq!(ADD_CHILD, "+ add child");
        let model = fixture.state.app.session_view().unwrap();
        let names: Vec<String> = add_choices(&model)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["api", "web"]);
        assert!(ui.find("pending").is_err());
    }

    #[test]
    fn removing_and_adding_children_edits_the_draft() {
        let mut fixture = fixture();
        let id = fixture.propose();

        let mut ui = simulator(view(&fixture.state));
        ui.click(widget(&id, "remove-0")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [proposal(&id, ProposalMessage::Remove(0))]);
        apply(&mut fixture.state, messages);
        apply(
            &mut fixture.state,
            [proposal(&id, ProposalMessage::Add(repository("web")))],
        );

        let draft = fixture.draft(&id).unwrap();
        let repositories: Vec<_> = draft.children.iter().map(|c| &c.repository).collect();
        assert_eq!(repositories, [&repository("web"), &repository("web")]);
        assert_eq!(draft.children[0].title, "Web page");
        assert_eq!(draft.children[1].title, "");
        assert_eq!(draft.children[1].prompt.text().trim(), "");
        let mut ui = simulator(view(&fixture.state));
        ui.find("proposal · 2 children").unwrap();
        assert!(ui.find("API endpoint").is_err());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn approving_sends_the_draft_to_the_app() {
        let mut fixture = fixture();
        let id = fixture.propose();
        let before = fixture.new_sessions();

        let mut ui = simulator(view(&fixture.state));
        ui.click(widget(&id, "auto-1")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [proposal(&id, ProposalMessage::SetAutoAllow(1, true))]
        );
        apply(&mut fixture.state, messages);
        apply(
            &mut fixture.state,
            [
                proposal(&id, ProposalMessage::EditTitle(0, "  Endpoint v2 ".into())),
                proposal(&id, ProposalMessage::EditNote("ship it".into())),
            ],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.click("Approve").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [proposal(&id, ProposalMessage::Approve)]);
        apply(&mut fixture.state, messages);

        assert_eq!(fixture.state.error, None);
        assert_eq!(fixture.new_sessions(), before + 2);
        assert_eq!(fixture.child_titles(), ["Endpoint v2", "Web page"]);
        assert!(!fixture.child_auto_allows("Endpoint v2"));
        assert!(fixture.child_auto_allows("Web page"));
        assert!(fixture.draft(&id).is_none());
        let mut ui = simulator(view(&fixture.state));
        ui.find("approved · 2 children started").unwrap();
        assert!(ui.find("Approve").is_err());
    }

    #[test]
    fn approving_without_edits_uses_the_proposal_and_the_orchestrators_toggle() {
        let mut fixture = fixture();
        apply(
            &mut fixture.state,
            [Message::Session(SessionMessage::SetAutoAllow(true))],
        );
        let id = fixture.propose();
        assert!(fixture.draft(&id).unwrap().children[1].auto_allow);

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

        let mut ui = simulator(view(&fixture.state));
        ui.click("Reject").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [proposal(&id, ProposalMessage::Reject)]);
        apply(&mut fixture.state, messages);

        assert_eq!(fixture.state.error, None);
        assert_eq!(fixture.new_sessions(), before);
        assert!(fixture.draft(&id).is_none());
        let mut ui = simulator(view(&fixture.state));
        ui.find("rejected").unwrap();
        ui.find("note: not now").unwrap();
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
        ui.find("proposal · 0 children").unwrap();
        ui.click("Approve").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "Approve is disabled");
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
        assert!(fixture.draft(&id).unwrap().children.is_empty());
        let mut ui = simulator(view(&fixture.state));
        ui.find("Reject").unwrap();
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
}
