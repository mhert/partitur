//! How each kind of transcript entry is drawn.
//!
//! A prompt sits on the right in a tinted bubble, under a line that says who sent it: the
//! user, the session's orchestrator or Partitur. A user's prompt also shows what it was
//! sent with: a preview of each image, which opens the viewer, and a chip for each other
//! file. The agent's answers are Markdown. A thought is an event row that shows how it
//! begins and expands to the whole thought. Consecutive tool calls share one event row that
//! names what each call does to which file; expanded, it lists the calls, and each call
//! expands to its output and the files it touches. A call's MCP App widget sits right under
//! the call's line, or under the row while the row is collapsed (see
//! [`widget`](super::widget)). Permission requests are cards with a
//! button per option; notices are small muted lines; the end of a turn or a failure is a
//! thin separator that says why. An undecided proposal of children is the split card of
//! [`proposal`](super::proposal), and a question the agent asks is the card of
//! [`question`](super::question), dimmed once it is closed.
//!
//! From the first proposal the user decided on, the transcript continues as the timeline of
//! [`timeline`](super::timeline), which also draws what only an orchestrator's transcript
//! holds: a decided proposal, a child's result and a prompt sent to a child.
//!
//! Every row of the conversation and the timeline is built, but only those near the
//! viewport are laid out; spacers as tall as the rows they stand for take the place of the
//! others. A row that is laid out reports its height, which is used for it from then on in
//! place of a guess. The transcript is one area of selectable text, which tells the Session
//! tab while the user selects in it, and whose selection is cleared when the first row laid
//! out shifts.

use std::ops::Range;
use std::path::{Path, PathBuf};

use iced::widget::{
    Column, Row, Space, button, column, container, image, markdown, row, sensor, space, text,
};
use iced::{Alignment, Border, Color, Element, Length, Padding, Pixels, Size, Theme, border};
use partitur_core::{
    Entry, PermissionKind, PermissionOption, PermissionOptionId, SentAttachment, SessionViewModel,
    StopReason, ToolContent, ToolKind, ToolStatus,
};

use super::window::{self, Basis, Heights, RowKey, Window};
use super::{
    SessionMessage, SessionState, proposal, question, rows, timeline, widget, workflow_rows,
};
use crate::app::Message;
use crate::ellipsis::{keep_end, keep_start};
use crate::previews::Previews;
use crate::prose::{LinkTarget, Prose};
use crate::selectable::{self, Text};
use crate::theme::Tokens;
use crate::viewer::Shown;
use crate::widgets::Widgets;
use crate::{badge, style, theme};

/// The space between two entries of the transcript, in logical pixels.
pub(super) const ENTRY_GAP: f32 = 14.0;

/// The widest a prompt's bubble grows, in logical pixels.
const BUBBLE_MAX_WIDTH: f32 = 560.0;

/// The width of the column an event row's label sits in, so what follows the label starts
/// at the same place in every row.
const LABEL_WIDTH: f32 = 84.0;

/// The space between the parts of an event row.
pub(super) const PART_GAP: f32 = 10.0;

/// The most characters of a thought, or of a prompt Partitur wrote, its row shows; a longer
/// one is cut and expands.
pub(super) const EXCERPT_BUDGET: usize = 90;

/// The most characters of calls the `tools` row sums up; the calls beyond are counted.
const TOOLS_BUDGET: usize = 72;

/// The most characters of one call's file or title in the `tools` row.
const TARGET_BUDGET: usize = 40;

/// What the gaps and the dot between two calls take of the `tools` row's budget, in
/// characters.
const SEPARATOR_COST: usize = 4;

/// How far an expanded call's details are indented, in logical pixels.
const DETAILS_INDENT: f32 = 16.0;

/// The label of the divider before a turn the agent started on its own.
const AGENT_TURN: &str = "continued on its own";

/// How strongly an added or removed block is tinted with the colour of its kind.
const DIFF_TINT: f32 = 0.2;

/// The height of a line of prose.
pub(super) const LINE: Pixels = Pixels(theme::LINE_HEIGHT);

/// A colour picked from the active theme's tokens.
pub(super) type Ink = fn(&Tokens) -> Color;

/// The session's transcript, oldest entry first, with its answers drawn in the colours of
/// `tokens`: the conversation up to the first proposal the user decided on, and the
/// timeline from there.
///
/// Only the rows of the window the Session tab keeps around the viewport are built and laid
/// out; two spacers stand in for the rows before and after it. Each row laid out reports its
/// extent, which replaces the row's estimate from then on. The transcript is one selectable
/// area, which reports whether the user selects in it and whose selection is cleared when
/// the first row laid out changes.
pub(super) fn transcript<'a>(
    model: &SessionViewModel<'a>,
    state: &'a SessionState,
    widgets: &'a Widgets,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let answers = style::markdown(tokens);
    let all = state.rows.rows(model);
    let shown = shown(model, state, &all);
    let last = all.len().saturating_sub(1);
    // A row's sensor measures its gap with it, so what it reports is the row's extent.
    let measured = state.visible.map(|visible| visible.width);
    let laid_out = all
        .iter()
        .enumerate()
        .skip(shown.rows.start)
        .take(shown.rows.len())
        .map(|(index, row)| {
            let element = match (row.key, row.run(model.entries)) {
                (RowKey::Conversation(first), [entry]) => {
                    self::entry(model, first, entry, state, widgets, &answers)
                }
                (RowKey::Conversation(first), calls) => tools(model, first, calls, state, widgets),
                (RowKey::Timeline(_) | RowKey::Pending | RowKey::ChildPermission(_), _) => {
                    timeline::drawn(model, row, state, widgets, &answers, index == last)
                }
            };
            let key = row.key;
            let owner = model.id.clone();
            let measure = move |size: Size| {
                Message::Session(SessionMessage::Measured(owner.clone(), key, size.height))
            };
            // The rows keep each row's state as the window slides, so a row reports itself
            // when it comes in. The key names the row all the same, so that a row whose state
            // another row held before, such as the row of what is pending, reports itself too.
            // The width is part of the key: when it changes, every row reports itself anew,
            // after the measurements of the old width were forgotten.
            let row = sensor(container(element).padding(Padding::ZERO.top(row.gap)))
                .key((measured, model.id.clone(), key))
                // Every row laid out reports itself, also those in the overscan.
                .anticipate(f32::INFINITY)
                .on_show(measure.clone())
                .on_resize(measure);
            Element::from(row)
        });
    let column = column![
        Space::new().height(shown.above),
        rows::rows(shown.rows.start, laid_out),
        Space::new().height(shown.below),
    ]
    .width(Length::Fill);
    selectable::area(column)
        .epoch(shown.rows.start)
        .on_selection(|selects| Message::Session(SessionMessage::Selecting(selects)))
        .into()
}

/// The window of the rows `all` of the session `model` to lay out: the one the Session tab
/// keeps when it was picked for these rows, else one picked for the viewport now.
fn shown(model: &SessionViewModel<'_>, state: &SessionState, all: &[TranscriptRow]) -> Window {
    let width = super::text_width(model, state.visible);
    match &state.shown {
        Some(kept) if kept.fits(&model.id, width, all.len()) => kept.window.clone(),
        _ => window::window(&extents(model, &state.heights, all, width), state.visible),
    }
}

/// Every row of the transcript of the session `model`, oldest first, without what it draws:
/// the conversation's rows, then the timeline's from the first decided proposal on. The first
/// row has no gap above it. What the rows depend on is what
/// [`RowCache`](super::row_cache::RowCache) keeps them for.
pub(super) fn rows(model: &SessionViewModel<'_>) -> Vec<TranscriptRow> {
    let entries = model.entries;
    let decided = timeline::start(entries);
    let conversation = entries.get(..decided.unwrap_or(entries.len()));
    let mut rows: Vec<TranscriptRow> = runs(0, conversation.unwrap_or_default())
        .map(|(first, run)| TranscriptRow::of(RowKey::Conversation(first), ENTRY_GAP, first, run))
        .collect();
    if let Some(start) = decided {
        rows.extend(timeline::rows(model, start));
    }
    if let Some(first) = rows.first_mut() {
        first.gap = 0.0;
    }
    rows
}

/// The extents of the rows `all` of the session `model` at `width`: the measured ones, and
/// estimates for the rows that were never laid out, those `heights` keeps where they still
/// hold.
pub(super) fn extents(
    model: &SessionViewModel<'_>,
    heights: &Heights,
    all: &[TranscriptRow],
    width: f32,
) -> Vec<f32> {
    let session = &model.id;
    all.iter()
        .map(|row| {
            heights.get(session, row.key).unwrap_or_else(|| {
                let run = row.run(model.entries);
                let basis = Basis::of(run, row.open);
                let estimate = heights
                    .estimated(session, row.key, basis)
                    .unwrap_or_else(|| window::estimate(run, row.open, width));
                row.gap + estimate
            })
        })
        .collect()
}

/// Has `heights` keep an estimate at `width` for each of the rows `all` of the session
/// `model` that was never laid out and has none that still holds.
pub(super) fn keep_estimates(
    model: &SessionViewModel<'_>,
    heights: &mut Heights,
    all: &[TranscriptRow],
    width: f32,
) {
    let session = &model.id;
    for row in all {
        let run = row.run(model.entries);
        let basis = Basis::of(run, row.open);
        if heights.get(session, row.key).is_none()
            && heights.estimated(session, row.key, basis).is_none()
        {
            let estimate = window::estimate(run, row.open, width);
            heights.keep_estimate(session, row.key, basis, estimate);
        }
    }
}

/// One row of the transcript, with what it takes to guess its extent before it is laid out.
/// It names its entries by their place in the transcript rather than holding them, so the
/// rows outlive the view of the session they were built from.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TranscriptRow {
    /// Which row it is.
    pub(super) key: RowKey,
    /// The space above the row.
    pub(super) gap: f32,
    /// Where the entries the row draws stand in the transcript; none for the row of what is
    /// pending and for a child's permission request.
    pub(super) entries: Range<usize>,
    /// Whether the row holds a permission request that waits for an answer.
    pub(super) open: bool,
}

impl TranscriptRow {
    /// The row `key` with `gap` above it that draws `run`, the entries of the transcript from
    /// the one at `first` on.
    pub(super) fn of(key: RowKey, gap: f32, first: usize, run: &[Entry]) -> Self {
        Self {
            key,
            gap,
            entries: first..first.saturating_add(run.len()),
            open: open(run),
        }
    }

    /// The row `key` with no gap above it that draws no entries.
    pub(super) fn empty(key: RowKey) -> Self {
        Self {
            key,
            gap: 0.0,
            entries: 0..0,
            open: false,
        }
    }

    /// The entries the row draws, out of `transcript`, the entries of its session.
    pub(super) fn run<'a>(&self, transcript: &'a [Entry]) -> &'a [Entry] {
        transcript.get(self.entries.clone()).unwrap_or_default()
    }
}

/// Whether the row of `run` is a permission request that waits for an answer.
pub(super) fn open(run: &[Entry]) -> bool {
    matches!(run, [Entry::Permission { answer: None, .. }])
}

/// `entries` in the pieces the transcript draws as one row each: every entry by itself,
/// except that consecutive tool calls share a row. Each piece comes with the index of its
/// first entry in the transcript, of which `entries` is the part from `start` on.
pub(super) fn runs(start: usize, entries: &[Entry]) -> impl Iterator<Item = (usize, &[Entry])> {
    let mut first = start;
    entries
        .chunk_by(|one, next| is_tool_call(one) && is_tool_call(next))
        .map(move |run| {
            let index = first;
            first = first.saturating_add(run.len());
            (index, run)
        })
}

/// Whether `entry` is a tool call.
fn is_tool_call(entry: &Entry) -> bool {
    matches!(entry, Entry::ToolCall { .. })
}

/// The entry at `index` of the transcript of the session `model`, as the conversation
/// draws it.
pub(super) fn entry<'a>(
    model: &SessionViewModel<'_>,
    index: usize,
    entry: &'a Entry,
    state: &'a SessionState,
    widgets: &'a Widgets,
    answers: &markdown::Settings,
) -> Element<'a, Message> {
    let session = &model.id;
    let expanded = || state.expanded.contains(&(session.clone(), index));
    match entry {
        Entry::User { text, attachments } => prompt(
            Sender::User,
            text,
            attachments,
            model.attachment_dir.as_deref(),
            &state.previews,
        ),
        Entry::Agent(answer) => container(self::answer(
            state.markdown.items(session, index),
            answer,
            answers,
        ))
        .padding([4, 12])
        .into(),
        Entry::Thought(thought) => excerpt_row("thinking", index, thought, expanded()),
        Entry::ToolCall { .. } => tools(model, index, std::slice::from_ref(entry), state, widgets),
        Entry::Permission { .. } => permission(model, index, entry, expanded()),
        Entry::Notice(notice) => container(
            selectable::text(notice)
                .size(theme::SMALL)
                .style(style::muted),
        )
        .padding([0, 12])
        .into(),
        Entry::AgentTurn => separator(AGENT_TURN.into(), false),
        Entry::TurnEnded(reason) => separator(turn_ended(*reason).into(), false),
        Entry::Failed(error) => separator(format!("failed: {error}"), true),
        Entry::Question {
            id,
            question,
            outcome: None,
            ..
        } => {
            let other = state.question_drafts.get(&(session.clone(), id.clone()));
            question::card(model, id, question, other.map_or("", String::as_str))
        }
        Entry::Question {
            question,
            outcome: Some(outcome),
            decider,
            ..
        } => question::closed(question, outcome, *decider),
        Entry::OrchestratorPrompt(message) => relayed(Sender::Orchestrator, message),
        Entry::PartiturPrompt(message) => relayed(Sender::Partitur, message),
        Entry::ChildResult {
            repository,
            origin,
            outcome,
            state,
            text: answer,
            ..
        } => timeline::child_result(
            index,
            super::repository_name(model, repository),
            *origin,
            outcome,
            state,
            answer,
            expanded(),
        ),
        Entry::ChildPrompted {
            child,
            prompt,
            decision,
        } => timeline::child_prompted(model, child, prompt, *decision),
        Entry::TitleProposal(proposal) => timeline::title_proposal(proposal),
        Entry::Workflow { at, event, row } => workflow_rows::row(*at, event, row.as_ref()),
        Entry::Proposal {
            id,
            decision: Some(decision),
            ..
        } => proposal::decided(model, id, decision)
            .unwrap_or_else(|| timeline::decided(model, decision)),
        Entry::Proposal {
            id,
            children,
            decision: None,
        } => match state.proposals.get(&(session.clone(), id.clone())) {
            Some(draft) => proposal::card(model, id, draft),
            None => proposal::read_only(model, children),
        },
    }
}

/// Who sent a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sender {
    /// The user, in the prompt box.
    User,
    /// The orchestrator the session was spawned from.
    Orchestrator,
    /// Partitur itself, to tell an orchestrator what its children did.
    Partitur,
}

/// The height of an image's preview in a prompt, in logical pixels; its width follows the
/// picture's aspect ratio.
pub(super) const PREVIEW_HEIGHT: f32 = 120.0;

/// A prompt `message` that `sender` sent without attachments, drawn as [`prompt`] draws it.
fn relayed(sender: Sender, message: &str) -> Element<'_, Message> {
    prompt(sender, message, &[], None, &Previews::default())
}

/// A prompt: on the right, who sent it over the tinted bubble with `message` and the
/// `attachments` it was sent with, whose files are in `dir` and whose decoded previews are in
/// `previews`. Neither a message nor an attachment leaves the sender's line alone.
fn prompt<'a>(
    sender: Sender,
    message: &'a str,
    attachments: &'a [SentAttachment],
    dir: Option<&Path>,
    previews: &Previews,
) -> Element<'a, Message> {
    let source: Element<'_, Message> = match sender {
        Sender::User => source("you"),
        Sender::Orchestrator => container(text("from orchestrator").size(theme::TINY))
            .padding([0, 6])
            .style(|theme: &Theme| {
                let tint = theme::tokens(theme).orchestrator;
                container::Style {
                    background: Some(tint.bg.into()),
                    text_color: Some(tint.fg),
                    border: Border::default().rounded(3),
                    ..container::Style::default()
                }
            })
            .into(),
        Sender::Partitur => source("Partitur"),
    };
    if message.is_empty() && attachments.is_empty() {
        return container(column![source].align_x(Alignment::End))
            .align_right(Length::Fill)
            .into();
    }
    let mut content = Column::new().spacing(8).align_x(Alignment::End);
    if !message.is_empty() {
        content =
            content.push(container(selectable::text(message).line_height(LINE)).id("prompt-text"));
    }
    if !attachments.is_empty() {
        content = content.push(sent_with(attachments, dir, previews));
    }
    let bubble = container(content)
        .id("prompt-bubble")
        .padding([10, 14])
        .max_width(BUBBLE_MAX_WIDTH)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.prompt_bubble.into()),
                text_color: Some(tokens.prompt_text),
                // The corner towards the sender is nearly square, like a speech bubble's tail.
                border: Border::default().rounded(border::radius(8).bottom_right(2)),
                ..container::Style::default()
            }
        });
    container(column![source, bubble].spacing(6).align_x(Alignment::End))
        .align_right(Length::Fill)
        .into()
}

/// The `attachments` a prompt was sent with, whose files are in `dir` and whose decoded
/// previews are in `previews`, side by side and wrapped onto further lines when they do not
/// fit (see [`sent`]).
pub(super) fn sent_with<'a>(
    attachments: &'a [SentAttachment],
    dir: Option<&Path>,
    previews: &Previews,
) -> Element<'a, Message> {
    let shown = attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| sent(index, attachment, dir, previews));
    Row::with_children(shown).spacing(6).wrap().into()
}

/// One attachment of a sent prompt, in the container `attachment-<index>`. An image whose
/// preview file in `dir` was decoded into `previews` shows it, and opens the whole picture in
/// the viewer when pressed; an image without one (not decoded yet, missing or unreadable) is
/// a chip of its name that opens the same viewer; another file is a chip that opens nothing.
/// Without a `dir`, nothing opens.
fn sent<'a>(
    index: usize,
    attachment: &'a SentAttachment,
    dir: Option<&Path>,
    previews: &Previews,
) -> Element<'a, Message> {
    let id = format!("attachment-{index}");
    let element = match attachment {
        SentAttachment::Image {
            name,
            copy,
            preview,
        } => {
            let open = dir.map(|dir| Message::Show(Shown::Image(dir.join(copy))));
            let shown = dir
                .zip(preview.as_ref())
                .and_then(|(dir, preview)| previews.get(&dir.join(preview)));
            let content: Element<'a, Message> = match shown {
                Some(picture) => image(picture.clone()).height(PREVIEW_HEIGHT).into(),
                None => super::attachment_chip(
                    text(name.as_str()).size(theme::SMALL),
                    Padding::from([1, 8]),
                ),
            };
            let mut open_button = button(content).padding(0).style(chromeless);
            if let Some(open) = open {
                open_button = open_button.on_press(open);
            }
            open_button.into()
        }
        SentAttachment::File { name, .. } => super::attachment_chip(
            text(name.as_str()).size(theme::SMALL),
            Padding::from([1, 8]),
        ),
    };
    container(element).id(id).into()
}

/// The style of a button around an attachment: no chrome, and the text of the bubble it
/// sits in rather than the default button text, which is unreadable on a dark bubble.
fn chromeless(theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        text_color: theme::tokens(theme).prompt_text,
        ..button::Style::default()
    }
}

/// The plain line over a bubble that names the prompt's `sender`.
fn source(sender: &str) -> Element<'_, Message> {
    text(sender).size(theme::TINY).style(style::muted).into()
}

/// `content` in a bordered card across the transcript.
pub(super) fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(style::card)
        .into()
}

/// The agent's answer as Markdown drawn with the settings `answers`, or as plain text until
/// it has been parsed.
pub(super) fn answer<'a>(
    items: Option<&'a [markdown::Item]>,
    answer: &'a str,
    answers: &markdown::Settings,
) -> Element<'a, Message> {
    match items {
        Some(items) => markdown::view_with(items, *answers, &Prose::<InAnswer>::new()),
        None => selectable::text(answer).line_height(LINE).into(),
    }
}

/// The links in an agent's answer, which the Session tab opens.
struct InAnswer;

impl LinkTarget for InAnswer {
    fn message(uri: markdown::Uri) -> Message {
        Message::Session(SessionMessage::LinkClicked(uri))
    }
}

/// The event row of the kind `kind` for the `words` of the entry at `index`: the agent's
/// reasoning, or a prompt Partitur wrote. Words within [`EXCERPT_BUDGET`] are shown whole.
/// Longer ones show how they begin, with a button that expands the row to all of them and
/// collapses it again.
pub(super) fn excerpt_row<'a>(
    kind: &'a str,
    index: usize,
    words: &'a str,
    expanded: bool,
) -> Element<'a, Message> {
    let toggle = Message::Session(SessionMessage::ToggleEntry(index));
    let Some(beginning) = beginning(words, EXCERPT_BUDGET) else {
        return event_row(row![label(kind), dot(), prose(words)], None);
    };
    if expanded {
        let line = row![
            label(kind),
            space::horizontal(),
            control("collapse", toggle)
        ];
        return event_row(line, Some(prose(words).into()));
    }
    let line = row![
        label(kind),
        dot(),
        prose(beginning).width(Length::Fill),
        control("expand", toggle)
    ];
    event_row(line, None)
}

/// Words an event row quotes: prose in the stronger of the muted colours.
pub(super) fn prose<'a>(words: impl text::IntoFragment<'a>) -> Text<'a, (), Message> {
    selectable::text(words)
        .line_height(LINE)
        .style(style::ink(|tokens| tokens.muted_strong))
}

/// How `words` longer than `budget` characters begin: their first characters on one line,
/// ending in `…`. `None` for words that fit whole.
pub(super) fn beginning(words: &str, budget: usize) -> Option<String> {
    // Where the first character beyond the budget starts; words without one fit.
    let (cut, _) = words.char_indices().nth(budget)?;
    let kept = words.get(..cut)?;
    // The row is a summary: line breaks of the words would make it a block.
    let line = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("{line}…"))
}

/// A run of consecutive tool calls, the first of them at `first` in the transcript: one
/// event row that sums the calls up, with the calls' widgets under it. Expanded, it counts
/// the calls and lists them below, each with its own state of being expanded and its widget
/// under its line.
pub(super) fn tools<'a>(
    model: &SessionViewModel<'_>,
    first: usize,
    calls: &'a [Entry],
    state: &'a SessionState,
    widgets: &'a Widgets,
) -> Element<'a, Message> {
    let session = &model.id;
    let toggle = Message::Session(SessionMessage::ToggleTools(first));
    if !state.expanded_tools.contains(&(session.clone(), first)) {
        let line = row![
            label("tools"),
            container(summary(calls, &model.cwd)).width(Length::Fill),
            control("expand", toggle)
        ];
        let row = event_row(line, None);
        let below: Vec<Element<'a, Message>> = calls
            .iter()
            .filter_map(|call| widget::below(widgets, session, call))
            .collect();
        if below.is_empty() {
            return row;
        }
        return Column::with_children(std::iter::once(row).chain(below))
            .spacing(widget::GAP)
            .into();
    }

    let lines = calls.iter().enumerate().filter_map(|(offset, call)| {
        let Entry::ToolCall {
            title,
            kind,
            status,
            content,
            locations,
            ..
        } = call
        else {
            return None;
        };
        let index = first.saturating_add(offset);
        let expanded = state.expanded.contains(&(session.clone(), index));
        let line = tool_call(index, title, *kind, *status, content, locations, expanded);
        Some(match widget::below(widgets, session, call) {
            Some(below) => column![line, below].spacing(widget::GAP).into(),
            None => line,
        })
    });
    let line = row![
        label("tools"),
        text(calls_count(calls.len()))
            .line_height(LINE)
            .width(Length::Fill)
            .style(style::ink(|tokens| tokens.muted_strong)),
        control("collapse", toggle)
    ];
    event_row(line, Some(Column::with_children(lines).spacing(2).into()))
}

/// `count` tool calls in words: "1 call", "3 calls".
fn calls_count(count: usize) -> String {
    if count == 1 {
        "1 call".to_owned()
    } else {
        format!("{count} calls")
    }
}

/// What the `calls` do, in one line: each call's verb and file, parted by dots. The first
/// call is always named; further ones as long as they fit [`TOOLS_BUDGET`], and the rest
/// is counted as `+N more`. Paths are written relative to the session's folder `cwd`.
fn summary<'a>(calls: &'a [Entry], cwd: &Path) -> Element<'a, Message> {
    let mut line = Row::new().spacing(PART_GAP).align_y(Alignment::Center);
    let mut used = 0_usize;
    let mut named = 0_usize;
    for call in calls {
        let Entry::ToolCall {
            title,
            kind,
            status,
            locations,
            ..
        } = call
        else {
            continue;
        };
        let mention = Mention::of(title, *kind, locations, cwd);
        used = used.saturating_add(mention.cost());
        if named > 0 {
            if used > TOOLS_BUDGET {
                break;
            }
            line = line.push(dot());
        }
        line = mention.push_onto(line, *status == ToolStatus::Failed);
        named = named.saturating_add(1);
    }
    let rest = calls.len().saturating_sub(named);
    if rest > 0 {
        line = line.push(text(format!("+{rest} more")).style(style::muted));
    }
    // In a narrow window the budget is more than the row holds; the calls then flow onto
    // a second line instead of being squeezed.
    line.wrap().vertical_spacing(2).into()
}

/// One tool call as the `tools` row sums it up, such as `Reading src/main.rs`.
struct Mention {
    /// What the call does; none for a kind of call no verb describes.
    verb: Option<&'static str>,
    /// What the call does it to: a file's path, or the call's title.
    target: String,
    /// Whether the target is a file's path, which is written monospaced.
    file: bool,
}

impl Mention {
    /// Sums up a call of `kind` titled `title` that touches `locations`. A call with a
    /// verb names its first file, relative to the session's folder `cwd`, or its title
    /// when it touches no file; a call without a verb is its title alone.
    fn of(title: &str, kind: ToolKind, locations: &[PathBuf], cwd: &Path) -> Self {
        let verb = verb(kind);
        match verb.and(locations.first()) {
            Some(file) => Self {
                verb,
                target: keep_end(&relative(file, cwd), TARGET_BUDGET),
                file: true,
            },
            None => Self {
                verb,
                target: keep_start(title, TARGET_BUDGET),
                file: false,
            },
        }
    }

    /// How many characters of the row's budget the call takes, the gap to the next call
    /// included.
    fn cost(&self) -> usize {
        self.verb
            .map_or(0, str::len)
            .saturating_add(self.target.chars().count())
            .saturating_add(SEPARATOR_COST)
    }

    /// Adds the call to `line`: the verb, then the target. A call that `failed` leads in
    /// the colour of failures.
    fn push_onto(self, line: Row<'_, Message>, failed: bool) -> Row<'_, Message> {
        let target = text(self.target).line_height(LINE);
        let Some(verb) = self.verb else {
            let ink: Ink = if failed {
                |tokens| tokens.failed.fg
            } else {
                |tokens| tokens.text
            };
            return line.push(target.style(style::ink(ink)));
        };
        let ink: Ink = if failed {
            |tokens| tokens.failed.fg
        } else {
            |tokens| tokens.muted_strong
        };
        let target = if self.file {
            target.font(theme::MONO).size(theme::SMALL)
        } else {
            target
        };
        line.push(text(verb).line_height(LINE).style(style::ink(ink)))
            .push(target.style(style::ink(|tokens| tokens.text)))
    }
}

/// What a call of `kind` does, as the verb that leads its summary; `None` for the kinds
/// whose title says it better.
fn verb(kind: ToolKind) -> Option<&'static str> {
    match kind {
        ToolKind::Read => Some("Reading"),
        ToolKind::Edit => Some("Writing"),
        ToolKind::Delete => Some("Deleting"),
        ToolKind::Move => Some("Moving"),
        ToolKind::Search => Some("Searching"),
        ToolKind::Execute => Some("Running"),
        ToolKind::Fetch => Some("Fetching"),
        ToolKind::Think | ToolKind::SwitchMode | ToolKind::Other => None,
    }
}

/// `file` as a summary writes it: relative to the session's folder `cwd` when it lies in
/// it (`.` for the folder itself), as it is otherwise.
fn relative(file: &Path, cwd: &Path) -> String {
    match file.strip_prefix(cwd) {
        Ok(inside) if inside.as_os_str().is_empty() => ".".to_owned(),
        Ok(inside) => inside.display().to_string(),
        Err(_) => file.display().to_string(),
    }
}

/// A tool call in the list of an expanded `tools` row: its kind, title and status in a
/// line that shows its output and the files it touches when expanded.
fn tool_call<'a>(
    index: usize,
    title: &'a str,
    kind: ToolKind,
    status: ToolStatus,
    content: &'a [ToolContent],
    locations: &'a [PathBuf],
    expanded: bool,
) -> Element<'a, Message> {
    let failed = status == ToolStatus::Failed;
    let line = row![
        badge::chip(kind_word(kind)),
        text(title).width(Length::Fill),
        text(status_word(status))
            .size(theme::SMALL)
            .style(style::ink(status_ink(failed))),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let header = button(line)
        .width(Length::Fill)
        .padding([4, 6])
        .style(style::row_button)
        .on_press(Message::Session(SessionMessage::ToggleEntry(index)));
    if !expanded {
        return header.into();
    }

    let output = content.iter().map(tool_content);
    let files = locations.iter().map(|location| path(location).into());
    let body = Column::with_children(output.chain(files)).spacing(6);
    let details = container(body).padding(Padding::ZERO.left(DETAILS_INDENT).bottom(6));
    column![header, details].spacing(4).into()
}

/// One piece of a tool call's output.
fn tool_content(content: &ToolContent) -> Element<'_, Message> {
    match content {
        ToolContent::Text(output) => selectable::text(output).into(),
        ToolContent::Diff {
            path: file,
            old,
            new,
        } => {
            let mut diff = Column::new().spacing(4).push(path(file));
            if let Some(old) = old {
                diff = diff.push(code(old, |tokens| tokens.diff_del));
            }
            diff.push(code(new, |tokens| tokens.diff_add)).into()
        }
        ToolContent::Patch { paths, text } => {
            let files = paths.iter().map(|file| path(file).into());
            Column::with_children(files)
                .spacing(4)
                .push(patch(text))
                .into()
        }
        ToolContent::Terminal(output) if output.is_empty() => space().into(),
        ToolContent::Terminal(output) => code(output, |tokens| tokens.track),
    }
}

/// A file's path as a call's details write it: monospaced, small and muted.
fn path<'a>(file: &Path) -> Text<'a, (), Message> {
    selectable::text(file.display().to_string())
        .font(theme::MONO)
        .size(theme::SMALL)
        .style(style::muted)
}

/// The colour of the patch line `line`: green when it adds, red when it removes, `None`
/// for context. The `+++` and `---` file headers are context.
fn patch_ink(line: &str) -> Option<Ink> {
    if line.starts_with("+++ ") || line.starts_with("--- ") {
        return None;
    }
    match line.chars().next() {
        Some('+') => Some(|tokens| tokens.diff_add),
        Some('-') => Some(|tokens| tokens.diff_del),
        _ => None,
    }
}

/// The patch text `source` in a monospaced block, a line at a time in the colour of what
/// it does.
fn patch(source: &str) -> Element<'_, Message> {
    let lines = source.lines().map(|line| {
        let shown = selectable::text(line).font(theme::MONO).size(theme::SMALL);
        match patch_ink(line) {
            Some(ink) => shown.style(style::ink(ink)),
            None => shown.style(style::muted),
        }
        .into()
    });
    framed(Column::with_children(lines).into(), |tokens| tokens.track)
}

/// `source` in a monospaced block tinted with the colour `tint` picks from the tokens.
fn code(source: &str, tint: Ink) -> Element<'_, Message> {
    framed(
        selectable::text(source)
            .font(theme::MONO)
            .size(theme::SMALL)
            .into(),
        tint,
    )
}

/// `content` in a padded block tinted with the colour `tint` picks from the tokens.
fn framed(content: Element<'_, Message>, tint: Ink) -> Element<'_, Message> {
    container(content)
        .padding(8)
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(tint(theme::tokens(theme)).scale_alpha(DIFF_TINT).into()),
            border: Border::default().rounded(style::ROW_RADIUS),
            ..container::Style::default()
        })
        .into()
}

/// A permission request: what asks, then a button per option, or the outcome once it is
/// settled. While it waits, the heading shows the whole title. Once settled, a title longer
/// than [`EXCERPT_BUDGET`] shows how it begins, with a button that expands the card to the
/// whole title on a line of its own and collapses it again; short titles get no button. While
/// its paths are being checked, its options that allow cannot be pressed. `entry` is the
/// request; anything else draws nothing.
fn permission<'a>(
    model: &SessionViewModel<'_>,
    index: usize,
    entry: &'a Entry,
    expanded: bool,
) -> Element<'a, Message> {
    let Entry::Permission {
        id,
        title,
        kind,
        options,
        answer,
        ..
    } = entry
    else {
        return space::horizontal().into();
    };
    let (id, kind, answer) = (*id, *kind, answer.as_ref());
    let checking = model.resolving_permissions.contains(&id);
    let chip = badge::chip(kind_word(kind));
    let toggle = Message::Session(SessionMessage::ToggleEntry(index));
    let excerpt = answer.and_then(|_| beginning(title, EXCERPT_BUDGET));
    let (heading, whole) = match excerpt {
        None => (
            row![chip, selectable::text(title).font(theme::UI_SEMIBOLD)],
            None,
        ),
        Some(_) if expanded => (
            row![chip, space::horizontal(), control("collapse", toggle)],
            Some(selectable::text(title).font(theme::UI_SEMIBOLD)),
        ),
        Some(beginning) => (
            row![
                chip,
                selectable::text(beginning)
                    .font(theme::UI_SEMIBOLD)
                    .width(Length::Fill),
                control("expand", toggle)
            ],
            None,
        ),
    };
    let heading = heading.spacing(8).align_y(Alignment::Center);
    let outcome: Element<'a, Message> = match answer {
        None => option_buttons(options, checking, |option| {
            Message::Session(SessionMessage::AnswerPermission(id, Some(option)))
        }),
        Some(chosen) => text(settled(options, chosen.as_ref()))
            .size(theme::SMALL)
            .style(style::muted)
            .into(),
    };
    card(column![heading, whole, outcome].spacing(10))
}

/// A button per option of a permission request, each sending what `answer` makes of its
/// option: filled for an option that allows, outlined for one that rejects. While Partitur is
/// `checking` the paths the request names, the options that allow take no press, and a muted
/// note at the end of the row says why; the ones that reject still answer.
pub(super) fn option_buttons<'a>(
    options: &[PermissionOption],
    checking: bool,
    answer: impl Fn(PermissionOptionId) -> Message,
) -> Element<'a, Message> {
    let buttons = options.iter().map(|option| {
        let choice = button(text(option.name.clone())).padding([7, 14]);
        match option.kind {
            PermissionKind::AllowOnce | PermissionKind::AllowAlways => choice
                .on_press_maybe((!checking).then(|| answer(option.id.clone())))
                .style(style::primary_button)
                .into(),
            PermissionKind::RejectOnce | PermissionKind::RejectAlways => choice
                .on_press(answer(option.id.clone()))
                .style(style::outlined_button)
                .into(),
        }
    });
    let note = checking.then(|| {
        text(CHECKING_PATHS)
            .size(theme::SMALL)
            .style(style::dim)
            .into()
    });
    Row::with_children(buttons.chain(note))
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// What a permission card says while Partitur resolves the paths its request names.
const CHECKING_PATHS: &str = "checking paths…";

/// How a permission request was settled: `answered` and the name of the option chosen
/// (its id when the request does not name it), or `cancelled`.
fn settled(options: &[PermissionOption], chosen: Option<&PermissionOptionId>) -> String {
    let Some(chosen) = chosen else {
        return "cancelled".into();
    };
    let name = options
        .iter()
        .find(|option| &option.id == chosen)
        .map_or_else(|| chosen.to_string(), |option| option.name.clone());
    format!("answered · {name}")
}

/// A thin line across the transcript with `label` in the middle, in the colour of
/// failures when the turn `failed`.
fn separator<'a>(label: String, failed: bool) -> Element<'a, Message> {
    let label = text(label)
        .size(theme::TINY)
        .style(style::ink(status_ink(failed)));
    row![
        style::hairline(|tokens| tokens.border),
        label,
        style::hairline(|tokens| tokens.border)
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// An event row: `line` on a bordered panel, with `details`, when there are any, below it.
pub(super) fn event_row<'a>(
    line: Row<'a, Message>,
    details: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let line = line.spacing(PART_GAP).align_y(Alignment::Center);
    let content = match details {
        Some(details) => column![line, details].spacing(8).into(),
        None => Element::from(line),
    };
    panel(content)
}

/// `content` on the bordered panel of an event row, with its padding, across the transcript.
pub(super) fn panel<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding([8, 12])
        .width(Length::Fill)
        .style(style::event_row)
        .into()
}

/// The word an event row starts with, which says what kind of event it is.
pub(super) fn label<'a>(kind: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(kind)
        .line_height(LINE)
        .width(LABEL_WIDTH)
        .style(style::muted)
        .into()
}

/// The dot that parts two pieces of an event row.
pub(super) fn dot<'a>() -> Element<'a, Message> {
    text("·").line_height(LINE).style(style::dim).into()
}

/// The text button at the right end of an event row, which sends `on_press`.
pub(super) fn control(word: &str, on_press: Message) -> Element<'_, Message> {
    button(text(word).line_height(LINE))
        .padding(0)
        .style(style::link_button)
        .on_press(on_press)
        .into()
}

/// The colour of a status word: muted, or the colour of failures when something `failed`.
fn status_ink(failed: bool) -> Ink {
    if failed {
        |tokens| tokens.failed.fg
    } else {
        |tokens| tokens.muted
    }
}

/// What a tool call does, in one short word.
fn kind_word(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "mode",
        ToolKind::Other => "tool",
    }
}

/// How far a tool call got, in one short word.
fn status_word(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::InProgress => "running",
        ToolStatus::Completed => "done",
        ToolStatus::Failed => "failed",
    }
}

/// Why a turn ended, as the separator says it.
pub(super) fn turn_ended(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end of turn",
        StopReason::MaxTokens => "stopped: token limit reached",
        StopReason::MaxTurnRequests => "stopped: request limit reached",
        StopReason::Refusal => "the agent refused",
        StopReason::Cancelled => "cancelled",
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

    use iced::widget::Id;
    use iced_test::simulator;
    use tempfile::TempDir;

    use iced::widget::image::Handle;

    use super::*;

    /// Writes a 4 by 2 PNG to `dir/name`.
    fn png(dir: &Path, name: &str) {
        ::image::RgbaImage::new(4, 2).save(dir.join(name)).unwrap();
    }

    fn picture(preview: Option<&str>) -> SentAttachment {
        SentAttachment::Image {
            name: "s.png".into(),
            copy: "1-s.png".into(),
            preview: preview.map(PathBuf::from),
        }
    }

    /// The messages pressing `attachment-0` of a user prompt with `attachments` and the
    /// decoded `previews` sends.
    fn pressed_with(
        attachments: &[SentAttachment],
        dir: Option<&Path>,
        previews: &Previews,
    ) -> Vec<Message> {
        let mut ui = simulator(prompt(Sender::User, "look", attachments, dir, previews));
        ui.find("look").unwrap();
        ui.click(Id::new("attachment-0")).unwrap();
        ui.into_messages().collect()
    }

    /// The messages pressing `attachment-0` of a user prompt with `attachments` sends, while
    /// no preview is decoded.
    fn pressed(attachments: &[SentAttachment], dir: Option<&Path>) -> Vec<Message> {
        pressed_with(attachments, dir, &Previews::default())
    }

    /// The previews with the 4 by 2 picture of `path` decoded.
    fn decoded(path: PathBuf) -> Previews {
        let mut previews = Previews::default();
        previews.keep(path, Some(Handle::from_rgba(4, 2, vec![0; 32])));
        previews
    }

    #[test]
    fn a_pressable_chip_has_the_bubbles_text_colour_in_both_themes() {
        for mode in [iced::theme::Mode::Light, iced::theme::Mode::Dark] {
            let theme = theme::theme(mode);
            let style = chromeless(&theme, button::Status::Active);
            assert_eq!(style.text_color, theme::tokens(&theme).prompt_text);
            assert_eq!(style.background, None);
        }
    }

    #[test]
    fn a_sent_image_shows_its_preview_and_opens_the_viewer() {
        let dir = TempDir::new().unwrap();
        let attachments = [picture(Some("1-s.preview.png"))];
        let previews = decoded(dir.path().join("1-s.preview.png"));

        let mut ui = simulator(prompt(
            Sender::User,
            "look",
            &attachments,
            Some(dir.path()),
            &previews,
        ));
        ui.find("look").unwrap();
        // A preview carries no name chip.
        assert!(ui.find("s.png").is_err());

        assert_eq!(
            pressed_with(&attachments, Some(dir.path()), &previews),
            [Message::Show(Shown::Image(dir.path().join("1-s.png")))]
        );
    }

    #[test]
    fn sent_image_preview_appears_after_decode() {
        use partitur_core::TranscriptRecord;

        use crate::app::{Tab, update, view};
        use crate::test_support::{first_output, plain};

        let root = TempDir::new().unwrap();
        let mut fixture = plain();
        fixture.store.set_attachment_root(root.path());
        let dir = fixture.state.app.attachment_dir(&fixture.session).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        png(&dir, "1-s.png");
        png(&dir, "1-s.preview.png");
        let _task = update(&mut fixture.state, Message::SelectTab(Tab::Session));

        let attachments = TranscriptRecord::UserAttachments(vec![picture(Some("1-s.preview.png"))]);
        fixture
            .state
            .app
            .record(&fixture.session, &attachments)
            .unwrap();
        let task = update(&mut fixture.state, Message::SelectTab(Tab::Session));

        // Until the preview is decoded, the image is the chip of its name.
        simulator(view(&fixture.state)).find("s.png").unwrap();
        let landed = first_output(task);
        let _task = update(&mut fixture.state, landed);
        let mut ui = simulator(view(&fixture.state));
        ui.find("Build it").unwrap();
        assert!(ui.find("s.png").is_err());
    }

    #[test]
    fn an_image_without_a_preview_shows_its_name() {
        let dir = TempDir::new().unwrap();
        let attachments = [picture(None)];

        let mut ui = simulator(prompt(
            Sender::User,
            "look",
            &attachments,
            Some(dir.path()),
            &Previews::default(),
        ));
        ui.find("s.png").unwrap();

        assert_eq!(
            pressed(&attachments, Some(dir.path())),
            [Message::Show(Shown::Image(dir.path().join("1-s.png")))]
        );
    }

    #[test]
    fn a_missing_preview_file_shows_the_name() {
        let dir = TempDir::new().unwrap();
        let attachments = [picture(Some("1-s.preview.png"))];
        let mut previews = Previews::default();
        previews.keep(dir.path().join("1-s.preview.png"), None);

        let mut ui = simulator(prompt(
            Sender::User,
            "look",
            &attachments,
            Some(dir.path()),
            &previews,
        ));
        ui.find("s.png").unwrap();

        assert_eq!(
            pressed(&attachments, Some(dir.path())),
            [Message::Show(Shown::Image(dir.path().join("1-s.png")))]
        );
    }

    #[test]
    fn an_image_without_a_folder_shows_its_name_and_opens_nothing() {
        let attachments = [picture(Some("1-s.preview.png"))];

        let mut ui = simulator(prompt(
            Sender::User,
            "look",
            &attachments,
            None,
            &Previews::default(),
        ));
        ui.find("s.png").unwrap();

        assert_eq!(pressed(&attachments, None), []);
    }

    #[test]
    fn another_file_shows_its_name_and_opens_nothing() {
        let attachments = [SentAttachment::File {
            name: "a.rs".into(),
            path: "/work/a.rs".into(),
        }];

        let mut ui = simulator(prompt(
            Sender::User,
            "look",
            &attachments,
            None,
            &Previews::default(),
        ));
        ui.find("a.rs").unwrap();

        assert_eq!(pressed(&attachments, None), []);
    }

    #[test]
    fn a_prompt_of_attachments_only_has_no_text_line() {
        let attachments = [SentAttachment::File {
            name: "a.rs".into(),
            path: "/work/a.rs".into(),
        }];

        let mut ui = simulator(prompt(
            Sender::User,
            "",
            &attachments,
            None,
            &Previews::default(),
        ));

        ui.find("a.rs").unwrap();
        ui.find(Id::new("prompt-bubble")).unwrap();
        assert!(ui.find(Id::new("prompt-text")).is_err());
    }

    #[test]
    fn an_empty_prompt_without_attachments_draws_no_bubble() {
        let mut ui = simulator(prompt(Sender::User, "", &[], None, &Previews::default()));

        ui.find("you").unwrap();
        assert!(ui.find(Id::new("prompt-bubble")).is_err());
    }

    #[test]
    fn a_thought_within_the_budget_has_no_cut_beginning() {
        assert_eq!(beginning("", EXCERPT_BUDGET), None);
        assert_eq!(beginning(&"é".repeat(EXCERPT_BUDGET), EXCERPT_BUDGET), None);
        assert_eq!(beginning("four", 4), None);
    }

    #[test]
    fn a_longer_thought_begins_with_its_first_characters_on_one_line() {
        let thought = format!("one\n\ntwo  {}", "é".repeat(EXCERPT_BUDGET));

        let beginning = beginning(&thought, EXCERPT_BUDGET).unwrap();

        // "one\n\ntwo  " takes 10 of the budget; on one line it is "one two ".
        let kept = "é".repeat(EXCERPT_BUDGET - 10);
        assert_eq!(beginning, format!("one two {kept}…"));
    }

    #[test]
    fn consecutive_tool_calls_form_one_run_that_knows_its_first_index() {
        let call = |id: &str| Entry::ToolCall {
            id: partitur_core::ToolCallId::new(id),
            title: id.into(),
            kind: ToolKind::Read,
            status: ToolStatus::Completed,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
            drawing: None,
        };
        let entries = [
            Entry::user("go"),
            call("one"),
            call("two"),
            Entry::Agent("done".into()),
            call("three"),
        ];

        let runs: Vec<(usize, usize)> = runs(10, &entries)
            .map(|(first, run)| (first, run.len()))
            .collect();

        assert_eq!(runs, [(10, 1), (11, 2), (13, 1), (14, 1)]);
    }

    #[test]
    fn a_path_is_written_relative_to_the_sessions_folder() {
        let cwd = Path::new("/work/api");

        assert_eq!(
            relative(Path::new("/work/api/src/main.rs"), cwd),
            "src/main.rs"
        );
        assert_eq!(relative(Path::new("/work/api"), cwd), ".");
        assert_eq!(
            relative(Path::new("/work/web/index.ts"), cwd),
            "/work/web/index.ts"
        );
        assert_eq!(relative(Path::new("Cargo.toml"), cwd), "Cargo.toml");
    }

    #[test]
    fn a_call_with_a_verb_names_its_first_file_and_else_its_title() {
        let cwd = Path::new("/work");
        let files = [PathBuf::from("/work/src/a.rs"), PathBuf::from("/work/b.rs")];

        let read = Mention::of("Read a.rs", ToolKind::Read, &files, cwd);
        assert_eq!(
            (read.verb, read.target.as_str(), read.file),
            (Some("Reading"), "src/a.rs", true)
        );
        assert_eq!(
            read.cost(),
            "Reading".len() + "src/a.rs".len() + SEPARATOR_COST
        );

        let run = Mention::of("cargo test", ToolKind::Execute, &[], cwd);
        assert_eq!(
            (run.verb, run.target.as_str(), run.file),
            (Some("Running"), "cargo test", false)
        );

        let other = Mention::of("Plan the work", ToolKind::Other, &files, cwd);
        assert_eq!(
            (other.verb, other.target.as_str(), other.file),
            (None, "Plan the work", false)
        );
    }

    #[test]
    fn every_kind_with_a_verb_has_its_own() {
        let verbs = [
            (ToolKind::Read, Some("Reading")),
            (ToolKind::Edit, Some("Writing")),
            (ToolKind::Delete, Some("Deleting")),
            (ToolKind::Move, Some("Moving")),
            (ToolKind::Search, Some("Searching")),
            (ToolKind::Execute, Some("Running")),
            (ToolKind::Fetch, Some("Fetching")),
            (ToolKind::Think, None),
            (ToolKind::SwitchMode, None),
            (ToolKind::Other, None),
        ];
        for (kind, expected) in verbs {
            assert_eq!(verb(kind), expected, "{kind:?}");
        }
    }

    #[test]
    fn a_settled_request_names_the_option_or_says_cancelled() {
        let options = [PermissionOption {
            id: PermissionOptionId::new("allow"),
            name: "Allow once".into(),
            kind: PermissionKind::AllowOnce,
        }];

        assert_eq!(
            settled(&options, Some(&PermissionOptionId::new("allow"))),
            "answered · Allow once"
        );
        assert_eq!(
            settled(&options, Some(&PermissionOptionId::new("gone"))),
            "answered · gone"
        );
        assert_eq!(settled(&options, None), "cancelled");
    }

    #[test]
    fn patch_lines_are_coloured_by_what_they_do() {
        let tokens = &theme::DARK;
        let ink = |line| patch_ink(line).map(|pick| pick(tokens));

        assert_eq!(ink("+added"), Some(tokens.diff_add));
        assert_eq!(ink("-removed"), Some(tokens.diff_del));
        assert_eq!(ink(" context"), None);
        assert_eq!(ink("@@ -1 +1 @@"), None);
        assert_eq!(ink(""), None);
        assert_eq!(ink("+++ b/file.rs"), None);
        assert_eq!(ink("--- a/file.rs"), None);
        assert_eq!(ink("--removed dash"), Some(tokens.diff_del));
    }
}
