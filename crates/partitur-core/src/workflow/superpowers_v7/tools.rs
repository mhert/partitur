//! The superpowers workflow's report tools, the schemas of their arguments, how a call is
//! read as an [`Event`] and the instruction block that tells the agent when to call them.
//!
//! The fourteen tools follow the workflow's stages, each named `superpowers_v7_` and the stage
//! it reports, and each reports one kind of [`Event`]. None of them is a run-level tool, so
//! plain sessions and orchestrators are offered all of them. A call's arguments are read with
//! serde, whose message is the error result when they do not fit; a name that is none of the
//! tools is an unknown tool.

use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::workflow::WorkflowTool;
use crate::workflow::superpowers_v7::{Event, ExecutionMode, Outcome, PlannedTask};

/// The tool that starts a session on a topic.
const START_SESSION: &str = "superpowers_v7_start_session";
/// The tool that reports the start of brainstorming.
const START_BRAINSTORMING: &str = "superpowers_v7_start_brainstorming";
/// The tool that asks for the design's approval.
const REQUEST_DESIGN_APPROVAL: &str = "superpowers_v7_request_design_approval";
/// The tool that reports the start of writing the plan.
const START_WRITING_PLAN: &str = "superpowers_v7_start_writing_plan";
/// The tool that asks for the plan's approval.
const REQUEST_PLAN_APPROVAL: &str = "superpowers_v7_request_plan_approval";
/// The tool that reports the start of the execution.
const START_EXECUTION: &str = "superpowers_v7_start_execution";
/// The tool that reports the start of a task.
const START_TASK: &str = "superpowers_v7_start_task";
/// The tool that reports a task's failing test.
const WRITE_FAILING_TEST: &str = "superpowers_v7_write_failing_test";
/// The tool that reports a task's passing test.
const MAKE_TEST_PASS: &str = "superpowers_v7_make_test_pass";
/// The tool that reports the start of a task's refactoring.
const START_REFACTORING: &str = "superpowers_v7_start_refactoring";
/// The tool that reports a finished task.
const FINISH_TASK: &str = "superpowers_v7_finish_task";
/// The tool that reports a checkpoint.
const REACH_CHECKPOINT: &str = "superpowers_v7_reach_checkpoint";
/// The tool that reports the start of finishing the branch.
const START_FINISHING_BRANCH: &str = "superpowers_v7_start_finishing_branch";
/// The tool that finishes the session.
const FINISH_SESSION: &str = "superpowers_v7_finish_session";

/// The instructions appended to every session's system prompt: when to call the
/// `superpowers_v7_*` tools of Partitur's tool server, and that a gate ends the turn.
pub(super) const INSTRUCTIONS: &str = "<partitur-workflow>
Partitur shows the user where you stand in a spec-driven workflow. When you work with the \
superpowers skills (brainstorming, writing-plans, executing-plans, subagent-driven-development, \
test-driven-development, finishing-a-development-branch), call the matching superpowers_v7_* \
tool of the MCP server \"partitur\" at each event its description names. At a gate \
(superpowers_v7_request_design_approval, superpowers_v7_request_plan_approval, \
superpowers_v7_reach_checkpoint) call the tool, then end your turn: the user's next prompt is \
the decision. Never call these tools when you are not working with superpowers skills.
</partitur-workflow>";

/// The tools the module offers every session to report its workflow, in the order of the
/// workflow's stages. Each is the tool of one [`Event`].
pub(super) const TOOLS: &[WorkflowTool] = &[
    WorkflowTool {
        name: START_SESSION,
        description: "call when you start a superpowers workflow on a topic; a call after a finished workflow starts a new one.",
        schema: topic_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_BRAINSTORMING,
        description: "call when the brainstorming skill starts.",
        schema: empty_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_DESIGN_APPROVAL,
        description: "call when the design document is written; path is relative to your working folder. End your turn after calling it and wait for the user's decision.",
        schema: design_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_WRITING_PLAN,
        description: "call when the writing-plans skill starts.",
        schema: empty_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_PLAN_APPROVAL,
        description: "call when the plan is written; path is relative to your working folder, tasks lists the plan's numbered tasks and checkpoints the task numbers after which you stop for the user. End your turn after calling it and wait for the user's decision.",
        schema: plan_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_EXECUTION,
        description: "call when execution of the approved plan starts; mode is \"subagent\" or \"inline\".",
        schema: mode_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_TASK,
        description: "call when task n of the plan starts.",
        schema: task_schema,
        run_level: false,
    },
    WorkflowTool {
        name: WRITE_FAILING_TEST,
        description: "call after writing the failing test of task n (TDD red).",
        schema: task_schema,
        run_level: false,
    },
    WorkflowTool {
        name: MAKE_TEST_PASS,
        description: "call when the test of task n passes (TDD green).",
        schema: task_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_REFACTORING,
        description: "call when the refactoring of task n starts.",
        schema: task_schema,
        run_level: false,
    },
    WorkflowTool {
        name: FINISH_TASK,
        description: "call when task n is done.",
        schema: task_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REACH_CHECKPOINT,
        description: "call at a checkpoint of the plan, after task after_task; then stop, summarise your progress for the user and end your turn.",
        schema: checkpoint_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_FINISHING_BRANCH,
        description: "call when the finishing-a-development-branch skill starts.",
        schema: empty_schema,
        run_level: false,
    },
    WorkflowTool {
        name: FINISH_SESSION,
        description: "call when the workflow ends; outcome is \"merged\", \"pr_opened\", \"kept\" or \"discarded\".",
        schema: outcome_schema,
        run_level: false,
    },
];

impl Event {
    /// The name of the tool the agent reports this event with.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::StartSession { .. } => START_SESSION,
            Self::StartBrainstorming => START_BRAINSTORMING,
            Self::RequestDesignApproval { .. } => REQUEST_DESIGN_APPROVAL,
            Self::StartWritingPlan => START_WRITING_PLAN,
            Self::RequestPlanApproval { .. } => REQUEST_PLAN_APPROVAL,
            Self::StartExecution { .. } => START_EXECUTION,
            Self::StartTask { .. } => START_TASK,
            Self::WriteFailingTest { .. } => WRITE_FAILING_TEST,
            Self::MakeTestPass { .. } => MAKE_TEST_PASS,
            Self::StartRefactoring { .. } => START_REFACTORING,
            Self::FinishTask { .. } => FINISH_TASK,
            Self::ReachCheckpoint { .. } => REACH_CHECKPOINT,
            Self::StartFinishingBranch => START_FINISHING_BRANCH,
            Self::FinishSession { .. } => FINISH_SESSION,
        }
    }
}

/// An integer argument of at least 1, described by `description`.
fn number(description: &str) -> Value {
    json!({ "type": "integer", "minimum": 1, "description": description })
}

/// The schema of the tool that starts a session: its topic.
fn topic_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "topic": { "type": "string" } },
        "required": ["topic"],
    })
}

/// The schema of the tools without arguments.
fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The schema of the tool that asks for the design's approval: the document's path.
fn design_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "The design document, relative to your working folder",
            },
        },
        "required": ["path"],
    })
}

/// The schema of the tool that asks for the plan's approval: the document's path, its tasks
/// and its checkpoints.
fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "The plan document, relative to your working folder",
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "n": number("The task's number in the plan"),
                        "title": { "type": "string" },
                        "files": { "type": "array", "items": { "type": "string" } },
                    },
                    "required": ["n", "title", "files"],
                },
            },
            "checkpoints": {
                "type": "array",
                "items": { "type": "integer" },
                "description": "The task numbers after which you stop for the user",
            },
        },
        "required": ["path", "tasks", "checkpoints"],
    })
}

/// The schema of the tool that starts the execution: its mode.
fn mode_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "enum": ["subagent", "inline"] } },
        "required": ["mode"],
    })
}

/// The schema of the tools that report a step of one task: its number and a remark.
fn task_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "n": number("The task's number in the plan"),
            "note": { "type": "string", "description": "A short remark" },
        },
        "required": ["n"],
    })
}

/// The schema of the tool that reports a checkpoint: the task it follows.
fn checkpoint_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "after_task": number("The task the checkpoint follows") },
        "required": ["after_task"],
    })
}

/// The schema of the tool that finishes the session: its outcome.
fn outcome_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "outcome": {
                "type": "string",
                "enum": ["merged", "pr_opened", "kept", "discarded"],
            },
        },
        "required": ["outcome"],
    })
}

/// The arguments of the tool that starts a session.
#[derive(Debug, Deserialize)]
struct TopicArguments {
    topic: String,
}

/// The arguments of the tool that asks for the design's approval.
#[derive(Debug, Deserialize)]
struct PathArguments {
    path: PathBuf,
}

/// The arguments of the tool that asks for the plan's approval.
#[derive(Debug, Deserialize)]
struct PlanArguments {
    path: PathBuf,
    tasks: Vec<PlannedTask>,
    checkpoints: Vec<u32>,
}

/// The arguments of the tool that starts the execution.
#[derive(Debug, Deserialize)]
struct ModeArguments {
    mode: ExecutionMode,
}

/// The arguments of the tools that report a step of one task.
#[derive(Debug, Deserialize)]
struct TaskArguments {
    n: u32,
    note: Option<String>,
}

/// The arguments of the tool that reports a checkpoint.
#[derive(Debug, Deserialize)]
struct CheckpointArguments {
    after_task: u32,
}

/// The arguments of the tool that finishes the session.
#[derive(Debug, Deserialize)]
struct OutcomeArguments {
    outcome: Outcome,
}

/// Reads a call of the tool `name` with `arguments` as an event.
///
/// # Errors
///
/// Returns `unknown tool {name}` when `name` is none of the tools, or serde's message when an
/// argument is missing or of the wrong type.
pub(super) fn parse(name: &str, arguments: Value) -> Result<Event, String> {
    let event = match name {
        START_SESSION => {
            let TopicArguments { topic } = arguments_of(arguments)?;
            Event::StartSession { topic }
        }
        START_BRAINSTORMING => Event::StartBrainstorming,
        REQUEST_DESIGN_APPROVAL => {
            let PathArguments { path } = arguments_of(arguments)?;
            Event::RequestDesignApproval { path }
        }
        START_WRITING_PLAN => Event::StartWritingPlan,
        REQUEST_PLAN_APPROVAL => {
            let PlanArguments {
                path,
                tasks,
                checkpoints,
            } = arguments_of(arguments)?;
            Event::RequestPlanApproval {
                path,
                tasks,
                checkpoints,
            }
        }
        START_EXECUTION => {
            let ModeArguments { mode } = arguments_of(arguments)?;
            Event::StartExecution { mode }
        }
        START_TASK => {
            let TaskArguments { n, note } = arguments_of(arguments)?;
            Event::StartTask { n, note }
        }
        WRITE_FAILING_TEST => {
            let TaskArguments { n, note } = arguments_of(arguments)?;
            Event::WriteFailingTest { n, note }
        }
        MAKE_TEST_PASS => {
            let TaskArguments { n, note } = arguments_of(arguments)?;
            Event::MakeTestPass { n, note }
        }
        START_REFACTORING => {
            let TaskArguments { n, note } = arguments_of(arguments)?;
            Event::StartRefactoring { n, note }
        }
        FINISH_TASK => {
            let TaskArguments { n, note } = arguments_of(arguments)?;
            Event::FinishTask { n, note }
        }
        REACH_CHECKPOINT => {
            let CheckpointArguments { after_task } = arguments_of(arguments)?;
            Event::ReachCheckpoint { after_task }
        }
        START_FINISHING_BRANCH => Event::StartFinishingBranch,
        FINISH_SESSION => {
            let OutcomeArguments { outcome } = arguments_of(arguments)?;
            Event::FinishSession { outcome }
        }
        _ => return Err(format!("unknown tool {name}")),
    };
    Ok(event)
}

/// Reads a tool's `arguments` into `T`.
///
/// # Errors
///
/// Returns serde's message when an argument is missing or of the wrong type.
fn arguments_of<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| error.to_string())
}
