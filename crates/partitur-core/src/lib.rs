//! Domain types and ports shared by every other Partitur crate, with no I/O of its own.

mod age;
mod app;
mod archive;
mod backend;
mod clock;
mod context;
mod drawing;
pub mod files;
mod git;
mod git_card;
mod history_marks;
mod ids;
mod instructions;
mod jump;
mod link_opener;
mod orchestrator;
mod peers;
pub mod process;
mod project;
mod question;
mod record;
mod session;
mod settings;
mod split_draft;
mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod tool_call;
pub mod tools;
mod transcript;
mod view_model;
pub mod widget_bridge;
pub mod widget_host;
pub mod workflow;
mod worktree_path;

pub use age::age_label;
pub use app::{App, AppError, Approval, ChildChoice, Cleanup, Created, Prepared, image_mime};
pub use archive::{Archived, day_month};
pub use backend::{
    AgentUpdate, ApprovedChild, Backend, BackendError, BackendEvent, ChildOutcome, ChildState,
    ChildWait, ConfigCategory, ConfigChoice, ConfigOption, Decision, GateDecision,
    OrchestratorTool, PermissionKind, PermissionOption, PromptContent, ProposedChild,
    SentAttachment, SessionFailure, StopReason, TitleProposal, ToolContent, ToolKind, ToolStatus,
    TurnOrigin, Widget, WidgetSlot,
};
pub use clock::Clock;
pub use drawing::{Drawing, DrawingKind, EMPTY, KEPT, MAX_DRAWING_BYTES, SHOWN, TOO_LARGE};
pub use files::{
    Files, FilesAnswer, FilesError, FilesEvent, FilesQuery, FilesRequestId, FolderListing,
    ProjectListing, ResolvedPath,
};
pub use git::{
    Git, GitAnswer, GitError, GitEvent, GitQuery, GitStep, Head, LocalBranch, RepoInfo,
    WorktreeInfo, is_valid_branch_name,
};
pub use git_card::{GitCardView, LiveGit, head_label};
pub use history_marks::HistoryMarks;
pub use ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, GitRequestId,
    InvalidBackendId, InvalidRepositoryId, PermissionId, PermissionOptionId, ProjectId, ProposalId,
    QuestionId, RepositoryId, RequestId, SessionId, ToolCallId,
};
pub use instructions::{InstructionFile, InstructionScope, SkippedInstruction};
pub use jump::{JumpEntry, JumpModel};
pub use link_opener::{LinkOpener, LinkOpenerError};
pub use orchestrator::{PROPOSAL_RECORDED, TOOL_SERVER};
pub use peers::SiblingDefaults;
pub use process::{ProcessEnv, ProcessEnvSetter, canonical, no_window};
pub use project::{Project, Repository};
pub use question::{QUESTION_INSTRUCTIONS, Question, QuestionOption, QuestionOutcome};
pub use record::TranscriptRecord;
pub use session::{
    BranchChoice, BranchMode, Fold, Role, Scope, Session, Status, TitleSource, WaitMark, Workspace,
};
pub use settings::{
    GlobalSettings, Level, ProjectSettings, RepositorySettings, SettingError, SettingsLayer,
};
pub use split_draft::{Blocker, SplitDraft};
pub use store::{LoadId, Store, StoreCommand, StoreError, StoreEvent, StoreQueue};
pub use tool_call::{
    PartiturTool, PeerTool, SessionTool, ToolAnswer, ToolCall, ToolReply, ToolRole,
    tool_instructions,
};
pub use tools::ToolSpec;
pub use transcript::{Entry, Transcript};
pub use view_model::{
    Ancestry, ApprovalView, BranchCleanup, BranchView, ChildCheck, ChildPermission, ChoiceView,
    CleanupView, DraftChoice, DraftView, EnvViewModel, InstructionFileView, OpeningNode,
    ProjectNode, RepositoryNode, ScopeChoice, SessionNode, SessionSummary, SessionViewModel,
    StepCell, TreeModel, Warmup, WorkflowView, WorktreeCleanup, background_shown,
};
pub use workflow::display::{
    BlockMark, DocRef, GateView, OutlineBlock, OutlineLine, PhasePill, RailRight, RailStep,
    RailView, ReportedDoc, StepState, TaskList, TaskRowView, Tone, WorkflowRow,
};
pub use workflow::superpowers_v7::{
    Document, Event as SuperpowersV7Event, ExecutionMode, Outcome, PlannedTask,
    State as SuperpowersV7State, Step, TaskProgress, TddPhase,
};
pub use workflow::{
    Decider, GateChange, GateCloser, GateKind, GateTexts, ModuleEvent, ModuleId, ModuleState,
    OpenGate, WorkflowEvent, WorkflowModule, WorkflowStatus, WorkflowTool,
};
pub use worktree_path::{DEFAULT_WORKTREE_PATH, PatternError, PatternValues, WorktreePattern};
