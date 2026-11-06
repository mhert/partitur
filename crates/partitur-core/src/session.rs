//! A conversation with an agent: what it is scoped to, where it runs and how it is doing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{BackendSessionId, ConfigValueId, ProjectId, RepositoryId, SessionId};

/// How many characters of a prompt a derived session title keeps.
const TITLE_LIMIT: usize = 60;

/// What a session works on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// The whole project, running in its root folder.
    Project,
    /// One repository of the project, running in the repository's folder.
    Repository(RepositoryId),
}

/// Where a session stands, shown as a badge next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Nothing is happening; the user can prompt.
    Idle,
    /// The agent is working on a turn.
    Running,
    /// The agent is blocked on a decision from the user.
    WaitingForYou,
    /// The last turn finished and the user has not looked at it yet.
    Done,
    /// The session could not be created or broke down.
    Failed,
}

/// One agent session, as persisted in the project's session list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Stable identity.
    pub id: SessionId,
    /// The project the session belongs to.
    pub project: ProjectId,
    /// What the session works on.
    pub scope: Scope,
    /// The folder the agent runs in.
    pub cwd: PathBuf,
    /// Display title, derived from the first prompt.
    pub title: String,
    /// The id the backend assigned, `None` until the backend has created the session.
    pub backend_session: Option<BackendSessionId>,
    /// The model the user picked, `None` for the backend's default.
    pub model: Option<ConfigValueId>,
    /// The reasoning effort the user picked, `None` for the backend's default.
    pub effort: Option<ConfigValueId>,
    /// Where the session stands.
    pub status: Status,
    /// The session this one was spawned from, `None` for a top-level session.
    pub parent: Option<SessionId>,
    /// Whether permission requests are granted without asking.
    pub auto_allow: bool,
    /// Whether the project's instruction chain was already sent to the agent.
    pub instructions_injected: bool,
    /// When the session was created, in seconds since the Unix epoch.
    pub created_at: u64,
}

/// Derives a session title from the user's first prompt.
///
/// The title is the first line that is not blank, trimmed, and cut after 60 characters with
/// `…` appended when it was longer. A prompt without any text yields an empty title.
///
/// # Examples
///
/// ```
/// use partitur_core::title_from_prompt;
///
/// assert_eq!(title_from_prompt("\n  Fix the build \nthen deploy"), "Fix the build");
/// assert_eq!(title_from_prompt(&"a".repeat(61)), format!("{}…", "a".repeat(60)));
/// ```
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut chars = line.chars();
    let mut title: String = chars.by_ref().take(TITLE_LIMIT).collect();
    if chars.next().is_some() {
        title.push('…');
    }
    title
}
