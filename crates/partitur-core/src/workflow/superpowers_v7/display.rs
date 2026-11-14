//! The superpowers workflow's display data, in the wording every view shares.
//!
//! The rail names the workflow `superpowers` and shows its five steps. The current step adds
//! the plan's size while planning, the task being worked on while executing and the outcome
//! once finished. A click on brainstorm or design opens the design, a click on a later step
//! the plan. While a gate waits, the rail's right side holds the button that approves it;
//! otherwise, only while the plan is executed, it names the current task, the phase of its
//! test-driven cycle (`tdd red`, `tdd green` or `tdd refactor`) and links to the plan once
//! one was reported.
//!
//! The outline holds one block per step, marked while its gate waits or once its document
//! was approved, and telling when brainstorming started, the documents' paths, the plan's
//! size, whether execution waits for the plan's approval or which task runs, and how the
//! session finished or that it ends with a merge or a PR. The documents are always the design
//! and the plan, the plan with its tasks once it lists some. Starting a task and the phases
//! of its test-driven cycle have transcript rows of their own; every other event has none.

use crate::workflow::display::{
    BlockMark, DocRef, GateView, OutlineBlock, OutlineLine, PhasePill, RailRight, RailStep,
    RailView, ReportedDoc, StepCell, StepState, TaskList, TaskRowView, Tone, WorkflowRow,
};
use crate::workflow::{
    Document, GateKind, PlannedTask, Step, SuperpowersEvent, TddPhase, WorkflowEvent,
    WorkflowStatus, checkpoints_line, execute_detail, plan_summary, step_label,
};

/// The key of the design document.
const DESIGN: &str = "design";

/// The key of the plan document.
const PLAN: &str = "plan";

/// The rail of the workflow standing at `status`, whose latest plan lists `tasks`.
#[must_use]
pub fn rail(status: &WorkflowStatus, tasks: &[PlannedTask]) -> RailView {
    let current = status.step.and_then(position);
    let steps = Step::ALL
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            let state = match current {
                Some(current) if index < current => StepState::Done,
                Some(current) if index == current => StepState::Current,
                _ => StepState::Upcoming,
            };
            RailStep::Plain {
                label: step.label().to_owned(),
                state,
                detail: (state == StepState::Current)
                    .then(|| detail(status, step))
                    .flatten(),
                doc: Some(doc_of(step).to_owned()),
            }
        })
        .collect();
    RailView {
        chip: "superpowers".to_owned(),
        steps,
        right: right(status, tasks),
    }
}

/// The position of `step` among the workflow's steps.
fn position(step: Step) -> Option<usize> {
    Step::ALL.iter().position(|each| *each == step)
}

/// The key of the document `step` works on: the design while brainstorming and designing,
/// the plan from planning on.
fn doc_of(step: Step) -> &'static str {
    match step {
        Step::Brainstorm | Step::Design => DESIGN,
        Step::Plan | Step::Execute | Step::Finish => PLAN,
    }
}

/// What the current `step` of `status` adds to its label on the rail.
fn detail(status: &WorkflowStatus, step: Step) -> Option<String> {
    match step {
        Step::Plan => status.tasks.as_ref().map(plan_summary),
        Step::Execute => status.tasks.as_ref().and_then(execute_detail),
        Step::Finish => status.outcome.map(|outcome| outcome.label().to_owned()),
        Step::Brainstorm | Step::Design => None,
    }
}

/// The right of the rail: the approve button of a waiting gate, or else while the plan is
/// executed the current task's title, its phase and the link to the plan.
fn right(status: &WorkflowStatus, tasks: &[PlannedTask]) -> RailRight {
    if let Some(gate) = &status.gate {
        return RailRight::Gate {
            approve_label: gate.kind.approve_label().to_owned(),
        };
    }
    let executing = status.step == Some(Step::Execute);
    let progress = status.tasks.as_ref().filter(|_| executing);
    let current = progress.and_then(|progress| progress.current);
    RailRight::Unit {
        persona: None,
        title: current
            .and_then(|n| tasks.iter().find(|task| task.n == n))
            .map(|task| task.title.clone()),
        pill: progress
            .and_then(|progress| progress.phase)
            .map(|phase| pill(phase, format!("tdd {}", phase.label()))),
        link: (executing && status.plan.is_some())
            .then(|| ("open plan".to_owned(), PLAN.to_owned())),
    }
}

/// The pill that says `text` in the tone of a test-driven `phase`.
fn pill(phase: TddPhase, text: String) -> PhasePill {
    let tone = match phase {
        TddPhase::Red => Tone::Red,
        TddPhase::Green => Tone::Green,
        TddPhase::Refactor => Tone::Neutral,
    };
    PhasePill { text, tone }
}

/// The outline of the workflow standing at `status`: one block per step.
#[must_use]
pub fn outline(status: &WorkflowStatus) -> Vec<OutlineBlock> {
    Step::ALL
        .into_iter()
        .map(|step| {
            let waiting = status
                .gate
                .as_ref()
                .is_some_and(|gate| gate_step(gate.kind) == step);
            let approved =
                step_document(status, step).is_some_and(|document| document.approved_at.is_some());
            let mark = if waiting {
                BlockMark::Gate
            } else if approved {
                BlockMark::Approved
            } else {
                BlockMark::None
            };
            OutlineBlock {
                label: step.label().to_uppercase(),
                mark,
                lines: lines(status, step),
                doc: Some(doc_of(step).to_owned()),
                current: status.step == Some(step),
            }
        })
        .collect()
}

/// The document `step` asks the user to approve, as `status` reports it.
fn step_document(status: &WorkflowStatus, step: Step) -> Option<&Document> {
    match step {
        Step::Design => status.design.as_ref(),
        Step::Plan => status.plan.as_ref(),
        Step::Brainstorm | Step::Execute | Step::Finish => None,
    }
}

/// What the outline's block of `step` tells about it, given `status`.
fn lines(status: &WorkflowStatus, step: Step) -> Vec<OutlineLine> {
    let path = |document: Option<&Document>| {
        document.map(|document| OutlineLine::Path(document.path.display().to_string()))
    };
    match step {
        Step::Brainstorm => status
            .brainstorm_at
            .map(|at| OutlineLine::Timed {
                text: "in transcript".to_owned(),
                at,
            })
            .into_iter()
            .collect(),
        Step::Design => path(status.design.as_ref()).into_iter().collect(),
        Step::Plan => {
            let summary = status
                .plan
                .as_ref()
                .and(status.tasks.as_ref())
                .map(|tasks| OutlineLine::Note(plan_summary(tasks)));
            [path(status.plan.as_ref()), summary]
                .into_iter()
                .flatten()
                .collect()
        }
        Step::Execute if before_execution(status) => {
            vec![OutlineLine::Note("after approval".to_owned())]
        }
        Step::Execute => status
            .tasks
            .as_ref()
            .and_then(execute_detail)
            .map(OutlineLine::Note)
            .into_iter()
            .collect(),
        Step::Finish => vec![OutlineLine::Note(
            status
                .outcome
                .map_or("merge or PR", |outcome| outcome.label())
                .to_owned(),
        )],
    }
}

/// Whether the workflow standing at `status` has not reached the execution of its plan.
fn before_execution(status: &WorkflowStatus) -> bool {
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

/// The documents of the workflow standing at `status`, whose latest plan lists `tasks`: the
/// design and the plan, the plan with its tasks once it was reported and lists some.
#[must_use]
pub fn documents(status: &WorkflowStatus, tasks: &[PlannedTask]) -> Vec<DocRef> {
    let reported = |document: Option<&Document>| {
        document.map(|document| ReportedDoc {
            path: document.path.clone(),
            rev: document.rev,
            at: document.at,
        })
    };
    let progress = status.tasks.as_ref();
    let list = (status.plan.is_some() && !tasks.is_empty()).then(|| TaskList {
        rows: tasks
            .iter()
            .map(|task| TaskRowView {
                n: task.n.to_string(),
                title: task.title.clone(),
                files: task.files.clone(),
                done: progress.is_some_and(|progress| progress.done.contains(&task.n)),
                current: progress.is_some_and(|progress| progress.current == Some(task.n)),
            })
            .collect(),
        footnote: progress.and_then(|progress| checkpoints_line(&progress.checkpoints)),
    });
    vec![
        DocRef {
            key: DESIGN.to_owned(),
            label: DESIGN.to_owned(),
            reported: reported(status.design.as_ref()),
            tasks: None,
        },
        DocRef {
            key: PLAN.to_owned(),
            label: PLAN.to_owned(),
            reported: reported(status.plan.as_ref()),
            tasks: list,
        },
    ]
}

/// The tree's step cell for the workflow standing at `status`.
#[must_use]
pub fn step_cell(status: &WorkflowStatus) -> StepCell {
    StepCell {
        label: step_label(status),
        gate_waiting: status.gate.is_some(),
    }
}

/// The gate waiting at `status`, `None` when none waits.
#[must_use]
pub fn gate_view(status: &WorkflowStatus) -> Option<GateView> {
    let kind = status.gate.as_ref()?.kind;
    let (hint, doc) = match kind {
        GateKind::Design => (
            "approval is written to the transcript · the plan is written next",
            status.design.as_ref().map(|_| DESIGN),
        ),
        GateKind::Plan => (
            "approval is written to the transcript · execution starts with task 1",
            status.plan.as_ref().map(|_| PLAN),
        ),
        GateKind::Checkpoint => (
            "approval is written to the transcript · execution continues",
            None,
        ),
    };
    Some(GateView {
        approve_label: kind.approve_label().to_owned(),
        change_prefix: kind.change_prefix().to_owned(),
        hint: hint.to_owned(),
        doc: doc.map(str::to_owned),
    })
}

/// The transcript row of `event`, with the plan's `task` it is about when the plan lists it:
/// a row for starting a task and for each phase of its test-driven cycle, `None` for every
/// other event.
#[must_use]
pub fn transcript_row(event: &WorkflowEvent, task: Option<&PlannedTask>) -> Option<WorkflowRow> {
    let WorkflowEvent::Superpowers(event) = event else {
        return None;
    };
    let (phase, note) = match event {
        SuperpowersEvent::StartTask { n, note } => {
            return Some(WorkflowRow::Task {
                label: format!("task {n}"),
                title: task.map(|task| task.title.clone()),
                files: task.map(|task| task.files.clone()).unwrap_or_default(),
                note: note.clone(),
            });
        }
        SuperpowersEvent::WriteFailingTest { note, .. } => (TddPhase::Red, note),
        SuperpowersEvent::MakeTestPass { note, .. } => (TddPhase::Green, note),
        SuperpowersEvent::StartRefactoring { note, .. } => (TddPhase::Refactor, note),
        _ => return None,
    };
    Some(WorkflowRow::Phase {
        label: "tdd".to_owned(),
        pill: pill(phase, phase.label().to_owned()),
        note: note.clone(),
    })
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
    use crate::workflow::display::{
        BlockMark, DocRef, GateView, OutlineLine, PhasePill, RailRight, RailStep, ReportedDoc,
        StepCell, StepState, TaskList, TaskRowView, Tone, WorkflowRow,
    };
    use crate::workflow::{
        Document, GateCloser, GateKind, OpenGate, Outcome, PlannedTask, Step, SuperpowersEvent,
        TaskProgress, TddPhase, Workflow, WorkflowEvent, WorkflowStatus,
    };

    fn status(step: Option<Step>) -> WorkflowStatus {
        WorkflowStatus {
            workflow: Workflow::Superpowers,
            topic: "t".into(),
            applied: 1,
            updated_at: 1,
            step,
            brainstorm_at: None,
            gate: None,
            design: None,
            plan: None,
            tasks: None,
            mode: None,
            outcome: None,
        }
    }

    fn document(path: &str, rev: u32, at: u64, approved_at: Option<u64>) -> Document {
        Document {
            path: path.into(),
            rev,
            at,
            approved_at,
        }
    }

    fn gate(kind: GateKind) -> OpenGate {
        OpenGate { kind, since: 1 }
    }

    fn task(n: u32) -> PlannedTask {
        PlannedTask {
            n,
            title: format!("Task {n}"),
            files: vec![format!("src/{n}.rs")],
        }
    }

    fn eleven() -> Vec<PlannedTask> {
        (1..=11).map(task).collect()
    }

    /// Executing task 4 of a plan of eleven tasks with checkpoints after tasks 3, 6 and 9,
    /// tasks 1 to 3 done, in the red phase.
    fn executing() -> WorkflowStatus {
        WorkflowStatus {
            plan: Some(document("plans/p.md", 1, 20, Some(30))),
            tasks: Some(TaskProgress {
                total: 11,
                done: vec![1, 2, 3],
                current: Some(4),
                phase: Some(TddPhase::Red),
                checkpoints: vec![3, 6, 9],
            }),
            ..status(Some(Step::Execute))
        }
    }

    fn plain(rail: &[RailStep]) -> Vec<(String, StepState, Option<String>, Option<String>)> {
        rail.iter()
            .map(
                |RailStep::Plain {
                     label,
                     state,
                     detail,
                     doc,
                 }| (label.clone(), *state, detail.clone(), doc.clone()),
            )
            .collect()
    }

    fn unit(title: Option<&str>, pill: Option<(&str, Tone)>, link: bool) -> RailRight {
        RailRight::Unit {
            persona: None,
            title: title.map(str::to_owned),
            pill: pill.map(|(text, tone)| PhasePill {
                text: text.into(),
                tone,
            }),
            link: link.then(|| ("open plan".to_owned(), "plan".to_owned())),
        }
    }

    #[test]
    fn the_rail_marks_steps_around_the_current_one() {
        let view = rail(&executing(), &eleven());

        assert_eq!(view.chip, "superpowers");
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(
            plain(&view.steps),
            [
                ("brainstorm".into(), StepState::Done, None, some("design")),
                ("design".into(), StepState::Done, None, some("design")),
                ("plan".into(), StepState::Done, None, some("plan")),
                (
                    "execute".into(),
                    StepState::Current,
                    some("task 4 of 11"),
                    some("plan")
                ),
                ("finish".into(), StepState::Upcoming, None, some("plan")),
            ]
        );

        let states = |status: &WorkflowStatus| -> Vec<StepState> {
            plain(&rail(status, &[]).steps)
                .into_iter()
                .map(|(_, state, _, _)| state)
                .collect()
        };
        assert_eq!(states(&status(None)), [StepState::Upcoming; 5]);
        assert_eq!(
            states(&status(Some(Step::Brainstorm)))[..2],
            [StepState::Current, StepState::Upcoming]
        );

        let detail = |status: &WorkflowStatus, index: usize| {
            plain(&rail(status, &[]).steps)[index].2.clone()
        };
        let planning = WorkflowStatus {
            tasks: executing().tasks,
            ..status(Some(Step::Plan))
        };
        assert_eq!(detail(&planning, 2), some("11 tasks · 3 checkpoints"));
        assert_eq!(detail(&status(Some(Step::Plan)), 2), None);
        let finished = WorkflowStatus {
            outcome: Some(Outcome::PrOpened),
            ..status(Some(Step::Finish))
        };
        assert_eq!(detail(&finished, 4), some("PR opened"));
        assert_eq!(detail(&status(Some(Step::Finish)), 4), None);
        let designing = WorkflowStatus {
            tasks: executing().tasks,
            outcome: Some(Outcome::Merged),
            ..status(Some(Step::Design))
        };
        assert_eq!(detail(&designing, 1), None);
        let brainstorming = WorkflowStatus {
            step: Some(Step::Brainstorm),
            ..designing
        };
        assert_eq!(detail(&brainstorming, 0), None);
        // Only the current step carries its detail.
        assert_eq!(detail(&executing(), 2), None);
        assert_eq!(detail(&executing(), 4), None);
    }

    #[test]
    fn the_rail_right_names_task_phase_and_plan_link_while_executing() {
        assert_eq!(
            rail(&executing(), &eleven()).right,
            unit(Some("Task 4"), Some(("tdd red", Tone::Red)), true)
        );

        let phase = |phase| {
            let mut status = executing();
            status.tasks.as_mut().unwrap().phase = phase;
            rail(&status, &eleven()).right
        };
        assert_eq!(
            phase(Some(TddPhase::Green)),
            unit(Some("Task 4"), Some(("tdd green", Tone::Green)), true)
        );
        assert_eq!(
            phase(Some(TddPhase::Refactor)),
            unit(Some("Task 4"), Some(("tdd refactor", Tone::Neutral)), true)
        );
        assert_eq!(phase(None), unit(Some("Task 4"), None, true));

        // A task the plan does not list has no title, and without a plan there is no link.
        let unlisted = WorkflowStatus {
            plan: None,
            ..executing()
        };
        assert_eq!(
            rail(&unlisted, &[task(9)]).right,
            unit(None, Some(("tdd red", Tone::Red)), false)
        );

        // Outside execution the right of the rail is empty.
        let planning = WorkflowStatus {
            step: Some(Step::Plan),
            ..executing()
        };
        assert_eq!(rail(&planning, &eleven()).right, unit(None, None, false));
    }

    #[test]
    fn a_waiting_gate_puts_the_approve_label_on_the_rail() {
        let at = |kind| {
            let status = WorkflowStatus {
                gate: Some(gate(kind)),
                ..executing()
            };
            rail(&status, &eleven()).right
        };
        let approve = |label: &str| RailRight::Gate {
            approve_label: label.into(),
        };
        assert_eq!(at(GateKind::Plan), approve("Approve plan"));
        assert_eq!(at(GateKind::Design), approve("Approve design"));
        assert_eq!(at(GateKind::Checkpoint), approve("Continue"));
    }

    #[test]
    fn outline_blocks_follow_the_spec_wording() {
        let note = |text: &str| OutlineLine::Note(text.into());
        let path = |text: &str| OutlineLine::Path(text.into());

        let designing = WorkflowStatus {
            brainstorm_at: Some(10),
            design: Some(document("docs/design.md", 1, 12, None)),
            gate: Some(gate(GateKind::Design)),
            ..status(Some(Step::Design))
        };
        let blocks = outline(&designing);
        let labels: Vec<_> = blocks.iter().map(|block| block.label.as_str()).collect();
        assert_eq!(
            labels,
            ["BRAINSTORM", "DESIGN", "PLAN", "EXECUTE", "FINISH"]
        );
        let docs: Vec<_> = blocks.iter().map(|block| block.doc.as_deref()).collect();
        assert_eq!(
            docs,
            [
                Some("design"),
                Some("design"),
                Some("plan"),
                Some("plan"),
                Some("plan")
            ]
        );
        let current: Vec<_> = blocks.iter().map(|block| block.current).collect();
        assert_eq!(current, [false, true, false, false, false]);
        let lines: Vec<_> = blocks.iter().map(|block| block.lines.clone()).collect();
        assert_eq!(
            lines,
            [
                vec![OutlineLine::Timed {
                    text: "in transcript".into(),
                    at: 10
                }],
                vec![path("docs/design.md")],
                Vec::new(),
                vec![note("after approval")],
                vec![note("merge or PR")],
            ]
        );
        assert_eq!(outline(&status(None))[3].lines, [note("after approval")]);
        assert!(outline(&status(None))[0].lines.is_empty());
        assert_eq!(
            outline(&status(Some(Step::Plan)))[3].lines,
            [note("after approval")]
        );

        // A plan's tasks are summed up only once the plan was reported.
        let unreported = WorkflowStatus {
            plan: None,
            ..executing()
        };
        assert!(outline(&unreported)[2].lines.is_empty());

        let mut executing = executing();
        let blocks = outline(&executing);
        assert_eq!(
            blocks[2].lines,
            [path("plans/p.md"), note("11 tasks · 3 checkpoints")]
        );
        assert_eq!(blocks[3].lines, [note("task 4 of 11")]);

        executing.tasks.as_mut().unwrap().current = None;
        assert!(outline(&executing)[3].lines.is_empty());

        let finished = WorkflowStatus {
            outcome: Some(Outcome::Merged),
            ..status(Some(Step::Finish))
        };
        let blocks = outline(&finished);
        assert_eq!(blocks[4].lines, [note("merged")]);
        assert!(blocks[3].lines.is_empty());
        assert!(blocks[4].current);
    }

    #[test]
    fn outline_marks_follow_gates_and_approvals() {
        use BlockMark::{Approved, Gate, None as Plain};
        let marks = |status: &WorkflowStatus| -> Vec<BlockMark> {
            outline(status).iter().map(|block| block.mark).collect()
        };
        let designing = WorkflowStatus {
            design: Some(document("docs/design.md", 1, 12, None)),
            gate: Some(gate(GateKind::Design)),
            ..status(Some(Step::Design))
        };
        assert_eq!(marks(&designing), [Plain, Gate, Plain, Plain, Plain]);

        let mut executing = executing();
        executing.design = Some(document("docs/design.md", 2, 12, Some(13)));
        assert_eq!(marks(&executing), [Plain, Approved, Approved, Plain, Plain]);
        executing.gate = Some(gate(GateKind::Checkpoint));
        assert_eq!(marks(&executing), [Plain, Approved, Approved, Gate, Plain]);
        executing.gate = Some(gate(GateKind::Plan));
        assert_eq!(marks(&executing), [Plain, Approved, Gate, Plain, Plain]);
    }

    #[test]
    fn documents_list_design_and_plan_with_the_plans_tasks() {
        let row = |n: u32, done, current| TaskRowView {
            n: n.to_string(),
            title: format!("Task {n}"),
            files: vec![format!("src/{n}.rs")],
            done,
            current,
        };
        let mut reported = executing();
        reported.design = Some(document("docs/design.md", 2, 12, Some(13)));
        let mut rows: Vec<_> = (1..=11).map(|n| row(n, n <= 3, n == 4)).collect();

        assert_eq!(
            documents(&reported, &eleven()),
            [
                DocRef {
                    key: "design".into(),
                    label: "design".into(),
                    reported: Some(ReportedDoc {
                        path: "docs/design.md".into(),
                        rev: 2,
                        at: 12
                    }),
                    tasks: None,
                },
                DocRef {
                    key: "plan".into(),
                    label: "plan".into(),
                    reported: Some(ReportedDoc {
                        path: "plans/p.md".into(),
                        rev: 1,
                        at: 20
                    }),
                    tasks: Some(TaskList {
                        rows: rows.clone(),
                        footnote: Some("checkpoints after task 3, 6, 9".into()),
                    }),
                },
            ]
        );

        // Without checkpoints or progress there is no footnote, and nothing is done.
        let quiet = WorkflowStatus {
            tasks: None,
            ..reported.clone()
        };
        for each in &mut rows {
            each.done = false;
            each.current = false;
        }
        assert_eq!(
            documents(&quiet, &eleven())[1].tasks,
            Some(TaskList {
                rows,
                footnote: None
            })
        );
        assert_eq!(documents(&reported, &[])[1].tasks, None);

        let unreported = documents(&status(None), &eleven());
        let keys: Vec<_> = unreported.iter().map(|doc| doc.key.as_str()).collect();
        assert_eq!(keys, ["design", "plan"]);
        assert_eq!(
            (&unreported[1].reported, &unreported[1].tasks),
            (&None, &None)
        );
        assert_eq!(unreported[0].reported, None);
    }

    #[test]
    fn gate_views_carry_the_fixed_copy() {
        let view = |kind, design: Option<Document>, plan: Option<Document>| {
            gate_view(&WorkflowStatus {
                gate: Some(gate(kind)),
                design,
                plan,
                ..status(Some(Step::Plan))
            })
        };
        let gate_of = |approve: &str, change: &str, hint: &str, doc: Option<&str>| {
            Some(GateView {
                approve_label: approve.into(),
                change_prefix: change.into(),
                hint: hint.into(),
                doc: doc.map(str::to_owned),
            })
        };
        let reported = || Some(document("d.md", 1, 1, None));

        assert_eq!(gate_view(&status(Some(Step::Plan))), None);
        assert_eq!(
            view(GateKind::Design, reported(), None),
            gate_of(
                "Approve design",
                "Changes to the design: ",
                "approval is written to the transcript · the plan is written next",
                Some("design")
            )
        );
        assert_eq!(
            view(GateKind::Plan, None, reported()),
            gate_of(
                "Approve plan",
                "Changes to the plan: ",
                "approval is written to the transcript · execution starts with task 1",
                Some("plan")
            )
        );
        assert_eq!(
            view(GateKind::Checkpoint, reported(), reported()),
            gate_of(
                "Continue",
                "Before continuing: ",
                "approval is written to the transcript · execution continues",
                None
            )
        );
        assert_eq!(view(GateKind::Design, None, reported()).unwrap().doc, None);
        assert_eq!(view(GateKind::Plan, reported(), None).unwrap().doc, None);
    }

    #[test]
    fn transcript_rows_for_task_starts_and_tdd_phases() {
        let sp = WorkflowEvent::Superpowers;
        let note = || Some("first".to_owned());
        let listed = task(4);

        assert_eq!(
            transcript_row(
                &sp(SuperpowersEvent::StartTask { n: 4, note: note() }),
                Some(&listed)
            ),
            Some(WorkflowRow::Task {
                label: "task 4".into(),
                title: Some("Task 4".into()),
                files: vec!["src/4.rs".into()],
                note: note(),
            })
        );
        assert_eq!(
            transcript_row(&sp(SuperpowersEvent::StartTask { n: 9, note: None }), None),
            Some(WorkflowRow::Task {
                label: "task 9".into(),
                title: None,
                files: Vec::new(),
                note: None,
            })
        );

        let phase = |pill: &str, tone, note: Option<String>| {
            Some(WorkflowRow::Phase {
                label: "tdd".into(),
                pill: PhasePill {
                    text: pill.into(),
                    tone,
                },
                note,
            })
        };
        assert_eq!(
            transcript_row(
                &sp(SuperpowersEvent::WriteFailingTest { n: 4, note: note() }),
                Some(&listed)
            ),
            phase("red", Tone::Red, note())
        );
        assert_eq!(
            transcript_row(
                &sp(SuperpowersEvent::MakeTestPass { n: 4, note: None }),
                None
            ),
            phase("green", Tone::Green, None)
        );
        assert_eq!(
            transcript_row(
                &sp(SuperpowersEvent::StartRefactoring { n: 4, note: note() }),
                None
            ),
            phase("refactor", Tone::Neutral, note())
        );

        assert_eq!(
            transcript_row(&sp(SuperpowersEvent::StartBrainstorming), None),
            None
        );
        assert_eq!(
            transcript_row(
                &WorkflowEvent::GateClosed {
                    gate: GateKind::Plan,
                    by: GateCloser::Approve
                },
                None
            ),
            None
        );
    }

    #[test]
    fn the_step_cell_words_the_status_and_tells_a_waiting_gate() {
        assert_eq!(
            step_cell(&executing()),
            StepCell {
                label: "execute · 4/11".into(),
                gate_waiting: false,
            }
        );
        let gated = WorkflowStatus {
            gate: Some(gate(GateKind::Checkpoint)),
            ..executing()
        };
        assert_eq!(
            step_cell(&gated),
            StepCell {
                label: "execute · checkpoint".into(),
                gate_waiting: true,
            }
        );
    }
}
