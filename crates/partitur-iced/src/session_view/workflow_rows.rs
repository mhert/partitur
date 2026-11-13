//! The rows the agent's workflow reports leave in the transcript. Starting a task is a row
//! of its own with the task's title and files, each phase of its test-driven cycle shows
//! its pill and the agent's remark, and every other report is a small `step` row with a note
//! that tells what happened, such as the user approving a plan at a local time.

use iced::widget::{Row, container, row as line_of, text};
use iced::{Alignment, Element, Length};
use partitur_core::{PlannedTask, SuperpowersEvent, TddPhase, WorkflowEvent, workflow::step_note};

use super::entries::{dot, event_row, label};
use crate::app::Message;
use crate::flow_rail;
use crate::theme;
use crate::{clock, style};

/// The text size of a step row.
const STEP_SIZE: f32 = 12.0;

/// The gap between the parts of a step row.
const STEP_GAP: f32 = 8.0;

/// The row for `event`, reported at `at` seconds after the Unix epoch, with the plan's
/// `task` it is about when the plan names it.
pub(super) fn row<'a>(
    at: u64,
    event: &'a WorkflowEvent,
    task: Option<&'a PlannedTask>,
) -> Element<'a, Message> {
    match event {
        WorkflowEvent::Superpowers(SuperpowersEvent::StartTask { n, note }) => {
            task_start(*n, task, note.as_deref())
        }
        WorkflowEvent::Superpowers(
            SuperpowersEvent::WriteFailingTest { note, .. }
            | SuperpowersEvent::MakeTestPass { note, .. }
            | SuperpowersEvent::StartRefactoring { note, .. },
        ) => phase(phase_of(event), note.as_deref()),
        _ => step(step_note(event, &clock::local_time(at))),
    }
}

/// The phase of the test-driven cycle `event` reports, refactoring for any other event.
fn phase_of(event: &WorkflowEvent) -> TddPhase {
    match event {
        WorkflowEvent::Superpowers(SuperpowersEvent::WriteFailingTest { .. }) => TddPhase::Red,
        WorkflowEvent::Superpowers(SuperpowersEvent::MakeTestPass { .. }) => TddPhase::Green,
        _ => TddPhase::Refactor,
    }
}

/// The row of task `n` starting: the task's title and files when `task` is known, then
/// the agent's `note`.
fn task_start<'a>(
    n: u32,
    task: Option<&'a PlannedTask>,
    note: Option<&str>,
) -> Element<'a, Message> {
    let mut line = line_of![label(format!("task {n}"))];
    if let Some(task) = task {
        line = line.push(text(task.title.as_str()).line_height(super::entries::LINE));
        if !task.files.is_empty() {
            line = line.push(dot()).push(
                text(task.files.join(", "))
                    .font(theme::MONO)
                    .size(theme::SMALL)
                    .style(style::soft),
            );
        }
    }
    event_row(with_note(line, note), None)
}

/// The row of a `phase` of the test-driven cycle: its pill and the agent's `note`.
fn phase<'a>(phase: TddPhase, note: Option<&str>) -> Element<'a, Message> {
    let line = line_of![label("tdd"), flow_rail::tinted_pill(phase, phase.label())];
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
