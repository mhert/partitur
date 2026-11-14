//! The rows the agent's workflow reports leave in the transcript, drawn from the row the
//! workflow describes for a report. Starting a unit of work is a row of its own with its
//! label, its title and files, a phase of it shows its label, its pill and the agent's
//! remark, a persona taking up the work shows the persona, what it works on and the file it
//! works from, and every report without a row of its own is a small `step` row with a note
//! that tells what happened, such as the user approving a plan at a local time.

use iced::widget::{Row, container, row as line_of, text};
use std::path::Path;

use iced::{Alignment, Element, Length};
use partitur_core::{PhasePill, WorkflowEvent, WorkflowRow, workflow::step_note};

use super::entries::{dot, event_row, label};
use crate::app::Message;
use crate::flow_rail;
use crate::theme;
use crate::{clock, style};

/// The text size of a step row.
const STEP_SIZE: f32 = 12.0;

/// The gap between the parts of a step row.
const STEP_GAP: f32 = 8.0;

/// The row for `event`, reported at `at` seconds after the Unix epoch: the `row` the
/// workflow describes for it, or else a `step` row with the event's note.
pub(super) fn row<'a>(
    at: u64,
    event: &'a WorkflowEvent,
    row: Option<&'a WorkflowRow>,
) -> Element<'a, Message> {
    match row {
        Some(WorkflowRow::Task {
            label,
            title,
            files,
            note,
        }) => task_start(label, title.as_deref(), files, note.as_deref()),
        Some(WorkflowRow::Phase { label, pill, note }) => phase(label, pill, note.as_deref()),
        Some(WorkflowRow::Persona {
            persona,
            text,
            path,
        }) => persona_start(persona, text, path.as_deref()),
        None => step(step_note(event, &clock::local_time(at))),
    }
}

/// The row of a unit of work starting: its `label`, its `title` and `files` when known,
/// then the agent's `note`.
fn task_start<'a>(
    label_text: &'a str,
    title: Option<&'a str>,
    files: &[String],
    note: Option<&str>,
) -> Element<'a, Message> {
    let mut line = line_of![label(label_text)];
    if let Some(title) = title {
        line = line.push(text(title).line_height(super::entries::LINE));
    }
    if !files.is_empty() {
        line = line.push(dot()).push(
            text(files.join(", "))
                .font(theme::MONO)
                .size(theme::SMALL)
                .style(style::soft),
        );
    }
    event_row(with_note(line, note), None)
}

/// The row of a persona taking up the work: the `persona`, what it works on in `text`, then
/// the `path` of the file it works from when known.
fn persona_start<'a>(
    persona: &'a str,
    text_of: &'a str,
    path: Option<&Path>,
) -> Element<'a, Message> {
    let mut line = line_of![
        label(persona),
        text(text_of).line_height(super::entries::LINE)
    ];
    if let Some(path) = path {
        line = line.push(dot()).push(
            text(path.display().to_string())
                .font(theme::MONO)
                .size(theme::SMALL)
                .style(style::soft),
        );
    }
    event_row(line, None)
}

/// The row of a phase: its `label`, its `pill` and the agent's `note`.
fn phase<'a>(label_text: &'a str, pill: &PhasePill, note: Option<&str>) -> Element<'a, Message> {
    let line = line_of![label(label_text), flow_rail::tinted_pill(pill)];
    event_row(with_note(line, note), None)
}

/// A small row on the panel: `step` and the `note` in muted text, nothing when there is
/// no note.
fn step<'a>(note: Option<String>) -> Element<'a, Message> {
    let Some(note) = note else {
        return Row::new().into();
    };
    let line = line_of![
        text("step").size(STEP_SIZE).style(style::dim),
        text(note).size(STEP_SIZE).style(style::muted)
    ]
    .spacing(STEP_GAP)
    .align_y(Alignment::Center);
    container(line)
        .padding([6, 12])
        .width(Length::Fill)
        .style(style::fill(|tokens| tokens.panel))
        .into()
}

/// `line` with the agent's `note` at its end, in muted text, when there is one.
fn with_note<'a>(line: Row<'a, Message>, note: Option<&str>) -> Row<'a, Message> {
    match note {
        Some(note) => line.push(
            text(note.to_owned())
                .line_height(super::entries::LINE)
                .style(style::muted),
        ),
        None => line,
    }
}
