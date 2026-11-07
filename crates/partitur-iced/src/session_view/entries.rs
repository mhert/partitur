//! How each kind of transcript entry is drawn.
//!
//! The user's messages sit on the right in a tinted bubble, as do the prompts an orchestrator
//! or Partitur sent, labelled with who sent them; the agent's answers are Markdown; thoughts
//! and tool calls are one-line rows that expand on click; permission requests are cards with
//! a button per option; a child's result is a read-only card, as is a decided proposal of
//! children (an undecided one is an approval card, see [`proposal`](super::proposal));
//! notices are italic; the end of a turn or a failure is a thin separator that says why.

use std::path::PathBuf;

use iced::font::{self, Font};
use iced::widget::{Column, Row, button, column, container, markdown, row, rule, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Theme};
use partitur_core::{
    ChildOutcome, Decision, Entry, PermissionId, PermissionKind, PermissionOption,
    PermissionOptionId, ProposedChild, RepositoryId, SessionViewModel, StopReason, ToolContent,
    ToolKind, ToolStatus, TurnOrigin,
};

use super::{SessionMessage, SessionState};
use crate::app::Message;
use crate::projects_view::{bold, chip, dimmed, row_style};

/// The widest a user message's bubble grows, in logical pixels.
const BUBBLE_MAX_WIDTH: f32 = 560.0;

/// How far an expanded entry's details are indented, in logical pixels.
const DETAILS_INDENT: f32 = 16.0;

/// The session's transcript, oldest entry first.
pub(super) fn transcript<'a>(
    model: &SessionViewModel<'a>,
    state: &'a SessionState,
) -> Element<'a, Message> {
    let entries: &'a [Entry] = model.entries;
    let rows = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| self::entry(model, index, entry, state));
    Column::with_children(rows)
        .spacing(12)
        // Keeps the entries clear of the scrollbar.
        .padding(Padding::ZERO.right(12))
        .width(Length::Fill)
        .into()
}

/// The entry at `index` of the transcript of the session `model`.
fn entry<'a>(
    model: &SessionViewModel<'_>,
    index: usize,
    entry: &'a Entry,
    state: &'a SessionState,
) -> Element<'a, Message> {
    let session = &model.id;
    let expanded = state.expanded.contains(&(session.clone(), index));
    match entry {
        Entry::User(message) => user(message),
        Entry::Agent(answer) => agent(state.markdown.items(session, index), answer),
        Entry::Thought(thought) => self::thought(index, thought, expanded),
        Entry::ToolCall {
            title,
            kind,
            status,
            content,
            locations,
            ..
        } => tool_call(index, title, *kind, *status, content, locations, expanded),
        Entry::Permission {
            id,
            title,
            kind,
            options,
            answer,
            ..
        } => permission(*id, title, *kind, options, answer.as_ref()),
        Entry::Notice(notice) => text(notice).size(13).font(italic()).style(dimmed).into(),
        Entry::TurnEnded(reason) => separator(turn_ended(*reason).into(), false),
        Entry::Failed(error) => separator(format!("failed: {error}"), true),
        Entry::OrchestratorPrompt(prompt) => sent_by("orchestrator", prompt),
        Entry::PartiturPrompt(prompt) => sent_by("Partitur", prompt),
        Entry::ChildResult {
            repository,
            title,
            origin,
            outcome,
            text: answer,
            ..
        } => child_result(repository, title, *origin, outcome, answer),
        Entry::Proposal {
            id,
            children,
            decision,
        } => {
            let draft = decision
                .is_none()
                .then(|| state.proposals.get(&(session.clone(), id.clone())))
                .flatten();
            match draft {
                Some(draft) => super::proposal::card(model, id, draft),
                None => proposal(children, decision.as_ref()),
            }
        }
    }
}

/// The user's message, on the right in a tinted bubble.
fn user(message: &str) -> Element<'_, Message> {
    container(bubble(message)).align_right(Length::Fill).into()
}

/// A prompt someone other than the user sent: a user bubble with `sender` above it.
fn sent_by<'a>(sender: &'a str, message: &'a str) -> Element<'a, Message> {
    let labelled = column![text(sender).size(12).style(dimmed), bubble(message)]
        .spacing(2)
        .align_x(Alignment::End);
    container(labelled).align_right(Length::Fill).into()
}

/// `message` in the tinted bubble of the user's messages.
fn bubble(message: &str) -> Element<'_, Message> {
    container(text(message))
        .padding([8, 12])
        .max_width(BUBBLE_MAX_WIDTH)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.primary.weak.color.into()),
                text_color: Some(palette.primary.weak.text),
                border: Border::default().rounded(10),
                ..container::Style::default()
            }
        })
        .into()
}

/// A child's turn result as its orchestrator received it: who and what, a line for each
/// thing that went differently than a normal turn, then the child's answer.
fn child_result<'a>(
    repository: &RepositoryId,
    title: &str,
    origin: TurnOrigin,
    outcome: &'a ChildOutcome,
    answer: &'a str,
) -> Element<'a, Message> {
    let mut lines = Column::new()
        .spacing(6)
        .push(text(format!("child result · {repository} · {title}")).font(bold()));
    if origin == TurnOrigin::User {
        lines = lines.push(text("turn started by the user").size(12).style(dimmed));
    }
    match outcome {
        ChildOutcome::Failed(error) => {
            lines = lines.push(
                text(format!("failed: {error}"))
                    .size(12)
                    .style(|theme: &Theme| status_style(theme, true)),
            );
        }
        ChildOutcome::Ended(reason) if *reason != StopReason::EndTurn => {
            let word = turn_ended(*reason);
            let word = word.strip_prefix("stopped: ").unwrap_or(word);
            lines = lines.push(text(format!("turn ended: {word}")).size(12).style(dimmed));
        }
        ChildOutcome::Ended(_) => {}
    }
    card(lines.push(text(answer)))
}

/// An orchestrator's proposal of children, read-only: one row per child, then whether the
/// user decided.
fn proposal<'a>(
    children: &'a [ProposedChild],
    decision: Option<&'a Decision>,
) -> Element<'a, Message> {
    let rows = children.iter().map(|child| {
        let heading = row![
            chip(child.repository.as_str()),
            text(child.title.as_str()).font(bold())
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![heading, text(child.prompt.as_str()).size(13).style(dimmed)]
            .spacing(4)
            .into()
    });
    let footer: Element<'a, Message> = match decision {
        None => text("pending").style(dimmed).into(),
        Some(Decision::Approved(started)) => text(format!(
            "approved · {} started",
            children_count(started.len())
        ))
        .style(dimmed)
        .into(),
        Some(Decision::Rejected { note }) => {
            let mut footer = column![text("rejected").style(dimmed)].spacing(4);
            if let Some(note) = note {
                footer = footer.push(text(format!("note: {note}")).style(dimmed));
            }
            footer.into()
        }
    };
    card(
        Column::new()
            .spacing(8)
            .push(text(format!("proposal · {}", children_count(children.len()))).font(bold()))
            .extend(rows)
            .push(footer),
    )
}

/// `count` children in words: "1 child", "2 children".
pub(super) fn children_count(count: usize) -> String {
    if count == 1 {
        "1 child".to_owned()
    } else {
        format!("{count} children")
    }
}

/// `content` in a bordered card across the transcript.
pub(super) fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(12)
        .width(Length::Fill)
        .style(card_style)
        .into()
}

/// The border of the transcript's cards.
fn card_style(theme: &Theme) -> container::Style {
    container::Style {
        border: Border::default()
            .rounded(8)
            .width(1)
            .color(theme.extended_palette().background.strong.color),
        ..container::Style::default()
    }
}

/// The agent's answer as Markdown, or as plain text until it has been parsed.
fn agent<'a>(items: Option<&'a [markdown::Item]>, answer: &'a str) -> Element<'a, Message> {
    let Some(items) = items else {
        return text(answer).into();
    };
    // The style takes only the link color from the palette, and iced's light and dark
    // palettes share their primary color: the answer looks the same under either theme
    // without knowing which one the desktop picked.
    markdown::view(items, markdown::Settings::from(&Theme::Light))
        .map(|uri| Message::Session(SessionMessage::LinkClicked(uri)))
}

/// The agent's reasoning: a "thinking" row that shows the text when expanded.
fn thought(index: usize, thought: &str, expanded: bool) -> Element<'_, Message> {
    let label = if expanded {
        "thinking ▾"
    } else {
        "thinking ▸"
    };
    let toggle = button(text(label).size(13).style(dimmed))
        .padding([2, 6])
        .style(row_style)
        .on_press(toggle(index));
    if !expanded {
        return toggle.into();
    }
    column![toggle, details(text(thought).style(dimmed))]
        .spacing(4)
        .into()
}

/// A tool call: its kind, title and status in a row that shows its output and the files it
/// touches when expanded.
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
        chip(kind_word(kind)),
        text(title).width(Length::Fill),
        text(status_word(status))
            .size(12)
            .style(move |theme: &Theme| status_style(theme, failed)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let header = button(line)
        .width(Length::Fill)
        .padding([4, 6])
        .style(row_style)
        .on_press(toggle(index));
    if !expanded {
        return header.into();
    }

    let output = content.iter().map(tool_content);
    let files = locations.iter().map(|location| {
        text(location.display().to_string())
            .size(12)
            .style(dimmed)
            .into()
    });
    let body = Column::with_children(output.chain(files)).spacing(6);
    column![header, details(body)].spacing(4).into()
}

/// One piece of a tool call's output.
fn tool_content(content: &ToolContent) -> Element<'_, Message> {
    match content {
        ToolContent::Text(output) => text(output).into(),
        ToolContent::Diff { path, old, new } => {
            let mut diff = Column::new()
                .spacing(4)
                .push(text(path.display().to_string()).size(12).style(dimmed));
            if let Some(old) = old {
                diff = diff.push(code(old, |palette| palette.danger.weak.color));
            }
            diff.push(code(new, |palette| palette.success.weak.color))
                .into()
        }
        ToolContent::Terminal(terminal) => text(format!("terminal {terminal}"))
            .size(12)
            .style(dimmed)
            .into(),
    }
}

/// `source` in a monospace block tinted with the color `tint` picks from the palette.
fn code(source: &str, tint: fn(&iced::theme::palette::Extended) -> Color) -> Element<'_, Message> {
    container(text(source).font(Font::MONOSPACE).size(13))
        .padding(8)
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(tint(theme.extended_palette()).scale_alpha(0.3).into()),
            border: Border::default().rounded(6),
            ..container::Style::default()
        })
        .into()
}

/// A permission request: what asks, then a button per option, or the answer once given.
fn permission<'a>(
    id: PermissionId,
    title: &'a str,
    kind: ToolKind,
    options: &'a [PermissionOption],
    answer: Option<&'a Option<PermissionOptionId>>,
) -> Element<'a, Message> {
    let heading = row![chip(kind_word(kind)), text(title).font(bold())]
        .spacing(8)
        .align_y(Alignment::Center);
    let outcome: Element<'a, Message> = match answer {
        None => Row::with_children(options.iter().map(|option| {
            let style = match option.kind {
                PermissionKind::AllowOnce | PermissionKind::AllowAlways => button::primary,
                PermissionKind::RejectOnce | PermissionKind::RejectAlways => button::secondary,
            };
            button(text(option.name.as_str()))
                .style(style)
                .on_press(Message::Session(SessionMessage::AnswerPermission(
                    id,
                    Some(option.id.clone()),
                )))
                .into()
        }))
        .spacing(8)
        .into(),
        Some(chosen) => text(format!("→ {}", answer_name(options, chosen.as_ref())))
            .style(dimmed)
            .into(),
    };
    card(column![heading, outcome].spacing(8))
}

/// The name of the option chosen, its id when the request does not name it, or "cancelled".
fn answer_name(options: &[PermissionOption], chosen: Option<&PermissionOptionId>) -> String {
    let Some(chosen) = chosen else {
        return "cancelled".into();
    };
    options
        .iter()
        .find(|option| &option.id == chosen)
        .map_or_else(|| chosen.to_string(), |option| option.name.clone())
}

/// A thin line across the transcript with `label` in the middle, red for a failure.
fn separator<'a>(label: String, failed: bool) -> Element<'a, Message> {
    let label = text(label)
        .size(12)
        .style(move |theme: &Theme| status_style(theme, failed));
    row![rule::horizontal(1), label, rule::horizontal(1)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// `content` indented under the row it belongs to.
fn details<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(Padding::ZERO.left(DETAILS_INDENT))
        .into()
}

/// The message that expands or collapses the entry at `index`.
fn toggle(index: usize) -> Message {
    Message::Session(SessionMessage::ToggleEntry(index))
}

/// Secondary text, or the theme's danger color when something failed.
fn status_style(theme: &Theme, failed: bool) -> text::Style {
    if failed {
        text::Style {
            color: Some(theme.extended_palette().danger.base.color),
        }
    } else {
        dimmed(theme)
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
fn turn_ended(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end of turn",
        StopReason::MaxTokens => "stopped: token limit reached",
        StopReason::MaxTurnRequests => "stopped: request limit reached",
        StopReason::Refusal => "the agent refused",
        StopReason::Cancelled => "cancelled",
    }
}

/// The default font in italics.
fn italic() -> Font {
    Font {
        style: font::Style::Italic,
        ..Font::DEFAULT
    }
}
