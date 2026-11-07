//! Domain types and ports shared by every other Partitur crate, with no I/O of its own.

pub mod app;
pub mod backend;
pub mod ids;
pub mod instructions;
pub mod project;
pub mod session;
pub mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod transcript;
pub mod view_model;

pub use app::{App, AppError, Attachment};
pub use backend::{
    ApprovedChild, Backend, BackendError, BackendEvent, ChildOutcome, ConfigCategory, ConfigChoice,
    ConfigOption, Decision, OrchestratorTool, PermissionKind, PermissionOption, PromptContent,
    ProposedChild, StopReason, ToolContent, ToolKind, ToolStatus, TurnOrigin, Update,
};
pub use ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, InvalidBackendId,
    InvalidRepositoryId, PermissionId, PermissionOptionId, ProjectId, ProposalId, RepositoryId,
    RequestId, SessionId, ToolCallId, ToolRequestId,
};
pub use instructions::{
    InstructionChain, InstructionFile, InstructionScope, SkippedInstruction, inject,
    instruction_chain,
};
pub use project::{Project, ProjectError, Repository, discover_project, rescan};
pub use session::{Scope, Session, Status, title_from_prompt};
pub use store::{Store, StoreError};
pub use transcript::{Entry, Transcript};
pub use view_model::{
    ChoiceView, EnvViewModel, InstructionFileView, ProjectNode, RepositoryNode, SessionNode,
    SessionViewModel, TreeModel,
};
