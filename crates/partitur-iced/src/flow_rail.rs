//! The flow rail: the row under the breadcrumb of the Session, Spec and Env tabs that tells
//! where the session stands in its workflow. It is shown only for a session that reports
//! one, is a fixed row that never scrolls, and is drawn on the panel colour above a
//! one-pixel line.
//!
//! The rail draws the workflow's display data and knows no workflow of its own. At the left
//! a chip names the workflow (such as `superpowers 7`). Then come its steps, each a dot and a
//! label, joined by short lines. A step before the current one has a dim dot and a muted
//! label; the current step has a dot in the text colour, a label at medium weight and, where
//! the workflow gives one, a muted detail after `·`. A step after it has a hollow dot.
//! Clicking a step shows the document the workflow names for it on the Spec tab. Steps wider
//! than the room left of the rail's right side are cut there, never drawn over it.
//!
//! A loop sits in the chain like a step: a rounded box with a one-pixel border, dim before and
//! after the current step and in the text colour while it is the current one. Inside it come
//! its muted label, such as `story 3 of 8`, and its phases joined by arrows, the phase going on
//! at medium weight in the text colour and the others muted.
//!
//! While a gate waits for the user, the current step's dot and detail turn to the colour of
//! a session that asks the user, the detail reading `· waiting for your approval`, and the
//! right of the rail holds the primary button that approves the gate with its `Enter` key
//! cap. The button is disabled while the gate cannot be approved. Without a gate the right
//! of the rail tells the unit of work going on, as far as the workflow names it: who works
//! on it, its title, its phase as a pill (red, green, or outlined in a neutral tone) and a
//! link that shows a document on the Spec tab.

use iced::widget::{Row, Space, button, container, row, text};
use iced::{Alignment, Border, Color, Element, Length, Theme};
use partitur_core::{PhasePill, RailRight, RailStep, StepState, Tone, WorkflowView};

use crate::app::Message;
use crate::ellipsis::keep_start;
use crate::flow::FlowMessage;
use crate::theme::{self, Tokens};
use crate::{badge, style};

/// The rail's height in logical pixels.
pub(crate) const HEIGHT: f32 = 36.0;

/// The space between the window's edge and the rail's content, left and right.
const MARGIN_X: f32 = 24.0;

/// The space between the workflow's chip and the first step.
const CHIP_GAP: f32 = 18.0;

/// The least space between the last step and the right of the rail.
const RIGHT_GAP: f32 = 16.0;

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

/// The corner radius of a loop's box.
const LOOP_RADIUS: f32 = 6.0;

/// The most characters of the current task's title the rail shows.
const TITLE_BUDGET: usize = 48;

/// What the current step's detail says while a gate waits.
const WAITING: &str = "· waiting for your approval";

/// The flow rail of the workflow `view`, in the colours of `tokens`, the token set of the
/// active theme. Its approve button can be pressed only when `approvable`.
pub fn rail<'a>(view: &WorkflowView, approvable: bool, tokens: &Tokens) -> Element<'a, Message> {
    let rail = &view.rail;
    let waiting = matches!(rail.right, RailRight::Gate { .. });
    // Steps wider than their room are cut at its end, never drawn over the right side.
    let steps = container(steps(&rail.steps, waiting, tokens))
        .width(Length::Fill)
        .padding(iced::Padding::ZERO.right(RIGHT_GAP))
        .clip(true);
    let line = row![
        chip(&rail.chip, tokens),
        steps,
        unit_of_work(&rail.right, approvable, tokens)
    ]
    .align_y(Alignment::Center);
    let panel = tokens.panel;
    let bar = container(line)
        .width(Length::Fill)
        .height(HEIGHT)
        .padding([0.0, MARGIN_X])
        .align_y(Alignment::Center)
        .style(move |_: &Theme| container::Style {
            background: Some(panel.into()),
            ..container::Style::default()
        });
    iced::widget::column![bar, style::hairline(|tokens| tokens.border)].into()
}

/// The chip that says `name`, the workflow's, with the room that parts it from the steps.
fn chip<'a>(name: &str, tokens: &Tokens) -> Element<'a, Message> {
    let border = tokens.border;
    let chip = container(
        text(name.to_owned())
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

/// The workflow's `steps`, joined by lines, the current one drawn as waiting for the user
/// while a gate is `waiting`.
fn steps<'a>(steps: &[RailStep], waiting: bool, tokens: &Tokens) -> Row<'a, Message> {
    let mut line = Row::new().spacing(STEP_GAP).align_y(Alignment::Center);
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            line = line.push(join(tokens));
        }
        line = line.push(step_view(step, waiting, tokens));
    }
    line
}

/// One step as `step` describes it: a plain step or a loop.
fn step_view<'a>(step: &RailStep, waiting: bool, tokens: &Tokens) -> Element<'a, Message> {
    match step {
        RailStep::Plain {
            label,
            state,
            detail,
            doc,
        } => plain_step(
            label,
            *state,
            detail.as_deref(),
            doc.as_deref(),
            waiting,
            tokens,
        ),
        RailStep::Loop {
            label,
            phases,
            active,
            state,
        } => loop_box(label, phases, *active, *state, tokens),
    }
}

/// A plain step: its dot, its `label` and, for the current step, its `detail`, as a button
/// that shows the step's `doc` on the Spec tab.
fn plain_step<'a>(
    label: &str,
    state: StepState,
    detail: Option<&str>,
    doc: Option<&str>,
    waiting: bool,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let label = text(label.to_owned()).size(theme::SMALL);
    let parts = Row::new().spacing(PART_GAP).align_y(Alignment::Center);
    let parts = match state {
        StepState::Done => parts.push(dot(tokens.dim)).push(label.color(tokens.muted)),
        StepState::Upcoming => parts
            .push(hollow(tokens.dim))
            .push(label.color(tokens.muted)),
        StepState::Current if waiting => parts
            .push(dot(tokens.asks_you.fg))
            .push(label.font(theme::UI_MEDIUM).color(tokens.text))
            .push(text(WAITING).size(theme::SMALL).color(tokens.asks_you.fg)),
        StepState::Current => {
            let parts = parts
                .push(dot(tokens.text))
                .push(label.font(theme::UI_MEDIUM).color(tokens.text));
            match detail {
                Some(detail) => parts
                    .push(text("·").size(theme::SMALL).color(tokens.muted))
                    .push(
                        text(detail.to_owned())
                            .size(theme::SMALL)
                            .color(tokens.muted),
                    ),
                None => parts,
            }
        }
    };
    button(parts)
        .padding(0)
        .style(style::link_button)
        .on_press_maybe(doc.map(|doc| Message::Flow(FlowMessage::Open(doc.to_owned()))))
        .into()
}

/// A loop: a box around its muted `label` and its `phases` joined by arrows, the `active`
/// phase in the text colour, the box's border coloured by its `state` as a dot is.
fn loop_box<'a>(
    label: &str,
    phases: &[String],
    active: Option<usize>,
    state: StepState,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let mut parts = Row::new()
        .spacing(PART_GAP)
        .align_y(Alignment::Center)
        .push(
            text(label.to_owned())
                .size(theme::SMALL)
                .color(tokens.muted),
        );
    for (index, phase) in phases.iter().enumerate() {
        if index > 0 {
            parts = parts.push(text("→").size(theme::SMALL).color(tokens.muted));
        }
        let phase = text(phase.clone()).size(theme::SMALL);
        parts = parts.push(if active == Some(index) {
            phase.font(theme::UI_MEDIUM).color(tokens.text)
        } else {
            phase.color(tokens.muted)
        });
    }
    let border = match state {
        StepState::Done | StepState::Upcoming => tokens.dim,
        StepState::Current => tokens.text,
    };
    container(parts)
        .padding([1, 8])
        .style(move |_: &Theme| container::Style {
            border: Border::default()
                .rounded(LOOP_RADIUS)
                .width(1)
                .color(border),
            ..container::Style::default()
        })
        .into()
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

/// The right of the rail as `right` describes it: the button that approves a waiting gate,
/// enabled when `approvable`, or else the unit of work going on.
fn unit_of_work<'a>(right: &RailRight, approvable: bool, tokens: &Tokens) -> Element<'a, Message> {
    let (persona, title, pill, link) = match right {
        RailRight::Gate { approve_label } => {
            let caption = row![
                text(approve_label.clone()).size(theme::SMALL),
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
        RailRight::Unit {
            persona,
            title,
            pill,
            link,
        } => (persona, title, pill, link),
    };
    let mut work = Row::new().spacing(10).align_y(Alignment::Center);
    if let Some(persona) = persona {
        work = work.push(text(persona.clone()).size(theme::SMALL).color(tokens.muted));
    }
    if let Some(title) = title {
        work = work.push(
            text(keep_start(title, TITLE_BUDGET))
                .size(theme::SMALL)
                .color(tokens.text_soft),
        );
    }
    if let Some(pill) = pill {
        work = work.push(tinted_pill(pill));
    }
    if let Some((caption, doc)) = link {
        work = work.push(
            button(text(caption.clone()).size(theme::SMALL))
                .padding(0)
                .style(style::link_button)
                .on_press(Message::Flow(FlowMessage::Open(doc.clone()))),
        );
    }
    work.into()
}

/// The `pill` in its tone: red, green, or outlined for the neutral tone.
pub(crate) fn tinted_pill<'a>(pill: &PhasePill) -> Element<'a, Message> {
    let label = pill.text.clone();
    match pill.tone {
        Tone::Red => badge::pill(label, |tokens| tokens.tdd_red),
        Tone::Green => badge::pill(label, |tokens| tokens.running),
        Tone::Neutral => badge::outlined(label),
    }
}
