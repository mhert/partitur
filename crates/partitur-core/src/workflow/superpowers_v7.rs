//! The superpowers workflow (brainstorm, design, plan, execute, finish), version 7, as a
//! workflow module.
//!
//! The agent reports each stage with a `superpowers_v7_*` tool call, read as an [`Event`]. The
//! module folds the events into its [`State`]: the step it stands at, the design and plan
//! documents with their revisions and approvals, the plan's tasks and how far their execution
//! has come. Asking for the design's or the plan's approval opens the `design` or `plan` gate
//! and reaching a checkpoint the `checkpoint` gate; finishing the session closes any gate.
//! Starting a session over drops everything the module knew. Approving the design or plan
//! gate stamps the document as approved; answering a gate with a prompt does not.
//!
//! The module words its gates (the approve button, the approval prompt, the start of a
//! prompt asking for changes, the word an orchestrator is told and the hint under the gate)
//! and its events' transcript notes. Its tools, their schemas and the instruction block are
//! described in [`tools`], its display data in [`display`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::display::{DocRef, GateView, OutlineBlock, RailView, StepCell, WorkflowRow};
use crate::workflow::{
    CHECKPOINT_REACHED, GATE_REQUESTED, GateChange, GateCloser, GateKind, GateTexts, ModuleId,
    OpenGate, RECORDED, WorkflowModule, WorkflowTool,
};

mod display;
#[cfg(test)]
mod tests;
mod tools;

/// The superpowers workflow, version 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperpowersV7;

/// The kind of the gate at the design document.
pub const DESIGN: &str = "design";

/// The kind of the gate at the implementation plan.
pub const PLAN: &str = "plan";

/// The kind of the gate at a checkpoint between tasks.
pub const CHECKPOINT: &str = "checkpoint";

/// One stage of the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Exploring the idea with the user.
    Brainstorm,
    /// Writing and agreeing the design.
    Design,
    /// Writing and agreeing the implementation plan.
    Plan,
    /// Carrying out the plan task by task.
    Execute,
    /// Wrapping up the branch.
    Finish,
}

impl Step {
    /// Every step, in the order the workflow runs them.
    pub const ALL: [Self; 5] = [
        Self::Brainstorm,
        Self::Design,
        Self::Plan,
        Self::Execute,
        Self::Finish,
    ];

    /// The step's name as the UI shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Brainstorm => "brainstorm",
            Self::Design => "design",
            Self::Plan => "plan",
            Self::Execute => "execute",
            Self::Finish => "finish",
        }
    }
}

/// How the plan is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// A fresh subagent per task.
    Subagent,
    /// The session itself works through the tasks.
    Inline,
}

/// How a finished session ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The branch was merged.
    Merged,
    /// A pull request was opened.
    PrOpened,
    /// The branch was kept as it is.
    Kept,
    /// The work was discarded.
    Discarded,
}

impl Outcome {
    /// The outcome as the UI shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::PrOpened => "PR opened",
            Self::Kept => "kept",
            Self::Discarded => "discarded",
        }
    }
}

/// The phase of a task's test-driven cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TddPhase {
    /// A failing test is written.
    Red,
    /// The test passes.
    Green,
    /// The code is cleaned up.
    Refactor,
}

impl TddPhase {
    /// The phase as the UI shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Refactor => "refactor",
        }
    }
}

/// One task of an implementation plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTask {
    /// The task's number in the plan.
    pub n: u32,
    /// What the task does.
    pub title: String,
    /// The files the task touches.
    pub files: Vec<String>,
}

/// A report the agent made about the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// The workflow starts over, on `topic`.
    StartSession {
        /// What the session is about.
        topic: String,
    },
    /// Brainstorming starts.
    StartBrainstorming,
    /// The design is written and waits for the user's approval.
    RequestDesignApproval {
        /// The design document.
        path: PathBuf,
    },
    /// Writing the plan starts.
    StartWritingPlan,
    /// The plan is written and waits for the user's approval.
    RequestPlanApproval {
        /// The plan document.
        path: PathBuf,
        /// The plan's tasks.
        tasks: Vec<PlannedTask>,
        /// The tasks after which the agent stops for the user.
        checkpoints: Vec<u32>,
    },
    /// Execution starts.
    StartExecution {
        /// How the plan is executed.
        mode: ExecutionMode,
    },
    /// Work on a task starts.
    StartTask {
        /// The task's number.
        n: u32,
        /// A remark on the task.
        note: Option<String>,
    },
    /// A task's failing test is written.
    WriteFailingTest {
        /// The task's number.
        n: u32,
        /// A remark on the step.
        note: Option<String>,
    },
    /// A task's test passes.
    MakeTestPass {
        /// The task's number.
        n: u32,
        /// A remark on the step.
        note: Option<String>,
    },
    /// A task's refactoring starts.
    StartRefactoring {
        /// The task's number.
        n: u32,
        /// A remark on the step.
        note: Option<String>,
    },
    /// A task is done.
    FinishTask {
        /// The task's number.
        n: u32,
        /// A remark on the result.
        note: Option<String>,
    },
    /// The agent stops after a task and waits for the user.
    ReachCheckpoint {
        /// The task the checkpoint follows.
        after_task: u32,
    },
    /// Finishing the branch starts.
    StartFinishingBranch,
    /// The session is finished.
    FinishSession {
        /// How it ended up.
        outcome: Outcome,
    },
}

/// The latest revision of a design or plan document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Where the document is.
    pub path: PathBuf,
    /// How often the agent asked for approval of it, starting at 1.
    pub rev: u32,
    /// When the latest revision was submitted, in seconds since the Unix epoch.
    pub at: u64,
    /// When the user approved it, `None` until they did.
    pub approved_at: Option<u64>,
}

/// How far the plan's execution has come.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgress {
    /// How many tasks the plan has; `0` when the agent never submitted a plan.
    pub total: u32,
    /// The finished tasks, ascending.
    pub done: Vec<u32>,
    /// The task being worked on.
    pub current: Option<u32>,
    /// The phase of the current task's cycle.
    pub phase: Option<TddPhase>,
    /// The tasks after which the agent stops for the user.
    pub checkpoints: Vec<u32>,
}

/// Where a session stands in the superpowers workflow, folded from its [`Event`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// What the session is about; empty when the agent never said.
    pub topic: String,
    /// The current step, `None` before the first one.
    pub step: Option<Step>,
    /// When brainstorming started.
    pub brainstorm_at: Option<u64>,
    /// The design document.
    pub design: Option<Document>,
    /// The plan document.
    pub plan: Option<Document>,
    /// The plan's execution.
    pub tasks: Option<TaskProgress>,
    /// How the plan is executed.
    pub mode: Option<ExecutionMode>,
    /// How the session ended up, once finished.
    pub outcome: Option<Outcome>,
    /// The tasks of the latest plan the agent submitted.
    pub planned: Vec<PlannedTask>,
}

impl State {
    fn tasks_mut(&mut self) -> &mut TaskProgress {
        self.tasks.get_or_insert_with(|| TaskProgress {
            total: 0,
            done: Vec::new(),
            current: None,
            phase: None,
            checkpoints: Vec::new(),
        })
    }

    fn work_on(&mut self, n: u32, phase: Option<TddPhase>) {
        let tasks = self.tasks_mut();
        tasks.current = Some(n);
        tasks.phase = phase;
    }
}

/// The gates of the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// The design waits for approval.
    Design,
    /// The plan waits for approval.
    Plan,
    /// A checkpoint between tasks.
    Checkpoint,
}

impl Gate {
    /// The gate of `kind`, `None` for a kind the workflow never opens.
    fn of(kind: &GateKind) -> Option<Self> {
        match kind.as_str() {
            DESIGN => Some(Self::Design),
            PLAN => Some(Self::Plan),
            CHECKPOINT => Some(Self::Checkpoint),
            _ => None,
        }
    }
}

/// The next revision of `previous`, submitted at `at`.
fn next_revision(previous: Option<&Document>, path: &Path, at: u64) -> Document {
    Document {
        path: path.to_path_buf(),
        rev: previous.map_or(1, |document| document.rev.saturating_add(1)),
        at,
        approved_at: None,
    }
}

/// The progress of a fresh plan of `count` tasks that stops after `checkpoints`.
fn fresh_progress(count: usize, checkpoints: &[u32]) -> TaskProgress {
    TaskProgress {
        total: u32::try_from(count).unwrap_or(u32::MAX),
        done: Vec::new(),
        current: None,
        phase: None,
        checkpoints: checkpoints.to_vec(),
    }
}

impl WorkflowModule for SuperpowersV7 {
    type Event = Event;
    type State = State;
    const ID: ModuleId = ModuleId::SuperpowersV7;

    fn tools() -> &'static [WorkflowTool] {
        tools::TOOLS
    }

    fn parse(name: &str, arguments: Value) -> Result<Event, String> {
        tools::parse(name, arguments)
    }

    fn instructions() -> &'static str {
        tools::INSTRUCTIONS
    }

    fn starts_over(event: &Event) -> bool {
        matches!(event, Event::StartSession { .. })
    }

    fn apply(state: &mut State, event: &Event, at: u64) -> GateChange {
        match event {
            Event::StartSession { topic } => state.topic.clone_from(topic),
            Event::StartBrainstorming => {
                state.step = Some(Step::Brainstorm);
                state.brainstorm_at = Some(at);
            }
            Event::RequestDesignApproval { path } => {
                state.step = Some(Step::Design);
                state.design = Some(next_revision(state.design.as_ref(), path, at));
                return GateChange::Open(GateKind::new(DESIGN));
            }
            Event::StartWritingPlan => state.step = Some(Step::Plan),
            Event::RequestPlanApproval {
                path,
                tasks,
                checkpoints,
            } => {
                state.step = Some(Step::Plan);
                state.plan = Some(next_revision(state.plan.as_ref(), path, at));
                state.tasks = Some(fresh_progress(tasks.len(), checkpoints));
                state.planned.clone_from(tasks);
                return GateChange::Open(GateKind::new(PLAN));
            }
            Event::StartExecution { mode } => {
                state.step = Some(Step::Execute);
                state.mode = Some(*mode);
            }
            Event::StartTask { n, .. } => state.work_on(*n, None),
            Event::WriteFailingTest { n, .. } => state.work_on(*n, Some(TddPhase::Red)),
            Event::MakeTestPass { n, .. } => state.work_on(*n, Some(TddPhase::Green)),
            Event::StartRefactoring { n, .. } => state.work_on(*n, Some(TddPhase::Refactor)),
            Event::FinishTask { n, .. } => {
                let tasks = state.tasks_mut();
                if let Err(position) = tasks.done.binary_search(n) {
                    tasks.done.insert(position, *n);
                }
                tasks.phase = None;
            }
            Event::ReachCheckpoint { .. } => return GateChange::Open(GateKind::new(CHECKPOINT)),
            Event::StartFinishingBranch => state.step = Some(Step::Finish),
            Event::FinishSession { outcome } => {
                state.step = Some(Step::Finish);
                state.outcome = Some(*outcome);
                return GateChange::Close;
            }
        }
        GateChange::Keep
    }

    fn gate_closed(state: &mut State, gate: &GateKind, by: GateCloser, at: u64) {
        if by != GateCloser::Approve {
            return;
        }
        let document = match Gate::of(gate) {
            Some(Gate::Design) => state.design.as_mut(),
            Some(Gate::Plan) => state.plan.as_mut(),
            Some(Gate::Checkpoint) | None => None,
        };
        if let Some(document) = document {
            document.approved_at = Some(at);
        }
    }

    fn tool_answer(event: &Event) -> &'static str {
        match event {
            Event::RequestDesignApproval { .. } | Event::RequestPlanApproval { .. } => {
                GATE_REQUESTED
            }
            Event::ReachCheckpoint { .. } => CHECKPOINT_REACHED,
            _ => RECORDED,
        }
    }

    fn gate_texts(gate: &GateKind) -> Option<GateTexts> {
        Some(match Gate::of(gate)? {
            Gate::Design => GateTexts {
                approve_label: "Approve design",
                approval_text: "Design approved. Write the implementation plan.",
                change_prefix: "Changes to the design: ",
                word: "design",
                hint: "approval is written to the transcript · the plan is written next",
            },
            Gate::Plan => GateTexts {
                approve_label: "Approve plan",
                approval_text: "Plan approved. Execute it.",
                change_prefix: "Changes to the plan: ",
                word: "plan",
                hint: "approval is written to the transcript · execution starts with task 1",
            },
            Gate::Checkpoint => GateTexts {
                approve_label: "Continue",
                approval_text: "Continue.",
                change_prefix: "Before continuing: ",
                word: "checkpoint",
                hint: "approval is written to the transcript · execution continues",
            },
        })
    }

    fn rail(state: &State, gate: Option<&OpenGate>) -> RailView {
        display::rail(state, gate)
    }

    fn outline(state: &State, gate: Option<&OpenGate>) -> Vec<OutlineBlock> {
        display::outline(state, gate)
    }

    fn documents(state: &State) -> Vec<DocRef> {
        display::documents(state)
    }

    fn step_cell(state: &State, gate: Option<&OpenGate>) -> StepCell {
        display::step_cell(state, gate)
    }

    fn gate_view(state: &State, gate: &OpenGate) -> GateView {
        display::gate_view(state, gate)
    }

    fn transcript_row(state: &State, event: &Event) -> Option<WorkflowRow> {
        display::transcript_row(state, event)
    }

    fn step_note(event: &Event) -> Option<String> {
        match event {
            Event::StartSession { topic } => Some(format!("superpowers · {topic}")),
            Event::StartBrainstorming => Some("brainstorming".to_owned()),
            Event::RequestDesignApproval { path } => Some(format!(
                "design ready for your approval · {}",
                path.display()
            )),
            Event::StartWritingPlan => Some("writing the plan".to_owned()),
            Event::RequestPlanApproval {
                path,
                tasks,
                checkpoints,
            } => {
                let summary = plan_summary(&fresh_progress(tasks.len(), checkpoints));
                Some(format!(
                    "plan ready for your approval · {} · {summary}",
                    path.display()
                ))
            }
            Event::StartExecution { mode } => Some(
                match mode {
                    ExecutionMode::Subagent => "executing with subagents",
                    ExecutionMode::Inline => "executing inline",
                }
                .to_owned(),
            ),
            Event::FinishTask { n, note } => Some(match note {
                Some(note) => format!("task {n} done · {note}"),
                None => format!("task {n} done"),
            }),
            Event::ReachCheckpoint { after_task } => Some(format!(
                "checkpoint after task {after_task} · waiting for your approval"
            )),
            Event::StartFinishingBranch => Some("finishing the branch".to_owned()),
            Event::FinishSession { outcome } => Some(format!("finished · {}", outcome.label())),
            Event::StartTask { .. }
            | Event::WriteFailingTest { .. }
            | Event::MakeTestPass { .. }
            | Event::StartRefactoring { .. } => None,
        }
    }

    fn gate_note(gate: &GateKind, by: GateCloser, time: &str) -> Option<String> {
        Some(match (Gate::of(gate)?, by) {
            (Gate::Design, GateCloser::Approve) => format!("Design approved by you at {time}"),
            (Gate::Plan, GateCloser::Approve) => format!("Plan approved by you at {time}"),
            (Gate::Checkpoint, GateCloser::Approve) => {
                format!("Checkpoint passed by you at {time}")
            }
            (Gate::Design, GateCloser::Prompt) => {
                format!("Changes to the design requested at {time}")
            }
            (Gate::Plan, GateCloser::Prompt) => format!("Changes to the plan requested at {time}"),
            (Gate::Checkpoint, GateCloser::Prompt) => format!("Checkpoint answered at {time}"),
        })
    }
}

/// The state as the tree's step column words it, with `gate` waiting, such as
/// `execute · 4/11`.
fn step_label(state: &State, gate: Option<&OpenGate>) -> String {
    if let Some(outcome) = state.outcome {
        return format!("finish · {}", outcome.label());
    }
    if let Some(gate) = gate {
        return match Gate::of(&gate.kind) {
            Some(Gate::Design) => "design · gate",
            Some(Gate::Plan) => "plan · gate",
            Some(Gate::Checkpoint) => "execute · checkpoint",
            None => "gate",
        }
        .to_owned();
    }
    match (state.step, &state.tasks) {
        (
            Some(Step::Execute),
            Some(TaskProgress {
                total,
                current: Some(n),
                ..
            }),
        ) => {
            if *total == 0 {
                format!("execute · {n}")
            } else {
                format!("execute · {n}/{total}")
            }
        }
        (Some(step), _) => step.label().to_owned(),
        (None, _) => ModuleId::SuperpowersV7.chip().to_owned(),
    }
}

/// The size of a plan, such as `11 tasks · 3 checkpoints`.
fn plan_summary(tasks: &TaskProgress) -> String {
    let count = match tasks.total {
        1 => "1 task".to_owned(),
        total => format!("{total} tasks"),
    };
    match tasks.checkpoints.len() {
        0 => count,
        1 => format!("{count} · 1 checkpoint"),
        checkpoints => format!("{count} · {checkpoints} checkpoints"),
    }
}

/// The task being executed, such as `task 4 of 11`; `None` when there is none.
fn execute_detail(tasks: &TaskProgress) -> Option<String> {
    let n = tasks.current?;
    Some(if tasks.total == 0 {
        format!("task {n}")
    } else {
        format!("task {n} of {}", tasks.total)
    })
}

/// Where the plan stops for the user, such as `checkpoints after task 3, 6, 9`; `None` when
/// it never does.
fn checkpoints_line(checkpoints: &[u32]) -> Option<String> {
    if checkpoints.is_empty() {
        return None;
    }
    let list: Vec<String> = checkpoints.iter().map(u32::to_string).collect();
    Some(format!("checkpoints after task {}", list.join(", ")))
}
