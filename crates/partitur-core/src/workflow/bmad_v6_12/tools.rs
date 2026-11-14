//! The BMAD Method's report tools, the schemas of their arguments, how a call is read as an
//! [`Event`] and the instruction block that tells the agent when to call them.
//!
//! Every tool is named `bmad_v6_12_` and what it reports. Nine are workflow-level tools,
//! offered to every session: one starts the session's workflow, seven ask the user about a
//! file at one of BMAD's approval points and one finishes the workflow. Ten are run-level
//! tools, offered to orchestrators only: they report the whole run, its planning documents,
//! its story plan, sprint planning, each epic and story through build and review, the
//! retrospective and the run's end. A call's arguments are read with serde, whose message is
//! the error result when they do not fit; a name that is none of the tools is an unknown
//! tool.

use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::workflow::WorkflowTool;
use crate::workflow::bmad_v6_12::{
    DocKind, EpicPlan, Event, PlanningDoc, Readiness, RetroVerdict, ReviewVerdict, RunOutcome,
    RunPath, StoryOutcome, StoryRef, WorkflowKind,
};

/// The tool that starts the session's workflow.
const START_WORKFLOW: &str = "bmad_v6_12_start_workflow";
/// The tool that asks for a planning document's or a spec's approval.
const REQUEST_DOCUMENT_APPROVAL: &str = "bmad_v6_12_request_document_approval";
/// The tool that asks for a story order's approval.
const REQUEST_STORY_ORDER_APPROVAL: &str = "bmad_v6_12_request_story_order_approval";
/// The tool that asks the user about sprint planning's readiness verdict.
const REQUEST_READINESS_DECISION: &str = "bmad_v6_12_request_readiness_decision";
/// The tool that asks for a build plan's approval.
const REQUEST_BUILD_PLAN_APPROVAL: &str = "bmad_v6_12_request_build_plan_approval";
/// The tool that asks the user to review a build before its pull request.
const REQUEST_BUILD_REVIEW: &str = "bmad_v6_12_request_build_review";
/// The tool that asks the user about a code review's verdict.
const REQUEST_REVIEW_VERDICT: &str = "bmad_v6_12_request_review_verdict";
/// The tool that asks the user about a retrospective's verdict.
const REQUEST_RETROSPECTIVE_VERDICT: &str = "bmad_v6_12_request_retrospective_verdict";
/// The tool that finishes the session's workflow.
const FINISH_WORKFLOW: &str = "bmad_v6_12_finish_workflow";
/// The tool that starts an orchestrator's run.
const START_RUN: &str = "bmad_v6_12_start_run";
/// The tool that starts a planning document of the run.
const START_PLANNING: &str = "bmad_v6_12_start_planning";
/// The tool that records the run's epics and stories.
const PLAN_STORIES: &str = "bmad_v6_12_plan_stories";
/// The tool that starts the run's sprint planning.
const START_SPRINT_PLANNING: &str = "bmad_v6_12_start_sprint_planning";
/// The tool that starts an epic of the run.
const START_EPIC: &str = "bmad_v6_12_start_epic";
/// The tool that starts building a story of the run.
const START_STORY: &str = "bmad_v6_12_start_story";
/// The tool that starts reviewing a story of the run.
const START_STORY_REVIEW: &str = "bmad_v6_12_start_story_review";
/// The tool that finishes a story's review.
const FINISH_STORY: &str = "bmad_v6_12_finish_story";
/// The tool that starts the retrospective of an epic of the run.
const START_RETROSPECTIVE: &str = "bmad_v6_12_start_retrospective";
/// The tool that finishes the run.
const FINISH_RUN: &str = "bmad_v6_12_finish_run";

/// The instructions appended to every session's system prompt: when to call the
/// `bmad_v6_12_*` tools of Partitur's tool server, that an approval point ends the turn, and
/// that an orchestrator reports the whole run and gives each workflow a child of its own.
pub(super) const INSTRUCTIONS: &str = "<partitur-workflow>
Partitur shows the user where you stand in a BMAD Method workflow (version 6.12). BMAD runs \
each workflow in a fresh chat, so a session runs one: a product brief, a PRFAQ, a PRD, a UX \
design, an architecture, the epics and stories, sprint planning, a spec, the build of a story, \
the code review of a story or the retrospective of an epic. When you run one, call \
bmad_v6_12_start_workflow of the MCP server \"partitur\" as it starts and \
bmad_v6_12_finish_workflow as it ends. BMAD's own approval points end the turn: at each, call \
the matching tool (bmad_v6_12_request_document_approval, \
bmad_v6_12_request_story_order_approval, bmad_v6_12_request_readiness_decision, \
bmad_v6_12_request_build_plan_approval, bmad_v6_12_request_build_review, \
bmad_v6_12_request_review_verdict, bmad_v6_12_request_retrospective_verdict), then end your \
turn: the user's next prompt is the decision. Never call these tools when you are not running a \
BMAD workflow.
The run tools are for the orchestrator only, which leads a whole BMAD run and runs no workflow \
itself: it proposes one child per BMAD workflow and reports the run with bmad_v6_12_start_run, \
bmad_v6_12_start_planning, bmad_v6_12_plan_stories, bmad_v6_12_start_sprint_planning, \
bmad_v6_12_start_epic, bmad_v6_12_start_story, bmad_v6_12_start_story_review, \
bmad_v6_12_finish_story, bmad_v6_12_start_retrospective and bmad_v6_12_finish_run as the run \
reaches each point.
</partitur-workflow>";

/// The tools the module offers every session to report its workflow. Each is the tool of
/// one [`Event`].
pub(super) const TOOLS: &[WorkflowTool] = &[
    WorkflowTool {
        name: START_WORKFLOW,
        description: "call when a BMAD workflow starts in this session; kind names the workflow, persona the BMAD agent running it, story and epic what it works on.",
        schema: start_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_DOCUMENT_APPROVAL,
        description: "call when a planning document or a spec is written and BMAD asks for its approval; path is relative to your working folder. End your turn after calling it and wait for the user's decision.",
        schema: document_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_STORY_ORDER_APPROVAL,
        description: "call when the epics and stories are written and BMAD asks to approve the story order; stories lists them in their order. End your turn after calling it and wait for the user's decision.",
        schema: story_order_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_READINESS_DECISION,
        description: "call when sprint planning's readiness check has its verdict; path is the readiness report. End your turn after calling it and wait for the user's decision.",
        schema: readiness_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_BUILD_PLAN_APPROVAL,
        description: "call when the plan for building a story is written and BMAD asks for its approval. End your turn after calling it and wait for the user's decision.",
        schema: build_plan_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_BUILD_REVIEW,
        description: "call when a story is built, before its pull request is opened; path is the build's summary, if you wrote one. End your turn after calling it and wait for the user's decision.",
        schema: build_review_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_REVIEW_VERDICT,
        description: "call when the code review of a story has its verdict; path is the review. End your turn after calling it and wait for the user's decision.",
        schema: review_verdict_schema,
        run_level: false,
    },
    WorkflowTool {
        name: REQUEST_RETROSPECTIVE_VERDICT,
        description: "call when the retrospective of an epic has its verdict; path is the retrospective. End your turn after calling it and wait for the user's decision.",
        schema: retrospective_verdict_schema,
        run_level: false,
    },
    WorkflowTool {
        name: FINISH_WORKFLOW,
        description: "call when the BMAD workflow of this session ends; outcome says how, in a few words.",
        schema: finish_schema,
        run_level: false,
    },
    WorkflowTool {
        name: START_RUN,
        description: "call as an orchestrator when a BMAD run starts; topic says what it is about, path is single for one change, epic for one epic or project for a whole project.",
        schema: start_run_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_PLANNING,
        description: "call as an orchestrator when the run starts on a planning document; persona names the BMAD agent writing it.",
        schema: start_planning_schema,
        run_level: true,
    },
    WorkflowTool {
        name: PLAN_STORIES,
        description: "call as an orchestrator once the run's epics and stories are planned, with every epic and its stories in their order.",
        schema: plan_stories_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_SPRINT_PLANNING,
        description: "call as an orchestrator when the run's sprint planning starts.",
        schema: persona_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_EPIC,
        description: "call as an orchestrator when the run starts on an epic.",
        schema: start_epic_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_STORY,
        description: "call as an orchestrator when the build of a story starts; path is the story's file, relative to your working folder.",
        schema: start_story_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_STORY_REVIEW,
        description: "call as an orchestrator when the code review of a story starts.",
        schema: start_story_review_schema,
        run_level: true,
    },
    WorkflowTool {
        name: FINISH_STORY,
        description: "call as an orchestrator when a story's review ends; outcome is done, or changes_requested when it goes back to its build.",
        schema: finish_story_schema,
        run_level: true,
    },
    WorkflowTool {
        name: START_RETROSPECTIVE,
        description: "call as an orchestrator when the retrospective of an epic starts.",
        schema: start_retrospective_schema,
        run_level: true,
    },
    WorkflowTool {
        name: FINISH_RUN,
        description: "call as an orchestrator when the BMAD run ends; outcome is completed or abandoned.",
        schema: finish_run_schema,
        run_level: true,
    },
];

impl Event {
    /// The name of the tool the agent reports this event with.
    #[must_use]
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::StartWorkflow { .. } => START_WORKFLOW,
            Self::RequestDocumentApproval { .. } => REQUEST_DOCUMENT_APPROVAL,
            Self::RequestStoryOrderApproval { .. } => REQUEST_STORY_ORDER_APPROVAL,
            Self::RequestReadinessDecision { .. } => REQUEST_READINESS_DECISION,
            Self::RequestBuildPlanApproval { .. } => REQUEST_BUILD_PLAN_APPROVAL,
            Self::RequestBuildReview { .. } => REQUEST_BUILD_REVIEW,
            Self::RequestReviewVerdict { .. } => REQUEST_REVIEW_VERDICT,
            Self::RequestRetrospectiveVerdict { .. } => REQUEST_RETROSPECTIVE_VERDICT,
            Self::FinishWorkflow { .. } => FINISH_WORKFLOW,
            Self::StartRun { .. } => START_RUN,
            Self::StartPlanning { .. } => START_PLANNING,
            Self::PlanStories { .. } => PLAN_STORIES,
            Self::StartSprintPlanning { .. } => START_SPRINT_PLANNING,
            Self::StartEpic { .. } => START_EPIC,
            Self::StartStory { .. } => START_STORY,
            Self::StartStoryReview { .. } => START_STORY_REVIEW,
            Self::FinishStory { .. } => FINISH_STORY,
            Self::StartRetrospective { .. } => START_RETROSPECTIVE,
            Self::FinishRun { .. } => FINISH_RUN,
        }
    }
}

/// A string argument described by `description`.
fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

/// A string argument that is one of `values`.
fn one_of(values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values })
}

/// The path of a file, relative to the working folder.
fn path() -> Value {
    text("The file, relative to your working folder")
}

/// The schema of the tool that starts the workflow: its kind, persona, story and epic.
fn start_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": one_of(&[
                "brief",
                "prfaq",
                "prd",
                "ux",
                "architecture",
                "epics_and_stories",
                "sprint_planning",
                "spec",
                "build",
                "code_review",
                "retrospective",
            ]),
            "persona": text("The BMAD agent running the workflow, such as \"Amelia · dev\""),
            "story": text("The story the workflow works on, such as \"1.2\""),
            "epic": text("The epic the workflow works on"),
        },
        "required": ["kind"],
    })
}

/// The schema of the tool that asks for a document's approval: which one, and its path.
fn document_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "doc": one_of(&["brief", "prfaq", "prd", "ux", "architecture", "spec"]),
            "path": path(),
        },
        "required": ["doc", "path"],
    })
}

/// A list of stories, each with its id and title, in their order.
fn stories() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "id": text("The story's id, such as \"1.2\""),
                "title": { "type": "string" },
            },
            "required": ["id", "title"],
        },
        "description": "The stories, in their order",
    })
}

/// The BMAD agent that does what a tool reports.
fn persona() -> Value {
    text("The BMAD agent, such as \"Amelia · dev\"")
}

/// The schema of the tool that asks for a story order's approval: the breakdown's path and
/// its stories.
fn story_order_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "path": path(), "stories": stories() },
        "required": ["path", "stories"],
    })
}

/// The schema of the tool that asks about the readiness verdict: the verdict and the report.
fn readiness_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": one_of(&["pass", "concerns", "fail"]),
            "path": path(),
        },
        "required": ["verdict", "path"],
    })
}

/// The schema of the tool that asks for a build plan's approval: the story and the plan.
fn build_plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "story": text("The story built"), "path": path() },
        "required": ["story", "path"],
    })
}

/// The schema of the tool that asks to review a build: the story and the build's summary.
fn build_review_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "story": text("The story built"), "path": path() },
        "required": ["story"],
    })
}

/// The schema of the tool that asks about a code review's verdict: the story, the verdict and
/// the review.
fn review_verdict_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "story": text("The story reviewed"),
            "verdict": one_of(&["approved", "changes_requested"]),
            "path": path(),
        },
        "required": ["story", "verdict", "path"],
    })
}

/// The schema of the tool that asks about a retrospective's verdict: the epic, the verdict and
/// the retrospective.
fn retrospective_verdict_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "epic": text("The epic looked back on"),
            "verdict": one_of(&["accepted", "accepted_with_open_items", "rejected"]),
            "path": path(),
        },
        "required": ["epic", "verdict", "path"],
    })
}

/// The schema of the tool that finishes the workflow: its outcome.
fn finish_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "outcome": text("How the workflow ended, in a few words") },
    })
}

/// The schema of the tool that starts a run: its topic and path.
fn start_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": text("What the run is about"),
            "path": one_of(&["single", "epic", "project"]),
        },
        "required": ["topic", "path"],
    })
}

/// The schema of the tool that starts a planning document: which one, and its persona.
fn start_planning_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "doc": one_of(&["brief", "prfaq", "prd", "ux", "architecture"]),
            "persona": persona(),
        },
        "required": ["doc"],
    })
}

/// The schema of the tool that records the story plan: the epics with their stories.
fn plan_stories_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "epics": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": text("The epic's id, such as \"1\""),
                        "title": { "type": "string" },
                        "stories": stories(),
                    },
                    "required": ["id", "title", "stories"],
                },
                "description": "The epics, in their order",
            },
        },
        "required": ["epics"],
    })
}

/// The schema of a tool whose only argument is its persona.
fn persona_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "persona": persona() },
    })
}

/// The schema of the tool that starts an epic: its id.
fn start_epic_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "id": text("The epic's id") },
        "required": ["id"],
    })
}

/// The schema of the tool that starts a story's build: its id, file and persona.
fn start_story_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": text("The story's id, such as \"1.2\""),
            "path": path(),
            "persona": persona(),
        },
        "required": ["id"],
    })
}

/// The schema of the tool that starts a story's review: its id and persona.
fn start_story_review_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "id": text("The story's id"), "persona": persona() },
        "required": ["id"],
    })
}

/// The schema of the tool that finishes a story's review: its id and outcome.
fn finish_story_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": text("The story's id"),
            "outcome": one_of(&["done", "changes_requested"]),
        },
        "required": ["id", "outcome"],
    })
}

/// The schema of the tool that starts a retrospective: the epic and the persona.
fn start_retrospective_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "epic": text("The epic looked back on"), "persona": persona() },
        "required": ["epic"],
    })
}

/// The schema of the tool that finishes the run: its outcome.
fn finish_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "outcome": one_of(&["completed", "abandoned"]) },
        "required": ["outcome"],
    })
}

/// The arguments of the tool that starts the workflow.
#[derive(Debug, Deserialize)]
struct StartArguments {
    kind: WorkflowKind,
    persona: Option<String>,
    story: Option<String>,
    epic: Option<String>,
}

/// The arguments of the tool that asks for a document's approval.
#[derive(Debug, Deserialize)]
struct DocumentArguments {
    doc: DocKind,
    path: PathBuf,
}

/// The arguments of the tool that asks for a story order's approval.
#[derive(Debug, Deserialize)]
struct StoryOrderArguments {
    path: PathBuf,
    stories: Vec<StoryRef>,
}

/// The arguments of the tool that asks about the readiness verdict.
#[derive(Debug, Deserialize)]
struct ReadinessArguments {
    verdict: Readiness,
    path: PathBuf,
}

/// The arguments of the tool that asks for a build plan's approval.
#[derive(Debug, Deserialize)]
struct BuildPlanArguments {
    story: String,
    path: PathBuf,
}

/// The arguments of the tool that asks to review a build.
#[derive(Debug, Deserialize)]
struct BuildReviewArguments {
    story: String,
    path: Option<PathBuf>,
}

/// The arguments of the tool that asks about a code review's verdict.
#[derive(Debug, Deserialize)]
struct ReviewVerdictArguments {
    story: String,
    verdict: ReviewVerdict,
    path: PathBuf,
}

/// The arguments of the tool that asks about a retrospective's verdict.
#[derive(Debug, Deserialize)]
struct RetrospectiveVerdictArguments {
    epic: String,
    verdict: RetroVerdict,
    path: PathBuf,
}

/// The arguments of the tool that finishes the workflow.
#[derive(Debug, Deserialize)]
struct FinishArguments {
    outcome: Option<String>,
}

/// The arguments of the tool that starts a run.
#[derive(Debug, Deserialize)]
struct StartRunArguments {
    topic: String,
    path: RunPath,
}

/// The arguments of the tool that starts a planning document.
#[derive(Debug, Deserialize)]
struct StartPlanningArguments {
    doc: PlanningDoc,
    persona: Option<String>,
}

/// The arguments of the tool that records the story plan.
#[derive(Debug, Deserialize)]
struct PlanStoriesArguments {
    epics: Vec<EpicPlan>,
}

/// The arguments of a tool that names only its persona.
#[derive(Debug, Deserialize)]
struct PersonaArguments {
    persona: Option<String>,
}

/// The arguments of the tool that starts an epic.
#[derive(Debug, Deserialize)]
struct StartEpicArguments {
    id: String,
}

/// The arguments of the tool that starts a story's build.
#[derive(Debug, Deserialize)]
struct StartStoryArguments {
    id: String,
    path: Option<PathBuf>,
    persona: Option<String>,
}

/// The arguments of the tool that starts a story's review.
#[derive(Debug, Deserialize)]
struct StartStoryReviewArguments {
    id: String,
    persona: Option<String>,
}

/// The arguments of the tool that finishes a story's review.
#[derive(Debug, Deserialize)]
struct FinishStoryArguments {
    id: String,
    outcome: StoryOutcome,
}

/// The arguments of the tool that starts a retrospective.
#[derive(Debug, Deserialize)]
struct StartRetrospectiveArguments {
    epic: String,
    persona: Option<String>,
}

/// The arguments of the tool that finishes the run.
#[derive(Debug, Deserialize)]
struct FinishRunArguments {
    outcome: RunOutcome,
}

/// Reads a call of the tool `name` with `arguments` as an event.
///
/// # Errors
///
/// Returns `unknown tool {name}` when `name` is none of the tools, or serde's message when an
/// argument is missing or of the wrong type.
pub(super) fn parse(name: &str, arguments: Value) -> Result<Event, String> {
    let event = match name {
        START_WORKFLOW => {
            let StartArguments {
                kind,
                persona,
                story,
                epic,
            } = arguments_of(arguments)?;
            Event::StartWorkflow {
                kind,
                persona,
                story,
                epic,
            }
        }
        REQUEST_DOCUMENT_APPROVAL => {
            let DocumentArguments { doc, path } = arguments_of(arguments)?;
            Event::RequestDocumentApproval { doc, path }
        }
        REQUEST_STORY_ORDER_APPROVAL => {
            let StoryOrderArguments { path, stories } = arguments_of(arguments)?;
            Event::RequestStoryOrderApproval { path, stories }
        }
        REQUEST_READINESS_DECISION => {
            let ReadinessArguments { verdict, path } = arguments_of(arguments)?;
            Event::RequestReadinessDecision { verdict, path }
        }
        REQUEST_BUILD_PLAN_APPROVAL => {
            let BuildPlanArguments { story, path } = arguments_of(arguments)?;
            Event::RequestBuildPlanApproval { story, path }
        }
        REQUEST_BUILD_REVIEW => {
            let BuildReviewArguments { story, path } = arguments_of(arguments)?;
            Event::RequestBuildReview { story, path }
        }
        REQUEST_REVIEW_VERDICT => {
            let ReviewVerdictArguments {
                story,
                verdict,
                path,
            } = arguments_of(arguments)?;
            Event::RequestReviewVerdict {
                story,
                verdict,
                path,
            }
        }
        REQUEST_RETROSPECTIVE_VERDICT => {
            let RetrospectiveVerdictArguments {
                epic,
                verdict,
                path,
            } = arguments_of(arguments)?;
            Event::RequestRetrospectiveVerdict {
                epic,
                verdict,
                path,
            }
        }
        FINISH_WORKFLOW => {
            let FinishArguments { outcome } = arguments_of(arguments)?;
            Event::FinishWorkflow { outcome }
        }
        _ => return parse_run(name, arguments),
    };
    Ok(event)
}

/// Reads a call of the run-level tool `name` with `arguments` as an event.
///
/// # Errors
///
/// Returns `unknown tool {name}` when `name` is none of the run-level tools, or serde's
/// message when an argument is missing or of the wrong type.
fn parse_run(name: &str, arguments: Value) -> Result<Event, String> {
    let event = match name {
        START_RUN => {
            let StartRunArguments { topic, path } = arguments_of(arguments)?;
            Event::StartRun { topic, path }
        }
        START_PLANNING => {
            let StartPlanningArguments { doc, persona } = arguments_of(arguments)?;
            Event::StartPlanning { doc, persona }
        }
        PLAN_STORIES => {
            let PlanStoriesArguments { epics } = arguments_of(arguments)?;
            Event::PlanStories { epics }
        }
        START_SPRINT_PLANNING => {
            let PersonaArguments { persona } = arguments_of(arguments)?;
            Event::StartSprintPlanning { persona }
        }
        START_EPIC => {
            let StartEpicArguments { id } = arguments_of(arguments)?;
            Event::StartEpic { id }
        }
        START_STORY => {
            let StartStoryArguments { id, path, persona } = arguments_of(arguments)?;
            Event::StartStory { id, path, persona }
        }
        START_STORY_REVIEW => {
            let StartStoryReviewArguments { id, persona } = arguments_of(arguments)?;
            Event::StartStoryReview { id, persona }
        }
        FINISH_STORY => {
            let FinishStoryArguments { id, outcome } = arguments_of(arguments)?;
            Event::FinishStory { id, outcome }
        }
        START_RETROSPECTIVE => {
            let StartRetrospectiveArguments { epic, persona } = arguments_of(arguments)?;
            Event::StartRetrospective { epic, persona }
        }
        FINISH_RUN => {
            let FinishRunArguments { outcome } = arguments_of(arguments)?;
            Event::FinishRun { outcome }
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
