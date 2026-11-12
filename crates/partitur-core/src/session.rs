//! A conversation with an agent: what it is scoped to, where it runs and how it is doing.
//!
//! A session also records the git workspace it works in: the repository as it is checked out,
//! a new branch in the main checkout, or a new branch in a worktree of its own. A session saved
//! before workspaces were recorded reads as checked out. An orchestrator, which never edits,
//! records instead the branch settings its children start from. A session keeps its first
//! prompt and the skill that prompt starts with; its title stays `untitled` until the agent
//! titles it, unless the user or an orchestrator gave one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::archive::Archived;

use crate::ids::{BackendId, BackendSessionId, ConfigValueId, ProjectId, RepositoryId, SessionId};

/// How many characters of a prompt a derived session title keeps.
const TITLE_LIMIT: usize = 60;

/// Whether a part of the tree shows what is below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fold {
    /// What is below is shown.
    #[default]
    Open,
    /// What is below is hidden.
    Folded,
}

impl Fold {
    /// [`Folded`](Self::Folded), the default of the sessions saved before their children
    /// could be shown.
    const fn folded() -> Self {
        Self::Folded
    }
}

/// Where a session's title comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleSource {
    /// Not chosen by anyone: `untitled`, a title derived from a prompt or one the agent gave.
    #[default]
    Derived,
    /// Chosen by the user or an orchestrator; an agent's title never replaces it.
    Given,
}

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
    /// Partitur closed while the session was in a turn, which never ended. A new turn
    /// resumes the session.
    Interrupted,
    /// The last turn finished and the user has not looked at it yet.
    Done,
    /// The session could not be created or broke down.
    Failed,
}

/// How a session uses the git repository it works in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchMode {
    /// A new branch in a worktree of its own, which the session runs in.
    Worktree,
    /// A new branch that the repository's main checkout switches to.
    NewBranch,
    /// The repository as it is checked out now.
    #[default]
    CheckedOut,
}

/// The branch settings a session is started with: the mode, the new branch's name, the branch
/// it starts from and whether the base is fetched first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchChoice {
    /// How the session uses the repository.
    pub mode: BranchMode,
    /// The new branch's name; empty when none was given.
    pub branch: String,
    /// The branch the new one starts from, `None` when there is none to offer yet.
    pub base: Option<String>,
    /// Whether the base's remote is fetched and the branch starts from the remote base.
    pub fetch: bool,
}

impl Default for BranchChoice {
    /// Checked-out, with no branch name and no base, fetching.
    fn default() -> Self {
        Self {
            mode: BranchMode::CheckedOut,
            branch: String::new(),
            base: None,
            fetch: true,
        }
    }
}

/// The git workspace a session works in, as it was made when the session was created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Workspace {
    /// The repository as it was checked out; nothing was created.
    #[default]
    CheckedOut,
    /// A new branch the repository's main checkout switched to.
    NewBranch {
        /// The new branch.
        branch: String,
        /// The branch it started from.
        base: String,
        /// Whether it started from the fetched remote base.
        fetched: bool,
    },
    /// A new branch checked out in a worktree of its own, which is the session's folder.
    Worktree {
        /// The new branch.
        branch: String,
        /// The branch it started from.
        base: String,
        /// Whether it started from the fetched remote base.
        fetched: bool,
        /// Where the worktree is.
        path: PathBuf,
    },
}

impl Workspace {
    /// The mode this workspace was made in.
    pub fn mode(&self) -> BranchMode {
        match self {
            Self::CheckedOut => BranchMode::CheckedOut,
            Self::NewBranch { .. } => BranchMode::NewBranch,
            Self::Worktree { .. } => BranchMode::Worktree,
        }
    }

    /// The branch the session created, `None` when it works on the checked-out one.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::CheckedOut => None,
            Self::NewBranch { branch, .. } | Self::Worktree { branch, .. } => Some(branch),
        }
    }

    /// Whether the created branch started from the fetched remote base, `None` when the
    /// session created no branch.
    pub fn fetched(&self) -> Option<bool> {
        match self {
            Self::CheckedOut => None,
            Self::NewBranch { fetched, .. } | Self::Worktree { fetched, .. } => Some(*fetched),
        }
    }
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
    /// Display title: `untitled` until the agent titles the session, or the title the user
    /// or an orchestrator gave it.
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
    /// When the user archived the session, `None` while it is live. Session lists saved
    /// before archiving existed load as live.
    #[serde(default)]
    pub archived: Option<Archived>,
    /// The git workspace the session works in. Sessions saved before workspaces were
    /// recorded load as [`CheckedOut`](Workspace::CheckedOut).
    #[serde(default)]
    pub workspace: Workspace,
    /// For an orchestrator, the branch settings its children start from; `None` for any
    /// other session and for sessions saved before these were recorded.
    #[serde(default)]
    pub child_defaults: Option<BranchChoice>,
    /// The first prompt the user sent, whole; empty until there is one. Sessions saved before
    /// it was recorded load with it empty.
    #[serde(default)]
    pub first_prompt: String,
    /// The skill the first prompt starts with, such as `/brainstorming`; `None` when it
    /// names none.
    #[serde(default)]
    pub skill: Option<String>,
    /// Where the title comes from; a [given](TitleSource::Given) one is never replaced by
    /// the agent's.
    #[serde(default)]
    pub title_source: TitleSource,
    /// When the session last had activity, in seconds since the Unix epoch; `0` when none
    /// was recorded, as in sessions saved before it was.
    #[serde(default)]
    pub last_activity: u64,
    /// Whether the session's children are shown below it in the tree. Sessions saved before
    /// this was recorded load folded.
    #[serde(default = "Fold::folded")]
    pub children: Fold,
}

impl Session {
    /// When the session was last active: its last activity, or its creation when none was
    /// recorded.
    pub fn active_at(&self) -> u64 {
        self.last_activity.max(self.created_at)
    }
}

/// Splits a prompt into the skill it starts with and the rest.
///
/// The skill is the first word of the trimmed prompt when it starts with `/` and has at least
/// one more character; the rest is trimmed. Any other prompt has no skill and is returned
/// trimmed.
///
/// # Examples
///
/// ```
/// use partitur_core::split_skill;
///
/// assert_eq!(
///     split_skill("/brainstorming Worktree support"),
///     (Some("/brainstorming".to_owned()), "Worktree support")
/// );
/// assert_eq!(split_skill("fix it"), (None, "fix it"));
/// ```
pub fn split_skill(prompt: &str) -> (Option<String>, &str) {
    let prompt = prompt.trim();
    let (word, rest) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    if word.starts_with('/') && word.len() > 1 {
        (Some(word.to_owned()), rest.trim())
    } else {
        (None, prompt)
    }
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

    #[test]
    fn a_session_saved_before_archiving_loads_as_live() {
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
            "status": "Done",
            "parent": null,
            "auto_allow": false,
            "instructions_injected": false,
            "created_at": 1,
            "orchestrator": false
        }"#;

        let session: Session = serde_json::from_str(json).unwrap();

        assert_eq!(session.archived, None);
    }

    #[test]
    fn a_session_saved_before_branches_loads_checked_out_without_defaults() {
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
            "status": "Done",
            "parent": null,
            "auto_allow": false,
            "instructions_injected": false,
            "created_at": 1,
            "orchestrator": false,
            "archived": null
        }"#;

        let session: Session = serde_json::from_str(json).unwrap();

        assert_eq!(session.workspace, Workspace::CheckedOut);
        assert_eq!(session.child_defaults, None);
    }

    #[test]
    fn a_session_saved_before_the_tree_rework_loads_with_defaults() {
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
            "status": "Done",
            "parent": null,
            "auto_allow": false,
            "instructions_injected": false,
            "created_at": 7,
            "orchestrator": false,
            "archived": null
        }"#;
        let project = r#"{
            "id": "0d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a",
            "root": "/work",
            "name": "work",
            "repositories": [],
            "has_global_instructions": false
        }"#;

        let session: Session = serde_json::from_str(json).unwrap();
        let project: crate::project::Project = serde_json::from_str(project).unwrap();

        assert_eq!(session.first_prompt, "");
        assert_eq!(session.skill, None);
        assert_eq!(session.title_source, TitleSource::Derived);
        assert_eq!(session.last_activity, 0);
        assert_eq!(session.children, Fold::Folded);
        assert_eq!(session.active_at(), 7);
        assert_eq!(project.fold, Fold::Open);
    }

    #[test]
    fn a_session_is_active_at_its_last_activity_once_that_is_later_than_its_creation() {
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
            "status": "Done",
            "parent": null,
            "auto_allow": false,
            "instructions_injected": false,
            "created_at": 7,
            "last_activity": 9
        }"#;
        let mut session: Session = serde_json::from_str(json).unwrap();

        assert_eq!(session.active_at(), 9);
        session.last_activity = 3;
        assert_eq!(session.active_at(), 7);
    }

    #[test]
    fn split_skill_takes_a_leading_slash_word_as_the_skill() {
        assert_eq!(
            split_skill("/brainstorming Worktree support"),
            (Some("/brainstorming".to_owned()), "Worktree support")
        );
        assert_eq!(
            split_skill("  /brainstorming\n Worktree  "),
            (Some("/brainstorming".to_owned()), "Worktree")
        );
        assert_eq!(split_skill("/review"), (Some("/review".to_owned()), ""));
    }

    #[test]
    fn split_skill_leaves_a_prompt_without_a_skill_trimmed() {
        assert_eq!(split_skill("  fix the bug"), (None, "fix the bug"));
        assert_eq!(split_skill("/"), (None, "/"));
        assert_eq!(split_skill("fix /review"), (None, "fix /review"));
        assert_eq!(split_skill(""), (None, ""));
    }

    #[test]
    fn a_worktree_session_round_trips() {
        let workspace = Workspace::Worktree {
            branch: "feat/x".into(),
            base: "main".into(),
            fetched: true,
            path: PathBuf::from("/work/api-wt/feat/x"),
        };
        let defaults = Some(BranchChoice {
            mode: BranchMode::NewBranch,
            branch: "feat/y".into(),
            base: Some("develop".into()),
            fetch: false,
        });

        let json = serde_json::to_string(&(&workspace, &defaults)).unwrap();
        let read: (Workspace, Option<BranchChoice>) = serde_json::from_str(&json).unwrap();

        assert_eq!(read, (workspace, defaults));
    }

    #[test]
    fn a_workspace_names_its_mode_branch_and_fetch() {
        let new_branch = Workspace::NewBranch {
            branch: "feat/x".into(),
            base: "main".into(),
            fetched: false,
        };
        let worktree = Workspace::Worktree {
            branch: "feat/y".into(),
            base: "main".into(),
            fetched: true,
            path: PathBuf::from("/wt"),
        };

        assert_eq!(Workspace::CheckedOut.mode(), BranchMode::CheckedOut);
        assert_eq!(Workspace::CheckedOut.branch(), None);
        assert_eq!(Workspace::CheckedOut.fetched(), None);
        assert_eq!(new_branch.mode(), BranchMode::NewBranch);
        assert_eq!(new_branch.branch(), Some("feat/x"));
        assert_eq!(new_branch.fetched(), Some(false));
        assert_eq!(worktree.mode(), BranchMode::Worktree);
        assert_eq!(worktree.branch(), Some("feat/y"));
        assert_eq!(worktree.fetched(), Some(true));
    }

    #[test]
    fn a_branch_choice_starts_checked_out_without_a_name_and_fetching() {
        let choice = BranchChoice::default();

        assert_eq!(choice.mode, BranchMode::CheckedOut);
        assert_eq!(choice.branch, "");
        assert_eq!(choice.base, None);
        assert!(choice.fetch);
    }
}
