//! Domain types and ports shared by every other Partitur crate, with no I/O of its own.

pub mod app;
pub mod archive;
pub mod backend;
pub mod git;
pub mod ids;
pub mod instructions;
pub mod orchestrator;
pub mod project;
pub mod session;
pub mod settings;
pub mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod transcript;
pub mod view_model;
pub mod worktree_path;

pub use app::{App, AppError, Attachment, ChildChoice};
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
    RepositoryId, RequestId, SessionId, ToolCallId, ToolRequestId,
};
pub use instructions::{
    InstructionChain, InstructionFile, InstructionScope, SkippedInstruction, inject,
    instruction_chain,
};
pub use orchestrator::{PARTITUR_TOOL_PREFIX, PROPOSAL_RECORDED, preamble};
pub use project::{Project, ProjectError, Repository, discover_project, rescan};
pub use session::{Scope, Session, Status, title_from_prompt};
pub use settings::{
    GlobalSettings, Level, ProjectSettings, RepositorySettings, SettingError, SettingsLayer,
};
pub use store::{Store, StoreError};
pub use transcript::{Entry, Transcript};
pub use view_model::{
    ChoiceView, DraftChoice, DraftView, EnvViewModel, InstructionFileView, ProjectNode,
    RepositoryNode, SessionNode, SessionSummary, SessionViewModel, TreeModel, Warmup,
};
pub use worktree_path::{DEFAULT_WORKTREE_PATH, PatternError, PatternValues, WorktreePattern};
