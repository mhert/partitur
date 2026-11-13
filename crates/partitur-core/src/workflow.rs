//! The workflow a session reports to Partitur, and the fold from reported events to its status.
//!
//! An agent working through a development workflow (superpowers: brainstorm, design, plan,
//! execute, finish) reports each stage with a tool call. Each report is a [`WorkflowEvent`],
//! persisted in the transcript. [`apply`] folds one event into a [`WorkflowStatus`] and
//! [`rebuild`] folds a whole history, so the status a session shows can always be derived
//! again from its events. The wording the UI shows for a status lives here too, so every
//! view says the same thing. Times are never formatted here: callers pass the local time as
//! text where a note needs one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::orchestrator::ToolSpec;

/// The workflow a session follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workflow {
    /// The superpowers skill set.
    Superpowers,
}

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

/// What the agent waits for the user to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// The design document.
    Design,
    /// The implementation plan.
    Plan,
    /// A checkpoint between tasks.
    Checkpoint,
}

impl GateKind {
    /// The prompt that approves the gate, sent to the agent as the user's message.
    pub fn approval_text(self) -> &'static str {
        match self {
            Self::Design => "Design approved. Write the implementation plan.",
            Self::Plan => "Plan approved. Execute it.",
            Self::Checkpoint => "Continue.",
        }
    }

    /// The start of the prompt the user writes to ask for changes instead of approving.
    pub fn change_prefix(self) -> &'static str {
        match self {
            Self::Design => "Changes to the design: ",
            Self::Plan => "Changes to the plan: ",
            Self::Checkpoint => "Before continuing: ",
        }
    }

    /// The caption of the button that approves the gate.
    pub fn approve_label(self) -> &'static str {
        match self {
            Self::Design => "Approve design",
            Self::Plan => "Approve plan",
            Self::Checkpoint => "Continue",
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

/// How the user closed a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateCloser {
    /// With the approve button.
    Approve,
    /// By sending a prompt of their own.
    Prompt,
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
pub enum SuperpowersEvent {
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

impl SuperpowersEvent {
    /// The name of the tool the agent reports this event with.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::StartSession { .. } => "superpowers_start_session",
            Self::StartBrainstorming => "superpowers_start_brainstorming",
            Self::RequestDesignApproval { .. } => "superpowers_request_design_approval",
            Self::StartWritingPlan => "superpowers_start_writing_plan",
            Self::RequestPlanApproval { .. } => "superpowers_request_plan_approval",
            Self::StartExecution { .. } => "superpowers_start_execution",
            Self::StartTask { .. } => "superpowers_start_task",
            Self::WriteFailingTest { .. } => "superpowers_write_failing_test",
            Self::MakeTestPass { .. } => "superpowers_make_test_pass",
            Self::StartRefactoring { .. } => "superpowers_start_refactoring",
            Self::FinishTask { .. } => "superpowers_finish_task",
            Self::ReachCheckpoint { .. } => "superpowers_reach_checkpoint",
            Self::StartFinishingBranch => "superpowers_start_finishing_branch",
            Self::FinishSession { .. } => "superpowers_finish_session",
        }
    }
}

/// The tools Partitur's tool server offers every session to report its workflow, in the order
/// of the workflow's stages. Each is the tool of one [`SuperpowersEvent`].
pub const SUPERPOWERS_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "superpowers_start_session",
        signature: "superpowers_start_session(topic)",
        description: "call when you start a superpowers workflow on a topic; a call after a finished workflow starts a new one.",
    },
    ToolSpec {
        name: "superpowers_start_brainstorming",
        signature: "superpowers_start_brainstorming()",
        description: "call when the brainstorming skill starts.",
    },
    ToolSpec {
        name: "superpowers_request_design_approval",
        signature: "superpowers_request_design_approval(path)",
        description: "call when the design document is written; path is relative to your working folder. End your turn after calling it and wait for the user's decision.",
    },
    ToolSpec {
        name: "superpowers_start_writing_plan",
        signature: "superpowers_start_writing_plan()",
        description: "call when the writing-plans skill starts.",
    },
    ToolSpec {
        name: "superpowers_request_plan_approval",
        signature: "superpowers_request_plan_approval(path, tasks: [{n, title, files}], checkpoints)",
        description: "call when the plan is written; path is relative to your working folder, tasks lists the plan's numbered tasks and checkpoints the task numbers after which you stop for the user. End your turn after calling it and wait for the user's decision.",
    },
    ToolSpec {
        name: "superpowers_start_execution",
        signature: "superpowers_start_execution(mode)",
        description: "call when execution of the approved plan starts; mode is \"subagent\" or \"inline\".",
    },
    ToolSpec {
        name: "superpowers_start_task",
        signature: "superpowers_start_task(n, note?)",
        description: "call when task n of the plan starts.",
    },
    ToolSpec {
        name: "superpowers_write_failing_test",
        signature: "superpowers_write_failing_test(n, note?)",
        description: "call after writing the failing test of task n (TDD red).",
    },
    ToolSpec {
        name: "superpowers_make_test_pass",
        signature: "superpowers_make_test_pass(n, note?)",
        description: "call when the test of task n passes (TDD green).",
    },
    ToolSpec {
        name: "superpowers_start_refactoring",
        signature: "superpowers_start_refactoring(n, note?)",
        description: "call when the refactoring of task n starts.",
    },
    ToolSpec {
        name: "superpowers_finish_task",
        signature: "superpowers_finish_task(n, note?)",
        description: "call when task n is done.",
    },
    ToolSpec {
        name: "superpowers_reach_checkpoint",
        signature: "superpowers_reach_checkpoint(after_task)",
        description: "call at a checkpoint of the plan, after task after_task; then stop, summarise your progress for the user and end your turn.",
    },
    ToolSpec {
        name: "superpowers_start_finishing_branch",
        signature: "superpowers_start_finishing_branch()",
        description: "call when the finishing-a-development-branch skill starts.",
    },
    ToolSpec {
        name: "superpowers_finish_session",
        signature: "superpowers_finish_session(outcome)",
        description: "call when the workflow ends; outcome is \"merged\", \"pr_opened\", \"kept\" or \"discarded\".",
    },
];

/// The answer to a report that opens a gate: the agent must stop and wait for the user.
pub const GATE_REQUESTED: &str =
    "approval requested; end your turn now and wait for the user's decision";

/// The answer to a checkpoint report: the agent must stop and summarise for the user.
pub const CHECKPOINT_REACHED: &str =
    "checkpoint reached; stop, summarise your progress for the user and end your turn";

/// The answer to every other report.
pub const RECORDED: &str = "recorded";

/// What the agent is told after reporting `event`.
pub fn tool_answer(event: &SuperpowersEvent) -> &'static str {
    match event {
        SuperpowersEvent::RequestDesignApproval { .. }
        | SuperpowersEvent::RequestPlanApproval { .. } => GATE_REQUESTED,
        SuperpowersEvent::ReachCheckpoint { .. } => CHECKPOINT_REACHED,
        _ => RECORDED,
    }
}

/// Something that happened in a session's workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// The agent reported progress.
    Superpowers(SuperpowersEvent),
    /// The user answered an open gate.
    GateClosed {
        /// The gate that was open.
        gate: GateKind,
        /// How the user answered.
        by: GateCloser,
    },
}

/// A gate waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenGate {
    /// What is waiting for approval.
    pub kind: GateKind,
    /// When the gate opened, in seconds since the Unix epoch.
    pub since: u64,
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

/// Where a session stands in its workflow, folded from its [`WorkflowEvent`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStatus {
    /// The workflow followed.
    pub workflow: Workflow,
    /// What the session is about; empty when the agent never said.
    pub topic: String,
    /// How many events were applied, across restarts of the workflow.
    pub applied: u64,
    /// When the latest event was applied, in seconds since the Unix epoch.
    pub updated_at: u64,
    /// The current step, `None` before the first one.
    pub step: Option<Step>,
    /// When brainstorming started.
    pub brainstorm_at: Option<u64>,
    /// The gate waiting for the user.
    pub gate: Option<OpenGate>,
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
}

impl WorkflowStatus {
    fn open(applied: u64) -> Self {
        Self {
            workflow: Workflow::Superpowers,
            topic: String::new(),
            applied,
            updated_at: 0,
            step: None,
            brainstorm_at: None,
            gate: None,
            design: None,
            plan: None,
            tasks: None,
            mode: None,
            outcome: None,
        }
    }

    fn tasks_mut(&mut self) -> &mut TaskProgress {
        self.tasks.get_or_insert_with(|| TaskProgress {
            total: 0,
            done: Vec::new(),
            current: None,
            phase: None,
            checkpoints: Vec::new(),
        })
    }

    fn open_gate(&mut self, kind: GateKind, at: u64) {
        self.gate = Some(OpenGate { kind, since: at });
    }

    fn work_on(&mut self, n: u32, phase: Option<TddPhase>) {
        let tasks = self.tasks_mut();
        tasks.current = Some(n);
        tasks.phase = phase;
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

/// Folds `event`, reported at `at`, into `status`. Without a status the event opens a
/// workflow with an empty topic.
///
/// Every event counts in [`applied`](WorkflowStatus::applied). A `start_session` event drops
/// everything else but that count.
pub fn apply(status: Option<WorkflowStatus>, event: &WorkflowEvent, at: u64) -> WorkflowStatus {
    let mut status = status.unwrap_or_else(|| WorkflowStatus::open(0));
    status.applied = status.applied.saturating_add(1);
    status.updated_at = at;
    match event {
        WorkflowEvent::GateClosed { gate, by } => {
            if status.gate.as_ref().is_some_and(|open| open.kind == *gate) {
                status.gate = None;
            }
            if *by == GateCloser::Approve {
                let document = match gate {
                    GateKind::Design => status.design.as_mut(),
                    GateKind::Plan => status.plan.as_mut(),
                    GateKind::Checkpoint => None,
                };
                if let Some(document) = document {
                    document.approved_at = Some(at);
                }
            }
        }
        WorkflowEvent::Superpowers(event) => apply_superpowers(&mut status, event, at),
    }
    status
}

fn apply_superpowers(status: &mut WorkflowStatus, event: &SuperpowersEvent, at: u64) {
    match event {
        SuperpowersEvent::StartSession { topic } => {
            *status = WorkflowStatus {
                topic: topic.clone(),
                updated_at: at,
                ..WorkflowStatus::open(status.applied)
            };
        }
        SuperpowersEvent::StartBrainstorming => {
            status.step = Some(Step::Brainstorm);
            status.brainstorm_at = Some(at);
        }
        SuperpowersEvent::RequestDesignApproval { path } => {
            status.step = Some(Step::Design);
            status.open_gate(GateKind::Design, at);
            status.design = Some(next_revision(status.design.as_ref(), path, at));
        }
        SuperpowersEvent::StartWritingPlan => status.step = Some(Step::Plan),
        SuperpowersEvent::RequestPlanApproval {
            path,
            tasks,
            checkpoints,
        } => {
            status.step = Some(Step::Plan);
            status.open_gate(GateKind::Plan, at);
            status.plan = Some(next_revision(status.plan.as_ref(), path, at));
            status.tasks = Some(TaskProgress {
                total: u32::try_from(tasks.len()).unwrap_or(u32::MAX),
                done: Vec::new(),
                current: None,
                phase: None,
                checkpoints: checkpoints.clone(),
            });
        }
        SuperpowersEvent::StartExecution { mode } => {
            status.step = Some(Step::Execute);
            status.mode = Some(*mode);
        }
        SuperpowersEvent::StartTask { n, .. } => status.work_on(*n, None),
        SuperpowersEvent::WriteFailingTest { n, .. } => status.work_on(*n, Some(TddPhase::Red)),
        SuperpowersEvent::MakeTestPass { n, .. } => status.work_on(*n, Some(TddPhase::Green)),
        SuperpowersEvent::StartRefactoring { n, .. } => {
            status.work_on(*n, Some(TddPhase::Refactor));
        }
        SuperpowersEvent::FinishTask { n, .. } => {
            let tasks = status.tasks_mut();
            if let Err(position) = tasks.done.binary_search(n) {
                tasks.done.insert(position, *n);
            }
            tasks.phase = None;
        }
        SuperpowersEvent::ReachCheckpoint { .. } => status.open_gate(GateKind::Checkpoint, at),
        SuperpowersEvent::StartFinishingBranch => status.step = Some(Step::Finish),
        SuperpowersEvent::FinishSession { outcome } => {
            status.step = Some(Step::Finish);
            status.outcome = Some(*outcome);
            status.gate = None;
        }
    }
}

/// Folds a session's workflow events, each with the time it was reported, into its status;
/// `None` when there are none.
pub fn rebuild<'a>(
    events: impl IntoIterator<Item = (u64, &'a WorkflowEvent)>,
) -> Option<WorkflowStatus> {
    events
        .into_iter()
        .fold(None, |status, (at, event)| Some(apply(status, event, at)))
}

/// The status as the tree's step column words it, such as `execute · 4/11`.
pub fn step_label(status: &WorkflowStatus) -> String {
    if let Some(outcome) = status.outcome {
        return format!("finish · {}", outcome.label());
    }
    if let Some(gate) = &status.gate {
        return match gate.kind {
            GateKind::Design => "design · gate",
            GateKind::Plan => "plan · gate",
            GateKind::Checkpoint => "execute · checkpoint",
        }
        .to_owned();
    }
    match (status.step, &status.tasks) {
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
        (None, _) => "superpowers".to_owned(),
    }
}

/// The size of a plan, such as `11 tasks · 3 checkpoints`.
pub fn plan_summary(tasks: &TaskProgress) -> String {
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
pub fn execute_detail(tasks: &TaskProgress) -> Option<String> {
    let n = tasks.current?;
    Some(if tasks.total == 0 {
        format!("task {n}")
    } else {
        format!("task {n} of {}", tasks.total)
    })
}

/// Where the plan stops for the user, such as `checkpoints after task 3, 6, 9`; `None` when
/// it never does.
pub fn checkpoints_line(checkpoints: &[u32]) -> Option<String> {
    if checkpoints.is_empty() {
        return None;
    }
    let list: Vec<String> = checkpoints.iter().map(u32::to_string).collect();
    Some(format!("checkpoints after task {}", list.join(", ")))
}

/// The note a transcript shows for `event`. `time` is the event's local time as `HH:MM`, used
/// by the notes about the user answering a gate. Starting a task and the test-driven phases
/// have rows of their own and give `None`.
pub fn step_note(event: &WorkflowEvent, time: &str) -> Option<String> {
    let event = match event {
        WorkflowEvent::GateClosed { gate, by } => {
            return Some(match (gate, by) {
                (GateKind::Design, GateCloser::Approve) => {
                    format!("Design approved by you at {time}")
                }
                (GateKind::Plan, GateCloser::Approve) => format!("Plan approved by you at {time}"),
                (GateKind::Checkpoint, GateCloser::Approve) => {
                    format!("Checkpoint passed by you at {time}")
                }
                (GateKind::Design, GateCloser::Prompt) => {
                    format!("Changes to the design requested at {time}")
                }
                (GateKind::Plan, GateCloser::Prompt) => {
                    format!("Changes to the plan requested at {time}")
                }
                (GateKind::Checkpoint, GateCloser::Prompt) => {
                    format!("Checkpoint answered at {time}")
                }
            });
        }
        WorkflowEvent::Superpowers(event) => event,
    };
    match event {
        SuperpowersEvent::StartSession { topic } => Some(format!("superpowers · {topic}")),
        SuperpowersEvent::StartBrainstorming => Some("brainstorming".to_owned()),
        SuperpowersEvent::RequestDesignApproval { path } => Some(format!(
            "design ready for your approval · {}",
            path.display()
        )),
        SuperpowersEvent::StartWritingPlan => Some("writing the plan".to_owned()),
        SuperpowersEvent::RequestPlanApproval {
            path,
            tasks,
            checkpoints,
        } => {
            let summary = plan_summary(&TaskProgress {
                total: u32::try_from(tasks.len()).unwrap_or(u32::MAX),
                done: Vec::new(),
                current: None,
                phase: None,
                checkpoints: checkpoints.clone(),
            });
            Some(format!(
                "plan ready for your approval · {} · {summary}",
                path.display()
            ))
        }
        SuperpowersEvent::StartExecution { mode } => Some(
            match mode {
                ExecutionMode::Subagent => "executing with subagents",
                ExecutionMode::Inline => "executing inline",
            }
            .to_owned(),
        ),
        SuperpowersEvent::FinishTask { n, note } => Some(match note {
            Some(note) => format!("task {n} done · {note}"),
            None => format!("task {n} done"),
        }),
        SuperpowersEvent::ReachCheckpoint { after_task } => Some(format!(
            "checkpoint after task {after_task} · waiting for your approval"
        )),
        SuperpowersEvent::StartFinishingBranch => Some("finishing the branch".to_owned()),
        SuperpowersEvent::FinishSession { outcome } => {
            Some(format!("finished · {}", outcome.label()))
        }
        SuperpowersEvent::StartTask { .. }
        | SuperpowersEvent::WriteFailingTest { .. }
        | SuperpowersEvent::MakeTestPass { .. }
        | SuperpowersEvent::StartRefactoring { .. } => None,
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

    use super::*;
    use crate::ids::{BackendId, ProjectId, SessionId};
    use crate::session::Session;

    fn sp(event: SuperpowersEvent) -> WorkflowEvent {
        WorkflowEvent::Superpowers(event)
    }

    fn fold(events: &[(u64, WorkflowEvent)]) -> WorkflowStatus {
        let mut status = None;
        for (at, event) in events {
            status = Some(apply(status, event, *at));
        }
        status.unwrap()
    }

    fn task(n: u32) -> PlannedTask {
        PlannedTask {
            n,
            title: format!("Task {n}"),
            files: vec!["src/a.rs".into()],
        }
    }

    fn plan_request(count: u32, checkpoints: Vec<u32>) -> WorkflowEvent {
        sp(SuperpowersEvent::RequestPlanApproval {
            path: "plan.md".into(),
            tasks: (1..=count).map(task).collect(),
            checkpoints,
        })
    }

    fn progress(total: u32, current: Option<u32>) -> TaskProgress {
        TaskProgress {
            total,
            done: Vec::new(),
            current,
            phase: None,
            checkpoints: Vec::new(),
        }
    }

    fn plain_session() -> Session {
        Session {
            id: SessionId::new(),
            project: ProjectId::new(),
            scope: crate::session::Scope::Project,
            cwd: PathBuf::from("/work"),
            title: "t".into(),
            backend_session: None,
            backend: BackendId::trusted("claude"),
            model: None,
            effort: None,
            status: crate::session::Status::Idle,
            parent: None,
            auto_allow: false,
            instructions_injected: false,
            created_at: 1,
            orchestrator: false,
            archived: None,
            workspace: crate::session::Workspace::CheckedOut,
            child_defaults: None,
            first_prompt: String::new(),
            skill: None,
            title_source: crate::TitleSource::Derived,
            last_activity: 0,
            children: crate::Fold::Folded,
            workflow: None,
        }
    }

    fn status_with(step: Option<Step>) -> WorkflowStatus {
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

    #[test]
    fn an_event_before_start_session_opens_a_workflow_with_an_empty_topic() {
        let status = apply(None, &sp(SuperpowersEvent::StartBrainstorming), 10);
        assert_eq!(
            (
                status.topic.as_str(),
                status.step,
                status.brainstorm_at,
                status.applied,
                status.updated_at
            ),
            ("", Some(Step::Brainstorm), Some(10), 1, 10)
        );
    }

    #[test]
    fn start_session_replaces_the_view_but_keeps_counting() {
        let status = fold(&[
            (1, sp(SuperpowersEvent::StartBrainstorming)),
            (
                2,
                sp(SuperpowersEvent::RequestDesignApproval {
                    path: "d.md".into(),
                }),
            ),
            (3, sp(SuperpowersEvent::StartSession { topic: "x".into() })),
        ]);
        assert_eq!(status.applied, 3);
        assert_eq!(status.updated_at, 3);
        assert_eq!(status.step, None);
        assert_eq!(status.topic, "x");
        assert_eq!(status.design, None);
        assert_eq!(status.gate, None);
        assert_eq!(status.brainstorm_at, None);
    }

    #[test]
    fn a_repeated_design_request_bumps_the_revision_and_reopens_the_gate() {
        let request = sp(SuperpowersEvent::RequestDesignApproval {
            path: "d.md".into(),
        });
        let status = fold(&[
            (5, request.clone()),
            (
                6,
                WorkflowEvent::GateClosed {
                    gate: GateKind::Design,
                    by: GateCloser::Approve,
                },
            ),
            (9, request),
        ]);
        assert_eq!(
            status.design,
            Some(Document {
                path: "d.md".into(),
                rev: 2,
                at: 9,
                approved_at: None
            })
        );
        assert_eq!(
            status.gate,
            Some(OpenGate {
                kind: GateKind::Design,
                since: 9
            })
        );
        assert_eq!(status.step, Some(Step::Design));
    }

    #[test]
    fn plan_approval_replaces_the_task_list() {
        let mut status = fold(&[(4, plan_request(11, vec![3, 6, 9]))]);
        assert_eq!(
            status.tasks,
            Some(TaskProgress {
                total: 11,
                done: Vec::new(),
                current: None,
                phase: None,
                checkpoints: vec![3, 6, 9]
            })
        );
        assert_eq!(
            status.gate,
            Some(OpenGate {
                kind: GateKind::Plan,
                since: 4
            })
        );
        assert_eq!(status.step, Some(Step::Plan));
        assert_eq!(status.plan.as_ref().map(|plan| plan.rev), Some(1));
        status = apply(
            Some(status),
            &sp(SuperpowersEvent::StartTask { n: 1, note: None }),
            5,
        );
        status = apply(
            Some(status),
            &sp(SuperpowersEvent::FinishTask { n: 1, note: None }),
            6,
        );
        let status = apply(Some(status), &plan_request(2, Vec::new()), 7);
        let tasks = status.tasks.unwrap();
        assert_eq!(
            (tasks.total, tasks.done, tasks.current),
            (2, Vec::new(), None)
        );
        assert_eq!(status.plan.map(|plan| plan.rev), Some(2));
    }

    #[test]
    fn tasks_and_phases_move_the_current_task() {
        let mut status = None;
        let mut run = |at: u64, event: SuperpowersEvent| {
            status = Some(apply(status.take(), &sp(event), at));
            status.clone().unwrap().tasks.unwrap()
        };
        let tasks = run(1, SuperpowersEvent::StartTask { n: 4, note: None });
        assert_eq!(
            (tasks.total, tasks.current, tasks.phase),
            (0, Some(4), None)
        );
        let tasks = run(2, SuperpowersEvent::WriteFailingTest { n: 4, note: None });
        assert_eq!((tasks.current, tasks.phase), (Some(4), Some(TddPhase::Red)));
        let tasks = run(3, SuperpowersEvent::MakeTestPass { n: 4, note: None });
        assert_eq!(tasks.phase, Some(TddPhase::Green));
        let tasks = run(4, SuperpowersEvent::StartRefactoring { n: 4, note: None });
        assert_eq!(tasks.phase, Some(TddPhase::Refactor));
        let tasks = run(5, SuperpowersEvent::FinishTask { n: 4, note: None });
        assert_eq!((tasks.done.clone(), tasks.phase), (vec![4], None));
        let tasks = run(6, SuperpowersEvent::FinishTask { n: 4, note: None });
        assert_eq!(tasks.done, vec![4]);
        let tasks = run(7, SuperpowersEvent::FinishTask { n: 2, note: None });
        assert_eq!(tasks.done, vec![2, 4]);
    }

    #[test]
    fn approving_closes_only_the_matching_gate_and_stamps_the_document() {
        let design = sp(SuperpowersEvent::RequestDesignApproval {
            path: "d.md".into(),
        });
        let closed = |gate, by| WorkflowEvent::GateClosed { gate, by };

        let status = fold(&[
            (1, design.clone()),
            (2, closed(GateKind::Plan, GateCloser::Approve)),
        ]);
        assert_eq!(status.gate.map(|gate| gate.kind), Some(GateKind::Design));
        assert_eq!(status.plan, None);

        let status = fold(&[
            (1, design.clone()),
            (50, closed(GateKind::Design, GateCloser::Approve)),
        ]);
        assert_eq!(status.gate, None);
        assert_eq!(status.design.unwrap().approved_at, Some(50));

        let status = fold(&[
            (1, design),
            (50, closed(GateKind::Design, GateCloser::Prompt)),
        ]);
        assert_eq!(status.gate, None);
        assert_eq!(status.design.unwrap().approved_at, None);

        let status = fold(&[
            (1, sp(SuperpowersEvent::StartWritingPlan)),
            (2, sp(SuperpowersEvent::ReachCheckpoint { after_task: 1 })),
            (3, closed(GateKind::Checkpoint, GateCloser::Approve)),
        ]);
        assert_eq!(status.step, Some(Step::Plan));
        assert_eq!(status.gate, None);

        let status = fold(&[
            (1, plan_request(2, Vec::new())),
            (60, closed(GateKind::Plan, GateCloser::Approve)),
        ]);
        assert_eq!(status.plan.unwrap().approved_at, Some(60));
        assert_eq!(status.gate, None);
    }

    #[test]
    fn a_checkpoint_opens_a_gate_and_finish_session_closes_it() {
        let status = fold(&[
            (
                1,
                sp(SuperpowersEvent::StartExecution {
                    mode: ExecutionMode::Inline,
                }),
            ),
            (2, sp(SuperpowersEvent::ReachCheckpoint { after_task: 3 })),
        ]);
        assert_eq!(status.mode, Some(ExecutionMode::Inline));
        assert_eq!(status.step, Some(Step::Execute));
        assert_eq!(
            status.gate,
            Some(OpenGate {
                kind: GateKind::Checkpoint,
                since: 2
            })
        );
        let status = apply(Some(status), &sp(SuperpowersEvent::StartFinishingBranch), 3);
        assert_eq!(status.step, Some(Step::Finish));
        assert!(status.gate.is_some());
        let status = apply(
            Some(status),
            &sp(SuperpowersEvent::FinishSession {
                outcome: Outcome::Merged,
            }),
            4,
        );
        assert_eq!(status.outcome, Some(Outcome::Merged));
        assert_eq!(status.gate, None);
        assert_eq!(status.step, Some(Step::Finish));
    }

    #[test]
    fn rebuild_folds_every_event_and_nothing_yields_none() {
        assert_eq!(rebuild([]), None);
        let events = [
            (1, sp(SuperpowersEvent::StartSession { topic: "x".into() })),
            (2, sp(SuperpowersEvent::StartBrainstorming)),
            (3, plan_request(3, vec![2])),
        ];
        let rebuilt = rebuild(events.iter().map(|(at, event)| (*at, event)));
        assert_eq!(rebuilt, Some(fold(&events)));
        assert_eq!(rebuilt.unwrap().applied, 3);
    }

    #[test]
    fn step_labels_follow_the_tree_wording() {
        let label = |status: &WorkflowStatus| step_label(status);
        assert_eq!(label(&status_with(None)), "superpowers");
        assert_eq!(label(&status_with(Some(Step::Brainstorm))), "brainstorm");
        assert_eq!(label(&status_with(Some(Step::Design))), "design");
        assert_eq!(label(&status_with(Some(Step::Plan))), "plan");
        assert_eq!(label(&status_with(Some(Step::Execute))), "execute");
        assert_eq!(label(&status_with(Some(Step::Finish))), "finish");

        let mut status = status_with(Some(Step::Execute));
        status.tasks = Some(progress(11, Some(4)));
        assert_eq!(label(&status), "execute · 4/11");
        status.tasks = Some(progress(0, Some(4)));
        assert_eq!(label(&status), "execute · 4");
        status.tasks = Some(progress(11, None));
        assert_eq!(label(&status), "execute");

        let gate = |kind| OpenGate { kind, since: 1 };
        let mut status = status_with(Some(Step::Design));
        status.gate = Some(gate(GateKind::Design));
        assert_eq!(label(&status), "design · gate");
        status.gate = Some(gate(GateKind::Plan));
        assert_eq!(label(&status), "plan · gate");
        status.gate = Some(gate(GateKind::Checkpoint));
        assert_eq!(label(&status), "execute · checkpoint");

        status.outcome = Some(Outcome::PrOpened);
        assert_eq!(label(&status), "finish · PR opened");
        status.outcome = Some(Outcome::Kept);
        assert_eq!(label(&status), "finish · kept");
        status.outcome = Some(Outcome::Discarded);
        assert_eq!(label(&status), "finish · discarded");
        status.outcome = Some(Outcome::Merged);
        assert_eq!(label(&status), "finish · merged");
    }

    #[test]
    fn summaries_count_tasks_and_checkpoints() {
        let with = |total, checkpoints: Vec<u32>| TaskProgress {
            checkpoints,
            ..progress(total, None)
        };
        assert_eq!(plan_summary(&with(1, vec![])), "1 task");
        assert_eq!(
            plan_summary(&with(11, vec![3, 6, 9])),
            "11 tasks · 3 checkpoints"
        );
        assert_eq!(plan_summary(&with(2, vec![1])), "2 tasks · 1 checkpoint");

        assert_eq!(
            execute_detail(&progress(11, Some(4))).as_deref(),
            Some("task 4 of 11")
        );
        assert_eq!(execute_detail(&progress(11, None)), None);
        assert_eq!(
            execute_detail(&progress(0, Some(4))).as_deref(),
            Some("task 4")
        );

        assert_eq!(checkpoints_line(&[]), None);
        assert_eq!(
            checkpoints_line(&[3, 6, 9]).as_deref(),
            Some("checkpoints after task 3, 6, 9")
        );
        assert_eq!(
            checkpoints_line(&[3]).as_deref(),
            Some("checkpoints after task 3")
        );
    }

    #[test]
    fn step_notes_word_every_event() {
        let note = |event: SuperpowersEvent| step_note(&sp(event), "14:02");
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(
            note(SuperpowersEvent::StartSession {
                topic: "login".into()
            }),
            some("superpowers · login")
        );
        assert_eq!(
            note(SuperpowersEvent::StartBrainstorming),
            some("brainstorming")
        );
        assert_eq!(
            note(SuperpowersEvent::RequestDesignApproval {
                path: "d.md".into()
            }),
            some("design ready for your approval · d.md")
        );
        assert_eq!(
            note(SuperpowersEvent::StartWritingPlan),
            some("writing the plan")
        );
        assert_eq!(
            note(SuperpowersEvent::RequestPlanApproval {
                path: "p.md".into(),
                tasks: vec![task(1), task(2)],
                checkpoints: vec![1],
            }),
            some("plan ready for your approval · p.md · 2 tasks · 1 checkpoint")
        );
        assert_eq!(
            note(SuperpowersEvent::StartExecution {
                mode: ExecutionMode::Subagent
            }),
            some("executing with subagents")
        );
        assert_eq!(
            note(SuperpowersEvent::StartExecution {
                mode: ExecutionMode::Inline
            }),
            some("executing inline")
        );
        assert_eq!(
            note(SuperpowersEvent::FinishTask { n: 4, note: None }),
            some("task 4 done")
        );
        assert_eq!(
            note(SuperpowersEvent::FinishTask {
                n: 4,
                note: Some("all green".into())
            }),
            some("task 4 done · all green")
        );
        assert_eq!(
            note(SuperpowersEvent::ReachCheckpoint { after_task: 3 }),
            some("checkpoint after task 3 · waiting for your approval")
        );
        assert_eq!(
            note(SuperpowersEvent::StartFinishingBranch),
            some("finishing the branch")
        );
        assert_eq!(
            note(SuperpowersEvent::FinishSession {
                outcome: Outcome::PrOpened
            }),
            some("finished · PR opened")
        );
        assert_eq!(note(SuperpowersEvent::StartTask { n: 1, note: None }), None);
        assert_eq!(
            note(SuperpowersEvent::WriteFailingTest { n: 1, note: None }),
            None
        );
        assert_eq!(
            note(SuperpowersEvent::MakeTestPass { n: 1, note: None }),
            None
        );
        assert_eq!(
            note(SuperpowersEvent::StartRefactoring { n: 1, note: None }),
            None
        );
    }

    #[test]
    fn step_notes_word_the_users_answers_with_the_given_time() {
        let some = |text: &str| Some(text.to_owned());
        let closed = |gate, by| step_note(&WorkflowEvent::GateClosed { gate, by }, "14:02");
        let (approve, prompt) = (GateCloser::Approve, GateCloser::Prompt);
        assert_eq!(
            closed(GateKind::Design, approve),
            some("Design approved by you at 14:02")
        );
        assert_eq!(
            closed(GateKind::Plan, approve),
            some("Plan approved by you at 14:02")
        );
        assert_eq!(
            closed(GateKind::Checkpoint, approve),
            some("Checkpoint passed by you at 14:02")
        );
        assert_eq!(
            closed(GateKind::Design, prompt),
            some("Changes to the design requested at 14:02")
        );
        assert_eq!(
            closed(GateKind::Plan, prompt),
            some("Changes to the plan requested at 14:02")
        );
        assert_eq!(
            closed(GateKind::Checkpoint, prompt),
            some("Checkpoint answered at 14:02")
        );
    }

    #[test]
    fn gate_texts_are_the_fixed_copy() {
        assert_eq!(
            GateKind::Design.approval_text(),
            "Design approved. Write the implementation plan."
        );
        assert_eq!(GateKind::Plan.approval_text(), "Plan approved. Execute it.");
        assert_eq!(GateKind::Checkpoint.approval_text(), "Continue.");
        assert_eq!(GateKind::Design.change_prefix(), "Changes to the design: ");
        assert_eq!(GateKind::Plan.change_prefix(), "Changes to the plan: ");
        assert_eq!(GateKind::Checkpoint.change_prefix(), "Before continuing: ");
        assert_eq!(GateKind::Design.approve_label(), "Approve design");
        assert_eq!(GateKind::Plan.approve_label(), "Approve plan");
        assert_eq!(GateKind::Checkpoint.approve_label(), "Continue");
    }

    #[test]
    fn tool_names_carry_the_superpowers_prefix() {
        use SuperpowersEvent as E;
        let tools = [
            (
                E::StartSession {
                    topic: String::new(),
                },
                "superpowers_start_session",
            ),
            (E::StartBrainstorming, "superpowers_start_brainstorming"),
            (
                E::RequestDesignApproval { path: "d".into() },
                "superpowers_request_design_approval",
            ),
            (E::StartWritingPlan, "superpowers_start_writing_plan"),
            (
                E::RequestPlanApproval {
                    path: "p".into(),
                    tasks: Vec::new(),
                    checkpoints: Vec::new(),
                },
                "superpowers_request_plan_approval",
            ),
            (
                E::StartExecution {
                    mode: ExecutionMode::Subagent,
                },
                "superpowers_start_execution",
            ),
            (E::StartTask { n: 1, note: None }, "superpowers_start_task"),
            (
                E::WriteFailingTest { n: 1, note: None },
                "superpowers_write_failing_test",
            ),
            (
                E::MakeTestPass { n: 1, note: None },
                "superpowers_make_test_pass",
            ),
            (
                E::StartRefactoring { n: 1, note: None },
                "superpowers_start_refactoring",
            ),
            (
                E::FinishTask { n: 1, note: None },
                "superpowers_finish_task",
            ),
            (
                E::ReachCheckpoint { after_task: 1 },
                "superpowers_reach_checkpoint",
            ),
            (
                E::StartFinishingBranch,
                "superpowers_start_finishing_branch",
            ),
            (
                E::FinishSession {
                    outcome: Outcome::Kept,
                },
                "superpowers_finish_session",
            ),
        ];
        for (event, name) in &tools {
            assert_eq!(event.tool_name(), *name);
        }
        let catalogued: Vec<_> = SUPERPOWERS_TOOLS.iter().map(|spec| spec.name).collect();
        let reported: Vec<_> = tools.iter().map(|(_, name)| *name).collect();
        assert_eq!(catalogued, reported);
    }

    #[test]
    fn every_superpowers_tool_signature_starts_with_its_name() {
        for spec in SUPERPOWERS_TOOLS {
            assert!(
                spec.signature.starts_with(&format!("{}(", spec.name)),
                "{}",
                spec.signature
            );
        }
    }

    #[test]
    fn labels_name_steps_outcomes_and_phases() {
        let labels: Vec<_> = Step::ALL.iter().map(|step| step.label()).collect();
        assert_eq!(
            labels,
            ["brainstorm", "design", "plan", "execute", "finish"]
        );
        assert_eq!(Outcome::Merged.label(), "merged");
        assert_eq!(TddPhase::Red.label(), "red");
        assert_eq!(TddPhase::Green.label(), "green");
        assert_eq!(TddPhase::Refactor.label(), "refactor");
    }

    #[test]
    fn a_status_round_trips_through_json_and_a_session_without_one_omits_it() {
        let mut status = fold(&[
            (1, sp(SuperpowersEvent::StartSession { topic: "x".into() })),
            (2, plan_request(3, vec![2])),
            (3, sp(SuperpowersEvent::StartTask { n: 1, note: None })),
        ]);
        status.mode = Some(ExecutionMode::Inline);
        status.outcome = Some(Outcome::PrOpened);
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkflowStatus>(&json).unwrap(),
            status
        );

        let event = WorkflowEvent::GateClosed {
            gate: GateKind::Plan,
            by: GateCloser::Prompt,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<WorkflowEvent>(&json).unwrap(), event);

        let mut session = plain_session();
        let value = serde_json::to_value(&session).unwrap();
        assert!(value.get("workflow").is_none());
        assert_eq!(
            serde_json::from_value::<Session>(value).unwrap().workflow,
            None
        );

        session.workflow = Some(status);
        let value = serde_json::to_value(&session).unwrap();
        assert!(value.get("workflow").is_some());
        assert_eq!(serde_json::from_value::<Session>(value).unwrap(), session);
    }
}
