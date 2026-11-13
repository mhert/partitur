//! The flow rail: the row under the breadcrumb of the Session and Env tabs that tells where
//! the session stands in its workflow. It is shown only for a session that reports one, is
//! a fixed row that never scrolls, and is drawn on the panel colour above a one-pixel line.
//!
//! At the left a chip names the workflow (`superpowers`). Then come its five steps, each a
//! dot and a label, joined by short lines. A step before the current one has a dim dot and
//! a muted label; the current step has a dot in the text colour, a label at medium weight
//! and, where it has one, a muted detail after `·`: the plan's size for the plan step, the
//! task being worked on for the execute step and the outcome for the finish step. A step
//! after it has a hollow dot. Clicking a step shows its document on the Spec tab: the
//! design for brainstorming and designing, the plan for the steps after.
//!
//! While a gate waits for the user, the current step's dot and detail turn to the colour of
//! a session that asks the user, the detail reading `· waiting for your approval`, and the
//! right of the rail holds the primary button that approves the gate with its `Enter` key
//! cap. The button is disabled while the gate cannot be approved. Without a gate the right
//! of the rail tells, while the plan is executed, the current task's title and the phase of
//! its test-driven cycle as a pill: `tdd red`, `tdd green` or an outlined `tdd refactor`,
//! followed by an `open plan` link to the plan on the Spec tab once one was reported.

use iced::widget::{Row, Space, button, container, row, text};
use iced::{Alignment, Border, Color, Element, Length, Theme};
use partitur_core::workflow::{execute_detail, plan_summary};
use partitur_core::{Step, TddPhase, Workflow, WorkflowStatus, WorkflowView};

use crate::app::Message;
use crate::ellipsis::keep_start;
use crate::flow::{self, FlowMessage, SpecDoc};
use crate::theme::{self, Tokens};
use crate::{badge, style};

/// The rail's height in logical pixels.
pub(crate) const HEIGHT: f32 = 36.0;

/// The space between the window's edge and the rail's content, left and right.
const MARGIN_X: f32 = 24.0;

/// The space between the workflow's chip and the first step.
const CHIP_GAP: f32 = 18.0;

/// The space between a step and the lines that join it to its neighbours.
const STEP_GAP: f32 = 8.0;

/// The space between a step's dot, its label and its detail.
const PART_GAP: f32 = 6.0;

/// The length of the line that joins two steps.
const JOIN: f32 = 28.0;

/// The side of a step's dot.
const DOT: f32 = 8.0;

/// The corner radius of the workflow's chip.
const CHIP_RADIUS: f32 = 4.0;

/// The most characters of the current task's title the rail shows.
const TITLE_BUDGET: usize = 48;

/// What the current step's detail says while a gate waits.
const WAITING: &str = "· waiting for your approval";

/// Where a step stands against the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Before the current step.
    Done,
    /// The current step.
    Current,
    /// After the current step, or any step before the first.
    Upcoming,
}

/// The flow rail of the workflow `view`, in the colours of `tokens`, the token set of the
/// active theme. Its approve button can be pressed only when `approvable`.
pub fn rail<'a>(view: &WorkflowView, approvable: bool, tokens: &Tokens) -> Element<'a, Message> {
    let status = &view.status;
    let steps = container(steps(status, tokens)).width(Length::Fill);
    let line = row![
        chip(status.workflow, tokens),
        steps,
        unit_of_work(view, approvable, tokens)
    ]
    .align_y(Alignment::Center);
    let panel = tokens.panel;
    let rail = container(line)
        .width(Length::Fill)
        .height(HEIGHT)
        .padding([0.0, MARGIN_X])
        .align_y(Alignment::Center)
        .style(move |_: &Theme| container::Style {
            background: Some(panel.into()),
            ..container::Style::default()
        });
    iced::widget::column![rail, style::hairline(|tokens| tokens.border)].into()
}

/// The chip that names `workflow`, with the room that parts it from the steps.
fn chip<'a>(workflow: Workflow, tokens: &Tokens) -> Element<'a, Message> {
    let name = match workflow {
        Workflow::Superpowers => "superpowers",
    };
    let border = tokens.border;
    let chip = container(
        text(name)
            .size(theme::TINY)
            .font(theme::MONO)
            .color(tokens.muted),
    )
    .padding([1, 8])
    .style(move |_: &Theme| container::Style {
        border: Border::default()
            .rounded(CHIP_RADIUS)
            .width(1)
            .color(border),
        ..container::Style::default()
    });
    container(chip)
        .padding(iced::Padding::ZERO.right(CHIP_GAP))
        .into()
}

/// The workflow's steps, joined by lines, each drawn as [`Place`] says against the current
/// step of `status`.
fn steps<'a>(status: &WorkflowStatus, tokens: &Tokens) -> Row<'a, Message> {
    let current = status
        .step
        .and_then(|step| Step::ALL.iter().position(|each| *each == step));
    let mut line = Row::new().spacing(STEP_GAP).align_y(Alignment::Center);
    for (index, step) in Step::ALL.into_iter().enumerate() {
        if index > 0 {
            line = line.push(join(tokens));
        }
        let place = match current {
            Some(current) if index < current => Place::Done,
            Some(current) if index == current => Place::Current,
            _ => Place::Upcoming,
        };
        line = line.push(step_view(status, step, place, tokens));
    }
    line
}

/// One step: its dot, its label and, for the current step, its detail, as a button that
/// shows the step's document on the Spec tab.
fn step_view<'a>(
    status: &WorkflowStatus,
    step: Step,
    place: Place,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let label = text(step.label()).size(theme::SMALL);
    let parts = Row::new().spacing(PART_GAP).align_y(Alignment::Center);
    let parts = match place {
        Place::Done => parts.push(dot(tokens.dim)).push(label.color(tokens.muted)),
        Place::Upcoming => parts
            .push(hollow(tokens.dim))
            .push(label.color(tokens.muted)),
        Place::Current if status.gate.is_some() => parts
            .push(dot(tokens.asks_you.fg))
            .push(label.font(theme::UI_MEDIUM).color(tokens.text))
            .push(text(WAITING).size(theme::SMALL).color(tokens.asks_you.fg)),
        Place::Current => {
            let parts = parts
                .push(dot(tokens.text))
                .push(label.font(theme::UI_MEDIUM).color(tokens.text));
            match detail(status, step) {
                Some(detail) => parts
                    .push(text("·").size(theme::SMALL).color(tokens.muted))
                    .push(text(detail).size(theme::SMALL).color(tokens.muted)),
                None => parts,
            }
        }
    };
    button(parts)
        .padding(0)
        .style(style::link_button)
        .on_press(Message::Flow(FlowMessage::Open(flow::spec_doc(step))))
        .into()
}

/// What the current `step` of `status` adds to its label: the plan's size, the task being
/// executed or the outcome; `None` for the other steps and before there is anything to say.
fn detail(status: &WorkflowStatus, step: Step) -> Option<String> {
    match step {
        Step::Plan => status.tasks.as_ref().map(plan_summary),
        Step::Execute => status.tasks.as_ref().and_then(execute_detail),
        Step::Finish => status.outcome.map(|outcome| outcome.label().to_owned()),
        Step::Brainstorm | Step::Design => None,
    }
}

/// The line that joins two steps.
fn join<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let border = tokens.border;
    container(Space::new())
        .width(JOIN)
        .height(1)
        .style(move |_: &Theme| container::Style {
            background: Some(border.into()),
            ..container::Style::default()
        })
        .into()
}

/// A filled dot in `colour`.
fn dot<'a>(colour: Color) -> Element<'a, Message> {
    container(Space::new())
        .width(DOT)
        .height(DOT)
        .style(move |_: &Theme| container::Style {
            background: Some(colour.into()),
            border: Border::default().rounded(DOT / 2.0),
            ..container::Style::default()
        })
        .into()
}

/// A hollow dot: only its outline, in `colour`.
fn hollow<'a>(colour: Color) -> Element<'a, Message> {
    container(Space::new())
        .width(DOT)
        .height(DOT)
        .style(move |_: &Theme| container::Style {
            border: Border::default().rounded(DOT / 2.0).width(1).color(colour),
            ..container::Style::default()
        })
        .into()
}

/// The right of the rail: the button that approves a waiting gate, enabled when
/// `approvable`, or else while the plan is executed the current task's title, its phase and
/// the link to the plan.
fn unit_of_work<'a>(
    view: &WorkflowView,
    approvable: bool,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let status = &view.status;
    if let Some(gate) = &status.gate {
        let caption = row![
            text(gate.kind.approve_label()).size(theme::SMALL),
            badge::key_cap_on_primary("Enter"),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        return button(caption)
            .padding([3, 10])
            .style(style::primary_button)
            .on_press_maybe(approvable.then_some(Message::Flow(FlowMessage::Approve)))
            .into();
    }
    let mut work = Row::new().spacing(10).align_y(Alignment::Center);
    if status.step != Some(Step::Execute) {
        return work.into();
    }
    if let Some(task) = view.current_task() {
        work = work.push(
            text(keep_start(&task.title, TITLE_BUDGET))
                .size(theme::SMALL)
                .color(tokens.text_soft),
        );
    }
    if let Some(phase) = status.tasks.as_ref().and_then(|tasks| tasks.phase) {
        work = work.push(phase_pill(phase));
    }
    if status.plan.is_some() {
        work = work.push(
            button(text("open plan").size(theme::SMALL))
                .padding(0)
                .style(style::link_button)
                .on_press(Message::Flow(FlowMessage::Open(SpecDoc::Plan))),
        );
    }
    work.into()
}

/// The pill of a task's test-driven `phase`.
fn phase_pill<'a>(phase: TddPhase) -> Element<'a, Message> {
    let label = format!("tdd {}", phase.label());
    match phase {
        TddPhase::Red => badge::pill(label, |tokens| tokens.tdd_red),
        TddPhase::Green => badge::pill(label, |tokens| tokens.running),
        TddPhase::Refactor => badge::outlined(label),
    }
}
