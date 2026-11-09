//! How each kind of transcript entry is drawn.
//!
//! A prompt sits on the right in a tinted bubble, under a line that says who sent it: the
//! user, the session's orchestrator or Partitur. The agent's answers are Markdown. A thought
//! is an event row that shows how it begins and expands to the whole thought. Consecutive
//! tool calls share one event row that names what each call does to which file; expanded,
//! it lists the calls, and each call expands to its output and the files it touches.
//! Permission requests are cards with a button per option; notices are small muted lines;
//! the end of a turn or a failure is a thin separator that says why. An undecided proposal
//! of children is the split card of [`proposal`](super::proposal).
//!
//! From the first proposal the user decided on, the transcript continues as the timeline of
//! [`timeline`](super::timeline), which also draws what only an orchestrator's transcript
//! holds: a decided proposal, a child's result and a prompt sent to a child.

use std::path::{Path, PathBuf};

use iced::widget::{
    Column, Row, button, column, container, markdown, row, scrollable, space, text,
};
use iced::{Alignment, Border, Color, Element, Length, Padding, Pixels, Theme, border};
use partitur_core::{
    Entry, PermissionId, PermissionKind, PermissionOption, PermissionOptionId, SessionViewModel,
    StopReason, ToolContent, ToolKind, ToolStatus,
};

use super::{SessionMessage, SessionState, proposal, timeline};
use crate::app::Message;
use crate::ellipsis::{keep_end, keep_start};
use crate::selectable::{self, Text};
use crate::theme::Tokens;
use crate::{badge, style, theme};

/// The space between two entries of the transcript, in logical pixels.
const ENTRY_GAP: f32 = 14.0;

/// The widest a prompt's bubble grows, in logical pixels.
const BUBBLE_MAX_WIDTH: f32 = 560.0;

/// The width of the column an event row's label sits in, so what follows the label starts
/// at the same place in every row.
const LABEL_WIDTH: f32 = 84.0;

/// The space between the parts of an event row.
pub(super) const PART_GAP: f32 = 10.0;

/// The most characters of a thought, or of a prompt Partitur wrote, its row shows; a longer
/// one is cut and expands.
const EXCERPT_BUDGET: usize = 90;

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
pub(super) fn transcript<'a>(
    model: &SessionViewModel<'a>,
    state: &'a SessionState,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let entries: &'a [Entry] = model.entries;
    let answers = style::markdown(tokens);
    let decided = timeline::start(entries);
    let conversation = entries.get(..decided.unwrap_or(entries.len()));
    let mut rows = Column::new().spacing(ENTRY_GAP).width(Length::Fill);
    for (first, run) in runs(0, conversation.unwrap_or_default()) {
        rows = rows.push(match run {
            [entry] => self::entry(model, first, entry, state, &answers),
            calls => tools(model, first, calls, state),
        });
    }
    if let Some(start) = decided {
        rows = rows.push(timeline::view(model, start, state, &answers));
    }
    rows.into()
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
    answers: &markdown::Settings,
) -> Element<'a, Message> {
    let session = &model.id;
    let expanded = || state.expanded.contains(&(session.clone(), index));
    match entry {
        Entry::User(message) => prompt(Sender::User, message),
        Entry::Agent(answer) => container(self::answer(
            state.markdown.items(session, index),
            answer,
            answers,
        ))
        .padding([4, 12])
        .into(),
        Entry::Thought(thought) => excerpt_row("thinking", index, thought, expanded()),
        Entry::ToolCall { .. } => tools(model, index, std::slice::from_ref(entry), state),
        Entry::Permission {
            id,
            title,
            kind,
            options,
            answer,
            ..
        } => permission(*id, title, *kind, options, answer.as_ref()),
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
        Entry::OrchestratorPrompt(message) => prompt(Sender::Orchestrator, message),
        Entry::PartiturPrompt(message) => prompt(Sender::Partitur, message),
        Entry::ChildResult {
            repository,
            origin,
            outcome,
            text: answer,
            ..
        } => timeline::child_result(
            model,
            index,
            repository,
            *origin,
            outcome,
            answer,
            expanded(),
        ),
        Entry::ChildPrompted { child, prompt } => timeline::child_prompted(model, child, prompt),
        Entry::Proposal {
            decision: Some(decision),
            ..
        } => timeline::decided(model, decision),
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

/// A prompt: on the right, who sent it over the tinted bubble with `message`.
fn prompt(sender: Sender, message: &str) -> Element<'_, Message> {
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
    let bubble = container(selectable::text(message).line_height(LINE))
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

/// The plain line over a bubble that names the prompt's `sender`.
fn source(sender: &str) -> Element<'_, Message> {
    text(sender).size(theme::TINY).style(style::muted).into()
}

/// `content` in a bordered card across the transcript.
fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
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
        Some(items) => markdown::view_with(items, *answers, &Answer),
        None => selectable::text(answer).line_height(LINE).into(),
    }
}

/// Draws an answer's Markdown the way iced does, except that its text is selectable and a
/// paragraph's lines are as high as those of all other prose.
struct Answer;

impl<'a> markdown::Viewer<'a, Message> for Answer {
    fn on_link_click(uri: markdown::Uri) -> Message {
        Message::Session(SessionMessage::LinkClicked(uri))
    }

    fn paragraph(
        &self,
        settings: markdown::Settings,
        text: &markdown::Text,
    ) -> Element<'a, Message> {
        selectable::rich(text.spans(settings.style))
            .size(settings.text_size)
            .line_height(LINE)
            .on_link_click(Self::on_link_click)
            .into()
    }

    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> Element<'a, Message> {
        let size = match level {
            markdown::HeadingLevel::H1 => settings.h1_size,
            markdown::HeadingLevel::H2 => settings.h2_size,
            markdown::HeadingLevel::H3 => settings.h3_size,
            markdown::HeadingLevel::H4 => settings.h4_size,
            markdown::HeadingLevel::H5 => settings.h5_size,
            markdown::HeadingLevel::H6 => settings.h6_size,
        };
        // Like iced, a heading after other blocks keeps half a line's distance to them.
        let top = if index > 0 {
            settings.text_size.0 / 2.0
        } else {
            0.0
        };
        container(
            selectable::rich(text.spans(settings.style))
                .size(size)
                .on_link_click(Self::on_link_click),
        )
        .padding(Padding::ZERO.top(top))
        .into()
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [markdown::Text],
    ) -> Element<'a, Message> {
        let lines = lines.iter().map(|line| {
            selectable::rich(line.spans(settings.style))
                .font(settings.style.code_block_font)
                .size(settings.code_size)
                .on_link_click(Self::on_link_click)
                .into()
        });
        let bar = settings.code_size.0 / 2.0;
        // Like iced, long lines scroll sideways instead of wrapping.
        let lines =
            scrollable(container(Column::with_children(lines)).padding(settings.code_size.0))
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default()
                        .width(bar)
                        .scroller_width(bar),
                ));
        container(lines)
            .width(Length::Fill)
            .padding(settings.code_size.0 / 4.0)
            .class(<Theme as markdown::Catalog>::code_block())
            .into()
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
/// event row that sums the calls up. Expanded, it counts them and lists them below, each
/// with its own state of being expanded.
pub(super) fn tools<'a>(
    model: &SessionViewModel<'_>,
    first: usize,
    calls: &'a [Entry],
    state: &'a SessionState,
) -> Element<'a, Message> {
    let session = &model.id;
    let toggle = Message::Session(SessionMessage::ToggleTools(first));
    if !state.expanded_tools.contains(&(session.clone(), first)) {
        let line = row![
            label("tools"),
            container(summary(calls, &model.cwd)).width(Length::Fill),
            control("expand", toggle)
        ];
        return event_row(line, None);
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
        Some(tool_call(
            index, title, *kind, *status, content, locations, expanded,
        ))
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
/// settled.
fn permission<'a>(
    id: PermissionId,
    title: &'a str,
    kind: ToolKind,
    options: &'a [PermissionOption],
    answer: Option<&'a Option<PermissionOptionId>>,
) -> Element<'a, Message> {
    let heading = row![
        badge::chip(kind_word(kind)),
        selectable::text(title).font(theme::UI_SEMIBOLD)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let outcome: Element<'a, Message> = match answer {
        None => Row::with_children(options.iter().map(|option| {
            let label = text(option.name.as_str());
            let answer = Message::Session(SessionMessage::AnswerPermission(
                id,
                Some(option.id.clone()),
            ));
            let choice = button(label).padding([7, 14]).on_press(answer);
            match option.kind {
                PermissionKind::AllowOnce | PermissionKind::AllowAlways => {
                    choice.style(style::primary_button).into()
                }
                PermissionKind::RejectOnce | PermissionKind::RejectAlways => {
                    choice.style(style::outlined_button).into()
                }
            }
        }))
        .spacing(8)
        .into(),
        Some(chosen) => text(settled(options, chosen.as_ref()))
            .size(theme::SMALL)
            .style(style::muted)
            .into(),
    };
    card(column![heading, outcome].spacing(10))
}

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
    container(content)
        .padding([8, 12])
        .width(Length::Fill)
        .style(style::event_row)
        .into()
}

/// The word an event row starts with, which says what kind of event it is.
pub(super) fn label(kind: &str) -> Element<'_, Message> {
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

    use super::*;

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
        };
        let entries = [
            Entry::User("go".into()),
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
