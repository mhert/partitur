//! Plain data the UI renders, built by [`App`](crate::App) so views stay free of logic.

use std::path::PathBuf;

use crate::ids::{ProjectId, RepositoryId, SessionId};
use crate::session::Status;

/// Everything the navigation tree shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeModel {
    /// The open projects, in the order they were opened.
    pub projects: Vec<ProjectNode>,
}

/// One open project in the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNode {
    /// The project's identity.
    pub id: ProjectId,
    /// Display name.
    pub name: String,
    /// The folder the project was opened from.
    pub root: PathBuf,
    /// Whether `AGENTS.md` exists in the project root.
    pub has_global_instructions: bool,
    /// The project's top-level sessions, newest first.
    pub sessions: Vec<SessionNode>,
    /// The project's repositories, as discovered.
    pub repositories: Vec<RepositoryNode>,
}

/// One session in the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNode {
    /// The session's identity.
    pub id: SessionId,
    /// Display title.
    pub title: String,
    /// Where the session stands.
    pub status: Status,
    /// What the session works on: the project's name, or the repository's name.
    pub scope_label: String,
    /// Sessions spawned from this one, newest first.
    pub children: Vec<SessionNode>,
}

/// One repository in the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryNode {
    /// The repository's identity within its project.
    pub id: RepositoryId,
    /// Display name.
    pub name: String,
    /// Whether `AGENTS.md` exists in the repository.
    pub has_instructions: bool,
}
