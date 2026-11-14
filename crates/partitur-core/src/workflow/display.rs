//! The display data of a workflow: neutral descriptions of what the UI draws for a session
//! that follows one, so that the UI needs to know no workflow of its own.
//!
//! A workflow describes its rail (a chip naming it, its steps and what the rail's right side
//! holds), the outline of its blocks, the documents it works on, the tree's step cell, the
//! gate waiting for the user and the rows its events leave in the transcript. The data
//! carries every word the UI shows, but never a formatted time: a time is carried in seconds
//! since the Unix epoch, and the UI formats it in the user's local time.

use std::path::PathBuf;

/// The flow rail: the row that tells where a session stands in its workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailView {
    /// The chip at the left that names the workflow.
    pub chip: String,
    /// The workflow's steps, in the order it runs them.
    pub steps: Vec<RailStep>,
    /// What the right of the rail holds.
    pub right: RailRight,
}

/// Where a step stands against the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Before the current step.
    Done,
    /// The current step.
    Current,
    /// After the current step, or any step before the first.
    Upcoming,
}

/// One step on the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RailStep {
    /// A step with a label and a dot.
    Plain {
        /// The step's name.
        label: String,
        /// Where it stands against the current step.
        state: StepState,
        /// What the step adds to its label, such as the task being worked on.
        detail: Option<String>,
        /// The key of the document a click on the step opens.
        doc: Option<String>,
    },
}

/// The right of the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RailRight {
    /// The unit of work going on.
    Unit {
        /// Who works on it.
        persona: Option<String>,
        /// What is worked on.
        title: Option<String>,
        /// The phase it is in.
        pill: Option<PhasePill>,
        /// A link as `(caption, document key)`.
        link: Option<(String, String)>,
    },
    /// The button that approves a waiting gate.
    Gate {
        /// The button's caption.
        approve_label: String,
    },
}

/// A pill that names a phase in a tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhasePill {
    /// The pill's words.
    pub text: String,
    /// Its colour.
    pub tone: Tone,
}

/// The colour of a pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Red, as a failing test.
    Red,
    /// Green, as a passing test.
    Green,
    /// No colour: the pill is drawn outlined.
    Neutral,
}

/// One block of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineBlock {
    /// The block's heading.
    pub label: String,
    /// What the heading is marked with.
    pub mark: BlockMark,
    /// What the block tells, line by line.
    pub lines: Vec<OutlineLine>,
    /// The key of the document a click on the block shows.
    pub doc: Option<String>,
    /// Whether the block is the current one.
    pub current: bool,
}

/// The mark after a block's heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMark {
    /// No mark.
    None,
    /// A gate waits at the block.
    Gate,
    /// The block's document was approved.
    Approved,
}

/// One line of an outline block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineLine {
    /// A document's path.
    Path(String),
    /// A remark.
    Note(String),
    /// A remark with the time it is about, drawn as `text · HH:MM` in local time.
    Timed {
        /// The remark.
        text: String,
        /// The time, in seconds since the Unix epoch.
        at: u64,
    },
}

/// A document the workflow works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRef {
    /// The key the rail, the outline and the gate name the document by.
    pub key: String,
    /// The document's name as the UI says it.
    pub label: String,
    /// The latest revision the agent reported, `None` before it reported one.
    pub reported: Option<ReportedDoc>,
    /// The tasks the document lists, `None` when it lists none.
    pub tasks: Option<TaskList>,
}

/// The latest revision of a document the agent reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedDoc {
    /// Where the document is.
    pub path: PathBuf,
    /// Its revision, starting at 1.
    pub rev: u32,
    /// When it was submitted, in seconds since the Unix epoch.
    pub at: u64,
}

/// The tasks of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskList {
    /// One row per task, in the document's order.
    pub rows: Vec<TaskRowView>,
    /// What follows the rows, such as where the plan stops for the user.
    pub footnote: Option<String>,
}

/// One task of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRowView {
    /// The task's number as text.
    pub n: String,
    /// What the task does.
    pub title: String,
    /// The files it touches.
    pub files: Vec<String>,
    /// Whether it is finished.
    pub done: bool,
    /// Whether it is worked on.
    pub current: bool,
}

/// The tree's step column for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCell {
    /// The step in words, such as `execute · 4/11`.
    pub label: String,
    /// Whether a gate waits for the user.
    pub gate_waiting: bool,
}

/// A gate waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateView {
    /// The caption of the button that approves it.
    pub approve_label: String,
    /// The start of the prompt that asks for changes instead.
    pub change_prefix: String,
    /// What approving does.
    pub hint: String,
    /// The key of the document the gate asks to approve, `None` when it has none reported.
    pub doc: Option<String>,
}

/// The row a workflow event leaves in the transcript, for an event with a row of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRow {
    /// A unit of work starts.
    Task {
        /// The row's label, such as `task 4`.
        label: String,
        /// What the unit does, when known.
        title: Option<String>,
        /// The files it touches.
        files: Vec<String>,
        /// The agent's remark.
        note: Option<String>,
    },
    /// A unit of work enters a phase.
    Phase {
        /// The row's label, such as `tdd`.
        label: String,
        /// The phase.
        pill: PhasePill,
        /// The agent's remark.
        note: Option<String>,
    },
    /// A persona takes up the work.
    Persona {
        /// Who works, such as `Amelia · dev`.
        persona: String,
        /// What it works on, such as `build · story 1.2`.
        text: String,
        /// The file it works from, when known.
        path: Option<PathBuf>,
    },
}
