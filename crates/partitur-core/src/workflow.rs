//! The workflow a session reports to Partitur, and the fold from reported events to its status.
//!
//! An agent working through a development workflow reports each stage with a tool call. A
//! workflow is a versioned module ([`ModuleId`]) that implements [`WorkflowModule`]: it reads
//! its reports as events, folds them into a state of its own, says which gate an event opens
//! or closes and describes what the UI shows. Each report is a [`WorkflowEvent`], persisted in
//! the transcript, and so is the user's answer to a gate. [`apply`] folds one event into a
//! [`WorkflowStatus`] and [`rebuild`] folds a whole history, so the status a session shows
//! can always be derived again from its events.
//!
//! The fold counts every event and stamps the time of the latest one. A report of another
//! module than the status follows, or one that starts its module over, replaces the module's
//! state with a fresh one and drops the open gate. The module's answer to a report then opens
//! a gate, closes the open one or keeps it. The user answering a gate closes it when it is
//! the open one, and the module learns how it was answered. An answer to a gate of another
//! module than the status follows only counts: it closes no gate and the module never sees
//! it. The free functions here dispatch to the module a status or event belongs to, so the
//! rest of Partitur names no workflow.
//! Times are never formatted here: callers pass the local time as text where a note needs one.
//!
//! Each module offers its report tools ([`WorkflowTool`]) and the instruction block that tells
//! the agent when to call them. Every tool name starts with its module's
//! [prefix](ModuleId::prefix), such as `superpowers_v7_` or `bmad_v6_12_`, so [`parse`] finds
//! the module of a call by its name, and a name no module lists, such as the unversioned
//! `superpowers_*` of earlier versions, is no tool at all. A tool of the run level is offered
//! to orchestrators only.
//!
//! Old stored data still loads: an event tagged `superpowers` reads as one of
//! `superpowers_v7`, a `gate_closed` event without a module as one of `superpowers_v7`, and
//! a status stored with `"workflow": "superpowers"` as a `superpowers_v7` status that keeps
//! its gate and counts no events, so it is rebuilt from the history once the history is read.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::bmad_v6_12::BmadV6_12;
use crate::workflow::display::{DocRef, GateView, OutlineBlock, RailView, StepCell, WorkflowRow};
use crate::workflow::superpowers_v7::SuperpowersV7;

pub mod bmad_v6_12;
pub mod display;
pub mod superpowers_v7;
#[cfg(test)]
mod tests;

/// The workflow modules Partitur knows, each a workflow in one version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleId {
    /// The superpowers skill set, version 7.
    SuperpowersV7,
    /// The BMAD Method, version 6.12.
    #[serde(rename = "bmad_v6_12")]
    BmadV6_12,
}

impl ModuleId {
    /// Every module, in the order the tool server lists their tools.
    pub const ALL: [Self; 2] = [Self::SuperpowersV7, Self::BmadV6_12];

    /// The start of every tool name of the module.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::SuperpowersV7 => "superpowers_v7_",
            Self::BmadV6_12 => "bmad_v6_12_",
        }
    }

    /// The module's configuration file, relative to a working folder, when it has one. Its
    /// text is what [`document_folders`] takes as `config`.
    #[must_use]
    pub fn config_file(self) -> Option<&'static str> {
        match self {
            Self::SuperpowersV7 => SuperpowersV7::CONFIG_FILE,
            Self::BmadV6_12 => BmadV6_12::CONFIG_FILE,
        }
    }

    /// The name the rail's chip shows for the module.
    #[must_use]
    pub fn chip(self) -> &'static str {
        match self {
            Self::SuperpowersV7 => "superpowers 7",
            Self::BmadV6_12 => "bmad 6.12",
        }
    }
}

/// The module of a `gate_closed` event stored before events named their module.
fn legacy_module() -> ModuleId {
    ModuleId::SuperpowersV7
}

/// A report an agent made, read as an event of the module it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleEvent {
    /// A report of the superpowers workflow.
    #[serde(rename = "superpowers_v7", alias = "superpowers")]
    SuperpowersV7(superpowers_v7::Event),
    /// A report of a BMAD workflow.
    #[serde(rename = "bmad_v6_12")]
    BmadV6_12(bmad_v6_12::Event),
}

impl ModuleEvent {
    /// The module the event belongs to.
    #[must_use]
    pub fn module(&self) -> ModuleId {
        match self {
            Self::SuperpowersV7(_) => SuperpowersV7::ID,
            Self::BmadV6_12(_) => BmadV6_12::ID,
        }
    }

    /// Whether the event reports on a whole orchestrated run, which only an orchestrator may
    /// do.
    #[must_use]
    pub fn run_level(&self) -> bool {
        match self {
            Self::SuperpowersV7(_) => false,
            Self::BmadV6_12(event) => event.run_level(),
        }
    }
}

impl From<superpowers_v7::Event> for ModuleEvent {
    fn from(event: superpowers_v7::Event) -> Self {
        Self::SuperpowersV7(event)
    }
}

impl From<bmad_v6_12::Event> for ModuleEvent {
    fn from(event: bmad_v6_12::Event) -> Self {
        Self::BmadV6_12(event)
    }
}

/// Where a session stands in the module it follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleState {
    /// The state of the superpowers workflow.
    #[serde(rename = "superpowers_v7")]
    SuperpowersV7(superpowers_v7::State),
    /// The state of a BMAD workflow.
    #[serde(rename = "bmad_v6_12")]
    BmadV6_12(bmad_v6_12::State),
}

impl ModuleState {
    /// The module the state belongs to.
    #[must_use]
    pub fn module(&self) -> ModuleId {
        match self {
            Self::SuperpowersV7(_) => SuperpowersV7::ID,
            Self::BmadV6_12(_) => BmadV6_12::ID,
        }
    }

    /// The fresh state of `module`, before any event.
    fn fresh(module: ModuleId) -> Self {
        match module {
            ModuleId::SuperpowersV7 => Self::SuperpowersV7(superpowers_v7::State::default()),
            ModuleId::BmadV6_12 => Self::BmadV6_12(bmad_v6_12::State::default()),
        }
    }
}

/// What waits at a gate, as the module that opened it names it, such as `plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GateKind(String);

impl GateKind {
    /// The gate kind named `kind`.
    #[must_use]
    pub fn new(kind: &str) -> Self {
        Self(kind.to_owned())
    }

    /// The kind's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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

/// Who decided a gate or a question: the user, or the orchestrator on the user's behalf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decider {
    /// The user, the default for every record that does not say.
    #[default]
    User,
    /// The session's orchestrator.
    Orchestrator,
}

impl Decider {
    /// Whether the user decided; a user's decision is not written out.
    #[must_use]
    pub fn is_user(&self) -> bool {
        *self == Self::User
    }
}

/// How and by whom a gate is closed by the prompt that starts a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Closing {
    /// How the gate is answered.
    pub by: GateCloser,
    /// Who answers it.
    pub decider: Decider,
}

/// Something that happened in a session's workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// The user answered an open gate.
    GateClosed {
        /// The module whose gate it was.
        #[serde(default = "legacy_module")]
        module: ModuleId,
        /// The gate that was open.
        gate: GateKind,
        /// How the gate was answered.
        by: GateCloser,
        /// Who answered it; the user when the record does not say.
        #[serde(default, skip_serializing_if = "Decider::is_user")]
        decider: Decider,
    },
    /// The agent reported progress.
    #[serde(untagged)]
    Reported(ModuleEvent),
}

impl From<ModuleEvent> for WorkflowEvent {
    fn from(event: ModuleEvent) -> Self {
        Self::Reported(event)
    }
}

impl From<superpowers_v7::Event> for WorkflowEvent {
    fn from(event: superpowers_v7::Event) -> Self {
        Self::Reported(event.into())
    }
}

impl From<bmad_v6_12::Event> for WorkflowEvent {
    fn from(event: bmad_v6_12::Event) -> Self {
        Self::Reported(event.into())
    }
}

/// A gate waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenGate {
    /// What is waiting for approval.
    pub kind: GateKind,
    /// When the gate opened, in seconds since the Unix epoch.
    pub since: u64,
}

/// Where a session stands in its workflow, folded from its [`WorkflowEvent`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredStatus")]
pub struct WorkflowStatus {
    /// How many events were applied, across restarts of the workflow.
    pub applied: u64,
    /// When the latest event was applied, in seconds since the Unix epoch.
    pub updated_at: u64,
    /// The gate waiting for the user.
    pub gate: Option<OpenGate>,
    /// Where the session stands in its module.
    pub state: ModuleState,
}

impl WorkflowStatus {
    /// The module the session follows.
    #[must_use]
    pub fn module(&self) -> ModuleId {
        self.state.module()
    }
}

/// A status as it is stored now, or as it was stored before workflows were modules.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredStatus {
    /// The stored form of today.
    Current {
        applied: u64,
        updated_at: u64,
        gate: Option<OpenGate>,
        state: ModuleState,
    },
    /// The superpowers status stored before workflows were modules.
    Legacy(LegacyStatus),
}

/// The workflow a status stored before workflows were modules followed.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyWorkflow {
    /// The superpowers skill set.
    Superpowers,
}

/// The superpowers status stored before workflows were modules.
#[derive(Deserialize)]
struct LegacyStatus {
    #[serde(rename = "workflow")]
    _workflow: LegacyWorkflow,
    topic: String,
    updated_at: u64,
    step: Option<superpowers_v7::Step>,
    brainstorm_at: Option<u64>,
    gate: Option<OpenGate>,
    design: Option<superpowers_v7::Document>,
    plan: Option<superpowers_v7::Document>,
    tasks: Option<superpowers_v7::TaskProgress>,
    mode: Option<superpowers_v7::ExecutionMode>,
    outcome: Option<superpowers_v7::Outcome>,
}

impl From<StoredStatus> for WorkflowStatus {
    /// A legacy status keeps its gate but counts no events, so the first read of its history
    /// rebuilds it, the plan's tasks included.
    fn from(stored: StoredStatus) -> Self {
        match stored {
            StoredStatus::Current {
                applied,
                updated_at,
                gate,
                state,
            } => Self {
                applied,
                updated_at,
                gate,
                state,
            },
            StoredStatus::Legacy(legacy) => Self {
                applied: 0,
                updated_at: legacy.updated_at,
                gate: legacy.gate,
                state: ModuleState::SuperpowersV7(superpowers_v7::State {
                    topic: legacy.topic,
                    step: legacy.step,
                    brainstorm_at: legacy.brainstorm_at,
                    design: legacy.design,
                    plan: legacy.plan,
                    tasks: legacy.tasks,
                    mode: legacy.mode,
                    outcome: legacy.outcome,
                    planned: Vec::new(),
                }),
            },
        }
    }
}

/// The words the UI and the agent are given for a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateTexts {
    /// The caption of the button that approves the gate.
    pub approve_label: &'static str,
    /// The prompt that approves the gate, sent to the agent as the user's message.
    pub approval_text: &'static str,
    /// The start of the prompt the user writes to ask for changes instead of approving.
    pub change_prefix: &'static str,
    /// The word for what waits at the gate, as an orchestrator is told.
    pub word: &'static str,
    /// What approving does, shown under the gate.
    pub hint: &'static str,
}

impl GateTexts {
    /// The words for a gate its module does not word.
    pub const FALLBACK: Self = Self {
        approve_label: "Continue",
        approval_text: "Continue.",
        change_prefix: "Before continuing: ",
        word: "decision",
        hint: "approval is written to the transcript",
    };
}

/// One report tool a module offers on Partitur's tool server.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowTool {
    /// The tool's name, which starts with its module's [prefix](ModuleId::prefix).
    pub name: &'static str,
    /// What the tool reports and when to call it, as `tools/list` describes it.
    pub description: &'static str,
    /// The JSON schema of the tool's arguments.
    pub schema: fn() -> Value,
    /// Whether the tool reports on a whole orchestrated run, so only orchestrators are
    /// offered it.
    pub run_level: bool,
}

impl WorkflowTool {
    /// Whether a session is offered the tool, an orchestrator when `orchestrator` is set.
    fn offered(&self, orchestrator: bool) -> bool {
        orchestrator || !self.run_level
    }
}

/// What a report does to the open gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateChange {
    /// The gate stays as it is.
    Keep,
    /// A gate of this kind opens, replacing any open one.
    Open(GateKind),
    /// The open gate closes.
    Close,
}

/// A workflow in one version: how its reports fold into its state, how it words its gates
/// and what the UI shows of it.
pub trait WorkflowModule {
    /// A report of the workflow.
    type Event;
    /// Where a session stands in the workflow; its default is the state before any event.
    type State: Default;
    /// The module's id.
    const ID: ModuleId;
    /// The module's configuration file, relative to a working folder, if it has one.
    const CONFIG_FILE: Option<&'static str>;

    /// The module's report tools, in the order the tool server lists them.
    fn tools() -> &'static [WorkflowTool];

    /// Reads a call of the tool `name` with `arguments` as an event.
    ///
    /// # Errors
    ///
    /// Returns the error result text when `name` is none of the module's tools, or serde's
    /// message when `arguments` do not fit it.
    fn parse(name: &str, arguments: Value) -> Result<Self::Event, String>;

    /// The block appended to every session's system prompt that tells the agent when to call
    /// the module's tools.
    fn instructions() -> &'static str;

    /// Whether `event` starts the workflow over, from a fresh state.
    fn starts_over(event: &Self::Event) -> bool;

    /// Folds `event`, reported at `at`, into `state`, and says what it does to the gate.
    fn apply(state: &mut Self::State, event: &Self::Event, at: u64) -> GateChange;

    /// Tells `state` that the user answered `gate` at `at`, `by` approving or prompting;
    /// `waiting` says whether `gate` was the gate that waited for the user.
    fn gate_closed(
        state: &mut Self::State,
        gate: &GateKind,
        by: GateCloser,
        at: u64,
        waiting: bool,
    );

    /// What the agent is told after reporting `event`.
    fn tool_answer(event: &Self::Event) -> &'static str;

    /// The words for `gate`, `None` for a gate the module does not word.
    fn gate_texts(gate: &GateKind) -> Option<GateTexts>;

    /// The rail of the workflow standing at `state` with `gate` waiting.
    fn rail(state: &Self::State, gate: Option<&OpenGate>) -> RailView;

    /// The outline of the workflow standing at `state` with `gate` waiting.
    fn outline(state: &Self::State, gate: Option<&OpenGate>) -> Vec<OutlineBlock>;

    /// The folders, relative to a working folder, the workflow writes its documents in.
    /// `config` is the text of the module's [configuration file](Self::CONFIG_FILE), when
    /// it exists.
    fn document_folders(config: Option<&str>) -> Vec<PathBuf>;

    /// The documents of the workflow standing at `state`.
    fn documents(state: &Self::State) -> Vec<DocRef>;

    /// The tree's step cell of the workflow standing at `state` with `gate` waiting.
    fn step_cell(state: &Self::State, gate: Option<&OpenGate>) -> StepCell;

    /// What the UI shows of `gate`, waiting at `state`.
    fn gate_view(state: &Self::State, gate: &OpenGate) -> GateView;

    /// The transcript row of `event`, given the `state` after it, `None` for an event
    /// without one.
    fn transcript_row(state: &Self::State, event: &Self::Event) -> Option<WorkflowRow>;

    /// The note a transcript shows for `event`, `None` for an event without one.
    fn step_note(event: &Self::Event) -> Option<String>;

    /// The note a transcript shows for `decider` answering `gate` `by` approving or prompting
    /// at the local `time`, `None` for a gate the module does not word.
    fn gate_note(gate: &GateKind, by: GateCloser, decider: Decider, time: &str) -> Option<String>;
}

/// The answer to a report that opens a gate: the agent must stop and wait for the user.
pub const GATE_REQUESTED: &str =
    "approval requested; end your turn now and wait for the user's decision";

/// The answer to a checkpoint report: the agent must stop and summarise for the user.
pub const CHECKPOINT_REACHED: &str =
    "checkpoint reached; stop, summarise your progress for the user and end your turn";

/// The answer to every other report.
pub const RECORDED: &str = "recorded";

/// The report tools of every module in [`ModuleId::ALL`] order, without the run-level ones
/// unless the session is an `orchestrator`.
#[must_use]
pub fn tools(orchestrator: bool) -> Vec<&'static WorkflowTool> {
    ModuleId::ALL
        .into_iter()
        .flat_map(module_tools)
        .filter(|tool| tool.offered(orchestrator))
        .collect()
}

/// The folders, relative to a working folder, that `module` writes its documents in.
/// `config` is the text of [`ModuleId::config_file`], when the file exists.
#[must_use]
pub fn document_folders(module: ModuleId, config: Option<&str>) -> Vec<PathBuf> {
    match module {
        ModuleId::SuperpowersV7 => SuperpowersV7::document_folders(config),
        ModuleId::BmadV6_12 => BmadV6_12::document_folders(config),
    }
}

/// The report tools of `module`.
fn module_tools(module: ModuleId) -> &'static [WorkflowTool] {
    match module {
        ModuleId::SuperpowersV7 => SuperpowersV7::tools(),
        ModuleId::BmadV6_12 => BmadV6_12::tools(),
    }
}

/// Reads a call of the report tool `name` with `arguments`, made by an `orchestrator` or a
/// plain session, as an event of the module whose prefix the name starts with. `None` when
/// [`tools`] lists no tool of that name for the session; `Some` of the error result text when
/// `arguments` do not fit the tool.
#[must_use]
pub fn parse(
    name: &str,
    arguments: Value,
    orchestrator: bool,
) -> Option<Result<ModuleEvent, String>> {
    let module = ModuleId::ALL
        .into_iter()
        .find(|module| name.starts_with(module.prefix()))?;
    let listed = module_tools(module)
        .iter()
        .any(|tool| tool.name == name && tool.offered(orchestrator));
    if !listed {
        return None;
    }
    Some(match module {
        ModuleId::SuperpowersV7 => SuperpowersV7::parse(name, arguments).map(ModuleEvent::from),
        ModuleId::BmadV6_12 => BmadV6_12::parse(name, arguments).map(ModuleEvent::from),
    })
}

/// The block appended to every session's system prompt: every module's instructions, joined
/// by a line break.
#[must_use]
pub fn instructions() -> String {
    ModuleId::ALL
        .into_iter()
        .map(|module| match module {
            ModuleId::SuperpowersV7 => SuperpowersV7::instructions(),
            ModuleId::BmadV6_12 => BmadV6_12::instructions(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the agent is told after reporting `event`.
#[must_use]
pub fn tool_answer(event: &ModuleEvent) -> &'static str {
    match event {
        ModuleEvent::SuperpowersV7(event) => SuperpowersV7::tool_answer(event),
        ModuleEvent::BmadV6_12(event) => BmadV6_12::tool_answer(event),
    }
}

/// The words for `gate` of `module`; [`GateTexts::FALLBACK`] for a gate the module does not
/// word.
#[must_use]
pub fn gate_texts(module: ModuleId, gate: &GateKind) -> GateTexts {
    match module {
        ModuleId::SuperpowersV7 => SuperpowersV7::gate_texts(gate),
        ModuleId::BmadV6_12 => BmadV6_12::gate_texts(gate),
    }
    .unwrap_or(GateTexts::FALLBACK)
}

/// The module `event` belongs to.
fn module_of(event: &WorkflowEvent) -> ModuleId {
    match event {
        WorkflowEvent::GateClosed { module, .. } => *module,
        WorkflowEvent::Reported(event) => event.module(),
    }
}

/// Folds `event`, reported at `at`, into `status`. Without a status the event opens a fresh
/// one of its module.
///
/// Every event counts in [`applied`](WorkflowStatus::applied) and stamps
/// [`updated_at`](WorkflowStatus::updated_at). A report that starts its module over, of the
/// status's module or another one, replaces the state with a fresh one of its module and drops
/// the gate. Any other report of another module than the status, and an answer to a gate of
/// another module, change nothing else.
pub fn apply(status: Option<WorkflowStatus>, event: &WorkflowEvent, at: u64) -> WorkflowStatus {
    let mut status = status.unwrap_or_else(|| WorkflowStatus {
        applied: 0,
        updated_at: 0,
        gate: None,
        state: ModuleState::fresh(module_of(event)),
    });
    status.applied = status.applied.saturating_add(1);
    status.updated_at = at;
    match event {
        WorkflowEvent::GateClosed {
            module, gate, by, ..
        } => {
            if *module != status.module() {
                return status;
            }
            let waiting = status.gate.as_ref().is_some_and(|open| open.kind == *gate);
            if waiting {
                status.gate = None;
            }
            gate_closed(&mut status.state, gate, *by, at, waiting);
        }
        WorkflowEvent::Reported(event) => {
            if starts_over(event) {
                status.state = ModuleState::fresh(event.module());
                status.gate = None;
            } else if event.module() != status.module() {
                return status;
            }
            match apply_report(&mut status.state, event, at) {
                GateChange::Keep => {}
                GateChange::Open(kind) => status.gate = Some(OpenGate { kind, since: at }),
                GateChange::Close => status.gate = None,
            }
        }
    }
    status
}

/// Tells `state` that the user answered `gate` at `at`, `by` approving or prompting, and
/// whether `gate` was `waiting`.
fn gate_closed(state: &mut ModuleState, gate: &GateKind, by: GateCloser, at: u64, waiting: bool) {
    match state {
        ModuleState::SuperpowersV7(state) => {
            SuperpowersV7::gate_closed(state, gate, by, at, waiting);
        }
        ModuleState::BmadV6_12(state) => BmadV6_12::gate_closed(state, gate, by, at, waiting),
    }
}

/// Whether `event` starts its module over, from a fresh state.
fn starts_over(event: &ModuleEvent) -> bool {
    match event {
        ModuleEvent::SuperpowersV7(event) => SuperpowersV7::starts_over(event),
        ModuleEvent::BmadV6_12(event) => BmadV6_12::starts_over(event),
    }
}

/// Folds the report `event`, made at `at`, into `state` of the same module, and says what it
/// does to the gate; a report of another module than `state` keeps the gate and changes
/// nothing.
fn apply_report(state: &mut ModuleState, event: &ModuleEvent, at: u64) -> GateChange {
    match (state, event) {
        (ModuleState::SuperpowersV7(state), ModuleEvent::SuperpowersV7(event)) => {
            SuperpowersV7::apply(state, event, at)
        }
        (ModuleState::BmadV6_12(state), ModuleEvent::BmadV6_12(event)) => {
            BmadV6_12::apply(state, event, at)
        }
        (ModuleState::SuperpowersV7(_), ModuleEvent::BmadV6_12(_))
        | (ModuleState::BmadV6_12(_), ModuleEvent::SuperpowersV7(_)) => GateChange::Keep,
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

/// What the UI shows of the workflow standing at `status`: its rail, its outline, its
/// documents and the gate waiting for the user, `None` when none waits.
#[must_use]
pub fn view(
    status: &WorkflowStatus,
) -> (RailView, Vec<OutlineBlock>, Vec<DocRef>, Option<GateView>) {
    let gate = status.gate.as_ref();
    match &status.state {
        ModuleState::SuperpowersV7(state) => (
            SuperpowersV7::rail(state, gate),
            SuperpowersV7::outline(state, gate),
            SuperpowersV7::documents(state),
            gate.map(|gate| SuperpowersV7::gate_view(state, gate)),
        ),
        ModuleState::BmadV6_12(state) => (
            BmadV6_12::rail(state, gate),
            BmadV6_12::outline(state, gate),
            BmadV6_12::documents(state),
            gate.map(|gate| BmadV6_12::gate_view(state, gate)),
        ),
    }
}

/// The tree's step cell for the workflow standing at `status`.
#[must_use]
pub fn step_cell(status: &WorkflowStatus) -> StepCell {
    match &status.state {
        ModuleState::SuperpowersV7(state) => SuperpowersV7::step_cell(state, status.gate.as_ref()),
        ModuleState::BmadV6_12(state) => BmadV6_12::step_cell(state, status.gate.as_ref()),
    }
}

/// The transcript row of `event`, given the `status` after it: a row for an event with a row
/// of its own, `None` for every other event, for a report of another module than the status
/// and for the user answering a gate.
#[must_use]
pub fn transcript_row(status: &WorkflowStatus, event: &WorkflowEvent) -> Option<WorkflowRow> {
    let WorkflowEvent::Reported(event) = event else {
        return None;
    };
    match (&status.state, event) {
        (ModuleState::SuperpowersV7(state), ModuleEvent::SuperpowersV7(event)) => {
            SuperpowersV7::transcript_row(state, event)
        }
        (ModuleState::BmadV6_12(state), ModuleEvent::BmadV6_12(event)) => {
            BmadV6_12::transcript_row(state, event)
        }
        (ModuleState::SuperpowersV7(_), ModuleEvent::BmadV6_12(_))
        | (ModuleState::BmadV6_12(_), ModuleEvent::SuperpowersV7(_)) => None,
    }
}

/// The note a transcript shows for `event`, worded by the module the event belongs to. `time`
/// is the event's local time as `HH:MM`, used by the notes about a gate being answered, which
/// name the orchestrator when it, not the user, answered.
/// An event with a row of its own gives `None`, and so does the answer to a gate its module
/// does not word.
#[must_use]
pub fn step_note(event: &WorkflowEvent, time: &str) -> Option<String> {
    match event {
        WorkflowEvent::GateClosed {
            module,
            gate,
            by,
            decider,
        } => match module {
            ModuleId::SuperpowersV7 => SuperpowersV7::gate_note(gate, *by, *decider, time),
            ModuleId::BmadV6_12 => BmadV6_12::gate_note(gate, *by, *decider, time),
        },
        WorkflowEvent::Reported(ModuleEvent::SuperpowersV7(event)) => {
            SuperpowersV7::step_note(event)
        }
        WorkflowEvent::Reported(ModuleEvent::BmadV6_12(event)) => BmadV6_12::step_note(event),
    }
}
