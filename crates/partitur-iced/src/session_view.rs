//! The Session tab: the selected session's transcript and the prompt that continues it.
//!
//! Under the breadcrumb row, which names the session and carries its pills (orchestrator,
//! status, a child that asks the user) and how many of its children are done, sits one
//! centered column. A session with children starts it with a strip of cards, one per
//! child, that tell where the child stands and open it. Then comes the transcript, which
//! stays pinned to its end while the agent streams, and under it the composer. The composer
//! lists the files attached to the next prompt as chips, then frames the attach button, the
//! prompt box, the keys that send and break the line, and the send button (the stop button
//! while a turn runs). Its last row holds the model, effort and backend pickers, the
//! auto-allow switch, for an orchestrator whose children work a button that stops them
//! and, at the right, how much of its context the session has used. An archived session is
//! read-only: a banner under the breadcrumb says when it was archived and restores it, and
//! the composer is gone.
//!
//! How each transcript entry is drawn lives in [`entries`]; the card that approves or
//! rejects a proposal of children ([`ProposalDraft`], [`ProposalMessage`]) lives in
//! `proposal`, and the timeline an orchestrator's transcript becomes once a proposal is
//! decided in `timeline`.

mod entries;
mod proposal;
mod timeline;

pub use proposal::{DraftChild, ProposalDraft, ProposalMessage};

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use iced::keyboard::{self, key};
use iced::widget::{
    Column, Id, Row, Space, button, column, container, markdown, operation, pick_list, row,
    scrollable, space, stack, text, text_editor, toggler,
};
use iced::{Alignment, Border, Element, Length, Padding, Pixels, Task, Theme};
use partitur_core::{
    App, AppError, Attachment, BackendId, BackendInfo, ChoiceView, ConfigValueId, Entry,
    PermissionId, PermissionOptionId, ProposalId, RepositoryId, SessionId, SessionNode,
    SessionViewModel, Status,
};

use crate::app::{self, Message, State};
use crate::ellipsis::keep_start;
use crate::icon::{Icon, icon};
use crate::jump::JumpMessage;
use crate::markdown_cache::MarkdownCache;
use crate::projects_view::{CHILD_ASKS_YOU, ProjectsMessage};
use crate::theme::Tokens;
use crate::{badge, breadcrumb, layout, selectable, style, theme, tokens_label};

/// The id of the prompt box.
const PROMPT: &str = "prompt";

/// The ids of the backend, model and effort pickers and of the auto-allow switch: none of
/// them exposes its text to selectors.
const BACKEND: &str = "backend-picker";
const MODEL: &str = "model";
const EFFORT: &str = "effort";
const AUTO_ALLOW: &str = "auto-allow";

/// The tallest the prompt box grows before it scrolls, in logical pixels.
const PROMPT_MAX_HEIGHT: f32 = 200.0;

/// The widest the transcript and the composer grow, in logical pixels.
const COLUMN_WIDTH: f32 = 780.0;

/// The widest they grow for an orchestrator, whose cards list several children, each with
/// its prompt.
const ORCHESTRATOR_COLUMN_WIDTH: f32 = 860.0;

/// The space between the window's edge and the column, left and right.
const MARGIN_X: f32 = 40.0;

/// The space above the transcript, and the smaller one that parts it from the children's
/// strip when there is one.
const TRANSCRIPT_TOP: (f32, f32) = (24.0, 16.0);

/// The most cards of children one line of the strip holds.
const CARDS_PER_LINE: usize = 3;

/// The space between two cards of the strip, side by side and line to line.
const CARD_GAP: f32 = 12.0;

/// What the prompt box of an orchestrator says while one of its proposals is undecided.
const CHANGE_THE_SPLIT: &str = "Change the split, or ask the orchestrator…";

/// What the prompt box of an orchestrator says while its children work.
const STEER: &str = "Steer the orchestrator… (it keeps going while you type)";

/// The side of the attach button, in logical pixels.
const ATTACH_SIZE: f32 = 24.0;

/// The side of a picker's chevron, in logical pixels.
const CHEVRON_SIZE: f32 = 10.0;

/// The room a picker keeps around its value: at the left the gap to its label, which its
/// menu's entries keep as well, and above and below what makes the picker easy to hit.
const PICKER_PADDING: Padding = Padding {
    top: 3.0,
    right: 0.0,
    bottom: 3.0,
    left: 6.0,
};

/// The width and the height of the bar that shows the context usage, in logical pixels.
const USAGE_BAR: (u16, f32) = (80, 4.0);

/// What the Session tab remembers between frames.
///
/// The prompt and its attachments belong to the tab, not to a session: switching sessions
/// keeps the draft.
#[derive(Debug)]
pub struct SessionState {
    /// The prompt being written.
    pub(crate) prompt: text_editor::Content,
    /// The files that go with the next prompt.
    pub(crate) attachments: Vec<PathBuf>,
    /// The transcript entries the user expanded (thoughts and tool calls), by session and
    /// entry index.
    pub(crate) expanded: HashSet<(SessionId, usize)>,
    /// The runs of consecutive tool calls the user expanded into their list of calls, by
    /// session and the entry index of the run's first call.
    pub(crate) expanded_tools: HashSet<(SessionId, usize)>,
    /// The agent's answers, parsed as Markdown.
    pub(crate) markdown: MarkdownCache,
    /// The approval cards' drafts of the undecided proposals, by session and proposal.
    pub(crate) proposals: HashMap<(SessionId, ProposalId), ProposalDraft>,
    /// The transcript's scrollable.
    pub(crate) scroll: Id,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            prompt: text_editor::Content::new(),
            attachments: Vec::new(),
            expanded: HashSet::new(),
            expanded_tools: HashSet::new(),
            markdown: MarkdownCache::default(),
            proposals: HashMap::new(),
            scroll: Id::new("transcript"),
        }
    }
}

/// An interaction on the Session tab.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMessage {
    /// The user edited the prompt.
    Prompt(text_editor::Action),
    /// The user sent the prompt.
    Submit,
    /// The user asked to stop the running turn.
    Stop,
    /// The user asked to attach files; the file dialog opens.
    Attach,
    /// The file dialog closed with these files, none when it was cancelled.
    Attached(Vec<PathBuf>),
    /// The user removed the attachment at this position.
    RemoveAttachment(usize),
    /// The user expanded or collapsed the current session's transcript entry at this index.
    ToggleEntry(usize),
    /// The user expanded or collapsed the run of consecutive tool calls whose first call is
    /// the current session's transcript entry at this index.
    ToggleTools(usize),
    /// The user answered a permission request with an option, or cancelled it with `None`.
    AnswerPermission(PermissionId, Option<PermissionOptionId>),
    /// The user switched granting permission requests without asking on or off.
    SetAutoAllow(bool),
    /// The user picked another backend to run the session on.
    SetBackend(BackendId),
    /// The user picked a model.
    SetModel(ConfigValueId),
    /// The user picked a reasoning effort.
    SetEffort(ConfigValueId),
    /// The user clicked a link in an answer.
    LinkClicked(markdown::Uri),
    /// An interaction on the approval card of the current session's proposal.
    Proposal(ProposalId, ProposalMessage),
    /// The user asked to stop the turns of the current session's children.
    StopChildren,
}

/// The Session tab for the selected session `model`, its answers drawn in the colours of
/// `tokens`, the token set of the active theme.
pub fn view<'a>(
    model: &SessionViewModel<'a>,
    state: &'a SessionState,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let width = if model.orchestrator {
        ORCHESTRATOR_COLUMN_WIDTH
    } else {
        COLUMN_WIDTH
    };
    let mut page = column![breadcrumb::breadcrumb(&model.breadcrumb, trailing(model))];
    if let Some(archived) = model.archived {
        page = page.push(layout::archived_banner(&model.id, archived));
    }
    let (alone, under_strip) = TRANSCRIPT_TOP;
    let top = if model.children.is_empty() {
        alone
    } else {
        page = page.push(centered(
            children_strip(model),
            width,
            Padding::ZERO.top(under_strip),
        ));
        under_strip
    };
    // The scrollable spans the window, so its scrollbar sits at the window's edge, clear of
    // the column.
    let transcript = scrollable(centered(
        selectable::area(entries::transcript(model, state, tokens)).into(),
        width,
        Padding::ZERO.top(top),
    ))
    .anchor_bottom()
    .id(state.scroll.clone())
    .width(Length::Fill)
    .height(Length::Fill);
    page = page.push(transcript);
    // An archived session is read-only: it takes no prompt until it is restored.
    if model.archived.is_none() {
        page = page.push(centered(
            composer(model, state),
            width,
            Padding::ZERO.top(12).bottom(16),
        ));
    }
    page.into()
}

/// `content` in a centered column at most `width` wide, with the window's margin left and
/// right of it and `padding` above and below.
fn centered(content: Element<'_, Message>, width: f32, padding: Padding) -> Element<'_, Message> {
    container(container(content).width(Length::Fill).max_width(width))
        .center_x(Length::Fill)
        .padding(padding.left(MARGIN_X).right(MARGIN_X))
        .into()
}

/// What follows the breadcrumb: the pills (`orchestrator` for an orchestrator, the
/// session's status unless it is idle, and a mark when a session below it waits for the
/// user) and, for a session with children, how many of them are done.
fn trailing<'a>(model: &SessionViewModel<'_>) -> impl Iterator<Item = Element<'a, Message>> {
    let progress = (!model.children.is_empty()).then(|| {
        let done = model
            .children
            .iter()
            .filter(|child| child.status == Status::Done)
            .count();
        format!("{done} of {} children done", model.children.len())
    });
    let small = |words: String| text(words).size(theme::SMALL);
    [
        model.orchestrator.then(badge::orchestrator),
        badge::status(shown_status(model)),
        model
            .attention
            .then(|| badge::pill(CHILD_ASKS_YOU, |tokens| tokens.asks_you)),
        progress
            .is_some()
            .then(|| small("·".to_owned()).style(style::dim).into()),
        progress.map(|progress| small(progress).style(style::muted).into()),
    ]
    .into_iter()
    .flatten()
}

/// Where the session stands, as its pill says it: its own status, except that a session
/// that is done while a child of it is in a turn shows as running, like its row in the
/// tree, since the work is not over.
fn shown_status(model: &SessionViewModel<'_>) -> Status {
    if model.status == Status::Done && model.children.iter().any(in_turn) {
        Status::Running
    } else {
        model.status
    }
}

/// The strip of the session's children: one card each, in the order they were started, at
/// most [`CARDS_PER_LINE`] on a line and all of the same width.
fn children_strip<'a>(model: &SessionViewModel<'_>) -> Element<'a, Message> {
    // The view model lists the newest child first.
    let children: Vec<&SessionNode> = model.children.iter().rev().collect();
    let per_line = children.len().min(CARDS_PER_LINE);
    let lines = children.chunks(CARDS_PER_LINE).map(|line| {
        let cards = line.iter().map(|child| child_card(child, per_line));
        // A line that is not full keeps the width of its cards: empty room stands in for
        // the missing ones.
        let missing = per_line.saturating_sub(line.len());
        let room = (0..missing).map(|_| Space::new().width(Length::Fill).into());
        Row::with_children(cards.chain(room))
            .spacing(CARD_GAP)
            .into()
    });
    Column::with_children(lines).spacing(CARD_GAP).into()
}

/// The card of `child`, found by `child-<id>`, in a strip with `per_line` cards on a line:
/// the repository it works in, its title, its status and, at the right, how many tokens of
/// its context it has used, once that is known. A click opens the child.
fn child_card<'a>(child: &SessionNode, per_line: usize) -> Element<'a, Message> {
    let mut line = row![
        text(child_name(child).to_owned()).font(theme::UI_MEDIUM),
        text(keep_start(&child.title, card_title_budget(per_line)))
            .size(theme::SMALL)
            .style(style::muted),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if let Some(status) = badge::status(child.status) {
        line = line.push(status);
    }
    line = line.push(space::horizontal());
    if let Some((used, _)) = child.usage {
        line = line.push(
            text(tokens_label::compact(used))
                .font(theme::MONO)
                .size(theme::SMALL)
                .style(style::muted),
        );
    }
    let card = button(line)
        .padding([8, 12])
        .width(Length::Fill)
        .style(style::event_row_button)
        .on_press(Message::Projects(ProjectsMessage::Select(child.id.clone())));
    container(card)
        .id(format!("child-{}", child.id))
        .width(Length::Fill)
        .into()
}

/// The most characters of a child's title its card shows when `per_line` cards share a
/// line: the narrower the card, the less of the title fits beside the rest.
fn card_title_budget(per_line: usize) -> usize {
    match per_line {
        0 | 1 => 60,
        2 => 28,
        _ => 10,
    }
}

/// What a child is called beside its siblings: the repository it works in, which tells
/// them apart, or its title when it works in none.
fn child_name(child: &SessionNode) -> &str {
    if child.scope_label.is_empty() {
        &child.title
    } else {
        &child.scope_label
    }
}

/// Whether `child` is in a turn: it runs, or it waits for the user's answer to a
/// permission request (a child proposes nothing, so it waits for nothing else).
fn in_turn(child: &SessionNode) -> bool {
    matches!(child.status, Status::Running | Status::WaitingForYou)
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

/// Applies `message` to the Session tab and returns the follow-up work.
///
/// Prompts, permission answers and settings go to the current session through the core
/// [`App`]; when it refuses, the error bar shows why. A clicked link is copied to the
/// clipboard: opening links is not supported yet.
pub fn update(state: &mut State, message: SessionMessage) -> Task<Message> {
    let session = &mut state.session;
    match message {
        SessionMessage::Prompt(action) => session.prompt.perform(action),
        SessionMessage::Submit => return submit(state),
        SessionMessage::Stop => {
            app::report(&mut state.error, state.app.cancel());
        }
        SessionMessage::Attach => {
            return Task::perform(pick_files(), |files| {
                Message::Session(SessionMessage::Attached(files))
            });
        }
        SessionMessage::Attached(files) => session.attachments.extend(files),
        SessionMessage::RemoveAttachment(index) => {
            if index < session.attachments.len() {
                session.attachments.remove(index);
            }
        }
        SessionMessage::ToggleEntry(index) => {
            if let Some(current) = state.app.current_session() {
                toggle(&mut session.expanded, (current, index));
            }
        }
        SessionMessage::ToggleTools(first) => {
            if let Some(current) = state.app.current_session() {
                toggle(&mut session.expanded_tools, (current, first));
            }
        }
        SessionMessage::AnswerPermission(permission, option) => {
            on_current_session(state, |app, session| {
                app.answer_permission(session, permission, option)
            });
        }
        SessionMessage::SetAutoAllow(enabled) => {
            on_current_session(state, |app, session| app.set_auto_allow(session, enabled));
        }
        SessionMessage::SetBackend(backend) => {
            on_current_session(state, |app, session| app.switch_backend(session, backend));
            // The switch can append a notice to the transcript.
            sync_markdown(state);
        }
        SessionMessage::SetModel(value) => {
            on_current_session(state, |app, session| app.set_model(session, value));
        }
        SessionMessage::SetEffort(value) => {
            on_current_session(state, |app, session| app.set_effort(session, value));
        }
        SessionMessage::LinkClicked(uri) => return iced::clipboard::write(uri),
        SessionMessage::Proposal(id, message) => return proposal::update(state, id, message),
        SessionMessage::StopChildren => {
            on_current_session(state, |app, session| app.stop_children(&session));
        }
    }
    Task::none()
}

/// What Enter does on the Session tab when no widget took the key: it approves the newest
/// undecided proposal of the current session, as its `Approve split` button does, unless
/// the session is archived, which keeps it read-only.
pub(crate) fn press_enter(state: &mut State) -> Task<Message> {
    let archived = state
        .app
        .session_view()
        .is_some_and(|model| model.archived.is_some());
    if archived {
        return Task::none();
    }
    proposal::approve_newest(state)
}

/// Takes `entry` out of `expanded` when it is in it, and puts it in when it is not.
fn toggle(expanded: &mut HashSet<(SessionId, usize)>, entry: (SessionId, usize)) {
    if !expanded.remove(&entry) {
        expanded.insert(entry);
    }
}

/// Parses what the current session's answers gained since the last call, and gives every
/// undecided proposal of the session a draft for its approval card.
///
/// Runs after every change that can add answer text or proposals or switch the session, so
/// the view only reads parsed Markdown and existing drafts.
pub(crate) fn sync_markdown(state: &mut State) {
    if let Some(model) = state.app.session_view() {
        state.session.markdown.sync(&model);
    }
    proposal::sync_drafts(state);
}

/// Sends the prompt and the attachments, then empties the prompt box and scrolls to the end.
///
/// The prompt goes out trimmed; a blank prompt without attachments is not sent.
fn submit(state: &mut State) -> Task<Message> {
    let session = &mut state.session;
    let text = session.prompt.text();
    let prompt = text.trim();
    if !sendable(prompt, &session.attachments) {
        return Task::none();
    }
    let attachments = session
        .attachments
        .iter()
        .map(|path| Attachment { path: path.clone() })
        .collect();
    if app::report(
        &mut state.error,
        state.app.submit_prompt(prompt, attachments),
    )
    .is_none()
    {
        return Task::none();
    }
    state.session.prompt = text_editor::Content::new();
    state.session.attachments.clear();
    operation::snap_to_end(state.session.scroll.clone())
}

/// Whether there is something to send: trimmed prompt text or an attachment.
fn sendable(trimmed_prompt: &str, attachments: &[PathBuf]) -> bool {
    !trimmed_prompt.is_empty() || !attachments.is_empty()
}

/// Runs `command` on the current session, showing its error in the error bar.
fn on_current_session(
    state: &mut State,
    command: impl FnOnce(&mut App, SessionId) -> Result<(), AppError>,
) {
    let result = state
        .app
        .current_session()
        .ok_or(AppError::NoCurrentSession)
        .and_then(|session| command(&mut state.app, session));
    app::report(&mut state.error, result);
}

/// Lets the user pick files in the desktop's file dialog; none when it is cancelled.
async fn pick_files() -> Vec<PathBuf> {
    let files = rfd::AsyncFileDialog::new().pick_files().await;
    files
        .unwrap_or_default()
        .iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}

/// Everything under the transcript: the attachments, the framed prompt and the options row.
fn composer<'a>(model: &SessionViewModel<'_>, state: &'a SessionState) -> Element<'a, Message> {
    let mut composer = Column::new().spacing(8);
    if !state.attachments.is_empty() {
        composer = composer.push(attachment_chips(&state.attachments));
    }
    composer
        .push(prompt_frame(model, state))
        .push(options(model))
        .into()
}

/// The frame around the attach button, the prompt box, the keys that send the prompt and
/// break its line, and the send button. While a turn runs the stop button stands for the
/// send button and the keys are not named, since Enter then sends nothing.
fn prompt_frame<'a>(model: &SessionViewModel<'_>, state: &'a SessionState) -> Element<'a, Message> {
    let running = model.running();
    let attach = button(container(icon(Icon::Plus, |tokens| tokens.muted)).center(Length::Fill))
        .padding(0)
        .width(ATTACH_SIZE)
        .height(ATTACH_SIZE)
        .style(style::row_button)
        .on_press(Message::Session(SessionMessage::Attach));
    // The frame is the prompt's only border, so the box itself has none and no padding.
    let editor = text_editor(&state.prompt)
        .id(PROMPT)
        .placeholder(placeholder(model))
        .on_action(|action| Message::Session(SessionMessage::Prompt(action)))
        .key_binding(move |press| prompt_binding(press, running))
        .padding(0)
        .line_height(Pixels(theme::LINE_HEIGHT))
        .max_height(PROMPT_MAX_HEIGHT)
        .style(|theme: &Theme, _status| {
            let tokens = theme::tokens(theme);
            text_editor::Style {
                background: tokens.panel.into(),
                border: Border::default(),
                placeholder: tokens.muted,
                value: tokens.text,
                selection: tokens.selection,
            }
        });
    let mut line = row![attach, editor].spacing(10).align_y(Alignment::Center);
    let label = |word| text(word).size(theme::SMALL);
    line = if running {
        line.push(
            button(label("Stop"))
                .padding([3, 10])
                .style(style::danger_button)
                .on_press(Message::Session(SessionMessage::Stop)),
        )
    } else {
        let can_send = sendable(state.prompt.text().trim(), &state.attachments);
        line.push(key_hints()).push(
            button(label("Send"))
                .padding([3, 10])
                .style(style::primary_button)
                .on_press_maybe(can_send.then_some(Message::Session(SessionMessage::Submit))),
        )
    };
    container(line)
        .padding([10, 14])
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.panel.into()),
                border: Border::default()
                    .rounded(style::CARD_RADIUS)
                    .width(1)
                    .color(tokens.border_input),
                ..container::Style::default()
            }
        })
        .into()
}

/// What the empty prompt box says: whom the prompt goes to. An orchestrator's says what a
/// prompt is good for now: changing the split while a proposal is undecided, steering
/// while its children work.
fn placeholder(model: &SessionViewModel<'_>) -> String {
    if model.orchestrator {
        let undecided = |entry: &Entry| matches!(entry, Entry::Proposal { decision: None, .. });
        if model.entries.iter().any(undecided) {
            return CHANGE_THE_SPLIT.to_owned();
        }
        if model.children.iter().any(in_turn) {
            return STEER.to_owned();
        }
    }
    let scope = model.breadcrumb.last().map_or("", String::as_str);
    format!("Ask {scope}…")
}

/// The keys the prompt box answers to, each with what it does.
fn key_hints<'a>() -> Element<'a, Message> {
    let hints = [("Enter", "send"), ("Shift Enter", "newline")]
        .into_iter()
        .map(|(keys, action)| {
            row![
                badge::key_cap(keys),
                text(action).size(theme::TINY).style(style::muted)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        });
    Row::with_children(hints).spacing(10).into()
}

/// One chip per attached file: its name and a button that removes it.
fn attachment_chips(attachments: &[PathBuf]) -> Element<'_, Message> {
    let chips = attachments.iter().enumerate().map(|(index, path)| {
        let remove = button(icon(Icon::Close, |tokens| tokens.muted))
            .padding(2)
            .style(style::row_button)
            .on_press(Message::Session(SessionMessage::RemoveAttachment(index)));
        let remove = container(remove).id(format!("remove-attachment-{index}"));
        container(
            row![text(file_name(path)).size(theme::SMALL), remove]
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .padding(Padding::from([1, 4]).left(8))
        .style(badge::surface)
        .into()
    });
    Row::with_children(chips).spacing(6).wrap().into()
}

/// The file's name, or the whole path when it has none.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The row under the prompt: the model and effort pickers (when the backend offers them),
/// the backend picker, the auto-allow switch, the button that stops an orchestrator's
/// children (while one of them is in a turn) and, at the right, the context usage (once
/// the agent reported it).
fn options<'a>(model: &SessionViewModel<'_>) -> Element<'a, Message> {
    let mut options = Row::new().spacing(18).align_y(Alignment::Center);
    if let Some(choice) = &model.model {
        let picker = picker(choice, SessionMessage::SetModel);
        options = options.push(labelled("Model:", container(picker).id(MODEL)));
    }
    if let Some(choice) = &model.effort {
        let picker = picker(choice, SessionMessage::SetEffort);
        options = options.push(labelled("Effort:", container(picker).id(EFFORT)));
    }
    let backend = container(backend_picker(model)).id(BACKEND);
    let auto_allow = toggler(model.auto_allow)
        .label("auto-allow")
        .size(14)
        .spacing(6)
        .text_size(theme::SMALL)
        .style(style::switch)
        .on_toggle(|enabled| Message::Session(SessionMessage::SetAutoAllow(enabled)));
    options = options
        .push(labelled("Backend:", backend))
        .push(container(auto_allow).id(AUTO_ALLOW));
    if model.orchestrator && model.children.iter().any(in_turn) {
        options = options.push(
            button(text("Stop children").size(theme::SMALL))
                .padding([3, 6])
                .style(style::danger_button)
                .on_press(Message::Session(SessionMessage::StopChildren)),
        );
    }
    options = options.push(space::horizontal());
    if let Some((used, size)) = model.usage {
        options = options.push(usage(used, size));
    }
    container(options).padding([0, 4]).into()
}

/// `value` behind the muted `label` that says what it sets.
fn labelled<'a>(label: &'a str, value: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        text(label).size(theme::SMALL).style(style::muted),
        value.into()
    ]
    .align_y(Alignment::Center)
    .into()
}

/// The context usage: a bar filled as far as `used` of `size` tokens reach, then both
/// counts.
fn usage<'a>(used: u64, size: u64) -> Element<'a, Message> {
    let (width, height) = USAGE_BAR;
    let part = |width: f32, pick: fn(&Tokens) -> iced::Color| {
        container(Space::new())
            .width(width)
            .height(height)
            .style(move |theme: &Theme| container::Style {
                background: Some(pick(theme::tokens(theme)).into()),
                border: Border::default().rounded(height / 2.0),
                ..container::Style::default()
            })
    };
    let filled = f32::from(usage_fill(used, size, width));
    let bar = stack![
        part(f32::from(width), |tokens| tokens.border),
        part(filled, |tokens| tokens.muted_strong)
    ];
    row![
        bar,
        text(tokens_label::usage(used, size))
            .font(theme::MONO)
            .size(theme::SMALL)
            .style(style::muted)
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// How many of a bar's `width` pixels (or equal parts of any other kind) are filled when
/// `used` of `size` tokens are used: none for an unknown size, all of them for a usage
/// beyond the size.
pub(crate) fn usage_fill(used: u64, size: u64, width: u16) -> u16 {
    // Twice the bits of a count, so no count is too large to multiply by the width.
    let filled = (u128::from(used.min(size)) * u128::from(width))
        .checked_div(u128::from(size))
        .unwrap_or(0);
    u16::try_from(filled).unwrap_or(width)
}

/// The configured backends to switch the session to, with the session's own selected.
///
/// While a turn runs the session cannot switch, and iced's pick list has no disabled state,
/// so the backend's name alone stands in for it.
fn backend_picker<'a>(model: &SessionViewModel<'_>) -> Element<'a, Message> {
    let (options, current) = backend_choices(model);
    if model.running() {
        let name = current.map_or_else(|| model.backend.to_string(), |option| option.name);
        return container(text(name).size(theme::SMALL).style(style::soft))
            .padding(PICKER_PADDING)
            .into();
    }
    let configured = current.is_some();
    let picker = pick_list(options, current, |picked: BackendChoice| {
        Message::Session(SessionMessage::SetBackend(picked.id))
    })
    .text_size(theme::SMALL)
    .padding(PICKER_PADDING)
    .handle(pick_list::Handle::None)
    .style(bare_picker)
    .menu_style(style::menu);
    // A pick list is as wide as its widest text, so the placeholder is set only when it
    // shows: beside a backend's name it would only push the chevron away.
    if configured {
        with_chevron(picker)
    } else {
        with_chevron(picker.placeholder(format!("{} (not configured)", model.backend)))
    }
}

/// The pick list's entries (the configured backends, in order) and the one the session runs
/// on, which is none when that backend is no longer configured.
fn backend_choices(model: &SessionViewModel<'_>) -> (Vec<BackendChoice>, Option<BackendChoice>) {
    let options: Vec<BackendChoice> = model.backends.iter().map(BackendChoice::from).collect();
    let current = options
        .iter()
        .find(|option| option.id == model.backend)
        .cloned();
    (options, current)
}

/// One configured backend in the backend picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct BackendChoice {
    id: BackendId,
    name: String,
}

impl From<&BackendInfo> for BackendChoice {
    fn from(info: &BackendInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
        }
    }
}

impl fmt::Display for BackendChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// A picker over `choice`'s values that sends `on_select` with the picked value.
fn picker<'a>(
    choice: &ChoiceView,
    on_select: fn(ConfigValueId) -> SessionMessage,
) -> Element<'a, Message> {
    let options: Vec<Choice> = choice
        .choices
        .iter()
        .map(|option| Choice {
            id: option.id.clone(),
            name: option.name.clone(),
        })
        .collect();
    let current = Choice {
        id: choice.current.clone(),
        name: choice.current_name.clone(),
    };
    let picker = pick_list(options, Some(current), move |picked: Choice| {
        Message::Session(on_select(picked.id))
    })
    .text_size(theme::SMALL)
    .padding(PICKER_PADDING)
    .handle(pick_list::Handle::None)
    .style(bare_picker)
    .menu_style(style::menu);
    with_chevron(picker)
}

/// `picker` with a chevron at its right end. A pick list keeps room for a handle there
/// even when it draws none; the chevron lies over that room and takes no clicks, so a
/// click on it opens the picker like one on the value.
fn with_chevron<'a>(picker: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let chevron = icon(Icon::ChevronDown, |tokens| tokens.muted)
        .width(CHEVRON_SIZE)
        .height(CHEVRON_SIZE);
    stack![
        picker.into(),
        container(chevron)
            .align_right(Length::Fill)
            .center_y(Length::Fill)
    ]
    .into()
}

/// A picker without a frame: only its value, which lights up under the pointer.
fn bare_picker(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let tokens = theme::tokens(theme);
    pick_list::Style {
        text_color: if status == pick_list::Status::Active {
            tokens.text_soft
        } else {
            tokens.text
        },
        placeholder_color: tokens.muted,
        handle_color: tokens.muted,
        // The row lies on the window's background, so this fill does not show.
        background: tokens.bg.into(),
        border: Border::default(),
    }
}

/// One value in a picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct Choice {
    id: ConfigValueId,
    name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// The prompt box's keys: Enter sends (and does nothing while a turn is `running`, keeping
/// the text), Shift+Enter breaks the line, the tab shortcuts switch tabs instead of typing
/// their digit, and Ctrl+K opens the jump panel instead of typing its letter. Everything
/// else edits as usual.
///
/// Enter stays with the box even while it does nothing: a key the box lets through goes
/// to the window, where Enter approves a proposed split.
fn prompt_binding(
    press: text_editor::KeyPress,
    running: bool,
) -> Option<text_editor::Binding<Message>> {
    if !matches!(press.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    if let Some(tab) = app::tab_shortcut(&press.key, press.physical_key, press.modifiers) {
        return Some(text_editor::Binding::Custom(Message::SelectTab(tab)));
    }
    if app::jump_shortcut(&press.key, press.physical_key, press.modifiers) {
        return Some(text_editor::Binding::Custom(Message::Jump(
            JumpMessage::Open,
        )));
    }
    match press.key.as_ref() {
        keyboard::Key::Named(key::Named::Enter) if !press.modifiers.shift() => Some(if running {
            text_editor::Binding::Sequence(Vec::new())
        } else {
            text_editor::Binding::Custom(Message::Session(SessionMessage::Submit))
        }),
        _ => text_editor::Binding::from_key_press(press),
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

    use std::path::PathBuf;

    use futures::channel::mpsc;
    use iced::keyboard::{self, Key, Modifiers, key};
    use iced::widget::{Id, text_editor};
    use iced::{Point, mouse};
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendId, BackendInfo, BackendSessionId, ChildOutcome, ConfigCategory,
        ConfigChoice, ConfigId, ConfigOption, ConfigValueId, Decision, Entry, OrchestratorTool,
        PermissionId, PermissionKind, PermissionOption, PermissionOptionId, PromptContent,
        ProposalId, ProposedChild, RepositoryId, Scope, SessionId, Status, StopReason, ToolCallId,
        ToolContent, ToolKind, ToolRequestId, ToolStatus, TurnOrigin, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Message, State, Tab, boot, update, view};
    use crate::test_support::{
        backend_id, deliver, orchestrated, shown, state_on_removed_backend, two_backends,
    };

    /// A state whose current session was created by the backend with a model and an effort
    /// option, plus the backend that recorded the calls and the session's id.
    struct Fixture {
        state: State,
        backend: RecordingBackend,
        session: SessionId,
        _dir: TempDir,
    }

    fn choice(id: &str, name: &str) -> ConfigChoice {
        ConfigChoice {
            id: ConfigValueId::new(id),
            name: name.into(),
        }
    }

    fn config() -> Vec<ConfigOption> {
        vec![
            ConfigOption {
                id: ConfigId::new("model"),
                name: "Model".into(),
                category: ConfigCategory::Model,
                current: ConfigValueId::new("sonnet"),
                choices: vec![choice("sonnet", "Sonnet"), choice("opus", "Opus")],
            },
            ConfigOption {
                id: ConfigId::new("effort"),
                name: "Effort".into(),
                category: ConfigCategory::Effort,
                current: ConfigValueId::new("medium"),
                choices: vec![choice("low", "Low"), choice("medium", "Medium")],
            },
        ]
    }

    fn fixture(updates: impl IntoIterator<Item = Update>) -> Fixture {
        let (backends, default) = single_claude();
        fixture_on(backends, default, updates)
    }

    /// Like [`fixture`], with `backends` configured and `default` running the session.
    fn fixture_on(
        backends: Vec<BackendInfo>,
        default: BackendId,
        updates: impl IntoIterator<Item = Update>,
    ) -> Fixture {
        fixture_in(TempDir::new().unwrap(), backends, default, updates)
    }

    /// Like [`fixture_on`], with the project opened from `dir`.
    fn fixture_in(
        dir: TempDir,
        backends: Vec<BackendInfo>,
        default: BackendId,
        updates: impl IntoIterator<Item = Update>,
    ) -> Fixture {
        let backend = RecordingBackend::new();
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
        let (request, ..) = backend
            .calls()
            .first()
            .and_then(Call::as_new_session)
            .map(|(request, session, cwd)| (request, session.clone(), cwd.to_path_buf()))
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: config(),
        };
        let events = std::iter::once(created).chain(updates.into_iter().map(|update| {
            BackendEvent::Update {
                session: session.clone(),
                update,
            }
        }));
        apply(&mut state, events.map(Message::Backend));
        state.tab = Tab::Session;
        assert_eq!(state.error, None);
        Fixture {
            state,
            backend,
            session,
            _dir: dir,
        }
    }

    /// A user prompt, an answer, a thought, a tool call and a permission request.
    fn conversation() -> Vec<Update> {
        vec![
            Update::UserText("Explain the build".into()),
            Update::AgentText("Hello **world**".into()),
            Update::AgentThought("the workspace has two crates".into()),
            Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Read Cargo.toml".into(),
                kind: ToolKind::Read,
                status: ToolStatus::Completed,
                content: vec![ToolContent::Text("[workspace]".into())],
                locations: vec![PathBuf::from("Cargo.toml")],
            },
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-2"),
                title: "Run cargo test".into(),
                kind: ToolKind::Execute,
                options: vec![
                    PermissionOption {
                        id: PermissionOptionId::new("allow"),
                        name: "Allow once".into(),
                        kind: PermissionKind::AllowOnce,
                    },
                    PermissionOption {
                        id: PermissionOptionId::new("reject"),
                        name: "Reject".into(),
                        kind: PermissionKind::RejectOnce,
                    },
                ],
                name: None,
            },
        ]
    }

    /// A thought longer than a row shows, and how the row begins it.
    const LONG_THOUGHT: &str = "The workspace has two crates, and the build script of the \
                                second one generates the bindings the first one links against, \
                                so the order of the build matters.";
    const LONG_THOUGHT_BEGINNING: &str = "The workspace has two crates, and the build script of the second one generates the binding…";

    /// A finished call of `kind` titled `title` that touches `file`.
    fn tool_call(id: &str, kind: ToolKind, title: &str, file: PathBuf) -> Update {
        Update::ToolCall {
            id: ToolCallId::new(id),
            title: title.into(),
            kind,
            status: ToolStatus::Completed,
            content: vec![ToolContent::Text(format!("output of {id}"))],
            locations: vec![file],
        }
    }

    /// Feeds every message through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    fn names(choices: &[BackendChoice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.name.as_str()).collect()
    }

    fn session(message: SessionMessage) -> Message {
        Message::Session(message)
    }

    /// A key press with `modifiers` held, as the windowing system reports it.
    fn press(key: Key, text: Option<&str>, modifiers: Modifiers) -> iced::Event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
    }

    #[test]
    fn a_session_renders_user_agent_thought_tool_and_permission_entries() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.find("Explain the build").unwrap();
        ui.find("thinking").unwrap();
        ui.find("the workspace has two crates").unwrap();
        ui.find("tools").unwrap();
        ui.find("Reading").unwrap();
        ui.find("Cargo.toml").unwrap();
        // The call's own line, with its kind and title, shows once the row is expanded.
        assert!(ui.find("Read Cargo.toml").is_err());
        assert!(ui.find("read").is_err());
        ui.find("Run cargo test").unwrap();
        ui.find("Allow once").unwrap();
        ui.find("Reject").unwrap();
        ui.find("asks you").unwrap();
        // The answer is rendered as Markdown from the cache, not as its raw text.
        assert!(ui.find("Hello **world**").is_err());
        let items = fixture
            .state
            .session
            .markdown
            .items(&fixture.session, 1)
            .unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn an_answers_headings_paragraphs_and_code_are_selectable_text() {
        let fixture = fixture([Update::AgentText(
            "# Plan\n\nFirst the **API**.\n\n## Then\n\n### Three\n\n#### Four\n\n##### Five\n\n\
             ###### Six\n\n```rust\nlet x = 1;\n```\n"
                .into(),
        )]);
        let mut ui = simulator(view(&fixture.state));

        // Selectable text says what it holds, so each block is found by its words.
        ui.find("Plan").unwrap();
        ui.find("First the API.").unwrap();
        for heading in ["Then", "Three", "Four", "Five", "Six"] {
            ui.find(heading).unwrap();
        }
        ui.find("let x = 1;").unwrap();
    }

    #[test]
    fn a_calls_diff_patch_and_terminal_output_are_selectable_text() {
        let mut fixture = fixture([Update::ToolCall {
            id: ToolCallId::new("t-1"),
            title: "Edit main.rs".into(),
            kind: ToolKind::Edit,
            status: ToolStatus::Completed,
            content: vec![
                ToolContent::Diff {
                    path: PathBuf::from("src/main.rs"),
                    old: Some("fn old() {}".into()),
                    new: "fn new() {}".into(),
                },
                ToolContent::Patch {
                    paths: vec![PathBuf::from("src/lib.rs")],
                    text: "+added line\n-removed line".into(),
                },
                ToolContent::Terminal("tests passed".into()),
            ],
            locations: Vec::new(),
        }]);
        apply(
            &mut fixture.state,
            [
                session(SessionMessage::ToggleTools(0)),
                session(SessionMessage::ToggleEntry(0)),
            ],
        );
        let mut ui = simulator(view(&fixture.state));

        for words in [
            "src/main.rs",
            "fn old() {}",
            "fn new() {}",
            "src/lib.rs",
            "+added line",
            "-removed line",
            "tests passed",
        ] {
            ui.find(words).unwrap();
        }
    }

    #[test]
    fn orchestration_entries_render_their_labels() {
        let api = RepositoryId::try_from("api").unwrap();
        let web = RepositoryId::try_from("web").unwrap();
        let proposal = ProposalId::new();
        let fixture = fixture([
            Update::OrchestratorPrompt("Add the endpoint".into()),
            Update::PartiturPrompt("children started:".into()),
            Update::ChildResult {
                child: SessionId::new(),
                repository: api.clone(),
                title: "API endpoint".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Failed("boom".into()),
                text: "half done".into(),
            },
            Update::ChildrenProposed {
                id: proposal.clone(),
                children: vec![
                    ProposedChild {
                        repository: api,
                        prompt: "Add the endpoint".into(),
                        title: "Endpoint".into(),
                    },
                    ProposedChild {
                        repository: web,
                        prompt: "Show the endpoint's data".into(),
                        title: "Page".into(),
                    },
                ],
            },
            Update::ProposalDecided {
                id: proposal,
                decision: Decision::Rejected {
                    note: Some("not now".into()),
                },
            },
        ]);
        let mut ui = simulator(view(&fixture.state));

        ui.find("from orchestrator").unwrap();
        // The toolbar's wordmark and the line over Partitur's prompt.
        assert_eq!(shown(&mut ui, "Partitur"), 2);
        ui.find("api failed: boom").unwrap();
        ui.find("turn started by the user").unwrap();
        ui.find("half done").unwrap();
        // A decided proposal says what the user decided, not what was proposed.
        ui.find("You rejected the split.").unwrap();
        ui.find("not now").unwrap();
        assert!(ui.find("Proposed split").is_err());
        assert!(ui.find("Show the endpoint's data").is_err());
    }

    #[test]
    fn a_prompt_names_who_sent_it() {
        let fixture = fixture([Update::UserText("Explain the build".into())]);
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, "you"), 1);
        assert_eq!(shown(&mut ui, "from orchestrator"), 0);
        // Only the toolbar's wordmark.
        assert_eq!(shown(&mut ui, "Partitur"), 1);
        drop(ui);

        let fixture = self::fixture([
            Update::OrchestratorPrompt("Add the endpoint".into()),
            Update::PartiturPrompt("children started:".into()),
        ]);
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, "you"), 0);
        assert_eq!(shown(&mut ui, "from orchestrator"), 1);
        assert_eq!(shown(&mut ui, "Partitur"), 2);
        ui.find("Add the endpoint").unwrap();
        ui.find("children started:").unwrap();
    }

    #[test]
    fn a_long_thought_is_cut_and_expands_to_the_whole() {
        let mut fixture = fixture([Update::AgentThought(LONG_THOUGHT.into())]);
        let mut ui = simulator(view(&fixture.state));

        ui.find(LONG_THOUGHT_BEGINNING).unwrap();
        assert!(ui.find(LONG_THOUGHT).is_err());
        assert!(ui.find("collapse").is_err());
        ui.click("expand").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::ToggleEntry(0))]);

        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find(LONG_THOUGHT).unwrap();
        assert!(ui.find(LONG_THOUGHT_BEGINNING).is_err());
        assert!(ui.find("expand").is_err());
        ui.click("collapse").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::ToggleEntry(0))]);

        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find(LONG_THOUGHT_BEGINNING).unwrap();
    }

    #[test]
    fn a_short_thought_is_shown_whole_without_a_control() {
        let fixture = fixture([Update::AgentThought("the workspace has two crates".into())]);
        let mut ui = simulator(view(&fixture.state));

        ui.find("thinking").unwrap();
        ui.find("the workspace has two crates").unwrap();
        assert!(ui.find("expand").is_err());
        assert!(ui.find("collapse").is_err());
    }

    #[test]
    fn consecutive_tool_calls_share_one_row_and_separate_runs_get_their_own() {
        let call = |id: &str| tool_call(id, ToolKind::Read, id, PathBuf::from(format!("{id}.rs")));
        let fixture = fixture([
            call("one"),
            call("two"),
            Update::AgentText("Now the third".into()),
            call("three"),
        ]);
        let mut ui = simulator(view(&fixture.state));

        assert_eq!(shown(&mut ui, "tools"), 2);
        assert_eq!(shown(&mut ui, "Reading"), 3);
        for file in ["one.rs", "two.rs", "three.rs"] {
            ui.find(file).unwrap();
        }
    }

    #[test]
    fn the_tools_row_names_each_call_by_its_verb_and_relative_path() {
        let dir = TempDir::new().unwrap();
        let (backends, default) = single_claude();
        let inside = dir.path().join("src").join("main.rs");
        let updates = [
            tool_call("t-1", ToolKind::Edit, "Edit main.rs", inside),
            tool_call(
                "t-2",
                ToolKind::Read,
                "Read hosts",
                PathBuf::from("/etc/hosts"),
            ),
        ];
        let fixture = fixture_in(dir, backends, default, updates);
        let mut ui = simulator(view(&fixture.state));

        assert_eq!(shown(&mut ui, "tools"), 1);
        ui.find("Writing").unwrap();
        ui.find("src/main.rs").unwrap();
        // A file outside the session's folder keeps its whole path.
        ui.find("Reading").unwrap();
        ui.find("/etc/hosts").unwrap();
        assert!(ui.find("+1 more").is_err());
    }

    #[test]
    fn a_call_without_a_file_or_a_verb_is_named_by_its_title() {
        let fixture = fixture([
            Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "cargo test".into(),
                kind: ToolKind::Execute,
                status: ToolStatus::InProgress,
                content: Vec::new(),
                locations: Vec::new(),
            },
            tool_call(
                "t-2",
                ToolKind::Other,
                "Plan the release",
                PathBuf::from("notes.md"),
            ),
        ]);
        let mut ui = simulator(view(&fixture.state));

        ui.find("Running").unwrap();
        ui.find("cargo test").unwrap();
        ui.find("Plan the release").unwrap();
        assert!(ui.find("notes.md").is_err());
    }

    #[test]
    fn calls_beyond_the_rows_budget_are_counted() {
        let call = |id: &str| {
            let file = PathBuf::from(format!("src/{id}_handler.rs"));
            tool_call(id, ToolKind::Read, id, file)
        };
        let fixture = fixture(["first", "second", "third", "fourth"].map(call));
        let mut ui = simulator(view(&fixture.state));

        ui.find("src/first_handler.rs").unwrap();
        ui.find("src/second_handler.rs").unwrap();
        assert!(ui.find("src/third_handler.rs").is_err());
        assert!(ui.find("src/fourth_handler.rs").is_err());
        ui.find("+2 more").unwrap();
    }

    #[test]
    fn an_expanded_tools_row_lists_its_calls_and_a_call_expands_to_its_content() {
        let mut fixture = fixture([
            Update::UserText("Explain the build".into()),
            tool_call(
                "t-1",
                ToolKind::Read,
                "Read Cargo.toml",
                PathBuf::from("Cargo.toml"),
            ),
            tool_call(
                "t-2",
                ToolKind::Search,
                "Search for build.rs",
                PathBuf::from("crates"),
            ),
        ]);
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Read Cargo.toml").is_err());
        ui.click("expand").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::ToggleTools(1))]);

        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find("2 calls").unwrap();
        ui.find("read").unwrap();
        ui.find("search").unwrap();
        assert_eq!(shown(&mut ui, "done"), 2);
        assert!(ui.find("expand").is_err());
        // The summary gave way to the list.
        assert!(ui.find("Reading").is_err());
        assert!(ui.find("output of t-2").is_err());
        ui.find("Read Cargo.toml").unwrap();
        ui.click("Search for build.rs").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::ToggleEntry(2))]);

        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find("output of t-2").unwrap();
        ui.find("crates").unwrap();
        assert!(ui.find("output of t-1").is_err());
        ui.click("collapse").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::ToggleTools(1))]);

        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find("Reading").unwrap();
        assert!(ui.find("Search for build.rs").is_err());
    }

    #[test]
    fn an_expanded_entry_stays_with_its_session() {
        let mut fixture = fixture([
            Update::UserText("Explain the build".into()),
            Update::AgentText("Hello".into()),
            Update::AgentThought(LONG_THOUGHT.into()),
        ]);
        apply(
            &mut fixture.state,
            [session(SessionMessage::ToggleEntry(2))],
        );
        let project = fixture.state.app.tree().projects[0].id.clone();
        let other = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        let request = fixture
            .backend
            .calls()
            .last()
            .and_then(Call::as_new_session)
            .map(|(request, ..)| request)
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session: other.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: Vec::new(),
        };
        let updates = [
            Update::UserText("Another question".into()),
            Update::AgentText("Another answer".into()),
            Update::AgentThought(format!("Other thoughts. {LONG_THOUGHT}")),
        ];
        let events = std::iter::once(created).chain(updates.into_iter().map(|update| {
            BackendEvent::Update {
                session: other.clone(),
                update,
            }
        }));
        apply(&mut fixture.state, events.map(Message::Backend));

        // The other session's thought sits at the same index and is not expanded.
        let mut ui = simulator(view(&fixture.state));
        ui.find("expand").unwrap();
        assert!(ui.find("collapse").is_err());
        drop(ui);

        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.session.clone()),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("collapse").unwrap();
        ui.find(LONG_THOUGHT).unwrap();
    }

    #[test]
    fn enter_submits_and_shift_enter_inserts_a_newline() {
        let fixture = fixture(Vec::new());
        let prompt = Id::new("prompt");

        let mut ui = simulator(view(&fixture.state));
        let _ = ui.tap_key(key::Named::Enter);
        let unfocused: Vec<Message> = ui.into_messages().collect();
        assert!(!unfocused.contains(&session(SessionMessage::Submit)));

        let mut ui = simulator(view(&fixture.state));
        ui.click(prompt.clone()).unwrap();
        let _ = ui.typewrite("hi");
        let _ = ui.tap_key(key::Named::Enter);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.contains(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Insert('h')
            ))))
        );
        assert_eq!(messages.last(), Some(&session(SessionMessage::Submit)));

        let mut ui = simulator(view(&fixture.state));
        ui.click(prompt).unwrap();
        let _ = ui.simulate([press(Key::Named(key::Named::Enter), None, Modifiers::SHIFT)]);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(!messages.contains(&session(SessionMessage::Submit)));
        assert_eq!(
            messages.last(),
            Some(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Enter
            ))))
        );
    }

    #[test]
    fn ctrl_and_a_digit_in_the_prompt_select_a_tab_instead_of_typing() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click(Id::new("prompt")).unwrap();
        let _ = ui.simulate([press(
            Key::Character("1".into()),
            Some("1"),
            Modifiers::CTRL,
        )]);
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages.last(), Some(&Message::SelectTab(Tab::Projects)));
        assert!(
            !messages.contains(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Insert('1')
            ))))
        );
    }

    #[test]
    fn ctrl_and_k_in_the_prompt_open_the_jump_panel_instead_of_typing() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click(Id::new("prompt")).unwrap();
        let _ = ui.simulate([press(
            Key::Character("k".into()),
            Some("k"),
            Modifiers::CTRL,
        )]);
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages.last(), Some(&Message::Jump(JumpMessage::Open)));
        assert!(
            !messages.contains(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Insert('k')
            ))))
        );
    }

    #[test]
    fn submitting_sends_the_prompt_and_clears_the_editor() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            "  Run it"
                .chars()
                .map(text_editor::Edit::Insert)
                .chain([text_editor::Edit::Enter, text_editor::Edit::Enter])
                .map(|edit| session(SessionMessage::Prompt(text_editor::Action::Edit(edit))))
                .chain([
                    session(SessionMessage::Attached(vec![PathBuf::from(
                        "/tmp/notes.txt",
                    )])),
                    session(SessionMessage::Submit),
                ]),
        );

        let prompt = fixture
            .backend
            .calls()
            .into_iter()
            .rev()
            .find_map(|call| match call {
                Call::Prompt { content, .. } => Some(content),
                _ => None,
            });
        let prompt = prompt.unwrap();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt.first(), Some(&PromptContent::Text("Run it".into())));
        let entries = fixture.state.app.session_view().unwrap().entries.to_vec();
        assert_eq!(entries.last(), Some(&Entry::User("Run it".into())));
        assert_eq!(fixture.state.session.prompt.text(), "");
        assert!(fixture.state.session.attachments.is_empty());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn an_empty_prompt_without_attachments_is_not_sent() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [text_editor::Edit::Insert(' '), text_editor::Edit::Enter]
                .map(|edit| session(SessionMessage::Prompt(text_editor::Action::Edit(edit)))),
        );

        let mut ui = simulator(view(&fixture.state));
        ui.click("Send").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "Send is disabled");
        apply(&mut fixture.state, [session(SessionMessage::Submit)]);

        assert_eq!(fixture.state.session.prompt.text(), " \n");
        assert!(
            !fixture
                .backend
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Prompt { .. }))
        );
        assert!(!fixture.state.app.session_view().unwrap().running());
    }

    #[test]
    fn attachments_show_as_chips_that_can_be_removed() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [session(SessionMessage::Attached(vec![
                PathBuf::from("/tmp/a.png"),
                PathBuf::from("/tmp/b.txt"),
            ]))],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("a.png").unwrap();
        ui.find("b.txt").unwrap();

        ui.click(Id::new("remove-attachment-0")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::RemoveAttachment(0))]);
        apply(&mut fixture.state, messages);

        assert_eq!(
            fixture.state.session.attachments,
            [PathBuf::from("/tmp/b.txt")]
        );
    }

    #[test]
    fn answering_a_permission_sends_the_option_to_the_app() {
        let mut fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click("Allow once").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [session(SessionMessage::AnswerPermission(
                PermissionId(1),
                Some(PermissionOptionId::new("allow"))
            ))]
        );
        apply(&mut fixture.state, messages);

        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::RespondPermission {
                session: fixture.session.clone(),
                permission: PermissionId(1),
                option: Some(PermissionOptionId::new("allow")),
            })
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("answered · Allow once").unwrap();
        assert!(ui.find("Reject").is_err());
    }

    #[test]
    fn choosing_a_model_calls_set_model() {
        let mut fixture = fixture(Vec::new());
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(MODEL)).unwrap();
        ui.find(Id::new(EFFORT)).unwrap();
        // The picked value comes from the pick list's overlay menu, whose entries selectors
        // cannot find; the messages it would send are applied directly.
        drop(ui);

        apply(
            &mut fixture.state,
            [
                session(SessionMessage::SetModel(ConfigValueId::new("opus"))),
                session(SessionMessage::SetEffort(ConfigValueId::new("low"))),
            ],
        );

        let calls = fixture.backend.calls();
        let set: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                Call::SetConfig { option, value, .. } => Some((option.clone(), value.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            set,
            [
                (ConfigId::new("model"), ConfigValueId::new("opus")),
                (ConfigId::new("effort"), ConfigValueId::new("low")),
            ]
        );
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn the_backend_picker_lists_the_configured_backends_and_selects_the_current() {
        let (backends, default) = two_backends();
        let fixture = fixture_on(backends.clone(), default, Vec::new());
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backends, backends);

        // A pick list exposes neither its selection nor its menu entries to selectors, so
        // the entries it is built from are checked directly.
        let (options, current) = backend_choices(&model);
        assert_eq!(names(&options), ["claude-main", "claude-work"]);
        assert_eq!(current.map(|choice| choice.id), Some(model.backend.clone()));
        assert_eq!(model.backend, backend_id("claude-main"));
        drop(model);
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(BACKEND)).unwrap();
    }

    #[test]
    fn choosing_another_backend_switches_the_session() {
        let (backends, default) = two_backends();
        let said = [Update::AgentText("Hello".into())];
        let mut fixture = fixture_on(backends, default, said);
        let calls = fixture.backend.calls().len();

        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id(
                "claude-work",
            )))],
        );

        assert_eq!(fixture.state.error, None);
        // The new agent is asked for a session at once.
        let recorded = fixture.backend.calls();
        assert_eq!(recorded.len(), calls + 1);
        let created = recorded.last().unwrap();
        assert!(matches!(
            created,
            Call::NewSession { backend, .. } if *backend == backend_id("claude-work")
        ));
        let (request, ..) = created.as_new_session().unwrap();
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("claude-work"));
        let (_, current) = backend_choices(&model);
        assert_eq!(
            current.map(|choice| choice.id),
            Some(backend_id("claude-work"))
        );
        drop(model);
        let mut ui = simulator(view(&fixture.state));
        // The transcript had an entry, so the switch is noticed.
        ui.find("switched to claude-work; the new agent has no memory of earlier turns")
            .unwrap();
        ui.find(Id::new(BACKEND)).unwrap();
        assert!(ui.find(Id::new(MODEL)).is_err());
        assert!(ui.find(Id::new(EFFORT)).is_err());
        drop(ui);

        // The pickers come back as soon as the new agent reports its options.
        let session = fixture.session.clone();
        apply(
            &mut fixture.state,
            [Message::Backend(BackendEvent::SessionCreated {
                request,
                session,
                backend_session: BackendSessionId::new("work-1"),
                config: config(),
            })],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(MODEL)).unwrap();
        ui.find(Id::new(EFFORT)).unwrap();
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn the_backend_picker_is_disabled_while_running() {
        let (backends, default) = two_backends();
        let mut fixture = fixture_on(backends, default, Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();

        // Disabled: the picker takes no clicks, and a switch that gets through anyway is
        // refused with the reason in the error bar.
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(BACKEND)).unwrap();
        ui.find("claude-main").unwrap();
        ui.click(Id::new(BACKEND)).unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id(
                "claude-work",
            )))],
        );

        let error = fixture.state.error.clone().unwrap();
        assert!(error.contains("a turn is still running"), "{error}");
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("claude-main"));
    }

    #[test]
    fn switching_to_an_unknown_backend_shows_the_error() {
        let mut fixture = fixture(Vec::new());

        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id("nope")))],
        );

        let error = fixture.state.error.clone().unwrap();
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn the_picker_offers_configured_backends_for_a_removed_one() {
        let dir = TempDir::new().unwrap();
        let mut state = state_on_removed_backend(&dir);
        state.tab = Tab::Session;
        let model = state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("gone"));
        let (options, current) = backend_choices(&model);
        assert_eq!(names(&options), ["claude-main", "claude-work"]);
        assert_eq!(current, None);
        drop(model);

        let mut ui = simulator(view(&state));
        ui.find(Id::new(BACKEND)).unwrap();
    }

    #[test]
    fn without_config_options_the_pickers_are_hidden() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [Message::Backend(BackendEvent::Update {
                session: fixture.session.clone(),
                update: Update::ConfigOptions(Vec::new()),
            })],
        );

        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find(Id::new(MODEL)).is_err());
        assert!(ui.find(Id::new(EFFORT)).is_err());
        ui.find(Id::new(AUTO_ALLOW)).unwrap();
    }

    #[test]
    fn the_auto_allow_toggle_answers_waiting_requests() {
        let mut fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click(Id::new(AUTO_ALLOW)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::SetAutoAllow(true))]);
        apply(&mut fixture.state, messages);

        assert!(fixture.state.app.session_view().unwrap().auto_allow);
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::RespondPermission { .. })
        ));
    }

    #[test]
    fn the_stop_button_replaces_submit_while_running() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();

        // Enter sends nothing while the turn runs, so the keys are not named.
        let mut ui = simulator(view(&fixture.state));
        ui.find("Stop").unwrap();
        assert!(ui.find("Send").is_err());
        for hint in ["Enter", "send", "Shift Enter", "newline"] {
            assert!(ui.find(hint).is_err(), "{hint}");
        }
        ui.click("Stop").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::Stop)]);
        apply(&mut fixture.state, messages);
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::Cancel { .. })
        ));

        let ended = BackendEvent::Update {
            session: fixture.session.clone(),
            update: Update::TurnEnded(StopReason::Cancelled),
        };
        apply(&mut fixture.state, [Message::Backend(ended)]);
        let mut ui = simulator(view(&fixture.state));
        ui.find("Send").unwrap();
        assert!(ui.find("Stop").is_err());
        for hint in ["Enter", "send", "Shift Enter", "newline"] {
            ui.find(hint).unwrap();
        }
        ui.find("cancelled").unwrap();
    }

    #[test]
    fn a_cancelled_permission_request_says_so() {
        let mut fixture = fixture(conversation());

        apply(
            &mut fixture.state,
            [session(SessionMessage::AnswerPermission(
                PermissionId(1),
                None,
            ))],
        );

        assert_eq!(fixture.state.error, None);
        let mut ui = simulator(view(&fixture.state));
        ui.find("Run cargo test").unwrap();
        ui.find("cancelled").unwrap();
        assert!(ui.find("Allow once").is_err());
    }

    #[test]
    fn enter_does_not_submit_while_the_turn_runs() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();
        let waiting = conversation().pop().unwrap();

        for turn in ["running", "waiting"] {
            if turn == "waiting" {
                let asked = BackendEvent::Update {
                    session: fixture.session.clone(),
                    update: waiting.clone(),
                };
                apply(&mut fixture.state, [Message::Backend(asked)]);
            }
            fixture.state.session.prompt = text_editor::Content::new();
            let mut ui = simulator(view(&fixture.state));
            ui.click(Id::new("prompt")).unwrap();
            let _ = ui.typewrite("hi");
            // The box keeps the key although it does nothing with it: a key it let through
            // would be the window's Enter, which approves a proposed split.
            assert_eq!(
                ui.tap_key(key::Named::Enter),
                iced::event::Status::Captured,
                "{turn}"
            );
            let _ = ui.simulate([press(Key::Named(key::Named::Enter), None, Modifiers::SHIFT)]);
            let messages: Vec<Message> = ui.into_messages().collect();
            assert!(
                !messages.contains(&session(SessionMessage::Submit)),
                "{turn}"
            );
            assert_eq!(
                messages.last(),
                Some(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                    text_editor::Edit::Enter
                )))),
                "{turn}"
            );
            apply(&mut fixture.state, messages);
            assert_eq!(fixture.state.session.prompt.text(), "hi\n", "{turn}");
        }
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn stop_is_shown_while_a_permission_waits() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();
        let asked = BackendEvent::Update {
            session: fixture.session.clone(),
            update: conversation().pop().unwrap(),
        };
        apply(&mut fixture.state, [Message::Backend(asked)]);

        let mut ui = simulator(view(&fixture.state));
        ui.find("asks you").unwrap();
        ui.find("Stop").unwrap();
        assert!(ui.find("Send").is_err());
        ui.click("Stop").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::Stop)]);
        apply(&mut fixture.state, messages);

        let calls = fixture.backend.calls();
        assert!(matches!(
            calls.as_slice(),
            [
                ..,
                Call::RespondPermission { option: None, .. },
                Call::Cancel { .. }
            ]
        ));
    }

    #[test]
    fn send_is_offered_while_only_a_card_pends() {
        let dir = TempDir::new().unwrap();
        for name in ["api", "web"] {
            std::fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        let (backends, default) = single_claude();
        let mut fixture = fixture_in(dir, backends, default, Vec::new());
        assert!(fixture.state.app.session_view().unwrap().orchestrator);
        fixture
            .state
            .app
            .submit_prompt("Plan it", Vec::new())
            .unwrap();
        let proposed = BackendEvent::ToolCalled {
            call: ToolRequestId(1),
            session: fixture.session.clone(),
            tool: OrchestratorTool::ProposeChildren(vec![ProposedChild {
                repository: RepositoryId::try_from("api").unwrap(),
                prompt: "Add the endpoint".into(),
                title: "API endpoint".into(),
            }]),
        };
        let ended = BackendEvent::Update {
            session: fixture.session.clone(),
            update: Update::TurnEnded(StopReason::EndTurn),
        };
        apply(&mut fixture.state, [proposed, ended].map(Message::Backend));

        assert_eq!(fixture.state.error, None);
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.status, Status::WaitingForYou);
        let mut ui = simulator(view(&fixture.state));
        ui.find("1 child session · 1 repo untouched").unwrap();
        ui.find("Send").unwrap();
        assert!(ui.find("Stop").is_err());
    }

    #[test]
    fn the_header_shows_the_breadcrumb_status_and_usage() {
        let fixture = fixture(vec![
            Update::Usage {
                used: 1200,
                size: 200_000,
            },
            Update::Notice("compacted".into()),
            Update::Failed("the agent crashed".into()),
        ]);
        let project = fixture.state.app.tree().projects[0].name.clone();

        let mut ui = simulator(view(&fixture.state));
        ui.find(project).unwrap();
        ui.find("New session").unwrap();
        ui.find("failed").unwrap();
        ui.find("1k / 200k").unwrap();
        ui.find("compacted").unwrap();
        ui.find("failed: the agent crashed").unwrap();
    }

    #[test]
    fn the_usage_bar_fills_with_the_used_share_of_the_context() {
        assert_eq!(usage_fill(0, 200_000, 80), 0);
        assert_eq!(usage_fill(50_000, 200_000, 80), 20);
        assert_eq!(usage_fill(41_260, 200_000, 80), 16);
        assert_eq!(usage_fill(200_000, 200_000, 80), 80);
        // More than the context holds still fills the bar only once.
        assert_eq!(usage_fill(300_000, 200_000, 80), 80);
        assert_eq!(usage_fill(u64::MAX, u64::MAX, 80), 80);
        // A size the agent did not report leaves it empty.
        assert_eq!(usage_fill(1200, 0, 80), 0);
    }

    #[test]
    fn without_a_reported_usage_none_is_shown() {
        let fixture = fixture(Vec::new());
        let mut ui = simulator(view(&fixture.state));

        assert!(ui.find("0 / 0").is_err());
        ui.find(Id::new(AUTO_ALLOW)).unwrap();
    }

    #[test]
    fn the_breadcrumb_row_marks_an_orchestrator_and_a_child_that_asks() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let mut ui = simulator(view(&fixture.state));
        ui.find("orchestrator").unwrap();
        ui.find("running").unwrap();
        assert!(ui.find(CHILD_ASKS_YOU).is_err());
        drop(ui);

        deliver(
            &mut fixture.state,
            BackendEvent::Update {
                session: fixture.child.clone(),
                update: conversation().pop().unwrap(),
            },
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find(CHILD_ASKS_YOU).unwrap();
        drop(ui);

        // The child itself asks: its row says so with its status, and it is no orchestrator.
        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.child.clone()),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("asks you").unwrap();
        assert!(ui.find("orchestrator").is_err());
        assert!(ui.find(CHILD_ASKS_YOU).is_err());
        ui.find("from orchestrator").unwrap();
    }

    /// The updates of a permission request for reading a file.
    fn read_request() -> Update {
        conversation().pop().unwrap()
    }

    /// Reports `update` for `session`, as its backend would.
    fn report(state: &mut State, session: &SessionId, update: Update) {
        deliver(
            state,
            BackendEvent::Update {
                session: session.clone(),
                update,
            },
        );
    }

    /// Clicks the text `label` and returns the messages the click sends.
    fn click(state: &State, label: &str) -> Vec<Message> {
        let mut ui = simulator(view(state));
        ui.click(label).unwrap();
        ui.into_messages().collect()
    }

    #[test]
    fn the_breadcrumb_row_counts_the_children_that_are_done() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let mut ui = simulator(view(&fixture.state));
        ui.find("0 of 1 children done").unwrap();
        assert_eq!(
            shown(&mut ui, "·"),
            2,
            "after the pills and in the wake row"
        );
        drop(ui);

        report(
            &mut fixture.state,
            &fixture.child,
            Update::TurnEnded(StopReason::EndTurn),
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("1 of 1 children done").unwrap();
        drop(ui);

        // A session without children says nothing about them.
        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.child.clone()),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("0 of 0 children done").is_err());
        assert!(
            ui.find(Id::from(format!("child-{}", fixture.child)))
                .is_err()
        );
    }

    #[test]
    fn an_orchestrator_that_is_done_shows_as_running_while_a_child_works() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        report(
            &mut fixture.state,
            &fixture.orchestrator,
            Update::TurnEnded(StopReason::EndTurn),
        );
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.status, Status::Done);
        assert_eq!(shown_status(&model), Status::Running);
        drop(model);
        // The orchestrator's pill and the child's.
        let mut ui = simulator(view(&fixture.state));
        assert_eq!(shown(&mut ui, "running"), 2);
        assert_eq!(shown(&mut ui, "done"), 0);
        drop(ui);

        report(
            &mut fixture.state,
            &fixture.child,
            Update::Failed("boom".into()),
        );
        report(
            &mut fixture.state,
            &fixture.orchestrator,
            Update::TurnEnded(StopReason::EndTurn),
        );
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(shown_status(&model), Status::Done);
    }

    #[test]
    fn the_children_strip_shows_each_child_and_a_click_opens_it() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let card = Id::from(format!("child-{}", fixture.child));
        let mut ui = simulator(view(&fixture.state));
        ui.find(card.clone()).unwrap();
        ui.find("api").unwrap();
        ui.find("API endpoint").unwrap();
        assert_eq!(
            shown(&mut ui, "running"),
            2,
            "the orchestrator's and the child's"
        );
        // The usage is not known yet.
        assert!(ui.find("18k").is_err());
        drop(ui);

        report(
            &mut fixture.state,
            &fixture.child,
            Update::Usage {
                used: 18_200,
                size: 200_000,
            },
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("18k").unwrap();
        ui.click(card).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.child.clone())
            )]
        );

        apply(&mut fixture.state, messages);
        assert_eq!(
            fixture.state.app.current_session(),
            Some(fixture.child.clone())
        );
        assert_eq!(fixture.state.tab, Tab::Session);
    }

    #[test]
    fn a_cards_title_gets_less_room_the_more_cards_share_a_line() {
        assert_eq!(card_title_budget(1), 60);
        assert_eq!(card_title_budget(2), 28);
        assert_eq!(card_title_budget(3), 10);
        assert!(card_title_budget(1) >= card_title_budget(0));
    }

    #[test]
    fn the_prompt_box_of_an_orchestrator_says_what_a_prompt_is_good_for() {
        let mut fixture = orchestrated();
        let placeholder_now = |state: &State| placeholder(&state.app.session_view().unwrap());
        assert_eq!(
            placeholder_now(&fixture.state),
            "Steer the orchestrator… (it keeps going while you type)"
        );

        // An undecided proposal comes first.
        let proposal = ProposalId::new();
        report(
            &mut fixture.state,
            &fixture.orchestrator,
            Update::ChildrenProposed {
                id: proposal.clone(),
                children: vec![ProposedChild {
                    repository: RepositoryId::try_from("web").unwrap(),
                    prompt: "Show the endpoint's data".into(),
                    title: "Web page".into(),
                }],
            },
        );
        assert_eq!(
            placeholder_now(&fixture.state),
            "Change the split, or ask the orchestrator…"
        );
        apply(
            &mut fixture.state,
            [session(SessionMessage::Proposal(
                proposal,
                ProposalMessage::Reject,
            ))],
        );
        assert_eq!(fixture.state.error, None);
        assert_eq!(
            placeholder_now(&fixture.state),
            "Steer the orchestrator… (it keeps going while you type)"
        );

        // A child that asks is still in its turn.
        report(&mut fixture.state, &fixture.child, read_request());
        assert_eq!(
            placeholder_now(&fixture.state),
            "Steer the orchestrator… (it keeps going while you type)"
        );

        report(
            &mut fixture.state,
            &fixture.child,
            Update::TurnEnded(StopReason::EndTurn),
        );
        let title = fixture
            .state
            .app
            .session_view()
            .unwrap()
            .breadcrumb
            .last()
            .cloned()
            .unwrap();
        assert_eq!(placeholder_now(&fixture.state), format!("Ask {title}…"));

        // The child is no orchestrator, whatever its transcript holds.
        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.child.clone()),
            )],
        );
        assert_eq!(placeholder_now(&fixture.state), "Ask API endpoint…");
    }

    #[test]
    fn stop_children_is_offered_while_a_child_is_in_a_turn_and_stops_them() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;

        let messages = click(&fixture.state, "Stop children");
        assert_eq!(messages, [session(SessionMessage::StopChildren)]);
        apply(&mut fixture.state, messages);
        assert_eq!(fixture.state.error, None);
        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::Cancel {
                session: fixture.child.clone()
            })
        );

        // A child that asks is still in its turn; a child whose turn is over is not.
        report(&mut fixture.state, &fixture.child, read_request());
        let mut ui = simulator(view(&fixture.state));
        ui.find("Stop children").unwrap();
        drop(ui);
        report(
            &mut fixture.state,
            &fixture.child,
            Update::TurnEnded(StopReason::Cancelled),
        );
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Stop children").is_err());
        drop(ui);

        // The child itself has no children to stop.
        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.child.clone()),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Stop children").is_err());
    }

    #[test]
    fn stopping_children_that_the_backend_refuses_shows_the_error() {
        let mut fixture = orchestrated();
        fixture.backend.fail_next_call();

        apply(&mut fixture.state, [session(SessionMessage::StopChildren)]);

        let error = fixture.state.error.clone().unwrap();
        assert!(error.contains("backend unavailable"), "{error}");
    }

    #[test]
    fn a_decided_proposal_turns_the_transcript_into_a_timeline() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;

        // The approval and the news of the started child, which woke the orchestrator.
        let mut ui = simulator(view(state));
        ui.find("You approved the split. Started").unwrap();
        ui.find("api.").unwrap();
        assert_eq!(
            shown(&mut ui, "Partitur"),
            2,
            "the wordmark and the wake row"
        );
        ui.find("expand").unwrap();
        assert!(ui.find("Proposed split").is_err());
        ui.find("Waiting for api.").unwrap();
        drop(ui);

        // The wake row expands to the whole prompt.
        let messages = click(state, "expand");
        assert_eq!(messages, [session(SessionMessage::ToggleEntry(1))]);
        apply(state, messages);
        let mut ui = simulator(view(state));
        ui.find("collapse").unwrap();
        drop(ui);
        apply(state, [session(SessionMessage::ToggleEntry(1))]);

        // What the orchestrator's agent does keeps its usual rows.
        for update in [
            Update::AgentThought("one child is enough".into()),
            tool_call(
                "t-1",
                ToolKind::Read,
                "Read notes",
                PathBuf::from("notes.md"),
            ),
            Update::AgentText("Waiting for the **API**.".into()),
            Update::TurnEnded(StopReason::EndTurn),
            Update::UserText("Also write the docs".into()),
        ] {
            report(state, &orchestrator, update);
        }
        let mut ui = simulator(view(state));
        ui.find("thinking").unwrap();
        ui.find("one child is enough").unwrap();
        ui.find("tools").unwrap();
        ui.find("notes.md").unwrap();
        ui.find("end of turn").unwrap();
        // The answer is Markdown from the cache, as in a conversation.
        assert!(ui.find("Waiting for the **API**.").is_err());
        assert!(state.session.markdown.items(&orchestrator, 4).is_some());
        // The user's prompt is a line of the timeline, not a bubble.
        ui.find("You:").unwrap();
        ui.find("Also write the docs").unwrap();
        assert_eq!(shown(&mut ui, "you"), 0);
    }

    #[test]
    fn a_proposal_made_after_a_decision_keeps_its_card_in_the_timeline() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        report(
            &mut fixture.state,
            &fixture.orchestrator,
            Update::ChildrenProposed {
                id: ProposalId::new(),
                children: vec![ProposedChild {
                    repository: RepositoryId::try_from("web").unwrap(),
                    prompt: "Show the endpoint's data".into(),
                    title: "Web page".into(),
                }],
            },
        );

        let mut ui = simulator(view(&fixture.state));
        ui.find("You approved the split. Started").unwrap();
        ui.find("Proposed split").unwrap();
        ui.find("1 child session · 1 repo untouched").unwrap();
        ui.find("Show the endpoint's data").unwrap();
        ui.find("Approve split").unwrap();
    }

    #[test]
    fn a_childs_result_is_a_block_that_expands_when_it_is_long() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        let state = &mut fixture.state;
        report(state, &orchestrator, Update::TurnEnded(StopReason::EndTurn));
        let lines: Vec<String> = (1..=8).map(|line| format!("line {line}")).collect();
        report(state, &child, Update::AgentText(lines.join("\n")));
        report(state, &child, Update::TurnEnded(StopReason::EndTurn));

        let mut ui = simulator(view(state));
        ui.find("reported back:").unwrap();
        ui.find("line 1").unwrap();
        ui.find("line 6").unwrap();
        ui.find("…").unwrap();
        assert!(ui.find("line 7").is_err());
        // The turn was the orchestrator's and ended by itself: nothing to remark.
        assert!(ui.find("turn started by the user").is_err());
        assert!(ui.find("turn ended: cancelled").is_err());
        // The child is done, so nothing is pending.
        assert!(ui.find("Waiting for api.").is_err());
        drop(ui);

        // The wake row before it has its own `expand`; the result's is the last entry's.
        let result = state.app.session_view().unwrap().entries.len() - 1;
        apply(state, [session(SessionMessage::ToggleEntry(result))]);
        let mut ui = simulator(view(state));
        ui.find("line 8").unwrap();
        ui.find("collapse").unwrap();
        assert!(ui.find("…").is_err());
    }

    #[test]
    fn a_childs_result_says_how_its_turn_went_differently() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        let state = &mut fixture.state;
        report(state, &orchestrator, Update::TurnEnded(StopReason::EndTurn));
        report(state, &child, Update::AgentText("half done".into()));
        report(state, &child, Update::TurnEnded(StopReason::Cancelled));

        let mut ui = simulator(view(state));
        ui.find("reported back:").unwrap();
        ui.find("turn ended: cancelled").unwrap();
        ui.find("half done").unwrap();
        // A short message has nothing to expand: the only control is the wake row's.
        assert_eq!(shown(&mut ui, "expand"), 1);
    }

    #[test]
    fn a_prompt_the_orchestrator_sent_a_child_names_the_child_and_opens_it() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let (orchestrator, child) = (fixture.orchestrator.clone(), fixture.child.clone());
        let state = &mut fixture.state;
        let long = format!("Also add tests\nfor {}", "every route ".repeat(10));
        deliver(
            state,
            BackendEvent::ToolCalled {
                call: ToolRequestId(1),
                session: orchestrator,
                tool: OrchestratorTool::SendToChild {
                    child: child.clone(),
                    prompt: long,
                },
            },
        );
        assert_eq!(state.error, None);

        let mut ui = simulator(view(state));
        ui.find("prompted").unwrap();
        // The child's card and the row both name its repository.
        assert_eq!(shown(&mut ui, "api"), 2);
        // The prompt on one line, as far as the row's budget reaches.
        ui.find("Also add tests for every route every route every route every route every…")
            .unwrap();
        ui.click("open").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(child.clone())
            )]
        );

        apply(state, messages);
        assert_eq!(state.app.current_session(), Some(child));
    }

    #[test]
    fn the_last_row_tells_which_children_work_and_which_ask() {
        let mut fixture = orchestrated();
        fixture.state.tab = Tab::Session;
        let child = fixture.child.clone();
        let state = &mut fixture.state;
        let mut ui = simulator(view(state));
        ui.find("Waiting for api.").unwrap();
        assert!(ui.find("asks you.").is_err());
        drop(ui);

        report(state, &child, read_request());
        let mut ui = simulator(view(state));
        assert!(ui.find("Waiting for api.").is_err());
        ui.find("asks you.").unwrap();
        // The card, and the name before `asks you.`.
        assert_eq!(shown(&mut ui, "api"), 2);
        drop(ui);

        report(state, &child, Update::TurnEnded(StopReason::EndTurn));
        let mut ui = simulator(view(state));
        assert!(ui.find("Waiting for api.").is_err());
        assert!(ui.find("asks you.").is_err());
        assert_eq!(shown(&mut ui, "api"), 1);
    }

    #[test]
    fn a_click_on_a_pickers_chevron_opens_its_menu() {
        let fixture = fixture(Vec::new());
        let mut ui = simulator(view(&fixture.state));
        let picker = ui.find(Id::new(MODEL)).unwrap().bounds();
        let moved = |position| iced::Event::Mouse(mouse::Event::CursorMoved { position });

        // The chevron lies over the picker's right end.
        let chevron = Point::new(picker.x + picker.width - 4.0, picker.center_y());
        ui.point_at(chevron);
        let _ = ui.simulate(iced_test::simulator::click());
        // The row sits at the window's bottom, so the menu opens upwards: its last entry,
        // the second model, ends where the picker begins.
        let last_entry = Point::new(picker.x + 8.0, picker.y - 4.0);
        ui.point_at(last_entry);
        let _ =
            ui.simulate(std::iter::once(moved(last_entry)).chain(iced_test::simulator::click()));

        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [session(SessionMessage::SetModel(ConfigValueId::new(
                "opus"
            )))]
        );
    }

    #[test]
    fn a_clicked_link_is_copied_not_opened() {
        let mut fixture = fixture(Vec::new());
        let task = update(
            &mut fixture.state,
            session(SessionMessage::LinkClicked("https://example.com".into())),
        );
        assert!(task.units() > 0);
        assert_eq!(fixture.state.error, None);
    }

    /// The text an archived session's banner shows for the current session of `state`.
    fn banner_text(state: &State) -> String {
        let mark = state.app.session_view().unwrap().archived.unwrap();
        format!("Archived {}", partitur_core::day_month(mark.at))
    }

    #[test]
    fn an_archived_session_shows_the_banner_and_no_composer() {
        let mut fixture = fixture(conversation());
        fixture.state.app.archive(fixture.session.clone()).unwrap();

        let banner = banner_text(&fixture.state);
        let mut ui = simulator(view(&fixture.state));

        ui.find(banner.as_str()).unwrap();
        ui.find(Id::new("banner-restore")).unwrap();
        ui.find("Explain the build").unwrap();
        assert!(ui.find(Id::new(PROMPT)).is_err());
    }

    #[test]
    fn a_live_session_shows_no_banner() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        assert!(ui.find(Id::new("banner-restore")).is_err());
        ui.find(Id::new(PROMPT)).unwrap();
    }

    #[test]
    fn restore_from_the_banner_brings_the_composer_back() {
        let mut fixture = fixture(conversation());
        fixture.state.app.archive(fixture.session.clone()).unwrap();

        let mut ui = simulator(view(&fixture.state));
        ui.click(Id::new("banner-restore")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Restore(fixture.session.clone())
            )]
        );
        apply(&mut fixture.state, messages);

        assert_eq!(fixture.state.error, None);
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(PROMPT)).unwrap();
        assert!(ui.find(Id::new("banner-restore")).is_err());
    }
}
