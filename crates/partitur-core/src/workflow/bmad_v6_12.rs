//! The BMAD Method, version 6.12, as a workflow module, for a session that runs one BMAD
//! workflow (a planning document, a story breakdown, sprint planning, a spec, a story build,
//! a code review or a retrospective), normally an orchestrator's child, and for the
//! orchestrator that reports the whole run ([`Run`], folded in [`run`]).
//!
//! Starting a run or a workflow starts the whole BMAD state over, so a workflow started in a
//! session with a run replaces the run, and the other way round. The run-level reports fold
//! into the run and never open or close a gate.
//!
//! The agent reports with `bmad_v6_12_*` tool calls, read as an [`Event`]. Starting a workflow
//! starts the module over at the first step of the workflow's kind ([`WorkflowKind::steps`]).
//! Each `request_*` report moves the workflow to the step its gate sits on, when the kind has
//! that step, and opens the gate. It also records the reported file as a [`BmadDoc`] named by
//! the gate: a new revision, unapproved, with the stories of a story order and the verdict of
//! a verdict gate. A request before any start opens the workflow its gate belongs to.
//! Approving the gate that waits stamps its document as approved and moves the workflow on to
//! the step after the gate's own, when the kind has that step and there is a next one;
//! answering it with a prompt only closes it, and answering a gate that does not wait changes
//! nothing. Finishing the workflow records its outcome and closes any gate.
//!
//! The module words its gates (the approve button, the approval prompt, the start of a prompt
//! asking for changes, the word an orchestrator is told and the hint under the gate) and the
//! notes the user's answers leave in the transcript. Its tools, their schemas and the
//! instruction block are described in [`tools`], and what the UI shows of it in [`display`].

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::display::{DocRef, GateView, OutlineBlock, RailView, StepCell, WorkflowRow};
use crate::workflow::{
    Decider, GATE_REQUESTED, GateChange, GateCloser, GateKind, GateTexts, ModuleId, OpenGate,
    RECORDED, WorkflowModule, WorkflowTool,
};

mod display;
mod run;
#[cfg(test)]
mod tests;
mod tools;

pub use run::{EpicPlan, LoopPhase, PlanningDoc, Run, RunOutcome, RunPath, RunStep, StoryOutcome};

/// The BMAD Method, version 6.12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmadV6_12;

/// The kind of the gate at a planning document or a spec.
pub const DOCUMENT: &str = "document";

/// The kind of the gate at the story order of a story breakdown.
pub const STORY_ORDER: &str = "story_order";

/// The kind of the gate at sprint planning's readiness verdict.
pub const READINESS: &str = "readiness";

/// The kind of the gate at a build's plan.
pub const BUILD_PLAN: &str = "build_plan";

/// The kind of the gate at a build's result, before its pull request.
pub const BUILD_REVIEW: &str = "build_review";

/// The kind of the gate at a code review's verdict.
pub const REVIEW_VERDICT: &str = "review_verdict";

/// The kind of the gate at a retrospective's verdict.
pub const RETROSPECTIVE_VERDICT: &str = "retrospective_verdict";

/// The workflows of the BMAD Method a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    /// A product brief.
    Brief,
    /// A press release and its frequently asked questions.
    Prfaq,
    /// A product requirements document.
    Prd,
    /// A UX design.
    Ux,
    /// An architecture.
    Architecture,
    /// The breakdown into epics and stories.
    EpicsAndStories,
    /// Sprint planning, with its readiness check.
    SprintPlanning,
    /// A spec for a single change.
    Spec,
    /// The build of one story.
    Build,
    /// The code review of one story.
    CodeReview,
    /// The retrospective of an epic.
    Retrospective,
}

impl WorkflowKind {
    /// The kind as the UI names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Prfaq => "prfaq",
            Self::Prd => "prd",
            Self::Ux => "ux",
            Self::Architecture => "architecture",
            Self::EpicsAndStories => "epics",
            Self::SprintPlanning => "sprint planning",
            Self::Spec => "spec",
            Self::Build => "build",
            Self::CodeReview => "code review",
            Self::Retrospective => "retro",
        }
    }

    /// The kind's steps, in the order the workflow runs them.
    #[must_use]
    pub fn steps(self) -> &'static [&'static str] {
        match self {
            Self::Brief | Self::Prfaq | Self::Prd | Self::Ux | Self::Architecture | Self::Spec => {
                &["draft", "approve"]
            }
            Self::EpicsAndStories => &["draft", "story order"],
            Self::SprintPlanning => &["plan", "readiness"],
            Self::Build => &["plan", "build", "review"],
            Self::CodeReview => &["review", "verdict"],
            Self::Retrospective => &["retro", "verdict"],
        }
    }
}

/// The planning documents and the spec whose approval a workflow asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    /// A product brief.
    Brief,
    /// A press release and its frequently asked questions.
    Prfaq,
    /// A product requirements document.
    Prd,
    /// A UX design.
    Ux,
    /// An architecture.
    Architecture,
    /// A spec for a single change.
    Spec,
}

impl DocKind {
    /// The document as the UI names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Prfaq => "prfaq",
            Self::Prd => "prd",
            Self::Ux => "ux",
            Self::Architecture => "architecture",
            Self::Spec => "spec",
        }
    }

    /// The workflow that writes the document.
    fn workflow(self) -> WorkflowKind {
        match self {
            Self::Brief => WorkflowKind::Brief,
            Self::Prfaq => WorkflowKind::Prfaq,
            Self::Prd => WorkflowKind::Prd,
            Self::Ux => WorkflowKind::Ux,
            Self::Architecture => WorkflowKind::Architecture,
            Self::Spec => WorkflowKind::Spec,
        }
    }
}

/// Sprint planning's verdict on whether the plans are ready to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// Ready.
    Pass,
    /// Ready, with concerns.
    Concerns,
    /// Not ready.
    Fail,
}

/// A code review's verdict on a story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// The story is approved.
    Approved,
    /// The story needs changes.
    ChangesRequested,
}

/// A retrospective's verdict on an epic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroVerdict {
    /// The epic is accepted.
    Accepted,
    /// The epic is accepted, with items left open.
    AcceptedWithOpenItems,
    /// The epic is rejected.
    Rejected,
}

/// The verdict a verdict gate asks the user to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Sprint planning's readiness verdict.
    Readiness(Readiness),
    /// A code review's verdict.
    Review(ReviewVerdict),
    /// A retrospective's verdict.
    Retrospective(RetroVerdict),
}

/// One story of a story breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryRef {
    /// The story's id, such as `1.2`.
    pub id: String,
    /// What the story is about.
    pub title: String,
}

/// A report the agent made about its BMAD workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A workflow of `kind` starts, from a fresh state.
    StartWorkflow {
        /// What the workflow does.
        kind: WorkflowKind,
        /// The BMAD persona that runs it, such as `Amelia · dev`.
        persona: Option<String>,
        /// The story it works on.
        story: Option<String>,
        /// The epic it works on.
        epic: Option<String>,
    },
    /// A planning document or a spec is written and waits for the user's approval.
    RequestDocumentApproval {
        /// Which document it is.
        doc: DocKind,
        /// Where it is.
        path: PathBuf,
    },
    /// A story breakdown is written and its story order waits for the user's approval.
    RequestStoryOrderApproval {
        /// Where the breakdown is.
        path: PathBuf,
        /// The stories, in their order.
        stories: Vec<StoryRef>,
    },
    /// Sprint planning's readiness verdict waits for the user.
    RequestReadinessDecision {
        /// The verdict.
        verdict: Readiness,
        /// Where the readiness report is.
        path: PathBuf,
    },
    /// A build's plan waits for the user's approval.
    RequestBuildPlanApproval {
        /// The story built.
        story: String,
        /// Where the plan is.
        path: PathBuf,
    },
    /// A build's result waits for the user's review before its pull request.
    RequestBuildReview {
        /// The story built.
        story: String,
        /// Where the build's summary is, when it wrote one.
        path: Option<PathBuf>,
    },
    /// A code review's verdict waits for the user.
    RequestReviewVerdict {
        /// The story reviewed.
        story: String,
        /// The verdict.
        verdict: ReviewVerdict,
        /// Where the review is.
        path: PathBuf,
    },
    /// A retrospective's verdict waits for the user.
    RequestRetrospectiveVerdict {
        /// The epic looked back on.
        epic: String,
        /// The verdict.
        verdict: RetroVerdict,
        /// Where the retrospective is.
        path: PathBuf,
    },
    /// The workflow ends.
    FinishWorkflow {
        /// How it ended, in the agent's words.
        outcome: Option<String>,
    },
    /// An orchestrator's BMAD run starts on `topic` along `path`, from a fresh state.
    StartRun {
        /// What the run is about.
        topic: String,
        /// The path it takes.
        path: RunPath,
    },
    /// The run starts writing a planning document.
    StartPlanning {
        /// Which document.
        doc: PlanningDoc,
        /// The BMAD persona that writes it.
        persona: Option<String>,
    },
    /// The run's epics and stories are planned.
    PlanStories {
        /// The epics with their stories, in their order.
        epics: Vec<EpicPlan>,
    },
    /// The run starts planning the sprint.
    StartSprintPlanning {
        /// The BMAD persona that plans it.
        persona: Option<String>,
    },
    /// The run starts an epic.
    StartEpic {
        /// The epic's id.
        id: String,
    },
    /// The run starts building a story.
    StartStory {
        /// The story's id.
        id: String,
        /// The story's file.
        path: Option<PathBuf>,
        /// The BMAD persona that builds it.
        persona: Option<String>,
    },
    /// The run starts reviewing a story.
    StartStoryReview {
        /// The story's id.
        id: String,
        /// The BMAD persona that reviews it.
        persona: Option<String>,
    },
    /// A story's review ends.
    FinishStory {
        /// The story's id.
        id: String,
        /// How its review ended.
        outcome: StoryOutcome,
    },
    /// The run starts the retrospective of an epic.
    StartRetrospective {
        /// The epic's id.
        epic: String,
        /// The BMAD persona that runs it.
        persona: Option<String>,
    },
    /// The run ends.
    FinishRun {
        /// How it ended.
        outcome: RunOutcome,
    },
}

/// The latest revision of a file a gate asks the user about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BmadDoc {
    /// The kind of the gate that asked about it, which names it.
    pub key: String,
    /// The file's name as the UI says it, such as `prd` or `build plan`.
    pub label: String,
    /// Where it is, `None` when the agent named no file.
    pub path: Option<PathBuf>,
    /// How often the agent asked about it, starting at 1.
    pub rev: u32,
    /// When the latest revision was submitted, in seconds since the Unix epoch.
    pub at: u64,
    /// When the user approved it, `None` until they did.
    pub approved_at: Option<u64>,
    /// The stories of a story order, in their order; empty for every other file.
    pub stories: Vec<StoryRef>,
    /// The verdict of a verdict gate; `None` for every other file.
    #[serde(default)]
    pub verdict: Option<Verdict>,
}

/// The one BMAD workflow a session runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// What the workflow does.
    pub kind: WorkflowKind,
    /// The BMAD persona that runs it.
    pub persona: Option<String>,
    /// The story it works on.
    pub story: Option<String>,
    /// The epic it works on.
    pub epic: Option<String>,
    /// The index of the current step in the kind's [steps](WorkflowKind::steps).
    pub step: usize,
    /// How the workflow ended, `None` while it runs.
    pub finished: Option<Finished>,
    /// The files its gates asked about, in the order they were first asked about.
    pub documents: Vec<BmadDoc>,
}

/// How a finished workflow ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finished {
    /// The outcome in the agent's words, `None` when it named none.
    pub outcome: Option<String>,
}

impl WorkflowRun {
    /// A fresh workflow of `kind` at its first step.
    fn fresh(kind: WorkflowKind) -> Self {
        Self {
            kind,
            persona: None,
            story: None,
            epic: None,
            step: 0,
            finished: None,
            documents: Vec::new(),
        }
    }

    /// The index of the step `gate` sits on, `None` when the workflow's kind lacks that step.
    fn position(&self, gate: Gate) -> Option<usize> {
        self.kind
            .steps()
            .iter()
            .position(|step| *step == gate.step())
    }

    /// Records a new revision of the file `request` asks about, submitted at `at`, moves to
    /// the step its gate sits on when the workflow's kind has that step.
    fn submit(&mut self, request: &Request<'_>, at: u64) {
        let key = request.gate.kind();
        if let Some(position) = self.position(request.gate) {
            self.step = position;
        }
        let doc = BmadDoc {
            key: key.to_owned(),
            label: request.label.to_owned(),
            path: request.path.map(Path::to_path_buf),
            rev: 1,
            at,
            approved_at: None,
            stories: request.stories.to_vec(),
            verdict: request.verdict,
        };
        match self.documents.iter_mut().find(|doc| doc.key == key) {
            Some(previous) => {
                *previous = BmadDoc {
                    rev: previous.rev.saturating_add(1),
                    ..doc
                };
            }
            None => self.documents.push(doc),
        }
    }
}

/// Where a session stands in the BMAD Method, folded from its [`Event`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The workflow the session runs, `None` before it reported one.
    pub workflow: Option<WorkflowRun>,
    /// The run an orchestrator reports, `None` before it started one.
    #[serde(default)]
    pub run: Option<Run>,
}

/// The gates of the workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// A planning document or a spec waits for approval.
    Document,
    /// A story order waits for approval.
    StoryOrder,
    /// A readiness verdict waits for the user.
    Readiness,
    /// A build's plan waits for approval.
    BuildPlan,
    /// A build's result waits for review.
    BuildReview,
    /// A code review's verdict waits for the user.
    ReviewVerdict,
    /// A retrospective's verdict waits for the user.
    RetrospectiveVerdict,
}

impl Gate {
    /// The gate of `kind`, `None` for a kind the module never opens.
    fn of(kind: &GateKind) -> Option<Self> {
        match kind.as_str() {
            DOCUMENT => Some(Self::Document),
            STORY_ORDER => Some(Self::StoryOrder),
            READINESS => Some(Self::Readiness),
            BUILD_PLAN => Some(Self::BuildPlan),
            BUILD_REVIEW => Some(Self::BuildReview),
            REVIEW_VERDICT => Some(Self::ReviewVerdict),
            RETROSPECTIVE_VERDICT => Some(Self::RetrospectiveVerdict),
            _ => None,
        }
    }

    /// The gate's kind.
    fn kind(self) -> &'static str {
        match self {
            Self::Document => DOCUMENT,
            Self::StoryOrder => STORY_ORDER,
            Self::Readiness => READINESS,
            Self::BuildPlan => BUILD_PLAN,
            Self::BuildReview => BUILD_REVIEW,
            Self::ReviewVerdict => REVIEW_VERDICT,
            Self::RetrospectiveVerdict => RETROSPECTIVE_VERDICT,
        }
    }

    /// The step the gate sits on.
    fn step(self) -> &'static str {
        match self {
            Self::Document => "approve",
            Self::StoryOrder => "story order",
            Self::Readiness => "readiness",
            Self::BuildPlan => "plan",
            Self::BuildReview => "review",
            Self::ReviewVerdict | Self::RetrospectiveVerdict => "verdict",
        }
    }

    /// The gate's words.
    fn texts(self) -> GateTexts {
        match self {
            Self::Document => GateTexts {
                approve_label: "Approve document",
                approval_text: "Document approved. Continue.",
                change_prefix: "Changes to the document: ",
                word: "document",
                hint: "approval is written to the transcript · the workflow goes on from the approved document",
            },
            Self::StoryOrder => GateTexts {
                approve_label: "Approve story order",
                approval_text: "Story order approved. Continue.",
                change_prefix: "Changes to the story order: ",
                word: "story order",
                hint: "approval is written to the transcript · the stories are built in this order",
            },
            Self::Readiness => GateTexts {
                approve_label: "Accept verdict",
                approval_text: "Readiness verdict accepted. Continue.",
                change_prefix: "Before continuing: ",
                word: "readiness verdict",
                hint: "approval is written to the transcript · sprint planning goes on",
            },
            Self::BuildPlan => GateTexts {
                approve_label: "Approve plan",
                approval_text: "Plan approved. Build it.",
                change_prefix: "Changes to the plan: ",
                word: "build plan",
                hint: "approval is written to the transcript · the story is built next",
            },
            Self::BuildReview => GateTexts {
                approve_label: "Approve build",
                approval_text: "Build approved. Open the pull request.",
                change_prefix: "Changes to the build: ",
                word: "build",
                hint: "approval is written to the transcript · the pull request is opened next",
            },
            Self::ReviewVerdict => GateTexts {
                approve_label: "Accept verdict",
                approval_text: "Verdict accepted. Continue.",
                change_prefix: "Changes to the review: ",
                word: "review verdict",
                hint: "approval is written to the transcript · the review's verdict stands",
            },
            Self::RetrospectiveVerdict => GateTexts {
                approve_label: "Accept verdict",
                approval_text: "Verdict accepted. Continue.",
                change_prefix: "Changes to the retrospective: ",
                word: "retrospective verdict",
                hint: "approval is written to the transcript · the retrospective closes the epic",
            },
        }
    }

    /// What the user did when approving the gate, such as `Document approved`.
    fn approved(self) -> &'static str {
        match self {
            Self::Document => "Document approved",
            Self::StoryOrder => "Story order approved",
            Self::Readiness => "Readiness verdict accepted",
            Self::BuildPlan => "Build plan approved",
            Self::BuildReview => "Build approved",
            Self::ReviewVerdict => "Review verdict accepted",
            Self::RetrospectiveVerdict => "Retrospective verdict accepted",
        }
    }
}

/// What a request asks the user about.
struct Request<'a> {
    /// The gate it opens.
    gate: Gate,
    /// The workflow the gate belongs to.
    kind: WorkflowKind,
    /// The file's name as the UI says it.
    label: &'static str,
    /// Where the file is, `None` when the agent named none.
    path: Option<&'a Path>,
    /// The stories of a story order; empty for every other request.
    stories: &'a [StoryRef],
    /// The verdict of a verdict gate; `None` for every other request.
    verdict: Option<Verdict>,
}

impl<'a> Request<'a> {
    /// A request to decide on the file at `path` at `gate` of a `kind` workflow, named by the
    /// gate's word.
    fn new(gate: Gate, kind: WorkflowKind, path: Option<&'a Path>) -> Self {
        Self {
            gate,
            kind,
            label: gate.texts().word,
            path,
            stories: &[],
            verdict: None,
        }
    }

    /// A request with `verdict`.
    fn with_verdict(self, verdict: Verdict) -> Self {
        Self {
            verdict: Some(verdict),
            ..self
        }
    }
}

/// What `event` asks the user about, `None` for an event that opens no gate.
fn request(event: &Event) -> Option<Request<'_>> {
    Some(match event {
        Event::StartWorkflow { .. }
        | Event::FinishWorkflow { .. }
        | Event::StartRun { .. }
        | Event::StartPlanning { .. }
        | Event::PlanStories { .. }
        | Event::StartSprintPlanning { .. }
        | Event::StartEpic { .. }
        | Event::StartStory { .. }
        | Event::StartStoryReview { .. }
        | Event::FinishStory { .. }
        | Event::StartRetrospective { .. }
        | Event::FinishRun { .. } => return None,
        Event::RequestDocumentApproval { doc, path } => Request {
            label: doc.label(),
            ..Request::new(Gate::Document, doc.workflow(), Some(path))
        },
        Event::RequestStoryOrderApproval { path, stories } => Request {
            stories,
            ..Request::new(Gate::StoryOrder, WorkflowKind::EpicsAndStories, Some(path))
        },
        Event::RequestReadinessDecision { verdict, path } => {
            Request::new(Gate::Readiness, WorkflowKind::SprintPlanning, Some(path))
                .with_verdict(Verdict::Readiness(*verdict))
        }
        Event::RequestBuildPlanApproval { path, .. } => {
            Request::new(Gate::BuildPlan, WorkflowKind::Build, Some(path))
        }
        Event::RequestBuildReview { path, .. } => {
            Request::new(Gate::BuildReview, WorkflowKind::Build, path.as_deref())
        }
        Event::RequestReviewVerdict { verdict, path, .. } => {
            Request::new(Gate::ReviewVerdict, WorkflowKind::CodeReview, Some(path))
                .with_verdict(Verdict::Review(*verdict))
        }
        Event::RequestRetrospectiveVerdict { verdict, path, .. } => Request::new(
            Gate::RetrospectiveVerdict,
            WorkflowKind::Retrospective,
            Some(path),
        )
        .with_verdict(Verdict::Retrospective(*verdict)),
    })
}

/// BMAD's output folder when its configuration does not set another.
const DEFAULT_OUTPUT: &str = "_bmad-output";

/// The prefix BMAD may put before a folder it resolves against the project root.
const PROJECT_ROOT: &str = "{project-root}/";

/// BMAD's output folder: the `output_folder` of the `[core]` table of the configuration
/// text `config`, without a leading `{project-root}/`, when that is a relative path made of
/// plain names, and `_bmad-output` otherwise, also when there is no configuration or it is
/// not TOML.
fn output_folder(config: Option<&str>) -> PathBuf {
    config
        .and_then(|text| toml::from_str::<toml::Table>(text).ok())
        .and_then(|config| {
            config
                .get("core")?
                .get("output_folder")?
                .as_str()
                .map(|value| PathBuf::from(value.strip_prefix(PROJECT_ROOT).unwrap_or(value)))
        })
        .filter(|folder| {
            !folder.as_os_str().is_empty()
                && folder
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT))
}

impl WorkflowModule for BmadV6_12 {
    type Event = Event;
    type State = State;
    const ID: ModuleId = ModuleId::BmadV6_12;
    const CONFIG_FILE: Option<&'static str> = Some("_bmad/config.toml");

    fn document_folders(config: Option<&str>) -> Vec<PathBuf> {
        vec![output_folder(config)]
    }

    fn orchestrator_note() -> Option<&'static str> {
        None
    }

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
        matches!(event, Event::StartWorkflow { .. } | Event::StartRun { .. })
    }

    fn apply(state: &mut State, event: &Event, at: u64) -> GateChange {
        match event {
            Event::StartWorkflow {
                kind,
                persona,
                story,
                epic,
            } => {
                state.workflow = Some(WorkflowRun {
                    persona: persona.clone(),
                    story: story.clone(),
                    epic: epic.clone(),
                    ..WorkflowRun::fresh(*kind)
                });
                GateChange::Keep
            }
            Event::FinishWorkflow { outcome } => {
                if let Some(workflow) = &mut state.workflow {
                    workflow.finished = Some(Finished {
                        outcome: outcome.clone(),
                    });
                }
                GateChange::Close
            }
            _ if event.run_level() => {
                run::apply(state, event, at);
                GateChange::Keep
            }
            _ => request(event).map_or(GateChange::Keep, |request| {
                state
                    .workflow
                    .get_or_insert_with(|| WorkflowRun::fresh(request.kind))
                    .submit(&request, at);
                GateChange::Open(GateKind::new(request.gate.kind()))
            }),
        }
    }

    fn gate_closed(state: &mut State, gate: &GateKind, by: GateCloser, at: u64, waiting: bool) {
        let Some(workflow) = state.workflow.as_mut().filter(|_| waiting) else {
            return;
        };
        if let (Some(gate), GateCloser::Approve) = (Gate::of(gate), by) {
            if let Some(doc) = workflow
                .documents
                .iter_mut()
                .find(|doc| doc.key == gate.kind())
            {
                doc.approved_at = Some(at);
            }
            if let Some(position) = workflow.position(gate) {
                let last = workflow.kind.steps().len().saturating_sub(1);
                workflow.step = (position + 1).min(last);
            }
        }
    }

    fn tool_answer(event: &Event) -> &'static str {
        match event {
            Event::StartWorkflow { .. }
            | Event::FinishWorkflow { .. }
            | Event::StartRun { .. }
            | Event::StartPlanning { .. }
            | Event::PlanStories { .. }
            | Event::StartSprintPlanning { .. }
            | Event::StartEpic { .. }
            | Event::StartStory { .. }
            | Event::StartStoryReview { .. }
            | Event::FinishStory { .. }
            | Event::StartRetrospective { .. }
            | Event::FinishRun { .. } => RECORDED,
            Event::RequestDocumentApproval { .. }
            | Event::RequestStoryOrderApproval { .. }
            | Event::RequestReadinessDecision { .. }
            | Event::RequestBuildPlanApproval { .. }
            | Event::RequestBuildReview { .. }
            | Event::RequestReviewVerdict { .. }
            | Event::RequestRetrospectiveVerdict { .. } => GATE_REQUESTED,
        }
    }

    fn gate_texts(gate: &GateKind) -> Option<GateTexts> {
        Gate::of(gate).map(Gate::texts)
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
        display::step_note(event)
    }

    fn gate_note(gate: &GateKind, by: GateCloser, decider: Decider, time: &str) -> Option<String> {
        let gate = Gate::of(gate)?;
        Some(match (by, decider) {
            (GateCloser::Approve, Decider::User) => {
                format!("{} by you at {time}", gate.approved())
            }
            (GateCloser::Approve, Decider::Orchestrator) => {
                format!("{} by orchestrator at {time}", gate.approved())
            }
            (GateCloser::Prompt, Decider::User) => {
                format!("Changes to the {} requested at {time}", gate.texts().word)
            }
            (GateCloser::Prompt, Decider::Orchestrator) => format!(
                "Changes to the {} requested by orchestrator at {time}",
                gate.texts().word
            ),
        })
    }
}

/// The words for the waiting `gate`, the fallback for a gate the module does not word.
fn texts(gate: &OpenGate) -> GateTexts {
    Gate::of(&gate.kind).map_or(GateTexts::FALLBACK, Gate::texts)
}
