//! Domain types and ports shared by every other Partitur crate, with no I/O of its own.

pub mod ids;
pub mod instructions;
pub mod project;

pub use ids::{
    BackendSessionId, ConfigId, ConfigValueId, InvalidRepositoryId, PermissionId,
    PermissionOptionId, ProjectId, RepositoryId, RequestId, SessionId, ToolCallId,
};
pub use instructions::{
    InstructionChain, InstructionFile, InstructionScope, SkippedInstruction, inject,
    instruction_chain,
};
pub use project::{Project, ProjectError, Repository, discover_project, rescan};
