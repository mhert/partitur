//! The BMAD workflow's display data, for a session that runs one BMAD workflow, and for now a
//! plain form of an orchestrator's whole run.
//!
//! A session that reports a run shows the run in place of a workflow: the rail's chip names
//! the module, the steps are the run's steps by name, those before the current step done and
//! all of them once the run ended, and the rail's right side holds the approve button while a
//! gate waits and is empty otherwise. Its outline has one plain block per step, and its tree
//! cell names the current step.
//!
//! The rail's chip names the module and the workflow's kind, such as `bmad 6.12 · build`, and
//! the rail shows the kind's steps: the ones before the current step are done, all of them
//! once the workflow finished. A step names the document its gate asked about last, so that a
//! click on it opens that document. While a gate waits, the rail's right side holds the button
//! that approves it; otherwise it names the persona at work and the story or else the epic it
//! works on.
//!
//! The tree's step cell names the kind and the gate waiting (`build · plan gate`), or else the
//! current step (`build · build`) or `done` once finished; before any workflow it is the chip
//! alone. The outline holds one block per step, marked while its gate waits or once its
//! document was approved, with the document's path and a verdict gate's verdict. The documents
//! are the files the gates asked about, in the order they were first asked about, a story
//! order with its stories as tasks. Starting a workflow with a persona leaves a row naming the
//! persona and what it works on; every other report leaves a note: the start without a
//! persona, each file ready for the user's approval and the end of the workflow.

use crate::workflow::GateKind;
use crate::workflow::bmad_v6_12::{
    BmadDoc, BmadV6_12, Event, Gate, PlanningDoc, Readiness, RetroVerdict, ReviewVerdict, Run,
    RunStep, State, Verdict, WorkflowRun, request, texts,
};
use crate::workflow::display::{
    BlockMark, DocRef, GateView, OutlineBlock, OutlineLine, RailRight, RailStep, RailView,
    ReportedDoc, StepCell, StepState, TaskList, TaskRowView, WorkflowRow,
};
use crate::workflow::{OpenGate, WorkflowModule};

/// The rail of the workflow standing at `state` with `gate` waiting.
pub(super) fn rail(state: &State, gate: Option<&OpenGate>) -> RailView {
    let chip = BmadV6_12::ID.chip();
    if let Some(run) = &state.run {
        return RailView {
            chip: chip.to_owned(),
            steps: run_steps(run),
            right: right(None, gate),
        };
    }
    let workflow = state.workflow.as_ref();
    RailView {
        chip: workflow.map_or_else(
            || chip.to_owned(),
            |workflow| format!("{chip} · {}", workflow.kind.label()),
        ),
        steps: workflow.map(steps).unwrap_or_default(),
        right: right(workflow, gate),
    }
}

/// The rail's steps of `workflow`.
fn steps(workflow: &WorkflowRun) -> Vec<RailStep> {
    workflow
        .kind
        .steps()
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let state = if workflow.finished.is_some() || index < workflow.step {
                StepState::Done
            } else if index == workflow.step {
                StepState::Current
            } else {
                StepState::Upcoming
            };
            RailStep::Plain {
                label: (*step).to_owned(),
                state,
                detail: None,
                doc: step_doc(workflow, step).map(|doc| doc.key.clone()),
            }
        })
        .collect()
}

/// The rail's steps of `run`, each named by [`run_step_label`].
fn run_steps(run: &Run) -> Vec<RailStep> {
    let current = run_position(run);
    run.steps()
        .into_iter()
        .enumerate()
        .map(|(index, step)| RailStep::Plain {
            label: run_step_label(step).to_owned(),
            state: if run.outcome.is_some() || current.is_some_and(|current| index < current) {
                StepState::Done
            } else if current == Some(index) {
                StepState::Current
            } else {
                StepState::Upcoming
            },
            detail: None,
            doc: None,
        })
        .collect()
}

/// The index of `run`'s current step among its steps, `None` when it stands at none of them.
fn run_position(run: &Run) -> Option<usize> {
    let current = run.current?;
    run.steps().iter().position(|step| *step == current)
}

/// `step` as the UI names it.
fn run_step_label(step: RunStep) -> &'static str {
    match step {
        RunStep::Planning(PlanningDoc::Brief) => "brief",
        RunStep::Planning(PlanningDoc::Prfaq) => "prfaq",
        RunStep::Planning(PlanningDoc::Prd) => "prd",
        RunStep::Planning(PlanningDoc::Ux) => "ux",
        RunStep::Planning(PlanningDoc::Architecture) => "architecture",
        RunStep::Spec => "spec",
        RunStep::Build => "build",
        RunStep::Epics => "epics",
        RunStep::Sprint => "sprint",
        RunStep::Loop => "stories",
        RunStep::Retro => "retro",
    }
}

/// The right of the rail: the approve button of a waiting gate, or else the persona at work
/// and the story or else the epic it works on.
fn right(workflow: Option<&WorkflowRun>, gate: Option<&OpenGate>) -> RailRight {
    if let Some(gate) = gate {
        return RailRight::Gate {
            approve_label: texts(gate).approve_label.to_owned(),
        };
    }
    RailRight::Unit {
        persona: workflow.and_then(|workflow| workflow.persona.clone()),
        title: workflow.and_then(|workflow| {
            workflow
                .story
                .as_ref()
                .map(|story| format!("story {story}"))
                .or_else(|| workflow.epic.as_ref().map(|epic| format!("epic {epic}")))
        }),
        pill: None,
        link: None,
    }
}

/// The document whose gate sits on `step` and that was asked about last, `None` when no gate
/// of the step asked about one.
fn step_doc<'a>(workflow: &'a WorkflowRun, step: &str) -> Option<&'a BmadDoc> {
    workflow
        .documents
        .iter()
        .filter(|doc| Gate::of(&GateKind::new(&doc.key)).is_some_and(|gate| gate.step() == step))
        .max_by_key(|doc| doc.at)
}

/// The outline of the workflow standing at `state` with `gate` waiting: one block per step.
pub(super) fn outline(state: &State, gate: Option<&OpenGate>) -> Vec<OutlineBlock> {
    if let Some(run) = &state.run {
        let current = run_position(run);
        return run
            .steps()
            .into_iter()
            .enumerate()
            .map(|(index, step)| OutlineBlock {
                label: run_step_label(step).to_uppercase(),
                mark: BlockMark::None,
                lines: Vec::new(),
                doc: None,
                current: current == Some(index),
            })
            .collect();
    }
    let Some(workflow) = &state.workflow else {
        return Vec::new();
    };
    let waiting = gate.and_then(|gate| Gate::of(&gate.kind)).map(Gate::step);
    workflow
        .kind
        .steps()
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let doc = step_doc(workflow, step);
            let mark = if waiting == Some(*step) {
                BlockMark::Gate
            } else if doc.is_some_and(|doc| doc.approved_at.is_some()) {
                BlockMark::Approved
            } else {
                BlockMark::None
            };
            OutlineBlock {
                label: step.to_uppercase(),
                mark,
                lines: doc.map(lines).unwrap_or_default(),
                doc: doc.map(|doc| doc.key.clone()),
                current: index == workflow.step,
            }
        })
        .collect()
}

/// What the outline tells about `doc`: its path and its verdict.
fn lines(doc: &BmadDoc) -> Vec<OutlineLine> {
    let path = doc
        .path
        .iter()
        .map(|path| OutlineLine::Path(path.display().to_string()));
    let verdict = doc
        .verdict
        .map(|verdict| OutlineLine::Note(verdict_word(verdict).to_owned()));
    path.chain(verdict).collect()
}

/// `verdict` as the UI names it, such as `concerns`.
fn verdict_word(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Readiness(Readiness::Pass) => "pass",
        Verdict::Readiness(Readiness::Concerns) => "concerns",
        Verdict::Readiness(Readiness::Fail) => "fail",
        Verdict::Review(ReviewVerdict::Approved) => "approved",
        Verdict::Review(ReviewVerdict::ChangesRequested) => "changes requested",
        Verdict::Retrospective(RetroVerdict::Accepted) => "accepted",
        Verdict::Retrospective(RetroVerdict::AcceptedWithOpenItems) => "accepted with open items",
        Verdict::Retrospective(RetroVerdict::Rejected) => "rejected",
    }
}

/// The documents of the workflow standing at `state`: one per file its gates asked about, a
/// story order with its stories as tasks.
pub(super) fn documents(state: &State) -> Vec<DocRef> {
    documents_of(state)
        .iter()
        .map(|doc| DocRef {
            key: doc.key.clone(),
            label: doc.label.clone(),
            reported: doc.path.as_ref().map(|path| ReportedDoc {
                path: path.clone(),
                rev: doc.rev,
                at: doc.at,
            }),
            tasks: (!doc.stories.is_empty()).then(|| TaskList {
                rows: doc
                    .stories
                    .iter()
                    .map(|story| TaskRowView {
                        n: story.id.clone(),
                        title: story.title.clone(),
                        files: Vec::new(),
                        done: false,
                        current: false,
                    })
                    .collect(),
                footnote: None,
            }),
        })
        .collect()
}

/// The files the workflow standing at `state` asked about.
fn documents_of(state: &State) -> &[BmadDoc] {
    state
        .workflow
        .as_ref()
        .map_or(&[], |workflow| workflow.documents.as_slice())
}

/// The tree's step cell for the workflow standing at `state` with `gate` waiting.
pub(super) fn step_cell(state: &State, gate: Option<&OpenGate>) -> StepCell {
    if let Some(run) = &state.run {
        return StepCell {
            label: run
                .current
                .map_or(BmadV6_12::ID.chip(), run_step_label)
                .to_owned(),
            gate_waiting: gate.is_some(),
        };
    }
    let label = match (&state.workflow, gate) {
        (None, _) => BmadV6_12::ID.chip().to_owned(),
        (Some(workflow), Some(gate)) => format!(
            "{} · {}",
            workflow.kind.label(),
            Gate::of(&gate.kind).map_or("gate", cell_word)
        ),
        (Some(workflow), None) if workflow.finished.is_some() => {
            format!("{} · done", workflow.kind.label())
        }
        (Some(workflow), None) => format!(
            "{} · {}",
            workflow.kind.label(),
            workflow
                .kind
                .steps()
                .get(workflow.step)
                .copied()
                .unwrap_or_default()
        ),
    };
    StepCell {
        label,
        gate_waiting: gate.is_some(),
    }
}

/// What the tree's step cell calls `gate` while it waits.
fn cell_word(gate: Gate) -> &'static str {
    match gate {
        Gate::Document => "approval",
        Gate::StoryOrder => "story order",
        Gate::Readiness => "readiness",
        Gate::BuildPlan => "plan gate",
        Gate::BuildReview => "review gate",
        Gate::ReviewVerdict | Gate::RetrospectiveVerdict => "verdict",
    }
}

/// What the UI shows of `gate`, waiting at `state`: its words, and the file it asks about once
/// that was reported.
pub(super) fn gate_view(state: &State, gate: &OpenGate) -> GateView {
    let texts = texts(gate);
    let doc = documents_of(state)
        .iter()
        .find(|doc| doc.key == gate.kind.as_str())
        .map(|doc| doc.key.clone());
    GateView {
        approve_label: texts.approve_label.to_owned(),
        change_prefix: texts.change_prefix.to_owned(),
        hint: texts.hint.to_owned(),
        doc,
    }
}

/// The transcript row of `event`: a row for starting a workflow with a persona, naming the
/// kind and the story; `None` for every other event.
pub(super) fn transcript_row(event: &Event) -> Option<WorkflowRow> {
    let Event::StartWorkflow {
        kind,
        persona: Some(persona),
        story,
        ..
    } = event
    else {
        return None;
    };
    let text = match story {
        Some(story) => format!("{} · story {story}", kind.label()),
        None => kind.label().to_owned(),
    };
    Some(WorkflowRow::Persona {
        persona: persona.clone(),
        text,
        path: None,
    })
}

/// The note a transcript shows for `event`: the start of a workflow without a persona, each
/// file ready for the user's approval, and the end of the workflow; `None` for a start with a
/// persona, which has a row of its own.
pub(super) fn step_note(event: &Event) -> Option<String> {
    match event {
        Event::StartWorkflow {
            kind,
            persona: None,
            ..
        } => Some(format!("bmad · {}", kind.label())),
        Event::FinishWorkflow { outcome } => Some(match outcome {
            Some(outcome) => format!("finished · {outcome}"),
            None => "finished".to_owned(),
        }),
        _ => request(event).map(|request| match request.path {
            Some(path) => format!(
                "{} ready for your approval · {}",
                request.label,
                path.display()
            ),
            None => format!("{} ready for your review", request.label),
        }),
    }
}
