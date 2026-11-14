//! The BMAD workflow's display data, for a session that runs one BMAD workflow and for an
//! orchestrator that leads a whole BMAD run.
//!
//! A session that reports a run shows the run in place of a workflow. The rail's chip names
//! the module, and its steps are the run's steps by name, those before the current step done
//! and all of them once the run ended. The story loop is a box with the phases `build` and
//! `review`, the story's phase active, labelled by where the current story stands: `story 3 of
//! 8` among all stories on the epic path, `epic 2 of 4 · story 3 of 8` on the project path,
//! counting the stories of the current epic; `story ? of 8` before any story, `stories` before
//! any are planned, and `story 9.9` for a story no epic lists. While a gate waits, the rail's
//! right side holds the approve button; otherwise it names the run's persona, the current
//! story's title and, when the story's file was reported, an `open story` link. The tree cell
//! names the current step, the loop as `story 3/8 · review`, the run's outcome once it ended
//! (`run · completed`) and the chip alone before any step. The outline has one block per step,
//! the loop's block noting how many stories of each epic are done. The documents are the story
//! list, each story marked done or current, and the current story's file. A run report that
//! names a persona leaves a row naming it and what it works on; starting the run, planning the
//! stories, starting an epic, starting and finishing a story and finishing the run leave a note
//! otherwise.
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
    BmadDoc, BmadV6_12, Event, Gate, LoopPhase, PlanningDoc, Readiness, RetroVerdict,
    ReviewVerdict, Run, RunOutcome, RunPath, RunStep, State, StoryOutcome, StoryRef, Verdict,
    WorkflowRun, request, texts,
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
            right: run_right(run, gate),
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

/// The rail's steps of `run`, each named by [`run_step_label`], the story loop as a box.
fn run_steps(run: &Run) -> Vec<RailStep> {
    let current = run_position(run);
    run.steps()
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            let state = if run.outcome.is_some() || current.is_some_and(|current| index < current) {
                StepState::Done
            } else if current == Some(index) {
                StepState::Current
            } else {
                StepState::Upcoming
            };
            if step == RunStep::Loop {
                return RailStep::Loop {
                    label: StoryPosition::of(run).label(),
                    phases: LOOP_PHASES
                        .iter()
                        .map(|phase| phase_word(*phase).to_owned())
                        .collect(),
                    active: run
                        .phase
                        .and_then(|phase| LOOP_PHASES.iter().position(|each| *each == phase)),
                    state,
                };
            }
            RailStep::Plain {
                label: run_step_label(step).to_owned(),
                state,
                detail: None,
                doc: None,
            }
        })
        .collect()
}

/// The phases of one round of the story loop, in their order.
const LOOP_PHASES: [LoopPhase; 2] = [LoopPhase::Build, LoopPhase::Review];

/// `phase` as the UI names it.
fn phase_word(phase: LoopPhase) -> &'static str {
    match phase {
        LoopPhase::Build => "build",
        LoopPhase::Review => "review",
    }
}

/// The key of a run's story list.
const STORIES: &str = "stories";

/// The key of the current story's file.
const STORY: &str = "story";

/// Where a run's current story stands among the stories it is counted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoryPosition<'a> {
    /// No stories are planned.
    Unplanned,
    /// The current story is one no epic lists, named by its id.
    Unlisted(&'a str),
    /// The current story among the stories counted, `None` before any story.
    Counted {
        /// The current epic as `(number, of)`, on the project path once one is known.
        epic: Option<(usize, usize)>,
        /// The current story's number among the stories counted.
        story: Option<usize>,
        /// How many stories are counted.
        of: usize,
    },
}

impl<'a> StoryPosition<'a> {
    /// Where `run`'s current story stands: among all stories on the epic path, among the
    /// stories of its epic on the project path.
    fn of(run: &'a Run) -> Self {
        let all: Vec<&StoryRef> = run.epics.iter().flat_map(|epic| &epic.stories).collect();
        let story = run.story.as_deref();
        if let Some(id) = story
            && !all.iter().any(|listed| listed.id == id)
        {
            return Self::Unlisted(id);
        }
        if all.is_empty() {
            return Self::Unplanned;
        }
        let epic = (run.path == RunPath::Project)
            .then(|| {
                run.epics.iter().enumerate().find(|(_, epic)| match story {
                    Some(id) => epic.stories.iter().any(|listed| listed.id == id),
                    None => run.epic.as_ref() == Some(&epic.id),
                })
            })
            .flatten();
        let (epic, counted) = match epic {
            Some((index, epic)) => (
                Some((index + 1, run.epics.len())),
                epic.stories.iter().collect(),
            ),
            None => (None, all),
        };
        Self::Counted {
            epic,
            story: story
                .and_then(|id| counted.iter().position(|listed| listed.id == id))
                .map(|index| index + 1),
            of: counted.len(),
        }
    }

    /// The loop's label on the rail, such as `epic 2 of 4 · story 3 of 8`.
    fn label(self) -> String {
        match self {
            Self::Unplanned => STORIES.to_owned(),
            Self::Unlisted(id) => format!("story {id}"),
            Self::Counted { epic, story, of } => {
                let story = format!("story {} of {of}", number(story));
                match epic {
                    Some((epic, epics)) => format!("epic {epic} of {epics} · {story}"),
                    None => story,
                }
            }
        }
    }

    /// The loop in the tree's step cell, such as `story 3/8`; a story without a count reads as
    /// on the rail.
    fn cell(self) -> String {
        match self {
            Self::Counted { story, of, .. } => format!("story {}/{of}", number(story)),
            Self::Unplanned | Self::Unlisted(_) => self.label(),
        }
    }
}

/// `number` as text, `?` when it is not known.
fn number(number: Option<usize>) -> String {
    number.map_or_else(|| "?".to_owned(), |number| number.to_string())
}

/// Whether `run` has stories planned: an epic of it lists at least one.
fn planned(run: &Run) -> bool {
    run.epics.iter().any(|epic| !epic.stories.is_empty())
}

/// The story of `run` with `id`, `None` when no epic lists it.
fn listed<'a>(run: &'a Run, id: &str) -> Option<&'a StoryRef> {
    run.epics
        .iter()
        .flat_map(|epic| &epic.stories)
        .find(|story| story.id == id)
}

/// The right of an orchestrator's rail: the approve button of a waiting gate, or else the
/// run's persona, the current story's title and the link that opens the story's file.
fn run_right(run: &Run, gate: Option<&OpenGate>) -> RailRight {
    if gate.is_some() {
        return right(None, gate);
    }
    RailRight::Unit {
        persona: run.persona.clone(),
        title: run.story.as_ref().map(|id| {
            listed(run, id).map_or_else(|| format!("story {id}"), |story| story.title.clone())
        }),
        pill: None,
        link: run
            .story_path
            .as_ref()
            .map(|_| ("open story".to_owned(), STORY.to_owned())),
    }
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
            .map(|(index, step)| {
                let stories = step == RunStep::Loop && planned(run);
                OutlineBlock {
                    label: run_step_label(step).to_uppercase(),
                    mark: BlockMark::None,
                    lines: if stories { epic_notes(run) } else { Vec::new() },
                    doc: stories.then(|| STORIES.to_owned()),
                    current: current == Some(index),
                }
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

/// How many stories of each of `run`'s epics are done, such as `epic 2 · 3 of 8 done`.
fn epic_notes(run: &Run) -> Vec<OutlineLine> {
    run.epics
        .iter()
        .map(|epic| {
            let done = epic
                .stories
                .iter()
                .filter(|story| run.done_stories.contains(&story.id))
                .count();
            OutlineLine::Note(format!(
                "epic {} · {done} of {} done",
                epic.id,
                epic.stories.len()
            ))
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

/// The documents of the workflow standing at `state`: a run's story list and current story,
/// then one per file its gates asked about, a story order with its stories as tasks.
pub(super) fn documents(state: &State) -> Vec<DocRef> {
    let run = state.run.iter().flat_map(run_documents);
    run.chain(documents_of(state).iter().map(|doc| DocRef {
        key: doc.key.clone(),
        label: doc.label.clone(),
        reported: doc.path.as_ref().map(|path| ReportedDoc {
            path: path.clone(),
            rev: doc.rev,
            at: doc.at,
        }),
        tasks: (!doc.stories.is_empty()).then(|| {
            TaskList {
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
            }
        }),
    }))
    .collect()
}

/// The documents of `run`: the story list once stories are planned, each story marked done or
/// current, and the current story's file once it was reported.
fn run_documents(run: &Run) -> Vec<DocRef> {
    let stories = planned(run).then(|| DocRef {
        key: STORIES.to_owned(),
        label: STORIES.to_owned(),
        reported: None,
        tasks: Some(TaskList {
            rows: run
                .epics
                .iter()
                .flat_map(|epic| &epic.stories)
                .map(|story| TaskRowView {
                    n: story.id.clone(),
                    title: story.title.clone(),
                    files: Vec::new(),
                    done: run.done_stories.contains(&story.id),
                    current: run.story.as_ref() == Some(&story.id),
                })
                .collect(),
            footnote: None,
        }),
    });
    let story = run
        .story
        .as_ref()
        .zip(run.story_path.as_ref())
        .map(|(id, path)| DocRef {
            key: STORY.to_owned(),
            label: format!("story {id}"),
            reported: Some(ReportedDoc {
                path: path.clone(),
                rev: run.story_rev,
                at: run.story_at,
            }),
            tasks: None,
        });
    stories.into_iter().chain(story).collect()
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
            label: run_cell(run),
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

/// The tree's step cell of `run`: its outcome once it ended, or else its current step, the
/// loop with where its story stands and its phase.
fn run_cell(run: &Run) -> String {
    if let Some(outcome) = run.outcome {
        return format!("run · {}", outcome_word(outcome));
    }
    match run.current {
        None => BmadV6_12::ID.chip().to_owned(),
        Some(RunStep::Loop) => {
            let position = StoryPosition::of(run).cell();
            match run.phase {
                Some(phase) => format!("{position} · {}", phase_word(phase)),
                None => position,
            }
        }
        Some(step) => run_step_label(step).to_owned(),
    }
}

/// `outcome` as the UI names it.
fn outcome_word(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Completed => "completed",
        RunOutcome::Abandoned => "abandoned",
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

/// The transcript row of `event`, given the `state` after it: a row for a report that names a
/// persona, naming it and what it works on; `None` for every other event, and for the review
/// of a story that is not the current one, which changes nothing.
pub(super) fn transcript_row(state: &State, event: &Event) -> Option<WorkflowRow> {
    let (persona, text, path) = match event {
        Event::StartWorkflow {
            kind,
            persona: Some(persona),
            story,
            ..
        } => (
            persona,
            match story {
                Some(story) => format!("{} · story {story}", kind.label()),
                None => kind.label().to_owned(),
            },
            None,
        ),
        Event::StartPlanning {
            doc,
            persona: Some(persona),
        } => (
            persona,
            run_step_label(RunStep::Planning(*doc)).to_owned(),
            None,
        ),
        Event::StartSprintPlanning {
            persona: Some(persona),
        } => (persona, "sprint planning".to_owned(), None),
        Event::StartStory {
            id,
            path,
            persona: Some(persona),
        } => {
            let title = state
                .run
                .as_ref()
                .and_then(|run| listed(run, id))
                .map(|story| &story.title);
            let text = match title {
                Some(title) => format!("story {id} · {title}"),
                None => format!("story {id}"),
            };
            (persona, text, path.clone())
        }
        Event::StartStoryReview {
            id,
            persona: Some(persona),
        } if state
            .run
            .as_ref()
            .is_some_and(|run| run.story.as_ref() == Some(id)) =>
        {
            (persona, format!("review of story {id}"), None)
        }
        Event::StartRetrospective {
            epic,
            persona: Some(persona),
        } => (persona, format!("retro · epic {epic}"), None),
        _ => return None,
    };
    Some(WorkflowRow::Persona {
        persona: persona.clone(),
        text,
        path,
    })
}

/// The note a transcript shows for `event`: the start of a workflow without a persona, each
/// file ready for the user's approval, the end of the workflow, and a run's start, story plan,
/// epics, stories and end; `None` for a start with a persona, which has a row of its own. A
/// story's start has a note even with a persona, which the transcript shows only when the
/// start leaves no row.
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
        Event::StartRun { topic, path } => Some(format!(
            "bmad · {topic} · {}",
            match path {
                RunPath::Single => "single",
                RunPath::Epic => "epic",
                RunPath::Project => "project",
            }
        )),
        Event::PlanStories { epics } => Some(format!(
            "{} epics · {} stories planned",
            epics.len(),
            epics.iter().map(|epic| epic.stories.len()).sum::<usize>()
        )),
        Event::StartEpic { id } => Some(format!("epic {id}")),
        Event::StartStory { id, .. } => Some(format!("story {id} started")),
        Event::FinishStory { id, outcome } => Some(match outcome {
            StoryOutcome::Done => format!("story {id} done"),
            StoryOutcome::ChangesRequested => format!("story {id} back to build"),
        }),
        Event::FinishRun { outcome } => Some(format!("run {}", outcome_word(*outcome))),
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
