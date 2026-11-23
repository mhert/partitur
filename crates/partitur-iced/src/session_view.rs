//! The Session tab: the selected session's transcript and the prompt that continues it.
//!
//! Under the breadcrumb row, which names the session and carries its pills (orchestrator,
//! status, a child that asks the user) and how many of its children are done, and under the
//! flow rail of a session that reports a workflow, sits one centered column. A session with children starts it with a strip of cards, one per
//! child, that tell where the child stands and open it. Then comes the transcript, which
//! stays pinned to its end while the agent streams. Only its rows near the viewport are laid
//! out, which keeps a long session quick to open and to scroll. Those rows stay as they are
//! while a scroll keeps the viewport well inside them, and while the user selects text in
//! the transcript the first of them stays put, so the selection holds. A row below the
//! viewport that turns out taller or shorter than guessed scrolls the transcript along, so
//! the view stays still. Under it sits the composer. The composer
//! lists the files attached to the next prompt as chips. `Ctrl V` in the prompt box attaches
//! copied files, and saves a copied image as a PNG and attaches it, with a `pasting…` chip
//! while it saves; other copied text goes into the prompt (see [`paste`]). A prompt with
//! attachments is not sent at once: its images are copied and previewed in the background
//! (see `prepare`), the composer says `sending…` in place of the send button and takes no
//! new input meanwhile (the prompt box still holds the focus and its keys, so none of them
//! reaches the tab's shortcuts), and the prompt goes out when the files are ready. A
//! failure leaves the composer as it was and says why in the error bar; a chip that lands
//! during the send stays for the next prompt. The composer then frames the attach button,
//! the prompt box, the keys that send and break the line, and the send button (the stop button
//! while a turn runs). Its last row holds the model, effort and backend pickers, the
//! auto-allow switch, for an orchestrator whose children work a button that stops them
//! and, at the right, how much of its context the session has used. An archived session is
//! read-only: a banner under the breadcrumb says when it was archived and restores it, and
//! the composer is gone. A child that waits for its orchestrator at a gate or with a
//! question says `waits for orchestrator` on its pill and on its card, not `asks you`.
//! A child's permission request that waits for the user is a card in its orchestrator's
//! session too, at the transcript's foot, and is answered there for the child.
//!
//! How each transcript entry is drawn lives in [`entries`]; the card that approves or
//! rejects a proposal of children ([`ProposalDraft`], [`ProposalMessage`]) lives in
//! `proposal`, the card of a question the agent asks ([`QuestionMessage`]) in [`question`],
//! the timeline an orchestrator's transcript becomes once a proposal is decided
//! in `timeline`, which rows are laid out, with the heights they were measured at, in
//! `window`, the run of rows that keeps each row's widget state as the window slides in
//! `rows`, and the MCP App widget under a tool call in `widget`.

mod entries;
mod proposal;
pub(crate) mod question;
mod rows;
mod timeline;
mod widget;
mod window;
mod workflow_rows;

pub(crate) use entries::{LinkTarget, Prose};
pub(crate) use proposal::refresh_checks;
pub use proposal::{DraftChild, ProposalDraft, ProposalMessage};
pub use question::QuestionMessage;
use window::{Heights, Kept};
pub use window::{RowKey, Visible};

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::keyboard::{self, key};
use iced::widget::{
    Column, Id, Row, Space, button, column, container, markdown, operation, pick_list, row,
    scrollable, space, stack, text, text_editor, toggler,
};
use iced::{Alignment, Border, Element, Length, Padding, Pixels, Task, Theme};
use partitur_core::{
    App, AppError, BackendId, BackendInfo, ChoiceView, ConfigValueId, Entry, PermissionId,
    PermissionOptionId, Prepared, ProposalId, QuestionId, RepositoryId, SessionId, SessionNode,
    SessionViewModel, Status, background_shown,
};

use crate::app::{self, Message, State};
use crate::ellipsis::keep_start;
use crate::icon::{Icon, icon};
use crate::jump::JumpMessage;
use crate::markdown_cache::MarkdownCache;
use crate::previews::Previews;
use crate::projects_view::{CHILD_ASKS_YOU, ProjectsMessage};
use crate::theme::Tokens;
use crate::widgets::Widgets;
use crate::{
    badge, breadcrumb, flow_rail, layout, link, paste, prepare, style, theme, tokens_label,
};

/// The id of the prompt box.
pub(crate) const PROMPT: &str = "prompt";

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
    /// Reads the clipboard when the user pastes into the prompt.
    pub(crate) clipboard: paste::Reader,
    /// Where pasted images are saved.
    pub(crate) paste_dir: PathBuf,
    /// How many pastes still wait for the clipboard to be read.
    pub(crate) reading: usize,
    /// How many pasted images are still being saved.
    pub(crate) pasting: usize,
    /// Whether the files of a sent prompt are being prepared; the composer reads `sending…`
    /// and takes no new input until the outcome arrives.
    pub(crate) sending: bool,
    /// The transcript entries the user expanded (thoughts and tool calls), by session and
    /// entry index.
    pub(crate) expanded: HashSet<(SessionId, usize)>,
    /// The runs of consecutive tool calls the user expanded into their list of calls, by
    /// session and the entry index of the run's first call.
    pub(crate) expanded_tools: HashSet<(SessionId, usize)>,
    /// The agent's answers, parsed as Markdown.
    pub(crate) markdown: MarkdownCache,
    /// The decoded previews of the images the prompts were sent with, by file.
    pub(crate) previews: Previews,
    /// The approval cards' drafts of the undecided proposals, by session and proposal.
    pub(crate) proposals: HashMap<(SessionId, ProposalId), ProposalDraft>,
    /// The answers typed into the `Other` field of the open questions' cards, by session and
    /// question.
    pub(crate) question_drafts: HashMap<(SessionId, QuestionId), String>,
    /// The transcript's scrollable.
    pub(crate) scroll: Id,
    /// Where the transcript's viewport stands, once its scrollable has reported it.
    pub(crate) visible: Option<Visible>,
    /// The measured extents of the transcript rows that were laid out, per session.
    pub(crate) heights: Heights,
    /// The rows of the current session's transcript that are laid out, kept while they
    /// serve the viewport.
    pub(crate) shown: Option<Kept>,
    /// Whether the user selects text in the transcript, or holds a press that may.
    pub(crate) selecting: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            prompt: text_editor::Content::new(),
            attachments: Vec::new(),
            clipboard: paste::system,
            paste_dir: paste::default_dir(),
            reading: 0,
            pasting: 0,
            sending: false,
            expanded: HashSet::new(),
            expanded_tools: HashSet::new(),
            markdown: MarkdownCache::default(),
            previews: Previews::default(),
            proposals: HashMap::new(),
            question_drafts: HashMap::new(),
            scroll: Id::new("transcript"),
            visible: None,
            heights: Heights::default(),
            shown: None,
            selecting: false,
        }
    }
}

impl SessionState {
    /// Forgets what the tab remembers about `sessions`, which left the tree: expanded
    /// entries, approval card drafts, question drafts, parsed answers, image previews and
    /// measured row heights.
    pub(crate) fn forget(&mut self, sessions: &[SessionId]) {
        self.expanded
            .retain(|(session, _)| !sessions.contains(session));
        self.expanded_tools
            .retain(|(session, _)| !sessions.contains(session));
        self.proposals
            .retain(|(session, _), _| !sessions.contains(session));
        self.question_drafts
            .retain(|(session, _), _| !sessions.contains(session));
        self.previews.forget(sessions);
        for session in sessions {
            self.markdown.forget(session);
            self.heights.forget(session);
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
    /// The user pasted into the prompt; the clipboard is read in the background.
    Paste,
    /// The clipboard held this when the user pasted.
    Pasted(paste::Pasted),
    /// A pasted image was saved to this file, or could not be, for this reason.
    PasteSaved(Result<PathBuf, String>),
    /// The window's own clipboard held this text, read when the system clipboard yielded
    /// nothing.
    PastedText(Option<String>),
    /// The files `files` of the prompt `text`, written in `session`, were prepared, or could
    /// not be, for this reason.
    Prepared {
        /// The session the prompt was written in.
        session: SessionId,
        /// The trimmed prompt text.
        text: String,
        /// The attached files the preparation took.
        files: Vec<PathBuf>,
        /// The folder the copies were written to, where a prompt that is not sent has them
        /// removed again.
        dir: PathBuf,
        /// What was prepared, one per file, or why it failed.
        result: Result<Vec<Prepared>, String>,
    },
    /// The user expanded or collapsed the current session's transcript entry at this index.
    ToggleEntry(usize),
    /// The user expanded or collapsed the run of consecutive tool calls whose first call is
    /// the current session's transcript entry at this index.
    ToggleTools(usize),
    /// The user answered a permission request with an option, or cancelled it with `None`.
    AnswerPermission(PermissionId, Option<PermissionOptionId>),
    /// The user answered, in its orchestrator's session, a permission request of the child
    /// with an option, or cancelled it with `None`.
    AnswerChildPermission(SessionId, PermissionId, Option<PermissionOptionId>),
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
    /// An interaction on the card of a question the current session asked.
    Question(QuestionId, QuestionMessage),
    /// The user asked to stop the turns of the current session's children.
    StopChildren,
    /// The transcript's viewport moved or changed its size.
    Scrolled(Visible),
    /// A row of the session's transcript was laid out with this extent: its height and the
    /// gap above it. A row below the viewport scrolls the transcript along, so the view
    /// stays still.
    Measured(SessionId, RowKey, f32),
    /// The user came to select text in the transcript, or nothing is selected any more.
    Selecting(bool),
}

/// The Session tab for the selected session `model`, its answers drawn in the colours of
/// `tokens`, the token set of the active theme, and its tool calls' widgets as `widgets`
/// holds them. The flow rail's approve button is enabled when the gate is `approvable`, as
/// the core tells it.
pub fn view<'a>(
    model: &SessionViewModel<'a>,
    state: &'a SessionState,
    widgets: &'a Widgets,
    approvable: bool,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let width = column_width(model);
    let mut page = column![breadcrumb::breadcrumb(&model.breadcrumb, trailing(model))];
    if let Some(workflow) = &model.workflow {
        page = page.push(flow_rail::rail(workflow, approvable, tokens));
    }
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
        entries::transcript(model, state, widgets, tokens),
        width,
        Padding::ZERO.top(top),
    ))
    .anchor_bottom()
    .on_scroll(|viewport| Message::Session(SessionMessage::Scrolled(Visible::of(viewport))))
    .id(state.scroll.clone())
    .width(Length::Fill)
    .height(Length::Fill);
    page = page.push(widget::wheel_to_widgets(transcript.into()));
    // An archived session is read-only: it takes no prompt until it is restored.
    if model.archived.is_none() {
        page = page.push(centered(
            composer(model, state),
            width,
            Padding::ZERO.top(12).bottom(16),
        ));
    }
    // No other tab's page is a container, so iced builds the tab's widgets anew whenever
    // it is shown again instead of handing them another tab's state: the transcript then
    // always stands at its end.
    container(page).into()
}

/// The widest the column of the session `model` grows.
fn column_width(model: &SessionViewModel<'_>) -> f32 {
    if model.orchestrator {
        ORCHESTRATOR_COLUMN_WIDTH
    } else {
        COLUMN_WIDTH
    }
}

/// How wide the transcript's rows of the session `model` are in the viewport `visible`:
/// the column's width, or less when the window's margins leave less. Without a viewport
/// the column is assumed to have room to grow to its widest.
fn text_width(model: &SessionViewModel<'_>, visible: Option<Visible>) -> f32 {
    let widest = column_width(model);
    visible.map_or(widest, |visible| {
        (visible.width - 2.0 * MARGIN_X).clamp(0.0, widest)
    })
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
/// session's status, outlined while it is idle, and a mark when a session below it waits for the
/// user) and, for a session with children, how many of them are done.
pub(crate) fn trailing<'a>(
    model: &SessionViewModel<'_>,
) -> impl Iterator<Item = Element<'a, Message>> {
    let progress = (!model.children.is_empty()).then(|| {
        let done = model
            .children
            .iter()
            .filter(|child| {
                child.status == Status::Done && !background_shown(child.status, child.background)
            })
            .count();
        format!("{done} of {} children done", model.children.len())
    });
    let small = |words: String| text(words).size(theme::SMALL);
    [
        model.orchestrator.then(badge::orchestrator),
        Some(badge::session(
            shown_status(model),
            model.background,
            model.delegated,
        )),
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
///
/// Everything but the title keeps its one line at its own width; the title takes the room
/// that is left and is cut off at its edge, so a narrow card neither wraps its pill nor
/// pushes its usage out.
fn child_card<'a>(child: &SessionNode, per_line: usize) -> Element<'a, Message> {
    let title = text(keep_start(&child.title, card_title_budget(per_line)))
        .size(theme::SMALL)
        .wrapping(text::Wrapping::None)
        .style(style::muted);
    let mut line = row![
        text(child_name(child).to_owned())
            .font(theme::UI_MEDIUM)
            .wrapping(text::Wrapping::None),
        container(title).width(Length::Fill).clip(true),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .push(badge::session(
        child.status,
        child.background,
        child.delegated,
    ));
    if let Some((used, _)) = child.usage {
        line = line.push(
            text(tokens_label::compact(used))
                .font(theme::MONO)
                .size(theme::SMALL)
                .wrapping(text::Wrapping::None)
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
/// [`App`], an answer to a child's permission request to that child; when it refuses, the
/// error bar shows why. A clicked link is opened with the session's `link_opener` in the
/// background, in the application's process environment (see [`link`]).
pub fn update(state: &mut State, message: SessionMessage) -> Task<Message> {
    let session = &mut state.session;
    if session.sending && starts_input(&message) {
        return Task::none();
    }
    match message {
        SessionMessage::Prompt(action) => session.prompt.perform(action),
        SessionMessage::Submit => return submit(state),
        SessionMessage::Prepared {
            session: id,
            text,
            files,
            dir,
            result,
        } => {
            return prepared(state, &id, &text, &files, dir, result);
        }
        SessionMessage::Stop => {
            app::report(&mut state.error, state.app.cancel());
        }
        SessionMessage::Attach => {
            return Task::perform(pick_files(), |files| {
                Message::Session(SessionMessage::Attached(files))
            });
        }
        SessionMessage::Attached(files) => session.attachments.extend(files),
        SessionMessage::RemoveAttachment(index) => remove_attachment(session, index),
        SessionMessage::Paste => {
            session.reading += 1;
            return paste::read(session.clipboard)
                .map(|pasted| Message::Session(SessionMessage::Pasted(pasted)));
        }
        SessionMessage::Pasted(pasted) => {
            session.reading = session.reading.saturating_sub(1);
            return pasted_into(session, pasted);
        }
        SessionMessage::PastedText(text) => {
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                paste_text(session, text);
            }
        }
        SessionMessage::PasteSaved(saved) => return image_saved(session, saved),
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
        SessionMessage::AnswerChildPermission(child, permission, option) => {
            app::report(
                &mut state.error,
                state.app.answer_permission(child, permission, option),
            );
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
        SessionMessage::LinkClicked(uri) => {
            let opener = state
                .app
                .current_session()
                .ok_or(AppError::NoCurrentSession)
                .and_then(|session| state.app.link_opener(&session));
            if let Some(opener) = app::report(&mut state.error, opener) {
                return link::open(opener, uri, state.process_env.clone());
            }
        }
        SessionMessage::Proposal(id, message) => return proposal::update(state, id, message),
        SessionMessage::Question(id, message) => return question::update(state, id, message),
        SessionMessage::StopChildren => {
            on_current_session(state, |app, session| app.stop_children(&session));
        }
        SessionMessage::Scrolled(visible) => {
            session.heights.set_width(visible.width);
            session.visible = Some(visible);
        }
        SessionMessage::Measured(owner, key, extent) => {
            return measured(state, &owner, key, extent);
        }
        SessionMessage::Selecting(selecting) => session.selecting = selecting,
    }
    Task::none()
}

/// Remembers that the row `key` of the session `owner` was laid out at `extent`.
///
/// The transcript is pinned to its end, which keeps the view still when a row above the
/// viewport turns out taller or shorter than assumed, but moves it when a row below does.
/// So for a row of the current session below the viewport the transcript is scrolled by the
/// difference, and the content in view stays where it is.
fn measured(state: &mut State, owner: &SessionId, key: RowKey, extent: f32) -> Task<Message> {
    let session = &mut state.session;
    let shift = state
        .app
        .session_view()
        .filter(|model| &model.id == owner)
        .zip(session.visible)
        .and_then(|(model, visible)| {
            let all = entries::rows(&model);
            let width = text_width(&model, Some(visible));
            let extents = entries::extents(owner, &session.heights, &all, width);
            let index = all.iter().position(|row| row.key == key)?;
            let assumed = *extents.get(index)?;
            let after: f32 = extents.iter().skip(index.saturating_add(1)).sum();
            (after + assumed <= visible.bottom).then_some(extent - assumed)
        })
        .filter(|shift| shift.abs() > f32::EPSILON);
    session.heights.record(owner, key, extent);
    let (Some(shift), Some(visible)) = (shift, session.visible.as_mut()) else {
        return Task::none();
    };
    visible.bottom += shift;
    operation::scroll_by(
        session.scroll.clone(),
        operation::AbsoluteOffset { x: 0.0, y: shift },
    )
}

/// Brings the window of the current session's transcript up to date with its rows, their
/// heights, the viewport and whether the user selects: the window kept so far stays while
/// it serves, see [`window::keep`].
///
/// Runs after every message, since nearly any of them can change one of those, so the view
/// only reads the window.
pub(crate) fn keep_window(state: &mut State) {
    let Some(model) = state.app.session_view() else {
        state.session.shown = None;
        return;
    };
    let session = &mut state.session;
    // A selection belongs to the transcript it was made in: it is gone once another
    // session's transcript or another tab is shown.
    let elsewhere = state.tab != app::Tab::Session;
    if elsewhere
        || session
            .shown
            .as_ref()
            .is_some_and(|kept| kept.session != model.id)
    {
        session.selecting = false;
    }
    let all = entries::rows(&model);
    let width = text_width(&model, session.visible);
    entries::keep_estimates(&model.id, &mut session.heights, &all, width);
    let extents = entries::extents(&model.id, &session.heights, &all, width);
    let kept = session
        .shown
        .as_ref()
        .filter(|kept| kept.session == model.id && kept.width.to_bits() == width.to_bits())
        .map(|kept| (kept.window.rows.clone(), kept.count));
    let window = window::keep(&extents, session.visible, kept, session.selecting);
    session.shown = Some(Kept {
        session: model.id.clone(),
        width,
        count: all.len(),
        window,
    });
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

/// Drops the attachment at `index`, if there is one.
fn remove_attachment(session: &mut SessionState, index: usize) {
    if index < session.attachments.len() {
        session.attachments.remove(index);
    }
}

/// Attaches the pasted image that was saved, or reports why it could not be.
fn image_saved(session: &mut SessionState, saved: Result<PathBuf, String>) -> Task<Message> {
    session.pasting = session.pasting.saturating_sub(1);
    match saved {
        Ok(path) => {
            session.attachments.push(path);
            Task::none()
        }
        Err(error) => Task::done(Message::Error(error)),
    }
}

/// Whether `message` starts new input, which the composer ignores while it prepares a prompt.
/// What is in flight (`Attached`, `Pasted`, `PastedText`, `PasteSaved`) still lands.
fn starts_input(message: &SessionMessage) -> bool {
    matches!(
        message,
        SessionMessage::Prompt(_)
            | SessionMessage::Submit
            | SessionMessage::Attach
            | SessionMessage::RemoveAttachment(_)
            | SessionMessage::Paste
    )
}

/// Sends the prompt and the attachments, then empties the prompt box and scrolls to the end.
///
/// The prompt goes out trimmed; a blank prompt without attachments is not sent.
fn submit(state: &mut State) -> Task<Message> {
    let session = &mut state.session;
    let text = session.prompt.text();
    let prompt = text.trim();
    if !sendable(prompt, &session.attachments) || pasting(session) {
        return Task::none();
    }
    if session.attachments.is_empty() {
        if app::report(&mut state.error, state.app.submit_prompt(prompt)).is_none() {
            return Task::none();
        }
        state.session.prompt = text_editor::Content::new();
        return to_the_end(&mut state.session);
    }
    let prompt = prompt.to_owned();
    let files = session.attachments.clone();
    let Some(id) = state.app.current_session() else {
        return Task::none();
    };
    let Some(dir) = app::report(&mut state.error, state.app.attachment_dir(&id)) else {
        return Task::none();
    };
    state.session.sending = true;
    prepare::prepare(dir.clone(), files.clone()).map(move |result| {
        Message::Session(SessionMessage::Prepared {
            session: id.clone(),
            text: prompt.clone(),
            files: files.clone(),
            dir: dir.clone(),
            result,
        })
    })
}

/// Sends the prompt whose files were prepared, then empties the prompt box, drops the
/// attachments the preparation took (a chip that landed meanwhile stays) and scrolls to
/// the end. A failed preparation, or a session that takes no prompt now, keeps the composer
/// and shows why; the copies already written in `dir` are removed then, even when the
/// session is gone.
fn prepared(
    state: &mut State,
    session: &SessionId,
    text: &str,
    files: &[PathBuf],
    dir: PathBuf,
    result: Result<Vec<Prepared>, String>,
) -> Task<Message> {
    state.session.sending = false;
    let list = match result {
        Ok(list) => list,
        Err(error) => {
            state.error = Some(error);
            return Task::none();
        }
    };
    // Only the records are kept for a refusal; the encoded contents move into the prompt.
    let records = list.iter().map(|prepared| prepared.sent.clone()).collect();
    let sent = app::report(
        &mut state.error,
        state.app.submit_prepared(session, text, list),
    );
    if sent.is_none() {
        return prepare::discard(dir, records).discard();
    }
    state.session.prompt = text_editor::Content::new();
    for file in files {
        if let Some(index) = state.session.attachments.iter().position(|a| a == file) {
            state.session.attachments.remove(index);
        }
    }
    to_the_end(&mut state.session)
}

/// Scrolls the transcript to its end, where the prompt just sent stands.
pub(crate) fn to_the_end(session: &mut SessionState) -> Task<Message> {
    // The rows at the end are laid out in the frame the transcript scrolls there.
    shows_the_end(session);
    // The transcript is anchored at the bottom, so its offsets count from its end: the
    // start offset is the newest entry, and `snap_to_end` would jump to the oldest.
    operation::snap_to(session.scroll.clone(), operation::RelativeOffset::START)
}

/// The transcript entries of the session `model` whose rows the Session tab lays out, as
/// the window it keeps says; none while it keeps no window for that session.
pub(crate) fn laid_out_entries(
    model: &SessionViewModel<'_>,
    session: &SessionState,
) -> Range<usize> {
    let Some(kept) = session
        .shown
        .as_ref()
        .filter(|kept| kept.session == model.id)
    else {
        return 0..0;
    };
    let all = entries::rows(model);
    let laid_out = all.get(kept.window.rows.clone()).unwrap_or_default();
    let mut entries = laid_out.iter().filter_map(|row| match row.key {
        RowKey::Conversation(first) | RowKey::Timeline(first) => {
            Some(first..first.saturating_add(row.run.len()))
        }
        RowKey::Pending | RowKey::ChildPermission(_) => None,
    });
    let Some(first) = entries.next() else {
        return 0..0;
    };
    // The rows of the window follow each other, so their entries do as well.
    entries.fold(first, |all, row| {
        all.start.min(row.start)..all.end.max(row.end)
    })
}

/// Notes that the transcript's viewport stands at its end, before its scrollable reports it.
pub(crate) fn shows_the_end(session: &mut SessionState) {
    if let Some(visible) = session.visible.as_mut() {
        visible.bottom = 0.0;
    }
}

/// Whether a paste has not landed yet: its clipboard read or its image save still runs, so
/// a prompt sent now would go without it.
fn pasting(session: &SessionState) -> bool {
    session.reading > 0 || session.pasting > 0
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
/// Puts what the clipboard held into the composer: files as attachments, text into the
/// prompt, and an image, once it is saved in the background, as an attachment.
fn pasted_into(session: &mut SessionState, pasted: paste::Pasted) -> Task<Message> {
    match pasted {
        paste::Pasted::Files(files) => session.attachments.extend(files),
        paste::Pasted::Text(text) => paste_text(session, text),
        paste::Pasted::Image(image) => {
            session.pasting += 1;
            return paste::save(session.paste_dir.clone(), image)
                .map(|saved| Message::Session(SessionMessage::PasteSaved(saved)));
        }
        // The system clipboard could not be read: the window's own clipboard may still
        // hold text, as before pasting files was possible.
        paste::Pasted::Nothing => {
            return iced::clipboard::read()
                .map(|text| Message::Session(SessionMessage::PastedText(text)));
        }
    }
    Task::none()
}

/// Pastes `text` into the prompt at the cursor, over the selection.
fn paste_text(session: &mut SessionState, text: String) {
    session
        .prompt
        .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new(text),
        )));
}

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
    if !state.attachments.is_empty() || state.pasting > 0 {
        composer = composer.push(attachment_chips(&state.attachments, state.pasting));
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
    // The box keeps its action handler while the prompt is being prepared, so it still takes
    // its keys (Enter and digits would reach the window's shortcuts otherwise); `update`
    // ignores the edits and the sending then.
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
    } else if state.sending {
        line.push(text("sending…").size(theme::SMALL).style(style::muted))
    } else {
        let can_send = sendable(state.prompt.text().trim(), &state.attachments) && !pasting(state);
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

/// One chip per attached file: its name and a button that removes it. While pasted images
/// are being saved, one more chip says so.
fn attachment_chips(attachments: &[PathBuf], pasting: usize) -> Element<'_, Message> {
    let chips = attachments.iter().enumerate().map(|(index, path)| {
        let remove = button(icon(Icon::Close, |tokens| tokens.muted))
            .padding(2)
            .style(style::row_button)
            .on_press(Message::Session(SessionMessage::RemoveAttachment(index)));
        let remove = container(remove).id(format!("remove-attachment-{index}"));
        attachment_chip(
            row![text(file_name(path)).size(theme::SMALL), remove]
                .spacing(4)
                .align_y(Alignment::Center),
            Padding::from([1, 4]).left(8),
        )
    });
    let pending = (pasting > 0).then(|| {
        container(text("pasting…").size(theme::SMALL).style(style::muted))
            .padding(Padding::from([1, 8]))
            .style(badge::surface)
            .into()
    });
    Row::with_children(chips.chain(pending))
        .spacing(6)
        .wrap()
        .into()
}

/// `content` in the small surface chip an attachment is shown in, padded by `padding`.
fn attachment_chip<'a>(
    content: impl Into<Element<'a, Message>>,
    padding: Padding,
) -> Element<'a, Message> {
    container(content)
        .padding(padding)
        .style(badge::surface)
        .into()
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
pub(crate) struct BackendChoice {
    pub(crate) id: BackendId,
    pub(crate) name: String,
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
pub(crate) fn with_chevron<'a>(picker: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    with_chevron_inset(picker, 0.0)
}

/// `picker` with a chevron `inset` logical pixels in from its right end, as
/// [`with_chevron`] draws it, for a picker whose frame the chevron must not touch.
pub(crate) fn with_chevron_inset<'a>(
    picker: impl Into<Element<'a, Message>>,
    inset: f32,
) -> Element<'a, Message> {
    let chevron = icon(Icon::ChevronDown, |tokens| tokens.muted)
        .width(CHEVRON_SIZE)
        .height(CHEVRON_SIZE);
    stack![
        picker.into(),
        container(chevron)
            .align_right(Length::Fill)
            .center_y(Length::Fill)
            .padding(Padding::default().right(inset))
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
pub(crate) struct Choice {
    pub(crate) id: ConfigValueId,
    pub(crate) name: String,
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
    let shift = press.modifiers.shift();
    match press.key.as_ref() {
        keyboard::Key::Named(key::Named::Enter) if !shift => Some(if running {
            text_editor::Binding::Sequence(Vec::new())
        } else {
            text_editor::Binding::Custom(Message::Session(SessionMessage::Submit))
        }),
        _ => match text_editor::Binding::from_key_press(press) {
            // Shift keeps the text box's own paste: plain text, even when the clipboard
            // also holds an image, as office programs offer for copied text.
            Some(text_editor::Binding::Paste) if shift => Some(text_editor::Binding::Paste),
            Some(text_editor::Binding::Paste) => Some(text_editor::Binding::Custom(
                Message::Session(SessionMessage::Paste),
            )),
            other => other,
        },
    }
}

#[cfg(test)]
mod tests;
