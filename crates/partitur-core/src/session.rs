//! A conversation with an agent: what it is scoped to, where it runs and how it is doing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{BackendId, BackendSessionId, ConfigValueId, ProjectId, RepositoryId, SessionId};

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
    /// The configured backend the session runs on. A session saved before sessions named
    /// their backend reads as [unset](BackendId::is_unset) until the app is loaded.
    #[serde(default = "BackendId::unset")]
    pub backend: BackendId,
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
    /// Whether the session is an orchestrator: project scope in a project with at least two
    /// repositories, decided when it was created.
    #[serde(default)]
    pub orchestrator: bool,
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    #[test]
    fn a_session_saved_before_orchestration_loads_as_no_orchestrator() {
        let json = r#"{
            "id": "6f1c1d8e-2b4a-4c3e-9f5d-1a2b3c4d5e6f",
            "project": "0d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a",
            "scope": "Project",
            "cwd": "/work",
            "title": "t",
            "backend_session": null,
            "backend": "claude",
            "model": null,
            "effort": null,
            "status": "Idle",
            "parent": null,
            "auto_allow": false,
            "instructions_injected": false,
            "created_at": 1
        }"#;

        let session: Session = serde_json::from_str(json).unwrap();

        assert!(!session.orchestrator);
    }
}
