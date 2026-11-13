//! Domain types and ports shared by every other Partitur crate, with no I/O of its own.

pub mod age;
pub mod app;
pub mod archive;
pub mod backend;
pub mod git;
pub mod ids;
pub mod instructions;
pub mod link_opener;
pub mod orchestrator;
pub mod project;
pub mod session;
pub mod settings;
pub mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tool_call;
pub mod transcript;
pub mod view_model;
pub mod workflow;
pub mod worktree_path;

pub use age::age_label;
pub use app::{App, AppError, Approval, Attachment, ChildChoice, Cleanup, Created};
pub use archive::{Archived, day_month};
pub use backend::{
    ApprovedChild, Backend, BackendError, BackendEvent, ChildOutcome, ConfigCategory, ConfigChoice,
    ConfigOption, Decision, OrchestratorTool, PermissionKind, PermissionOption, PromptContent,
    ProposedChild, StopReason, ToolContent, ToolKind, ToolStatus, TurnOrigin, Update,
};
pub use git::{
    Git, GitAnswer, GitError, GitEvent, GitQuery, GitStep, Head, LocalBranch, RepoInfo,
    WorktreeInfo, is_valid_branch_name,
};
pub use ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, GitRequestId,
    InvalidBackendId, InvalidRepositoryId, PermissionId, PermissionOptionId, ProjectId, ProposalId,
    RepositoryId, RequestId, SessionId, ToolCallId,
};
pub use instructions::{
    InstructionChain, InstructionFile, InstructionScope, SkippedInstruction, inject,
    instruction_chain,
};
pub use link_opener::{DEFAULT_LINK_OPENER, LinkOpener, LinkOpenerError};
pub use orchestrator::{PROPOSAL_RECORDED, TOOL_SERVER, TOOLS, ToolSpec, preamble};
pub use project::{Project, ProjectError, Repository, discover_project, rescan};
pub use session::{
    BranchChoice, BranchMode, Fold, Scope, Session, Status, TitleSource, Workspace, split_skill,
    title_from_prompt,
};
pub use settings::{
    GlobalSettings, Level, ProjectSettings, RepositorySettings, SettingError, SettingsLayer,
};
pub use store::{Store, StoreError};
pub use tool_call::{PartiturTool, ToolAnswer, ToolCall, ToolReply};
pub use transcript::{Entry, Transcript};
pub use view_model::{
    ApprovalView, BranchCleanup, BranchView, ChildCheck, ChoiceView, CleanupView, DraftChoice,
    DraftView, EnvViewModel, InstructionFileView, ProjectNode, RepositoryNode, ScopeChoice,
    SessionNode, SessionSummary, SessionViewModel, TreeModel, Warmup, WorktreeCleanup,
};
pub use workflow::{
    Document, ExecutionMode, GateCloser, GateKind, OpenGate, Outcome, PlannedTask, Step,
    SuperpowersEvent, TaskProgress, TddPhase, Workflow, WorkflowEvent, WorkflowStatus,
};
pub use worktree_path::{DEFAULT_WORKTREE_PATH, PatternError, PatternValues, WorktreePattern};
