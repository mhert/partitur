//! An orchestrator's transcript from the first decided proposal on, drawn as a timeline.
//!
//! Every entry is a row behind a gutter. The gutter holds a dot for what moved the work
//! on, and a line that joins the dots: the user's decision on a proposal, a child's result
//! (the child's message in a block that is cut when it is long), the user's own prompt, a
//! prompt Partitur wrote, and in the orchestrator's colour what the orchestrator said and
//! which child it prompted with what. What its agent did in between (thoughts, tool calls,
//! permission requests, notices, the end of a turn) keeps its usual row without a dot,
//! and the line runs past it. An undecided proposal keeps its split card.
//!
//! The last row, behind a hollow dot, tells what is pending now: which children are in a
//! turn and which wait for the user. It is left out when nothing is pending.
//!
//! The timeline's rows join the conversation's rows of the transcript, so only those near
//! the viewport are laid out, like the conversation's.

use std::borrow::Cow;

use iced::widget::{Column, Row, Space, column, container, markdown, row, text};
use iced::{Border, Element, Length, Padding, Theme};
use partitur_core::{
    ChildOutcome, Decision, Entry, RepositoryId, SessionId, SessionViewModel, Status, StopReason,
    TurnOrigin,
};

use super::entries::{self, ENTRY_GAP, Ink, LINE, PART_GAP, TranscriptRow};
use super::window::RowKey;
use super::{SessionMessage, SessionState, child_name, repository_name};
use crate::app::Message;
use crate::ellipsis::keep_start;
use crate::projects_view::ProjectsMessage;
use crate::selectable;
use crate::{style, theme};

/// The width of the gutter the dots and the line sit in, in logical pixels.
const GUTTER: f32 = 16.0;

/// The space between the gutter and a row's content.
const GUTTER_GAP: f32 = 10.0;

/// The space between two rows of the timeline.
const ROW_GAP: f32 = 14.0;

/// The side of a dot.
const DOT: f32 = 8.0;

/// How far a dot sits below its row's top: level with the middle of a first line of prose.
const DOT_TOP: f32 = 6.0;

/// How much lower the first line of an event row sits than a line of prose: by the row's
/// border and padding.
const FRAME: f32 = 9.0;

/// The space between a dot and the line that leaves it downwards.
const DOT_GAP: f32 = 4.0;

/// The space between the words of a sentence that is set from several pieces: about the
/// width of a blank in prose.
const WORD_GAP: f32 = 4.0;

/// The most lines of a child's message its block shows before it expands.
const REPORT_LINES: usize = 6;

/// The most characters of a child's message its block shows before it expands.
const REPORT_BUDGET: usize = 480;

/// The most characters of a prompt the orchestrator sent a child its row shows.
const PROMPTED_BUDGET: usize = 72;

/// What a row's gutter holds besides the line.
#[derive(Clone, Copy)]
enum Mark {
    /// A dot in the colour picked from the tokens: the row moved the work on.
    Dot(Ink),
    /// Such a dot beside an event row, level with the line inside the row's frame.
    FramedDot(Ink),
    /// A hollow dot: the row tells what is pending now.
    Ring,
    /// Nothing: the line runs past the row.
    Nothing,
}

/// The colour of the dot of a step the user, a child or Partitur took.
const STEP: Mark = Mark::Dot(|tokens| tokens.muted_strong);

/// The colour of the dot of what the orchestrator did by itself.
const BY_ORCHESTRATOR: Mark = Mark::Dot(|tokens| tokens.orchestrator.fg);

/// One word of a list of names set in a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// A name, with the punctuation that follows it.
    Name(String),
    /// The `and` before the last name.
    And,
}

/// Where the timeline starts in `entries`: at the first proposal the user decided on.
/// `None` while there is none.
pub(super) fn start(entries: &[Entry]) -> Option<usize> {
    entries.iter().position(|entry| {
        matches!(
            entry,
            Entry::Proposal {
                decision: Some(_),
                ..
            }
        )
    })
}

/// The rows of the timeline of the session `model`: its entries from the one at `start` on,
/// then what is pending now. The first row keeps the transcript's gap to the conversation
/// above it; the others have none, since each row ends in the gap to the next.
pub(super) fn rows<'a>(
    model: &SessionViewModel<'a>,
    start: usize,
    state: &'a SessionState,
    answers: &markdown::Settings,
) -> Vec<TranscriptRow<'a>> {
    let entries: &'a [Entry] = model.entries.get(start..).unwrap_or_default();
    let mut rows: Vec<(RowKey, &'a [Entry], Mark, Element<'a, Message>)> =
        entries::runs(start, entries)
            .map(|(first, run)| {
                let (mark, content) = match run {
                    [entry] => self::entry(model, first, entry, state, answers),
                    calls => (Mark::Nothing, entries::tools(model, first, calls, state)),
                };
                (RowKey::Timeline(first), run, mark, content)
            })
            .collect();
    if let Some(pending) = now(model) {
        rows.push((RowKey::Pending, &[], Mark::Ring, pending));
    }
    let last = rows.len().saturating_sub(1);
    rows.into_iter()
        .enumerate()
        .map(|(position, (key, run, mark, content))| TranscriptRow {
            key,
            gap: if position == 0 { ENTRY_GAP } else { 0.0 },
            run,
            open: entries::open(run),
            element: timeline_row(mark, content, position == last),
        })
        .collect()
}

/// The mark and the content of the row of `entry`, the entry at `index` of the session's
/// transcript.
fn entry<'a>(
    model: &SessionViewModel<'_>,
    index: usize,
    entry: &'a Entry,
    state: &'a SessionState,
    answers: &markdown::Settings,
) -> (Mark, Element<'a, Message>) {
    let session = &model.id;
    let expanded = || state.expanded.contains(&(session.clone(), index));
    match entry {
        Entry::User(message) => (STEP, you(message)),
        Entry::Agent(answer) => (
            BY_ORCHESTRATOR,
            entries::answer(state.markdown.items(session, index), answer, answers),
        ),
        Entry::PartiturPrompt(message) => (
            Mark::FramedDot(|tokens| tokens.dim),
            entries::excerpt_row("Partitur", index, message, expanded()),
        ),
        Entry::ChildPrompted { .. } => (
            Mark::FramedDot(|tokens| tokens.orchestrator.fg),
            entries::entry(model, index, entry, state, answers),
        ),
        Entry::ChildResult { .. }
        | Entry::Proposal {
            decision: Some(_), ..
        } => (STEP, entries::entry(model, index, entry, state, answers)),
        _ => (
            Mark::Nothing,
            entries::entry(model, index, entry, state, answers),
        ),
    }
}

/// One row of the timeline: the gutter with `mark` and, unless the row is the `last`, the
/// line down to the next row, then `content`.
fn timeline_row(mark: Mark, content: Element<'_, Message>, last: bool) -> Element<'_, Message> {
    // The line fills the gutter's height, which is the row's: the content's and, below it,
    // the gap to the next row.
    let line = || {
        container(Space::new())
            .width(1)
            .height(Length::Fill)
            .style(style::fill(|tokens| tokens.border))
    };
    let mut gutter = Column::new().width(GUTTER).align_x(iced::Alignment::Center);
    match mark {
        Mark::Nothing => {}
        Mark::Dot(ink) => gutter = gutter.push(dot(ink, false, DOT_TOP)),
        Mark::FramedDot(ink) => gutter = gutter.push(dot(ink, false, DOT_TOP + FRAME)),
        Mark::Ring => gutter = gutter.push(dot(|tokens| tokens.running.fg, true, DOT_TOP)),
    }
    if !last {
        gutter = gutter.push(line());
    }
    let gap = if last { 0.0 } else { ROW_GAP };
    row![
        gutter,
        container(content)
            .width(Length::Fill)
            .padding(Padding::ZERO.bottom(gap)),
    ]
    .spacing(GUTTER_GAP)
    .into()
}

/// A dot of the gutter in the colour `ink` picks from the tokens, `top` below the row's
/// top, which puts it level with the row's first line, and with the room below it that
/// parts it from the line: filled, or only a ring when `hollow`.
fn dot<'a>(ink: Ink, hollow: bool, top: f32) -> Element<'a, Message> {
    let dot = container(Space::new())
        .width(DOT)
        .height(DOT)
        .style(move |theme: &Theme| {
            let color = ink(theme::tokens(theme));
            let border = Border::default().rounded(DOT / 2.0);
            if hollow {
                container::Style {
                    border: border.width(1).color(color),
                    ..container::Style::default()
                }
            } else {
                container::Style {
                    background: Some(color.into()),
                    border,
                    ..container::Style::default()
                }
            }
        });
    container(dot)
        .padding(Padding::ZERO.top(top).bottom(DOT_GAP))
        .into()
}

/// A sentence set from several pieces, which flow onto further lines like one text.
fn sentence<'a>() -> Row<'a, Message> {
    Row::new().spacing(WORD_GAP)
}

/// Words of a sentence in the colour `ink` picks from the tokens.
fn words<'a>(words: impl text::IntoFragment<'a>, ink: Ink) -> Element<'a, Message> {
    text(words).line_height(LINE).style(style::ink(ink)).into()
}

/// The colour of what a step's sentence says.
const SAID: Ink = |tokens| tokens.muted_strong;

/// The colour of the names a step's sentence is about.
const NAMED: Ink = |tokens| tokens.text_soft;

/// `names` as a list in words (`a`, `a and b`, `a, b and c`), the last name followed by
/// `end`. The punctuation stays with its name, so a line never starts with it.
fn list(names: &[String], end: &str) -> Vec<Piece> {
    let last = names.len().saturating_sub(1);
    let mut pieces = Vec::new();
    for (position, name) in names.iter().enumerate() {
        let after = if position == last {
            end
        } else if position.saturating_add(1) == last {
            ""
        } else {
            ","
        };
        if position == last && position > 0 {
            pieces.push(Piece::And);
        }
        pieces.push(Piece::Name(format!("{name}{after}")));
    }
    pieces
}

/// The `pieces` as one text.
fn spelled(pieces: &[Piece]) -> String {
    let words: Vec<&str> = pieces
        .iter()
        .map(|piece| match piece {
            Piece::Name(name) => name.as_str(),
            Piece::And => "and",
        })
        .collect();
    words.join(" ")
}

/// Adds the `pieces` to `line`: the names in the colour `named` picks from the tokens, the
/// `and` between them in the colour `said` picks.
fn push_list(
    line: Row<'_, Message>,
    pieces: Vec<Piece>,
    named: Ink,
    said: Ink,
) -> Row<'_, Message> {
    pieces.into_iter().fold(line, |line, piece| {
        line.push(match piece {
            Piece::Name(name) => words(name, named),
            Piece::And => words("and", said),
        })
    })
}

/// The user's decision on a proposal: that the split was approved and in which
/// repositories children started, or that it was rejected, with the note when there is
/// one.
pub(super) fn decided<'a>(
    model: &SessionViewModel<'_>,
    decision: &'a Decision,
) -> Element<'a, Message> {
    match decision {
        Decision::Approved(started) if started.is_empty() => words("You approved the split.", SAID),
        Decision::Approved(started) => {
            let names: Vec<String> = started
                .iter()
                .map(|child| repository_name(model, &child.repository))
                .collect();
            let line = sentence().push(words("You approved the split. Started", SAID));
            push_list(line, list(&names, "."), NAMED, SAID)
                .wrap()
                .into()
        }
        Decision::Rejected { note } => {
            let mut lines = column![words("You rejected the split.", SAID)];
            if let Some(note) = note {
                lines = lines.push(words(note.as_str(), NAMED));
            }
            lines.into()
        }
    }
}

/// A child's turn as its orchestrator received it, the entry at `index` of the transcript:
/// the repository the child works in and that it reported back, or that it failed and
/// why; a line for each thing that went differently than in a turn the orchestrator
/// started and the agent ended; then the child's message `answer` in a block, cut to its
/// beginning unless `expanded`.
pub(super) fn child_result<'a>(
    model: &SessionViewModel<'_>,
    index: usize,
    repository: &RepositoryId,
    origin: TurnOrigin,
    outcome: &'a ChildOutcome,
    answer: &'a str,
    expanded: bool,
) -> Element<'a, Message> {
    let name = repository_name(model, repository);
    let note = |note: String| text(note).size(theme::SMALL).style(style::muted);
    let mut heading = Column::new().spacing(2);
    heading = heading.push(match outcome {
        ChildOutcome::Failed(error) => {
            words(format!("{name} failed: {error}"), |tokens| tokens.failed.fg)
        }
        ChildOutcome::Ended(_) => sentence()
            .push(words(name, NAMED))
            .push(words("reported back:", SAID))
            .wrap()
            .into(),
    });
    match origin {
        TurnOrigin::User => heading = heading.push(note("turn started by the user".to_owned())),
        TurnOrigin::Agent => heading = heading.push(note("turn started by the agent".to_owned())),
        TurnOrigin::Orchestrator => {}
    }
    if let ChildOutcome::Ended(reason) = outcome
        && *reason != StopReason::EndTurn
    {
        let word = entries::turn_ended(*reason);
        let word = word.strip_prefix("stopped: ").unwrap_or(word);
        heading = heading.push(note(format!("turn ended: {word}")));
    }
    column![heading, report(index, answer, expanded)]
        .spacing(8)
        .into()
}

/// The block with a child's `message`, one text per line. A message beyond the block's
/// budget shows its beginning, with a button that expands the block to the whole message
/// and collapses it again.
fn report(index: usize, message: &str, expanded: bool) -> Element<'_, Message> {
    let toggle = Message::Session(SessionMessage::ToggleEntry(index));
    let (shown, control) = match excerpt(message) {
        Some(beginning) if !expanded => (
            Cow::Owned(beginning),
            Some(entries::control("expand", toggle)),
        ),
        Some(_) => (
            Cow::Borrowed(message),
            Some(entries::control("collapse", toggle)),
        ),
        None => (Cow::Borrowed(message), None),
    };
    let lines = shown.lines().filter(|line| !line.trim().is_empty());
    let lines = lines.map(|line| {
        selectable::text(line.to_owned())
            .font(theme::MONO)
            .size(theme::SMALL)
            .line_height(LINE)
            .style(style::soft)
            .into()
    });
    let mut block = row![Column::with_children(lines).spacing(2).width(Length::Fill)];
    if let Some(control) = control {
        block = block.push(control);
    }
    container(block.spacing(PART_GAP))
        .padding([8, 12])
        .width(Length::Fill)
        .style(style::event_row)
        .into()
}

/// How a child's `message` begins when it is more than its block shows: its first
/// [`REPORT_LINES`] lines that are not blank, within [`REPORT_BUDGET`] characters, ending
/// in `…`. `None` for a message that fits whole.
fn excerpt(message: &str) -> Option<String> {
    let lines: Vec<&str> = message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let kept = lines
        .iter()
        .take(REPORT_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let mut beginning = keep_start(&kept, REPORT_BUDGET);
    let lines_left_out = lines.len() > REPORT_LINES;
    if beginning == kept {
        if !lines_left_out {
            return None;
        }
        // The lines kept are whole; a line of its own stands for the rest.
        beginning.push_str("\n…");
    }
    Some(beginning)
}

/// A prompt the orchestrator sent its child `child`: the repository the child works in,
/// how the prompt begins, and a link that opens the child.
pub(super) fn child_prompted<'a>(
    model: &SessionViewModel<'_>,
    child: &SessionId,
    prompt: &str,
) -> Element<'a, Message> {
    let name = model
        .children
        .iter()
        .find(|node| &node.id == child)
        .map_or_else(|| child.to_string(), |node| child_name(node).to_owned());
    let open = Message::Projects(ProjectsMessage::Select(child.clone()));
    let line = row![
        entries::label("prompted"),
        text(name).line_height(LINE),
        entries::dot(),
        entries::prose(one_line(prompt, PROMPTED_BUDGET)).width(Length::Fill),
        entries::control("open", open),
    ];
    entries::event_row(line, None)
}

/// `words` on one line, cut after `budget` characters.
fn one_line(words: &str, budget: usize) -> String {
    let line = words.split_whitespace().collect::<Vec<_>>().join(" ");
    keep_start(&line, budget)
}

/// The user's own prompt: `You:` and what the user wrote.
fn you(message: &str) -> Element<'_, Message> {
    row![
        words("You:", |tokens| tokens.muted),
        selectable::text(message)
            .line_height(LINE)
            .width(Length::Fill),
    ]
    .spacing(6)
    .into()
}

/// What is pending now: the children that run (`Waiting for a and b.`) and the children
/// that wait for the user (`c asks you.`, the names in the colour of that state), in the
/// order the children were started. `None` when no child is in a turn.
fn now<'a>(model: &SessionViewModel<'_>) -> Option<Element<'a, Message>> {
    // The view model lists the newest child first.
    let named = |status: Status| -> Vec<String> {
        model
            .children
            .iter()
            .rev()
            .filter(|child| child.status == status)
            .map(|child| child_name(child).to_owned())
            .collect()
    };
    let (running, asking) = (named(Status::Running), named(Status::WaitingForYou));
    if running.is_empty() && asking.is_empty() {
        return None;
    }
    let quiet: Ink = |tokens| tokens.muted;
    let mut line = sentence();
    if !running.is_empty() {
        line = line.push(words(waiting_for(&running), quiet));
    }
    if !asking.is_empty() {
        line = push_list(line, list(&asking, ""), |tokens| tokens.asks_you.fg, quiet);
        line = line.push(words(asks_you(asking.len()), quiet));
    }
    Some(line.wrap().into())
}

/// The sentence that names the children in `running` as what the orchestrator waits for.
fn waiting_for(running: &[String]) -> String {
    format!("Waiting for {}", spelled(&list(running, ".")))
}

/// How the sentence about `count` children that wait for the user ends.
fn asks_you(count: usize) -> &'static str {
    if count == 1 { "asks you." } else { "ask you." }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use partitur_core::{ProposalId, ProposedChild};

    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn name(name: &str) -> Piece {
        Piece::Name(name.to_owned())
    }

    #[test]
    fn names_are_listed_in_words_with_the_punctuation_on_the_name() {
        assert_eq!(list(&[], "."), []);
        assert_eq!(list(&names(&["a"]), "."), [name("a.")]);
        assert_eq!(
            list(&names(&["a", "b"]), "."),
            [name("a"), Piece::And, name("b.")]
        );
        assert_eq!(
            list(&names(&["a", "b", "c", "d"]), ""),
            [name("a,"), name("b,"), name("c"), Piece::And, name("d")]
        );
        assert_eq!(spelled(&list(&names(&["a", "b", "c"]), ".")), "a, b and c.");
    }

    #[test]
    fn the_pending_sentences_name_the_children() {
        assert_eq!(waiting_for(&names(&["web"])), "Waiting for web.");
        assert_eq!(
            waiting_for(&names(&["api", "web", "infra"])),
            "Waiting for api, web and infra."
        );
        assert_eq!(asks_you(1), "asks you.");
        assert_eq!(asks_you(2), "ask you.");
    }

    #[test]
    fn a_message_within_the_block_has_no_excerpt() {
        assert_eq!(excerpt(""), None);
        assert_eq!(excerpt("one\n\ntwo"), None);
        let lines = ["line"; REPORT_LINES].join("\n");
        assert_eq!(excerpt(&lines), None);
        // Blank lines are not drawn, so they do not count.
        assert_eq!(excerpt(&lines.replace('\n', "\n\n")), None);
    }

    #[test]
    fn a_longer_message_begins_with_its_first_lines() {
        let lines = ["line"; REPORT_LINES + 1].join("\n\n");
        let kept = ["line"; REPORT_LINES].join("\n");
        assert_eq!(excerpt(&lines), Some(format!("{kept}\n…")));

        let long = "é".repeat(REPORT_BUDGET + 1);
        assert_eq!(
            excerpt(&long),
            Some(format!("{}…", "é".repeat(REPORT_BUDGET)))
        );
    }

    #[test]
    fn a_prompt_is_quoted_on_one_line_within_its_budget() {
        assert_eq!(one_line("Add\n  the   endpoint", 40), "Add the endpoint");
        assert_eq!(one_line("Add the endpoint", 7), "Add the…");
    }

    #[test]
    fn the_timeline_starts_at_the_first_decided_proposal() {
        let proposal = |decision| Entry::Proposal {
            id: ProposalId::new(),
            children: Vec::<ProposedChild>::new(),
            decision,
        };
        let rejected = Decision::Rejected { note: None };

        assert_eq!(start(&[]), None);
        assert_eq!(start(&[Entry::User("go".into()), proposal(None)]), None);
        assert_eq!(
            start(&[
                Entry::User("go".into()),
                proposal(None),
                proposal(Some(rejected.clone())),
                proposal(Some(rejected)),
            ]),
            Some(2)
        );
    }
}
