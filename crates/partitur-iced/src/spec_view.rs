//! The Spec tab: the design and the plan of the selected session's workflow, for a session
//! that reports one. The toolbar's Spec tab is disabled for every other session.
//!
//! Under the breadcrumb and the flow rail sit, side by side, the outline and the document.
//! The outline, 240 pixels wide and parted from the document by a faint line, holds one
//! block per step of the workflow, the current step's block on the selection colour. Each
//! block starts with the step's name in capitals, which turns to the colour of a session that
//! asks the user and reads `· gate` while the session waits at that step's gate, and reads
//! `· approved` once the user approved the step's document. Under it the block tells where
//! the step stands: when brainstorming started, in the user's local time; the path of the
//! design or the plan; the plan's size; whether execution waits for the plan's approval or
//! which task runs; and how the session finished, or that it ends with a merge or a PR.
//! Clicking a block shows the document its step works on.
//!
//! The document shows its title, its revision with the local time it was submitted and a
//! link that reads it again. A plan lists its first four tasks, each with its number, its
//! title and its files, a finished task with a `done` pill and the current task on the
//! selection colour, then how many more there are and after which tasks it stops for the
//! user. Then comes the document's text as Markdown; its links open with the session's
//! `link_opener`. While the file is read the document says `reading…`, and it tells when
//! the file is not there or cannot be read. A document the agent has not reported yet is
//! named as missing.
//!
//! While a gate waits, a bar at the foot of the document approves it (also with Enter),
//! asks for changes (not for an archived session, which takes no prompt), opens the gate's
//! design or plan in the desktop's editor and tells what approving does. Enter on this tab
//! approves nothing but the gate: a proposed split is approved only where it is shown, on
//! the Session tab.

use iced::widget::{Column, Space, button, column, container, markdown, row, scrollable, text};
use iced::{Alignment, Border, Element, Length, Padding, Pixels, Theme};
use partitur_core::workflow::{checkpoints_line, execute_detail, plan_summary};
use partitur_core::{GateKind, PlannedTask, SessionViewModel, Step, WorkflowStatus, WorkflowView};

use crate::app::Message;
use crate::ellipsis::keep_start;
use crate::flow::{self, DocumentState, FlowMessage, FlowState, SpecDoc};
use crate::session_view::{self, LinkTarget, Prose};
use crate::theme::{self, Tokens};
use crate::{badge, breadcrumb, clock, flow_rail, layout, style};

/// The width of the outline, in logical pixels.
const OUTLINE_WIDTH: f32 = 240.0;

/// The most characters of a document's path a block of the outline shows.
const PATH_BUDGET: usize = 26;

/// The most characters of a task's files its row shows.
const FILES_BUDGET: usize = 24;

/// How many of the plan's tasks are listed before the rest is summed up.
const SHOWN_TASKS: usize = 4;

/// The size of a document's title.
const TITLE_SIZE: f32 = 18.0;

/// The size of a section heading in a document, between its title's and the text's.
const HEADING_SIZE: f32 = 15.0;

/// The widths of a task row's number and files columns.
const TASK_COLUMNS: (f32, f32) = (24.0, 180.0);

/// The space around the document's content.
const PANE_PADDING: Padding = Padding {
    top: 24.0,
    right: 32.0,
    bottom: 24.0,
    left: 32.0,
};

/// What the document says while its file is read.
const READING: &str = "reading…";

/// The Spec tab for the session `model`, its documents as `flow` holds them, drawn in the
/// colours of `tokens`, the token set of the active theme. The approve buttons are enabled
/// when the gate is `approvable`, as the core tells it. A session without a workflow has no
/// Spec tab and gets an empty page.
pub fn view<'a>(
    model: &SessionViewModel<'a>,
    flow: &'a FlowState,
    approvable: bool,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let Some(workflow) = &model.workflow else {
        return Space::new().into();
    };
    let mut page = column![
        breadcrumb::breadcrumb(&model.breadcrumb, session_view::trailing(model)),
        flow_rail::rail(workflow, approvable, tokens),
    ];
    if let Some(archived) = model.archived {
        page = page.push(layout::archived_banner(&model.id, archived));
    }
    let mut document = column![pane(model, workflow, flow, tokens)]
        .width(Length::Fill)
        .height(Length::Fill);
    let live = model.archived.is_none();
    if let Some(bar) = gate_bar(&workflow.status, approvable, live, tokens) {
        document = document.push(bar);
    }
    let divider = container(Space::new())
        .width(1)
        .height(Length::Fill)
        .style(style::fill(|tokens| tokens.border_soft));
    page.push(row![outline(&workflow.status, tokens), divider, document].height(Length::Fill))
        .into()
}

/// The outline: one block per step of the workflow standing at `status`.
fn outline<'a>(status: &WorkflowStatus, tokens: &Tokens) -> Element<'a, Message> {
    let blocks = Step::ALL.map(|step| block(status, step, tokens));
    container(Column::with_children(blocks).spacing(4))
        .width(OUTLINE_WIDTH)
        .height(Length::Fill)
        .padding([16, 8])
        .into()
}

/// The outline's block of `step`: its name, marked while its gate waits or once its
/// document was approved, and what it tells about the step.
fn block<'a>(status: &WorkflowStatus, step: Step, tokens: &Tokens) -> Element<'a, Message> {
    let waiting = status
        .gate
        .as_ref()
        .is_some_and(|gate| gate_step(gate.kind) == step);
    let approved = match step {
        Step::Design => status.design.as_ref(),
        Step::Plan => status.plan.as_ref(),
        Step::Brainstorm | Step::Execute | Step::Finish => None,
    }
    .is_some_and(|document| document.approved_at.is_some());
    let mut label = step.label().to_uppercase();
    if waiting {
        label.push_str(" · gate");
    } else if approved {
        label.push_str(" · approved");
    }
    let colour = if waiting {
        tokens.asks_you.fg
    } else {
        tokens.muted
    };
    let mut lines = Column::new().spacing(6).push(
        text(label)
            .size(theme::TINY)
            .font(theme::UI_MEDIUM)
            .color(colour),
    );
    for detail in details(status, step) {
        lines = lines.push(match detail {
            Detail::Path(path) => text(path)
                .size(theme::SMALL)
                .font(theme::MONO)
                .color(tokens.text_soft),
            Detail::Note(note) => text(note).size(theme::SMALL).color(tokens.muted),
        });
    }
    let current = status.step == Some(step);
    let style: fn(&Theme, button::Status) -> button::Style = if current {
        style::selected_row_button
    } else {
        style::row_button
    };
    button(lines)
        .width(Length::Fill)
        .padding([10, 12])
        .style(style)
        .on_press(Message::Flow(FlowMessage::Select(flow::spec_doc(step))))
        .into()
}

/// A line of an outline block.
enum Detail {
    /// A document's path, already cut to fit.
    Path(String),
    /// A remark on the step.
    Note(String),
}

/// What the outline's block of `step` tells about it, given `status`.
fn details(status: &WorkflowStatus, step: Step) -> Vec<Detail> {
    let path = |document: Option<&partitur_core::Document>| {
        document.map(|document| {
            Detail::Path(keep_start(
                &document.path.display().to_string(),
                PATH_BUDGET,
            ))
        })
    };
    match step {
        Step::Brainstorm => status
            .brainstorm_at
            .map(|at| Detail::Note(format!("in transcript · {}", clock::local_time(at))))
            .into_iter()
            .collect(),
        Step::Design => path(status.design.as_ref()).into_iter().collect(),
        Step::Plan => {
            let summary = status
                .plan
                .as_ref()
                .and(status.tasks.as_ref())
                .map(|tasks| Detail::Note(plan_summary(tasks)));
            [path(status.plan.as_ref()), summary]
                .into_iter()
                .flatten()
                .collect()
        }
        Step::Execute if before_execution(status) => {
            vec![Detail::Note("after approval".to_owned())]
        }
        Step::Execute => status
            .tasks
            .as_ref()
            .and_then(execute_detail)
            .map(Detail::Note)
            .into_iter()
            .collect(),
        Step::Finish => vec![Detail::Note(
            status
                .outcome
                .map_or("merge or PR", |outcome| outcome.label())
                .to_owned(),
        )],
    }
}

/// Whether the workflow standing at `status` has not reached the execution of its plan.
fn before_execution(status: &WorkflowStatus) -> bool {
    let position = |step: Step| Step::ALL.iter().position(|each| *each == step);
    status
        .step
        .is_none_or(|step| position(step) < position(Step::Execute))
}

/// The step whose block a gate of `kind` marks.
fn gate_step(kind: GateKind) -> Step {
    match kind {
        GateKind::Design => Step::Design,
        GateKind::Plan => Step::Plan,
        GateKind::Checkpoint => Step::Execute,
    }
}

/// The document the Spec tab shows for the session `model`, scrolling under its header.
fn pane<'a>(
    model: &SessionViewModel<'_>,
    workflow: &WorkflowView,
    flow: &'a FlowState,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let status = &workflow.status;
    let doc = flow.selected(&model.id, status);
    let Some(document) = doc.reported(status) else {
        let missing = text(format!("no {} reported yet", doc.name()))
            .size(theme::SMALL)
            .color(tokens.muted);
        return container(missing)
            .padding(PANE_PADDING)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let path = flow::absolute(model, document);
    let state = flow.document(&model.id, doc);
    let title = match state {
        Some(DocumentState::Ready { title, .. }) => title.clone(),
        _ => flow::document_title("", &path),
    };
    let refresh = button(text("refresh").size(theme::SMALL))
        .padding(0)
        .style(style::link_button)
        .on_press(Message::Flow(FlowMessage::Refresh));
    let header = row![
        text(title)
            .size(TITLE_SIZE)
            .font(theme::UI_SEMIBOLD)
            .color(tokens.text),
        text(format!(
            "rev {} · {}",
            document.rev,
            clock::local_time(document.at)
        ))
        .size(theme::SMALL)
        .font(theme::MONO)
        .color(tokens.dim),
        Space::new().width(Length::Fill),
        refresh,
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let mut content = column![header].spacing(20);
    if doc == SpecDoc::Plan && !workflow.tasks.is_empty() {
        content = content.push(tasks(workflow, tokens));
    }
    let note = |words: String| -> Element<'a, Message> {
        text(words).size(theme::SMALL).color(tokens.muted).into()
    };
    let body = match state {
        None | Some(DocumentState::Loading) => note(READING.to_owned()),
        Some(DocumentState::Missing) => note(format!("not found at {}", path.display())),
        Some(DocumentState::Failed(error)) => {
            note(format!("cannot read {}: {error}", path.display()))
        }
        Some(DocumentState::Ready { items, .. }) => {
            let soft = tokens.text_soft;
            let markdown = markdown::view_with(items, prose(tokens), &Prose::<InDocument>::new());
            container(markdown)
                .style(move |_: &Theme| container::Style {
                    text_color: Some(soft),
                    ..container::Style::default()
                })
                .into()
        }
    };
    scrollable(
        container(content.push(body))
            .padding(PANE_PADDING)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The plan's tasks: the first few as rows, then how many more there are and where the
/// plan stops for the user.
fn tasks<'a>(workflow: &WorkflowView, tokens: &Tokens) -> Element<'a, Message> {
    let progress = workflow.status.tasks.as_ref();
    let current = progress.and_then(|progress| progress.current);
    let mut section = Column::new().spacing(6).push(
        container(
            text("Tasks")
                .size(theme::TEXT)
                .font(theme::UI_SEMIBOLD)
                .color(tokens.text),
        )
        .padding(Padding::ZERO.bottom(4)),
    );
    for task in workflow.tasks.iter().take(SHOWN_TASKS) {
        let done = progress.is_some_and(|progress| progress.done.contains(&task.n));
        section = section.push(task_row(task, done, current == Some(task.n), tokens));
    }
    let more = workflow.tasks.len().saturating_sub(SHOWN_TASKS);
    let rest: Vec<String> = [
        (more > 0).then(|| format!("… {more} more")),
        progress.and_then(|progress| checkpoints_line(&progress.checkpoints)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !rest.is_empty() {
        section = section.push(
            container(
                text(rest.join(" · "))
                    .size(theme::SMALL)
                    .color(tokens.muted),
            )
            .padding([2, 12]),
        );
    }
    section.into()
}

/// One task's row: its number, its title with a `done` pill once it is `done`, and its
/// files, on the selection colour while it is the `current` task.
fn task_row<'a>(
    task: &PlannedTask,
    done: bool,
    current: bool,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let (number_width, files_width) = TASK_COLUMNS;
    let mut title = row![
        text(task.title.clone())
            .size(theme::TEXT)
            .color(tokens.text)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if done {
        title = title.push(badge::pill("done", |tokens| tokens.done));
    }
    let background = if current {
        tokens.selection
    } else {
        tokens.panel
    };
    let border = tokens.border;
    container(
        row![
            text(task.n.to_string())
                .size(theme::SMALL)
                .font(theme::MONO)
                .color(tokens.muted)
                .width(number_width),
            title.width(Length::Fill),
            text(keep_start(&task.files.join(", "), FILES_BUDGET))
                .size(theme::SMALL)
                .font(theme::MONO)
                .color(tokens.muted)
                .width(files_width),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .width(Length::Fill)
    .style(move |_: &Theme| container::Style {
        background: Some(background.into()),
        border: Border::default()
            .rounded(style::ROW_RADIUS)
            .width(1)
            .color(border),
        ..container::Style::default()
    })
    .into()
}

/// The bar under the document while a gate waits at `status`: approve (enabled when
/// `approvable`), request changes (enabled while the session is `live`, not archived, so
/// it can take the prompt), open the gate's document in the editor when it has one, and
/// what approving does.
fn gate_bar<'a>(
    status: &WorkflowStatus,
    approvable: bool,
    live: bool,
    tokens: &Tokens,
) -> Option<Element<'a, Message>> {
    let kind = status.gate.as_ref()?.kind;
    let approve = button(
        row![
            text(kind.approve_label()).size(theme::SMALL),
            badge::key_cap_on_primary("Enter"),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .style(style::primary_button)
    .on_press_maybe(approvable.then_some(Message::Flow(FlowMessage::Approve)));
    let secondary = |label: &'static str, message: Option<FlowMessage>| {
        button(text(label).size(theme::SMALL))
            .padding([6, 12])
            .style(style::outlined_button)
            .on_press_maybe(message.map(Message::Flow))
    };
    let changes = live.then_some(FlowMessage::RequestChanges);
    let mut bar = row![approve, secondary("Request changes", changes)]
        .spacing(10)
        .align_y(Alignment::Center);
    if SpecDoc::of_gate(kind)
        .and_then(|doc| doc.reported(status))
        .is_some()
    {
        bar = bar.push(secondary("Open in editor", Some(FlowMessage::OpenInEditor)));
    }
    bar = bar.push(
        container(text(hint(kind)).size(theme::SMALL).color(tokens.muted))
            .width(Length::Fill)
            .padding(Padding::ZERO.left(6)),
    );
    let bar = container(bar)
        .padding([12, 32])
        .width(Length::Fill)
        .style(style::fill(|tokens| tokens.panel));
    Some(column![style::hairline(|tokens| tokens.border), bar].into())
}

/// How a document's Markdown is drawn with `tokens`: as the transcript draws an answer,
/// except that its headings stay below the size of the document's title.
fn prose(tokens: &Tokens) -> markdown::Settings {
    let section = Pixels(HEADING_SIZE);
    let text = Pixels(theme::TEXT);
    markdown::Settings {
        h1_size: Pixels(TITLE_SIZE),
        h2_size: section,
        h3_size: text,
        h4_size: text,
        h5_size: text,
        h6_size: text,
        ..style::markdown(tokens)
    }
}

/// The links in a document, which the Spec tab opens.
struct InDocument;

impl LinkTarget for InDocument {
    fn message(uri: markdown::Uri) -> Message {
        Message::Flow(FlowMessage::LinkClicked(uri))
    }
}

/// What approving a gate of `kind` does, as the gate bar tells it.
fn hint(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Design => "approval is written to the transcript · the plan is written next",
        GateKind::Plan => "approval is written to the transcript · execution starts with task 1",
        GateKind::Checkpoint => "approval is written to the transcript · execution continues",
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

    use iced::keyboard::{Key, Modifiers, key};
    use iced_test::simulator;
    use partitur_core::{PartiturTool, PlannedTask, SessionId, SuperpowersEvent};

    use super::*;
    use crate::app::{BareKey, State, Tab, tab_shortcut, update, view};
    use crate::flow::ReadFailure;
    use crate::test_support::{Plain, deliver_tool, end_turn, plain, read_message};

    /// Feeds the superpowers report `event` of `session` through the application.
    fn report(state: &mut State, session: &SessionId, event: SuperpowersEvent) {
        deliver_tool(state, session.clone(), PartiturTool::Superpowers(event));
    }

    /// Feeds every message through `update`, dropping the tasks: the simulator cannot run
    /// them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    /// Asks for approval of a plan of eleven tasks, `Task n` each touching `src/n.rs`, with
    /// checkpoints after tasks 3, 6 and 9.
    fn request_plan(state: &mut State, session: &SessionId) {
        let tasks = (1..=11)
            .map(|n| PlannedTask {
                n,
                title: format!("Task {n}"),
                files: vec![format!("src/{n}.rs")],
            })
            .collect();
        report(
            state,
            session,
            SuperpowersEvent::RequestPlanApproval {
                path: PathBuf::from("plans/p.md"),
                tasks,
                checkpoints: vec![3, 6, 9],
            },
        );
    }

    /// A plain session whose design was approved and that waits at its plan's gate, its turn
    /// ended, on the Session tab.
    fn at_the_plan() -> Plain {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(state, &session, SuperpowersEvent::StartBrainstorming);
        report(
            state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &session);
        state.app.approve_gate(session.clone()).unwrap();
        request_plan(state, &session);
        end_turn(state, &session);
        state.tab = Tab::Session;
        fixture
    }

    /// The status of the current session's workflow.
    fn status(state: &State) -> WorkflowStatus {
        state.app.session_view().unwrap().workflow.unwrap().status
    }

    #[test]
    fn the_spec_tab_is_disabled_without_a_workflow() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let mut ui = simulator(view(&fixture.state));
        ui.click("Spec").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        apply(&mut fixture.state, [Message::SelectTab(Tab::Spec)]);
        assert_eq!(fixture.state.tab, Tab::Session);
        apply(&mut fixture.state, [Message::Key(BareKey::Tab)]);
        assert_eq!(fixture.state.tab, Tab::Env);
        assert_eq!(Tab::Session.next(|tab| tab != Tab::Spec), Tab::Env);
    }

    #[test]
    fn a_session_with_a_workflow_enables_the_spec_tab() {
        let mut fixture = at_the_plan();

        let mut ui = simulator(view(&fixture.state));
        ui.click("Spec").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::SelectTab(Tab::Spec)]);
        apply(&mut fixture.state, [Message::Key(BareKey::Tab)]);
        assert_eq!(fixture.state.tab, Tab::Spec);
        apply(&mut fixture.state, [Message::Key(BareKey::Tab)]);
        assert_eq!(fixture.state.tab, Tab::Env);
    }

    #[test]
    fn ctrl_digits_select_the_four_tabs() {
        let press = |character: &str, code| {
            tab_shortcut(
                &Key::Character(character.into()),
                key::Physical::Code(code),
                Modifiers::COMMAND,
            )
        };

        assert_eq!(press("1", key::Code::Digit1), Some(Tab::Projects));
        assert_eq!(press("2", key::Code::Digit2), Some(Tab::Session));
        assert_eq!(press("3", key::Code::Digit3), Some(Tab::Spec));
        assert_eq!(press("4", key::Code::Digit4), Some(Tab::Env));
        assert_eq!(press("&", key::Code::Digit3), Some(Tab::Spec));
        assert_eq!(press("'", key::Code::Digit4), Some(Tab::Env));
        assert_eq!(press("5", key::Code::Digit5), None);
    }

    #[test]
    fn opening_the_spec_tab_reads_the_plan_in_the_background() {
        let mut fixture = at_the_plan();
        let state = &mut fixture.state;

        apply(state, [Message::SelectTab(Tab::Spec)]);

        assert_eq!(state.tab, Tab::Spec);
        let session = fixture.session.clone();
        assert!(matches!(
            state.flow.document(&session, SpecDoc::Plan),
            Some(DocumentState::Loading)
        ));
        let mut ui = simulator(view(state));
        ui.find(READING).unwrap();
        drop(ui);

        let read = read_message(
            state,
            SpecDoc::Plan,
            Ok("# Worktree support — implementation plan\n\nBody".into()),
        );
        apply(state, [read]);

        let at = status(state).plan.unwrap().at;
        let mut ui = simulator(view(state));
        ui.find("Worktree support — implementation plan").unwrap();
        ui.find(format!("rev 1 · {}", clock::local_time(at)).as_str())
            .unwrap();
        ui.find("Body").unwrap();
        assert!(ui.find(READING).is_err());
    }

    #[test]
    fn a_missing_document_reads_as_not_found() {
        let mut fixture = at_the_plan();
        let state = &mut fixture.state;
        apply(state, [Message::SelectTab(Tab::Spec)]);
        let path = state.app.session_view().unwrap().cwd.join("plans/p.md");

        apply(
            state,
            [read_message(
                state,
                SpecDoc::Plan,
                Err(ReadFailure::NotFound),
            )],
        );
        let mut ui = simulator(view(state));
        ui.find(format!("not found at {}", path.display()).as_str())
            .unwrap();
        // Without a heading to name it, the document is named after its file.
        ui.find("p.md").unwrap();
        drop(ui);

        apply(
            state,
            [read_message(
                state,
                SpecDoc::Plan,
                Err(ReadFailure::Other("Permission denied".into())),
            )],
        );
        let mut ui = simulator(view(state));
        ui.find(format!("cannot read {}: Permission denied", path.display()).as_str())
            .unwrap();
    }

    #[test]
    fn a_document_not_reported_yet_says_so() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(state, &session, SuperpowersEvent::StartBrainstorming);
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));
        ui.find("no design reported yet").unwrap();
        drop(ui);
        apply(state, [Message::Flow(FlowMessage::Select(SpecDoc::Plan))]);
        let mut ui = simulator(view(state));
        ui.find("no plan reported yet").unwrap();
    }

    #[test]
    fn the_plan_shows_four_tasks_then_the_rest_and_the_checkpoints() {
        let mut fixture = at_the_plan();
        let state = &mut fixture.state;
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        ui.find("Tasks").unwrap();
        for n in 1..=4 {
            ui.find(format!("Task {n}").as_str()).unwrap();
            ui.find(format!("src/{n}.rs").as_str()).unwrap();
        }
        assert!(ui.find("Task 5").is_err());
        ui.find("… 7 more · checkpoints after task 3, 6, 9")
            .unwrap();
    }

    #[test]
    fn a_plan_without_checkpoints_names_none() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        let tasks = (1..=12)
            .map(|n| PlannedTask {
                n,
                title: format!("Task {n}"),
                files: Vec::new(),
            })
            .collect();
        report(
            state,
            &session,
            SuperpowersEvent::RequestPlanApproval {
                path: PathBuf::from("plans/p.md"),
                tasks,
                checkpoints: Vec::new(),
            },
        );
        end_turn(state, &session);
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        ui.find("12 tasks").unwrap();
        ui.find("… 8 more").unwrap();
    }

    #[test]
    fn finished_tasks_carry_a_done_pill() {
        let mut fixture = at_the_plan();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.app.approve_gate(session.clone()).unwrap();
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Inline,
            },
        );
        for n in 1..=2 {
            report(
                state,
                &session,
                SuperpowersEvent::StartTask { n, note: None },
            );
            report(
                state,
                &session,
                SuperpowersEvent::FinishTask { n, note: None },
            );
        }
        report(
            state,
            &session,
            SuperpowersEvent::StartTask { n: 3, note: None },
        );
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        assert_eq!(crate::test_support::shown(&mut ui, "done"), 2);
        ui.find("task 3 of 11").unwrap();
        ui.find("PLAN · approved").unwrap();
    }

    #[test]
    fn the_outline_marks_the_waiting_gate() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(state, &session, SuperpowersEvent::StartBrainstorming);
        report(
            state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &session);
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let brainstorm = status(state).brainstorm_at.unwrap();
        let mut ui = simulator(view(state));
        ui.find("DESIGN · gate").unwrap();
        ui.find("BRAINSTORM").unwrap();
        ui.find(format!("in transcript · {}", clock::local_time(brainstorm)).as_str())
            .unwrap();
        ui.find("docs/design.md").unwrap();
        ui.find("after approval").unwrap();
        ui.find("merge or PR").unwrap();
        drop(ui);

        state.app.approve_gate(session.clone()).unwrap();
        request_plan(state, &session);
        end_turn(state, &session);
        let mut ui = simulator(view(state));
        ui.find("PLAN · gate").unwrap();
        ui.find("11 tasks · 3 checkpoints").unwrap();
        ui.find("DESIGN · approved").unwrap();
        ui.click("DESIGN · approved").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Flow(FlowMessage::Select(SpecDoc::Design))]
        );
        apply(state, messages);
        assert_eq!(
            state.flow.selected(&session, &status(state)),
            SpecDoc::Design
        );
    }

    #[test]
    fn the_outline_tells_the_outcome() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(
            state,
            &session,
            SuperpowersEvent::FinishSession {
                outcome: partitur_core::Outcome::Merged,
            },
        );
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        ui.find("merged").unwrap();
        assert!(ui.find("merge or PR").is_err());
        assert!(ui.find("after approval").is_err());
    }

    #[test]
    fn the_gate_bar_approves_and_requests_changes() {
        let mut fixture = at_the_plan();
        apply(&mut fixture.state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(&fixture.state));
        ui.find("approval is written to the transcript · execution starts with task 1")
            .unwrap();
        ui.find("Open in editor").unwrap();
        ui.click("Request changes").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::RequestChanges)]);
        apply(&mut fixture.state, messages);
        assert_eq!(fixture.state.tab, Tab::Session);
        assert_eq!(fixture.state.session.prompt.text(), "Changes to the plan: ");

        let mut fixture = at_the_plan();
        apply(&mut fixture.state, [Message::SelectTab(Tab::Spec)]);
        let mut ui = simulator(view(&fixture.state));
        ui.click("Approve plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Approve)]);
        apply(&mut fixture.state, messages);
        assert!(status(&fixture.state).gate.is_none());
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Request changes").is_err());

        let mut fixture = at_the_plan();
        apply(
            &mut fixture.state,
            [Message::SelectTab(Tab::Spec), Message::Key(BareKey::Enter)],
        );
        assert!(status(&fixture.state).gate.is_none());
        assert_eq!(fixture.state.tab, Tab::Spec);
    }

    #[test]
    fn a_checkpoint_has_no_document_to_open() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Inline,
            },
        );
        report(
            state,
            &session,
            SuperpowersEvent::ReachCheckpoint { after_task: 3 },
        );
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        ui.find("EXECUTE · gate").unwrap();
        ui.find("Continue").unwrap();
        ui.find("approval is written to the transcript · execution continues")
            .unwrap();
        assert!(ui.find("Open in editor").is_err());
    }

    #[test]
    fn the_design_gate_tells_what_comes_next() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        report(
            state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        apply(state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(state));

        ui.find("approval is written to the transcript · the plan is written next")
            .unwrap();
        // The turn still runs, so the gate cannot be approved yet.
        ui.click("Approve design").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn the_spec_tab_gives_way_to_the_session_tab_for_a_session_without_a_workflow() {
        let mut fixture = at_the_plan();
        apply(&mut fixture.state, [Message::SelectTab(Tab::Spec)]);
        let project = fixture
            .state
            .app
            .tree()
            .projects
            .first()
            .map(|project| project.id.clone())
            .unwrap();
        let other = fixture
            .state
            .app
            .new_session(project, partitur_core::Scope::Project)
            .unwrap();

        apply(&mut fixture.state, [Message::Tick]);

        assert_eq!(fixture.state.app.current_session(), Some(other));
        assert_eq!(fixture.state.tab, Tab::Session);
    }

    #[test]
    fn an_archived_session_cannot_ask_for_changes() {
        let mut fixture = at_the_plan();
        let session = fixture.session.clone();
        fixture.state.app.archive(session).unwrap();
        apply(&mut fixture.state, [Message::SelectTab(Tab::Spec)]);

        let mut ui = simulator(view(&fixture.state));
        ui.find("Request changes").unwrap();
        ui.click("Request changes").unwrap();
        ui.click("Approve plan").unwrap();
        ui.click("Open in editor").unwrap();

        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            [Message::Flow(FlowMessage::OpenInEditor)]
        );
    }

    #[test]
    fn enter_on_the_spec_tab_never_approves_a_split() {
        let mut fixture = crate::test_support::orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;
        let id = partitur_core::ProposalId::new();
        crate::test_support::deliver(
            state,
            partitur_core::BackendEvent::Update {
                session: orchestrator.clone(),
                update: partitur_core::Update::ChildrenProposed {
                    id: id.clone(),
                    children: vec![partitur_core::ProposedChild {
                        repository: partitur_core::RepositoryId::try_from("web").unwrap(),
                        prompt: "Add the page".into(),
                        title: "Web page".into(),
                        title_source: partitur_core::TitleSource::Given,
                    }],
                },
            },
        );
        // The gate opens while the turn still runs, so it cannot be approved.
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        let undecided = |state: &State| {
            state.app.session_view().unwrap().entries.iter().any(|entry| {
                matches!(
                    entry,
                    partitur_core::Entry::Proposal { id: shown, decision: None, .. } if *shown == id
                )
            })
        };
        let before = fixture.backend.calls();

        apply(
            state,
            [Message::SelectTab(Tab::Spec), Message::Key(BareKey::Enter)],
        );

        assert_eq!(state.tab, Tab::Spec);
        assert!(undecided(state));
        assert_eq!(fixture.backend.calls(), before);
        assert!(status(state).gate.is_some());

        // On the Session tab, where the split is shown, the open gate keeps Enter off it too.
        apply(
            state,
            [
                Message::SelectTab(Tab::Session),
                Message::Key(BareKey::Enter),
            ],
        );
        assert!(undecided(state));
        assert_eq!(fixture.backend.calls(), before);
    }
}
