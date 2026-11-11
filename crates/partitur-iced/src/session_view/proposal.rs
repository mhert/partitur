//! The proposed split: an undecided proposal of children as a table, editable before it is
//! decided.
//!
//! Every undecided proposal of the current session gets a [`ProposalDraft`] as soon as it
//! shows up (see [`sync_drafts`]). The card lists the draft's children, one row each: the
//! repository, the title over the prompt, and when the child starts. The project's
//! repositories without a child follow as muted `skipped` rows. A click on a child's row
//! turns it into a form that edits the title, the prompt and the auto-allow switch or
//! removes the child; a click on a skipped row adds a child for that repository and opens
//! its form. `Edit split` opens every form at once and adds a picker that offers each of
//! the project's repositories for a further child. Under the card, `Approve split` (or
//! Enter) starts the draft's children, and `Reject` sends the optional rejection note to
//! the orchestrator.

use std::fmt;

use iced::widget::{
    Column, Row, button, column, container, pick_list, row, space, text, text_editor, text_input,
    toggler,
};
use iced::{Alignment, Border, Element, Length, Task, Theme, border};
use partitur_core::{
    ChildChoice, Entry, ProposalId, ProposedChild, RepositoryId, RepositoryNode, SessionViewModel,
};

use super::entries::LINE;
use super::{SessionMessage, repository_name, sync_markdown, with_chevron};
use crate::app::{self, Message, State};
use crate::ellipsis::keep_start;
use crate::icon::{Icon, icon};
use crate::{badge, style, theme};

/// The placeholder of the picker that adds a child.
const ADD_CHILD: &str = "add child";

/// The placeholder of the note that goes to the orchestrator with a rejection.
const REJECTION_NOTE: &str = "rejection note (optional)";

/// What the card's header says about its rows.
const HINT: &str = "click a row to edit its brief";

/// What stands for the prompt of a child that has none yet.
const NO_BRIEF: &str = "no brief yet";

/// The width of the column that names the repository, in logical pixels.
const REPOSITORY_WIDTH: f32 = 110.0;

/// The width of the column that says when a child starts.
const STARTS_WIDTH: f32 = 150.0;

/// The space between two columns of the table.
const COLUMN_GAP: f32 = 14.0;

/// The space between the card and the buttons that decide it.
const ACTIONS_GAP: f32 = 14.0;

/// The most characters of a repository's name its column shows.
const REPOSITORY_BUDGET: usize = 14;

/// The corner radius of what touches the card's border from inside: the card's own, less
/// the border.
const INNER_RADIUS: f32 = style::CARD_RADIUS - 1.0;

/// An undecided proposal as the user is editing it on its card.
#[derive(Debug)]
pub struct ProposalDraft {
    /// The children to start, in order.
    pub(crate) children: Vec<DraftChild>,
    /// The note that goes to the orchestrator with a rejection.
    pub(crate) note: String,
    /// Which children show their form.
    pub(crate) editing: Editing,
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

/// Which children of a draft show the form that edits them instead of their row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Editing {
    /// No child: each one is a row.
    #[default]
    Nothing,
    /// The child at this position, whose row the user clicked.
    Row(usize),
    /// Every child, after `Edit split`.
    All,
}

impl Editing {
    /// Whether the child at `index` shows its form.
    fn shows(self, index: usize) -> bool {
        match self {
            Self::Nothing => false,
            Self::Row(row) => row == index,
            Self::All => true,
        }
    }
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
    /// The user clicked a child's row, which becomes its form.
    OpenRow(usize),
    /// The user left the form of the one child that showed it.
    CloseRow,
    /// The user pressed `Edit split`: every child shows its form, or none any more.
    ToggleEditAll,
    /// The user edited the rejection note.
    EditNote(String),
    /// The user approved the proposal with the draft's children.
    Approve,
    /// The user rejected the proposal with the draft's note.
    Reject,
}

impl ProposalDraft {
    /// A draft of the proposed `children` as the orchestrator proposed them, each granting
    /// permission requests without asking when `auto_allow`, without a note and with every
    /// child shown as a row.
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
            editing: Editing::default(),
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

/// The split card of the proposal `id` of the session `model`, editing `draft`, over the
/// buttons that decide the proposal.
pub(crate) fn card<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    draft: &'a ProposalDraft,
) -> Element<'a, Message> {
    let taken: Vec<&RepositoryId> = draft
        .children
        .iter()
        .map(|child| &child.repository)
        .collect();
    let skipped = skipped(model, &taken);
    let editing_all = draft.editing == Editing::All;
    // The last row touches the card's rounded corners, unless the picker follows it.
    let last = draft
        .children
        .len()
        .saturating_add(skipped.len())
        .saturating_sub(1);
    let rounded = |position: usize| position == last && !editing_all;
    let children = draft.children.iter().enumerate().map(|(index, child)| {
        let name = repository_name(model, &child.repository);
        if draft.editing.shows(index) {
            form(id, index, &name, child, !editing_all)
        } else {
            let open = message(id, ProposalMessage::OpenRow(index));
            let row = child_row(
                &name,
                &child.title,
                &child.prompt.text(),
                Some(open),
                rounded(index),
            );
            container(row)
                .id(widget(id, &format!("row-{index}")))
                .into()
        }
    });
    let skipped_rows = skipped.iter().enumerate().map(|(offset, repository)| {
        let add = message(id, ProposalMessage::Add(repository.id.clone()));
        let position = draft.children.len().saturating_add(offset);
        let row = skipped_row(&repository.name, Some(add), rounded(position));
        container(row)
            .id(widget(id, &format!("skipped-{}", repository.id)))
            .into()
    });
    let picker = editing_all.then(|| add_row(model, id));
    let summary = summary(draft.children.len(), skipped.len());
    let rows = children.chain(skipped_rows).chain(picker);
    column![table(summary, Some(HINT), rows), actions(id, draft)]
        .spacing(ACTIONS_GAP)
        .into()
}

/// The split card of an undecided proposal of `children` that has no draft: the same
/// table, which takes no clicks and offers no decision.
pub(super) fn read_only<'a>(
    model: &SessionViewModel<'_>,
    children: &'a [ProposedChild],
) -> Element<'a, Message> {
    let taken: Vec<&RepositoryId> = children.iter().map(|child| &child.repository).collect();
    let skipped = skipped(model, &taken);
    let rows = children.iter().map(|child| {
        child_row(
            &repository_name(model, &child.repository),
            &child.title,
            &child.prompt,
            None,
            false,
        )
    });
    let skipped_rows = skipped
        .iter()
        .map(|repository| skipped_row(&repository.name, None, false));
    table(
        summary(children.len(), skipped.len()),
        None,
        rows.chain(skipped_rows),
    )
}

/// The repositories of the session's project that none of `taken`, the repositories of a
/// proposal's children, names, in project order.
fn skipped<'m>(
    model: &'m SessionViewModel<'_>,
    taken: &[&RepositoryId],
) -> Vec<&'m RepositoryNode> {
    model
        .repositories
        .iter()
        .filter(|repository| !taken.contains(&&repository.id))
        .collect()
}

/// What the card's header says the split comes to: how many child sessions it starts and,
/// when there are any, how many repositories it leaves `untouched`.
fn summary(children: usize, untouched: usize) -> String {
    let sessions = if children == 1 {
        "1 child session".to_owned()
    } else {
        format!("{children} child sessions")
    };
    match untouched {
        0 => sessions,
        1 => format!("{sessions} · 1 repo untouched"),
        several => format!("{sessions} · {several} repos untouched"),
    }
}

/// The card: its header with `summary` and, at the right, `hint`; the names of the
/// columns; then `rows`, parted by hairlines.
fn table<'a>(
    summary: String,
    hint: Option<&'a str>,
    rows: impl Iterator<Item = Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut header = row![
        text("Proposed split")
            .size(theme::HEADING)
            .font(theme::UI_SEMIBOLD),
        text(summary).style(style::muted),
        space::horizontal(),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if let Some(hint) = hint {
        header = header.push(text(hint).size(theme::SMALL).style(style::muted));
    }
    let column_name = |name| {
        text(name)
            .size(theme::SMALL)
            .font(theme::UI_MEDIUM)
            .style(style::muted)
    };
    let columns = cells(
        column_name("Repo").into(),
        column_name("Brief").into(),
        column_name("Starts").into(),
    );
    let mut table = column![
        container(header).padding([10, 14]),
        style::hairline(|tokens| tokens.border),
        container(columns).padding([6, 14]),
    ];
    for row in rows {
        table = table
            .push(style::hairline(|tokens| tokens.border_soft))
            .push(row);
    }
    // The padding keeps the rows off the border, which a hovered row would paint over.
    container(table)
        .padding(1)
        .width(Length::Fill)
        .clip(true)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.panel.into()),
                border: Border::default()
                    .rounded(style::CARD_RADIUS)
                    .width(1)
                    .color(tokens.border_split),
                ..container::Style::default()
            }
        })
        .into()
}

/// One line of the table: the repository, the brief and when the child starts, each in
/// its column and all starting at the line's top.
fn cells<'a>(
    repository: Element<'a, Message>,
    brief: Element<'a, Message>,
    starts: Element<'a, Message>,
) -> Row<'a, Message> {
    row![
        container(repository).width(REPOSITORY_WIDTH),
        container(brief).width(Length::Fill),
        container(starts).width(STARTS_WIDTH),
    ]
    .spacing(COLUMN_GAP)
    .align_y(Alignment::Start)
}

/// A repository's name as its column writes it, in the font of a child's row.
fn repository_cell<'a>(name: &str) -> iced::widget::Text<'a> {
    text(keep_start(name, REPOSITORY_BUDGET)).line_height(LINE)
}

/// The row of a child in the repository `name`: the title over the prompt, and that the
/// child starts at once. A click sends `on_press`; `rounded` says that the row is the
/// card's last, whose corners follow the card's.
fn child_row<'a>(
    name: &str,
    title: &'a str,
    prompt: &str,
    on_press: Option<Message>,
    rounded: bool,
) -> Element<'a, Message> {
    let mut brief = Column::new().spacing(2);
    if !title.trim().is_empty() {
        brief = brief.push(text(title).line_height(LINE));
    }
    let prompt = prompt.trim();
    brief = brief.push(if prompt.is_empty() {
        text(NO_BRIEF).line_height(LINE).style(style::muted)
    } else {
        text(prompt.to_owned()).line_height(LINE).style(style::soft)
    });
    let line = cells(
        repository_cell(name).font(theme::UI_MEDIUM).into(),
        brief.into(),
        text("starts now")
            .line_height(LINE)
            .style(style::muted)
            .into(),
    );
    table_row(line, on_press, rounded)
}

/// The row of the repository `name` that gets no child, all muted. A click sends
/// `on_press`; `rounded` as for a [`child_row`].
fn skipped_row<'a>(name: &str, on_press: Option<Message>, rounded: bool) -> Element<'a, Message> {
    let line = cells(
        repository_cell(name).style(style::muted).into(),
        text("—").line_height(LINE).style(style::dim).into(),
        text("skipped").line_height(LINE).style(style::muted).into(),
    );
    table_row(line, on_press, rounded)
}

/// `line` as a row of the table that sends `on_press` when clicked and lights up under the
/// pointer; with the bottom corners of the card when `rounded`.
fn table_row(
    line: Row<'_, Message>,
    on_press: Option<Message>,
    rounded: bool,
) -> Element<'_, Message> {
    let corners = if rounded {
        border::bottom(INNER_RADIUS)
    } else {
        border::radius(0)
    };
    button(line)
        .padding([10, 14])
        .width(Length::Fill)
        .style(move |theme: &Theme, status| {
            let tokens = theme::tokens(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then(|| tokens.border_soft.into()),
                text_color: tokens.text,
                border: Border::default().rounded(corners),
                ..button::Style::default()
            }
        })
        .on_press_maybe(on_press)
        .into()
}

/// The form of the child at `index`, which works in the repository `name`: its title, its
/// prompt, its auto-allow switch and the button that removes it; when `closable`, also the
/// link that turns the form back into the child's row.
fn form<'a>(
    id: &ProposalId,
    index: usize,
    name: &str,
    child: &'a DraftChild,
    closable: bool,
) -> Element<'a, Message> {
    let on_title = id.clone();
    let title = text_input("title", &child.title)
        .id(widget(id, &format!("title-{index}")))
        .on_input(move |title| message(&on_title, ProposalMessage::EditTitle(index, title)))
        // A field without a submit lets Enter through to the window, where it approves the
        // split; submitting the title as it is keeps the key in the field.
        .on_submit(message(
            id,
            ProposalMessage::EditTitle(index, child.title.clone()),
        ))
        .padding([5, 8])
        .style(field);
    let on_action = id.clone();
    let prompt = text_editor(&child.prompt)
        .id(widget(id, &format!("prompt-{index}")))
        .placeholder("brief")
        .on_action(move |action| message(&on_action, ProposalMessage::EditPrompt(index, action)))
        .padding([6, 8])
        .line_height(LINE)
        .style(area);
    let on_toggle = id.clone();
    let auto_allow = toggler(child.auto_allow)
        .label("auto-allow")
        .size(14)
        .spacing(6)
        .text_size(theme::SMALL)
        .style(style::switch)
        .on_toggle(move |enabled| {
            message(&on_toggle, ProposalMessage::SetAutoAllow(index, enabled))
        });
    let small = |word| text(word).size(theme::SMALL);
    let remove = button(small("remove"))
        .padding([3, 6])
        .style(style::danger_button)
        .on_press(message(id, ProposalMessage::Remove(index)));
    let mut controls = row![
        container(auto_allow).id(widget(id, &format!("auto-{index}"))),
        space::horizontal(),
        container(remove).id(widget(id, &format!("remove-{index}"))),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if closable {
        let done = button(small("done"))
            .padding([3, 0])
            .style(style::link_button)
            .on_press(message(id, ProposalMessage::CloseRow));
        controls = controls.push(container(done).id(widget(id, &format!("done-{index}"))));
    }
    let line = row![
        container(repository_cell(name).font(theme::UI_MEDIUM))
            .width(REPOSITORY_WIDTH)
            .padding([5, 0]),
        column![title, prompt, controls].spacing(8),
    ]
    .spacing(COLUMN_GAP);
    container(line).padding([10, 14]).into()
}

/// The card's last line while every child shows its form: the picker that adds a child in
/// any of the project's repositories.
fn add_row<'a>(model: &SessionViewModel<'_>, id: &ProposalId) -> Element<'a, Message> {
    let on_add = id.clone();
    let picker = pick_list(
        add_choices(model),
        None::<RepositoryChoice>,
        move |picked| message(&on_add, ProposalMessage::Add(picked.id)),
    )
    .placeholder(ADD_CHILD)
    .text_size(theme::SMALL)
    .padding([4, 8])
    .handle(pick_list::Handle::None)
    .style(|theme: &Theme, _status| {
        let tokens = theme::tokens(theme);
        pick_list::Style {
            text_color: tokens.text_soft,
            placeholder_color: tokens.muted,
            handle_color: tokens.muted,
            background: tokens.panel.into(),
            border: outline(theme, false),
        }
    })
    .menu_style(style::menu);
    let line = row![
        icon(Icon::Plus, |tokens| tokens.muted),
        container(with_chevron(picker)).id(widget(id, "add")),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    container(line).padding([10, 14]).into()
}

/// The buttons under the card: approve, edit every child at once, and reject with the
/// note written between them.
fn actions<'a>(id: &ProposalId, draft: &'a ProposalDraft) -> Element<'a, Message> {
    let approve = button(
        row![
            text("Approve split").font(theme::UI_SEMIBOLD),
            badge::key_cap_on_primary("Enter"),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([7, 16])
    .style(style::primary_button)
    .on_press_maybe((!draft.children.is_empty()).then(|| message(id, ProposalMessage::Approve)));
    let edit_label = if draft.editing == Editing::All {
        "Done editing"
    } else {
        "Edit split"
    };
    let edit = button(text(edit_label))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press(message(id, ProposalMessage::ToggleEditAll));
    let on_note = id.clone();
    let note = text_input(REJECTION_NOTE, &draft.note)
        .id(widget(id, "note"))
        .on_input(move |note| message(&on_note, ProposalMessage::EditNote(note)))
        // As in a child's title, Enter stays in the field instead of approving the split.
        .on_submit(message(id, ProposalMessage::EditNote(draft.note.clone())))
        .padding([7, 10])
        .style(field);
    let reject = button(text("Reject"))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press(message(id, ProposalMessage::Reject));
    row![approve, edit, note, reject]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

/// A text field of the card: the panel colour inside an outline.
fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let tokens = theme::tokens(theme);
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: tokens.panel.into(),
        border: outline(theme, focused),
        icon: tokens.muted,
        placeholder: tokens.muted,
        value: tokens.text,
        selection: tokens.selection,
    }
}

/// The box a child's prompt is edited in, drawn like a [`field`].
fn area(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let tokens = theme::tokens(theme);
    let focused = matches!(status, text_editor::Status::Focused { .. });
    text_editor::Style {
        background: tokens.panel.into(),
        border: outline(theme, focused),
        placeholder: tokens.muted,
        value: tokens.text,
        selection: tokens.selection,
    }
}

/// The outline of an input of the card, which stands out while the input is `focused`.
fn outline(theme: &Theme, focused: bool) -> Border {
    let tokens = theme::tokens(theme);
    let color = if focused {
        tokens.muted
    } else {
        tokens.border_input
    };
    Border::default()
        .rounded(style::ROW_RADIUS)
        .width(1)
        .color(color)
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
                // The positions after the removed child shifted, so a single open form
                // would now be another child's.
                if matches!(draft.editing, Editing::Row(_)) {
                    draft.editing = Editing::Nothing;
                }
            }
            return Task::none();
        }
        ProposalMessage::Add(repository) => {
            let auto_allow = state
                .app
                .session_view()
                .is_some_and(|model| model.auto_allow);
            // The new child has nothing to show in a row yet, so its form opens.
            if draft.editing != Editing::All {
                draft.editing = Editing::Row(draft.children.len());
            }
            draft.children.push(DraftChild {
                repository,
                title: String::new(),
                prompt: text_editor::Content::new(),
                auto_allow,
            });
            return Task::none();
        }
        ProposalMessage::OpenRow(index) => {
            if index < draft.children.len() {
                draft.editing = Editing::Row(index);
            }
            return Task::none();
        }
        ProposalMessage::CloseRow => {
            if matches!(draft.editing, Editing::Row(_)) {
                draft.editing = Editing::Nothing;
            }
            return Task::none();
        }
        ProposalMessage::ToggleEditAll => {
            draft.editing = if draft.editing == Editing::All {
                Editing::Nothing
            } else {
                Editing::All
            };
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

/// Approves the newest undecided proposal of the current session with its draft, as its
/// `Approve split` button does. Like the button, it does nothing while the draft has no
/// children, and nothing when no proposal is undecided.
pub(crate) fn approve_newest(state: &mut State) -> Task<Message> {
    let newest = state.app.session_view().and_then(|model| {
        model.entries.iter().rev().find_map(|entry| match entry {
            Entry::Proposal {
                id, decision: None, ..
            } => Some((model.id.clone(), id.clone())),
            _ => None,
        })
    });
    let Some(key) = newest else {
        return Task::none();
    };
    let approvable = state
        .session
        .proposals
        .get(&key)
        .is_some_and(|draft| !draft.children.is_empty());
    if !approvable {
        return Task::none();
    }
    let (_, id) = key;
    update(state, id, ProposalMessage::Approve)
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
    use iced::event;
    use iced::keyboard::key;
    use iced::widget::Id;
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryGit, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendSessionId, GlobalSettings, ProposalId, ProposedChild,
        RepositoryId, Scope, SessionId, SessionNode, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{BareKey, Message, State, Tab, boot, update, view};
    use crate::jump::JumpMessage;
    use crate::projects_view::ProjectsMessage;
    use crate::session_view::SessionMessage;
    use crate::test_support::shown;

    /// A state whose current session is the orchestrator of a project with the repositories
    /// `api`, `docs` and `web`, plus the backend that recorded the calls and the session's id.
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
        for name in ["api", "docs", "web"] {
            std::fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
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
        let repositories: Vec<_> = draft.children.iter().map(|c| &c.repository).collect();
        assert_eq!(repositories, [&repository("api"), &repository("web")]);
        assert_eq!(draft.children[0].title, "API endpoint");
        assert_eq!(draft.children[0].prompt.text().trim(), "Add the endpoint");
        assert!(!draft.children[0].auto_allow);
        assert_eq!(draft.note, "");
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
        assert_eq!(draft.children.len(), 3);
        assert_eq!(draft.children[2].repository, repository("docs"));
        assert_eq!(draft.children[2].title, "");
        assert_eq!(draft.children[2].prompt.text().trim(), "");
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
        let repositories: Vec<_> = draft.children.iter().map(|c| &c.repository).collect();
        assert_eq!(repositories, [&repository("web"), &repository("web")]);
        assert_eq!(draft.children[0].title, "Web page");
        assert_eq!(draft.children[1].title, "");
        assert_eq!(draft.children[1].prompt.text().trim(), "");
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
        assert!(fixture.draft(&id).unwrap().children.is_empty());
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
        assert_eq!(draft.children[0].title, "API endpoint");
        assert_eq!(draft.note, "");
        assert_eq!(draft.children.len(), 2);
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
}
