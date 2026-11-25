//! A conversation with an agent: what it is scoped to, where it runs and how it is doing.
//!
//! A session also records the git workspace it works in, and with it what Partitur made for
//! it: the repository as it is checked out, a new branch in the main checkout, an existing
//! branch the main checkout switched to, a new branch in a worktree of its own, a new worktree
//! on an existing branch, or the worktree an existing branch was already checked out in. A
//! session saved before workspaces were recorded reads as checked out. Its branch mode is a
//! folder, the main checkout or a worktree, and a branch in it: as it is (only in the main
//! checkout), an existing one or a new one; an existing one is remembered as a new one, since
//! a picked branch belongs to one session. An orchestrator, which never edits, records instead
//! the branch settings its children start from; settings saved before an existing branch
//! could be picked read without one. A session keeps its first
//! prompt and the skill that prompt starts with; its title stays `untitled` until the agent
//! titles it, unless the user or an orchestrator gave one or an agent renamed the session. A session whose stored workflow
//! status does not parse loads without one. A child of an orchestrator records which of its
//! waits, a gate or a question, its orchestrator was last told about, so that wait is not
//! reported twice; a session saved before this was recorded loads with none told.
//!
//! A session's role says whether it is plain, an orchestrator or a child of another session.
//! It is stored as `role`, together with the `orchestrator` flag and the `parent` v0.4.0 reads
//! instead, both derived from the role, so v0.4.0 still loads what this version saves. A
//! session without `role`, as v0.4.0 saves it, reads as an orchestrator when it is flagged one,
//! as a child when it has a parent and as plain otherwise. When `role` is there it wins, should
//! a hand edit make the old fields disagree with it.
//!
//! A session also keeps the [marks](HistoryMarks) of its history, stored last as `history`,
//! which v0.4.0 ignores and drops when it rewrites the session. A session read without them,
//! saved by v0.4.0 or before they were kept, has none until the store fills them in from its
//! history.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::archive::Archived;
use crate::history_marks::HistoryMarks;

use crate::ids::{
    BackendId, BackendSessionId, ConfigValueId, ProjectId, QuestionId, RepositoryId, SessionId,
};
use crate::tool_call::ToolRole;
use crate::workflow::WorkflowStatus;

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
    /// The other state: open becomes folded and folded becomes open.
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Open => Self::Folded,
            Self::Folded => Self::Open,
        }
    }

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
    /// Chosen by the user; an agent's title never replaces it. Titles an orchestrator gave
    /// its children before [`Chosen`](Self::Chosen) existed load as this too.
    Given,
    /// Chosen by an agent: set with `rename_session`, or an orchestrator's title for a child
    /// the approval card left as proposed. An agent's automatic title never replaces it.
    Chosen,
}

impl TitleSource {
    /// [`Given`](Self::Given), the default of the proposals recorded before titles had a
    /// source.
    pub(crate) const fn given() -> Self {
        Self::Given
    }
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

/// Which folder of a repository a session works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// The repository's main checkout.
    MainCheckout,
    /// A worktree of the repository, which the session runs in.
    Worktree,
}

/// Which branch a session works on in its folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchKind {
    /// Whatever the main checkout has checked out now; never in a worktree.
    AsIs,
    /// A branch the repository already has.
    Existing,
    /// A new branch.
    New,
}

/// How a session uses the git repository it works in: a [folder](Folder) and a
/// [branch](BranchKind) in it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchMode {
    /// A new branch in a worktree of its own, which the session runs in.
    Worktree,
    /// A new branch that the repository's main checkout switches to.
    NewBranch,
    /// The repository as it is checked out now.
    #[default]
    CheckedOut,
    /// An existing branch that the repository's main checkout switches to.
    ExistingBranch,
    /// An existing branch in a worktree, which the session runs in: the worktree the branch
    /// is already checked out in, or a new one.
    ExistingWorktree,
}

impl BranchMode {
    /// The folder the session works in.
    pub fn folder(self) -> Folder {
        match self {
            Self::CheckedOut | Self::NewBranch | Self::ExistingBranch => Folder::MainCheckout,
            Self::Worktree | Self::ExistingWorktree => Folder::Worktree,
        }
    }

    /// The branch the session works on in its folder.
    pub fn kind(self) -> BranchKind {
        match self {
            Self::CheckedOut => BranchKind::AsIs,
            Self::ExistingBranch | Self::ExistingWorktree => BranchKind::Existing,
            Self::NewBranch | Self::Worktree => BranchKind::New,
        }
    }

    /// The mode of `kind` in `folder`, `None` for [`AsIs`](BranchKind::AsIs) in a
    /// worktree, which has no branch checked out until one is chosen.
    pub fn from_parts(folder: Folder, kind: BranchKind) -> Option<Self> {
        match (folder, kind) {
            (Folder::MainCheckout, BranchKind::AsIs) => Some(Self::CheckedOut),
            (Folder::MainCheckout, BranchKind::Existing) => Some(Self::ExistingBranch),
            (Folder::MainCheckout, BranchKind::New) => Some(Self::NewBranch),
            (Folder::Worktree, BranchKind::AsIs) => None,
            (Folder::Worktree, BranchKind::Existing) => Some(Self::ExistingWorktree),
            (Folder::Worktree, BranchKind::New) => Some(Self::Worktree),
        }
    }

    /// The mode a later session starts from when this one was the latest: an existing
    /// branch belongs to one session, so it is remembered as a new branch in the same folder.
    #[must_use]
    pub fn remembered(self) -> Self {
        match self {
            Self::ExistingBranch => Self::NewBranch,
            Self::ExistingWorktree => Self::Worktree,
            Self::CheckedOut | Self::NewBranch | Self::Worktree => self,
        }
    }
}

/// The branch settings a session is started with: the mode, the new branch's name, the branch
/// it starts from, whether the base is fetched first and the existing branch picked.
///
/// A choice saved before existing branches could be picked reads without one, and a choice
/// without one is written without the `existing` key.
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
    /// The existing branch picked, `None` when none was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<String>,
}

impl BranchChoice {
    /// Moves the choice to `folder`, keeping its branch kind; [`AsIs`](BranchKind::AsIs)
    /// becomes [`New`](BranchKind::New) in a worktree.
    pub fn set_folder(&mut self, folder: Folder) {
        let kind = self.mode.kind();
        self.mode = BranchMode::from_parts(folder, kind).unwrap_or(BranchMode::Worktree);
    }

    /// Sets the branch kind in the current folder. Returns `false`, changing nothing, for
    /// [`AsIs`](BranchKind::AsIs) in a worktree.
    pub fn set_kind(&mut self, kind: BranchKind) -> bool {
        match BranchMode::from_parts(self.mode.folder(), kind) {
            Some(mode) => {
                self.mode = mode;
                true
            }
            None => false,
        }
    }

    /// Picks the existing branch `name` in the current folder.
    pub fn use_existing(&mut self, name: String) {
        self.set_kind(BranchKind::Existing);
        self.existing = Some(name);
    }
}

impl Default for BranchChoice {
    /// Checked-out, with no branch name, no base and no existing branch, fetching.
    fn default() -> Self {
        Self {
            mode: BranchMode::CheckedOut,
            branch: String::new(),
            base: None,
            fetch: true,
            existing: None,
        }
    }
}

/// The git workspace a session works in, as it was made when the session was created, and
/// so what Partitur made for it: cleanup and the undo of a cancelled create act only on that.
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
    /// An existing branch the repository's main checkout switched to; nothing was created.
    ExistingBranch {
        /// The branch switched to.
        branch: String,
    },
    /// A new worktree, which is the session's folder, on an existing branch; only the
    /// worktree was created.
    WorktreeOnExisting {
        /// The existing branch checked out in it.
        branch: String,
        /// Where the worktree is.
        path: PathBuf,
    },
    /// The worktree an existing branch was already checked out in, which is the session's
    /// folder; nothing was created.
    ExistingWorktree {
        /// The branch checked out in it.
        branch: String,
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
            Self::ExistingBranch { .. } => BranchMode::ExistingBranch,
            Self::WorktreeOnExisting { .. } | Self::ExistingWorktree { .. } => {
                BranchMode::ExistingWorktree
            }
        }
    }

    /// The branch the session works on, `None` when it works on the checked-out one.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::CheckedOut => None,
            Self::NewBranch { branch, .. }
            | Self::Worktree { branch, .. }
            | Self::ExistingBranch { branch }
            | Self::WorktreeOnExisting { branch, .. }
            | Self::ExistingWorktree { branch, .. } => Some(branch),
        }
    }

    /// The branch the session created, `None` when it created none.
    pub fn made_branch(&self) -> Option<&str> {
        match self {
            Self::NewBranch { branch, .. } | Self::Worktree { branch, .. } => Some(branch),
            Self::CheckedOut
            | Self::ExistingBranch { .. }
            | Self::WorktreeOnExisting { .. }
            | Self::ExistingWorktree { .. } => None,
        }
    }

    /// The worktree the session created, `None` when it created none.
    pub fn made_worktree(&self) -> Option<&Path> {
        match self {
            Self::Worktree { path, .. } | Self::WorktreeOnExisting { path, .. } => Some(path),
            Self::CheckedOut
            | Self::NewBranch { .. }
            | Self::ExistingBranch { .. }
            | Self::ExistingWorktree { .. } => None,
        }
    }

    /// The worktree the session runs in, made by it or not; `None` in the main checkout.
    pub fn worktree(&self) -> Option<&Path> {
        match self {
            Self::Worktree { path, .. }
            | Self::WorktreeOnExisting { path, .. }
            | Self::ExistingWorktree { path, .. } => Some(path),
            Self::CheckedOut | Self::NewBranch { .. } | Self::ExistingBranch { .. } => None,
        }
    }

    /// The branch the created branch started from, `None` when the session created no branch.
    pub fn base(&self) -> Option<&str> {
        match self {
            Self::NewBranch { base, .. } | Self::Worktree { base, .. } => Some(base),
            Self::CheckedOut
            | Self::ExistingBranch { .. }
            | Self::WorktreeOnExisting { .. }
            | Self::ExistingWorktree { .. } => None,
        }
    }

    /// Whether the created branch started from the fetched remote base, `None` when the
    /// session created no branch.
    pub fn fetched(&self) -> Option<bool> {
        match self {
            Self::NewBranch { fetched, .. } | Self::Worktree { fetched, .. } => Some(*fetched),
            Self::CheckedOut
            | Self::ExistingBranch { .. }
            | Self::WorktreeOnExisting { .. }
            | Self::ExistingWorktree { .. } => None,
        }
    }
}

/// What a session is in the tree of sessions: a plain session, an orchestrator, or a child
/// spawned from another session.
///
/// It is stored in a session's `role` field, externally tagged by its name: `"Plain"`,
/// `"Orchestrator"` or `{"Child":{"parent":"<session id>"}}`.
///
/// A `role` value this version does not know fails to read, and with it the project's whole
/// session list. A new variant therefore needs the store's format version raised (`FORMAT` in
/// `partitur-store`'s `file_store.rs`), so that an older Partitur refuses the data folder
/// instead of failing on its sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// A top-level session that orchestrates nothing.
    Plain,
    /// An orchestrator: project scope in a project with at least two repositories, decided
    /// when it was created. It is always top-level.
    Orchestrator,
    /// A session spawned from another one.
    Child {
        /// The session it was spawned from.
        parent: SessionId,
    },
}

impl Role {
    /// The session this one was spawned from, `None` for a top-level session.
    pub fn parent(&self) -> Option<&SessionId> {
        match self {
            Self::Child { parent } => Some(parent),
            Self::Plain | Self::Orchestrator => None,
        }
    }

    /// Whether the session is an orchestrator.
    pub fn is_orchestrator(&self) -> bool {
        matches!(self, Self::Orchestrator)
    }
}

/// One agent session, as persisted in the project's session list.
///
/// It is stored in a shape v0.4.0 still reads: next to the `role` it writes the
/// `orchestrator` flag and the `parent` that v0.4.0 reads instead, derived from the role, and
/// every other field keeps its name, place and attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "SessionWire", into = "SessionWire")]
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
    /// or an orchestrator gave it or an agent renamed it to.
    pub title: String,
    /// The id the backend assigned, `None` until the backend has created the session.
    pub backend_session: Option<BackendSessionId>,
    /// The configured backend the session runs on. A session saved before sessions named
    /// their backend reads as [unset](BackendId::is_unset) until the app is loaded.
    pub backend: BackendId,
    /// The model the user picked, `None` for the backend's default.
    pub model: Option<ConfigValueId>,
    /// The reasoning effort the user picked, `None` for the backend's default.
    pub effort: Option<ConfigValueId>,
    /// Where the session stands.
    pub status: Status,
    /// What the session is in the tree: plain, an orchestrator or a child.
    pub role: Role,
    /// Whether permission requests are granted without asking.
    pub auto_allow: bool,
    /// Whether the project's instruction chain was already sent to the agent.
    pub instructions_injected: bool,
    /// When the session was created, in seconds since the Unix epoch.
    pub created_at: u64,
    /// When the user archived the session, `None` while it is live. Session lists saved
    /// before archiving existed load as live.
    pub archived: Option<Archived>,
    /// The git workspace the session works in. Sessions saved before workspaces were
    /// recorded load as [`CheckedOut`](Workspace::CheckedOut).
    pub workspace: Workspace,
    /// For an orchestrator, the branch settings its children start from; `None` for any
    /// other session and for sessions saved before these were recorded.
    pub child_defaults: Option<BranchChoice>,
    /// The first prompt the user sent, whole; empty until there is one. Sessions saved before
    /// it was recorded load with it empty.
    pub first_prompt: String,
    /// The skill the first prompt starts with, such as `/brainstorming`; `None` when it
    /// names none.
    pub skill: Option<String>,
    /// Where the title comes from; only a [derived](TitleSource::Derived) one is replaced by
    /// the agent's.
    pub title_source: TitleSource,
    /// When the session last had activity, in seconds since the Unix epoch; `0` when none
    /// was recorded, as in sessions saved before it was.
    pub last_activity: u64,
    /// Whether the session's children are shown below it in the tree. Sessions saved before
    /// this was recorded load folded.
    pub children: Fold,
    /// Where the session stands in the workflow its agent reports; `None` for a session that
    /// reports none. A stored status that does not parse loads as `None`, so the session
    /// still loads and its status is rebuilt from its history.
    pub workflow: Option<WorkflowStatus>,
    /// The question the session asked the user and has no answer to yet; `None` when none is
    /// open.
    pub question: Option<QuestionId>,
    /// For a child of an orchestrator, the wait whose `child waits` prompt started a turn of
    /// the orchestrator last; `None` when none did, as in sessions saved before it was
    /// recorded.
    pub wait_reported: Option<WaitMark>,
    /// For a session in the repository as it was checked out, the first branch a reading of
    /// git saw it on, which a later branch is compared with; `None` until a reading saw one,
    /// and for every other session.
    pub seen_branch: Option<String>,
    /// The session's folder as the latest reading of git resolved it, through a symlink say,
    /// when that differs from `cwd`; the Git card counts both as the session's folder, also
    /// while it is archived or before git is read after a restart. `None` while no reading
    /// found another one.
    pub resolved_cwd: Option<PathBuf>,
    /// What the session's history holds, kept so that it is known without reading the
    /// history. `None` for a session saved without it, by v0.4.0 or before it was kept; the
    /// store fills it in from the history when it loads the session, and leaves it `None`
    /// only when the history cannot be read.
    pub history: Option<HistoryMarks>,
}

/// How a [`Session`] is stored: today's fields in v0.4.0's shape, with `role` and then
/// `history` added last.
///
/// Reading takes `role` when it is there, so it wins over `orchestrator` and `parent` should a
/// hand edit make them disagree; v0.4.0 rewrites a session without `role`, so its own writes
/// never do. Without `role`, as v0.4.0 stores a session, an `orchestrator` is an orchestrator,
/// a session with a `parent` a child of it, and any other plain. Writing derives
/// `orchestrator` and `parent` from the role.
#[derive(Serialize, Deserialize)]
struct SessionWire {
    id: SessionId,
    project: ProjectId,
    scope: Scope,
    cwd: PathBuf,
    title: String,
    backend_session: Option<BackendSessionId>,
    #[serde(default = "BackendId::unset")]
    backend: BackendId,
    model: Option<ConfigValueId>,
    effort: Option<ConfigValueId>,
    status: Status,
    parent: Option<SessionId>,
    auto_allow: bool,
    instructions_injected: bool,
    created_at: u64,
    #[serde(default)]
    orchestrator: bool,
    #[serde(default)]
    archived: Option<Archived>,
    #[serde(default)]
    workspace: Workspace,
    #[serde(default)]
    child_defaults: Option<BranchChoice>,
    #[serde(default)]
    first_prompt: String,
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    title_source: TitleSource,
    #[serde(default)]
    last_activity: u64,
    #[serde(default = "Fold::folded")]
    children: Fold,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_workflow"
    )]
    workflow: Option<WorkflowStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    question: Option<QuestionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait_reported: Option<WaitMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_cwd: Option<PathBuf>,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<HistoryMarks>,
}

impl From<SessionWire> for Session {
    fn from(wire: SessionWire) -> Self {
        let role = match wire.role {
            Some(role) => role,
            None if wire.orchestrator => Role::Orchestrator,
            None => wire
                .parent
                .map_or(Role::Plain, |parent| Role::Child { parent }),
        };
        Self {
            id: wire.id,
            project: wire.project,
            scope: wire.scope,
            cwd: wire.cwd,
            title: wire.title,
            backend_session: wire.backend_session,
            backend: wire.backend,
            model: wire.model,
            effort: wire.effort,
            status: wire.status,
            role,
            auto_allow: wire.auto_allow,
            instructions_injected: wire.instructions_injected,
            created_at: wire.created_at,
            archived: wire.archived,
            workspace: wire.workspace,
            child_defaults: wire.child_defaults,
            first_prompt: wire.first_prompt,
            skill: wire.skill,
            title_source: wire.title_source,
            last_activity: wire.last_activity,
            children: wire.children,
            workflow: wire.workflow,
            question: wire.question,
            wait_reported: wire.wait_reported,
            seen_branch: wire.seen_branch,
            resolved_cwd: wire.resolved_cwd,
            history: wire.history,
        }
    }
}

impl From<Session> for SessionWire {
    fn from(session: Session) -> Self {
        Self {
            id: session.id,
            project: session.project,
            scope: session.scope,
            cwd: session.cwd,
            title: session.title,
            backend_session: session.backend_session,
            backend: session.backend,
            model: session.model,
            effort: session.effort,
            status: session.status,
            parent: session.role.parent().cloned(),
            auto_allow: session.auto_allow,
            instructions_injected: session.instructions_injected,
            created_at: session.created_at,
            orchestrator: session.role.is_orchestrator(),
            archived: session.archived,
            workspace: session.workspace,
            child_defaults: session.child_defaults,
            first_prompt: session.first_prompt,
            skill: session.skill,
            title_source: session.title_source,
            last_activity: session.last_activity,
            children: session.children,
            workflow: session.workflow,
            question: session.question,
            wait_reported: session.wait_reported,
            seen_branch: session.seen_branch,
            resolved_cwd: session.resolved_cwd,
            role: Some(session.role),
            history: session.history,
        }
    }
}

/// Which wait of a child its orchestrator was told about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitMark {
    /// The gate that opened at `since`, in seconds since the Unix epoch.
    Gate {
        /// When the gate opened.
        since: u64,
    },
    /// The question with this id.
    Question(QuestionId),
}

/// Reads a stored workflow status, or `None` for any value that is not one.
fn lenient_workflow<'de, D>(deserializer: D) -> Result<Option<WorkflowStatus>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// A workflow status, or anything else.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient {
        Status(Box<WorkflowStatus>),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Lenient::deserialize(deserializer)? {
        Lenient::Status(status) => Some(*status),
        Lenient::Other(_) => None,
    })
}

impl Session {
    /// When the session was last active: its last activity, or its creation when none was
    /// recorded.
    pub fn active_at(&self) -> u64 {
        self.last_activity.max(self.created_at)
    }

    /// The tools the session is offered, after its role.
    pub fn tool_role(&self) -> ToolRole {
        match self.role {
            Role::Plain => ToolRole::Plain,
            Role::Orchestrator => ToolRole::Orchestrator,
            Role::Child { .. } => ToolRole::Child,
        }
    }
}

/// Splits a prompt into the skill it starts with and the rest.
///
/// The skill is the first word of the trimmed prompt when it starts with `/` and has at least
/// one more character; the rest is trimmed. Any other prompt has no skill and is returned
/// trimmed.
pub(crate) fn split_skill(prompt: &str) -> (Option<String>, &str) {
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

/// Derives a session title from a prompt: a child proposed without a title is titled from
/// the prompt its orchestrator gave it.
///
/// The title is the first line that is not blank, trimmed, and cut after 60 characters with
/// `…` appended when it was longer. A prompt without any text yields an empty title.
pub(crate) fn title_from_prompt(prompt: &str) -> String {
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

        assert_eq!(session.role, Role::Plain);
        assert_eq!(session.wait_reported, None);
    }

    #[test]
    fn a_reported_wait_round_trips() {
        for mark in [
            WaitMark::Gate { since: 7 },
            WaitMark::Question(QuestionId::new()),
        ] {
            let json = serde_json::to_string(&Some(mark.clone())).unwrap();
            let read: Option<WaitMark> = serde_json::from_str(&json).unwrap();

            assert_eq!(read, Some(mark));
        }
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

    /// A session as v0.4.0 stores it: `orchestrator` and `parent`, and no `role`.
    const V0_4_0_SESSION: &str = r#"{
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
            "created_at": 7
        }"#;

    /// [`V0_4_0_SESSION`] with `orchestrator` and `parent` set as given.
    fn v0_4_0_session(orchestrator: bool, parent: Option<&SessionId>) -> Session {
        let mut json: serde_json::Value = serde_json::from_str(V0_4_0_SESSION).unwrap();
        json["orchestrator"] = orchestrator.into();
        json["parent"] = serde_json::to_value(parent).unwrap();
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn tool_role_follows_the_role() {
        let mut session: Session = serde_json::from_str(V0_4_0_SESSION).unwrap();
        assert_eq!(session.tool_role(), ToolRole::Plain);
        session.role = Role::Child {
            parent: SessionId::new(),
        };
        assert_eq!(session.tool_role(), ToolRole::Child);
        session.role = Role::Orchestrator;
        assert_eq!(session.tool_role(), ToolRole::Orchestrator);
    }

    #[test]
    fn a_role_names_its_parent_and_whether_it_orchestrates() {
        let parent = SessionId::new();
        let child = Role::Child {
            parent: parent.clone(),
        };

        assert_eq!(Role::Plain.parent(), None);
        assert_eq!(Role::Orchestrator.parent(), None);
        assert_eq!(child.parent(), Some(&parent));
        assert!(!Role::Plain.is_orchestrator());
        assert!(Role::Orchestrator.is_orchestrator());
        assert!(!child.is_orchestrator());
    }

    #[test]
    fn v0_4_0_session_reads_role() {
        let parent = SessionId::new();

        assert_eq!(v0_4_0_session(false, None).role, Role::Plain);
        assert_eq!(
            v0_4_0_session(false, Some(&parent)).role,
            Role::Child {
                parent: parent.clone()
            }
        );
        assert_eq!(v0_4_0_session(true, None).role, Role::Orchestrator);
        // As `tool_role` read it before the role was stored: an orchestrator first.
        assert_eq!(v0_4_0_session(true, Some(&parent)).role, Role::Orchestrator);
    }

    #[test]
    fn a_stored_role_wins_over_the_old_fields() {
        let mut json: serde_json::Value = serde_json::from_str(V0_4_0_SESSION).unwrap();
        json["orchestrator"] = true.into();
        json["role"] = "Plain".into();

        let session: Session = serde_json::from_value(json).unwrap();

        assert_eq!(session.role, Role::Plain);
    }

    #[test]
    fn a_role_is_stored_tagged_by_its_name() {
        let parent = SessionId::new();
        let json = |role: Role| {
            let mut session: Session = serde_json::from_str(V0_4_0_SESSION).unwrap();
            session.role = role;
            serde_json::to_value(&session).unwrap()["role"].clone()
        };

        assert_eq!(json(Role::Plain), serde_json::json!("Plain"));
        assert_eq!(json(Role::Orchestrator), serde_json::json!("Orchestrator"));
        assert_eq!(
            json(Role::Child {
                parent: parent.clone()
            }),
            serde_json::json!({"Child": {"parent": parent.to_string()}})
        );
    }

    /// `Session` as v0.4.0 defines it (`crates/partitur-core/src/session.rs` at the tag): its
    /// field names, order and serde attributes.
    #[derive(Debug, Serialize, Deserialize)]
    struct V040Session {
        id: SessionId,
        project: ProjectId,
        scope: Scope,
        cwd: PathBuf,
        title: String,
        backend_session: Option<BackendSessionId>,
        #[serde(default = "BackendId::unset")]
        backend: BackendId,
        model: Option<ConfigValueId>,
        effort: Option<ConfigValueId>,
        status: Status,
        parent: Option<SessionId>,
        auto_allow: bool,
        instructions_injected: bool,
        created_at: u64,
        #[serde(default)]
        orchestrator: bool,
        #[serde(default)]
        archived: Option<Archived>,
        #[serde(default)]
        workspace: Workspace,
        #[serde(default)]
        child_defaults: Option<BranchChoice>,
        #[serde(default)]
        first_prompt: String,
        #[serde(default)]
        skill: Option<String>,
        #[serde(default)]
        title_source: TitleSource,
        #[serde(default)]
        last_activity: u64,
        #[serde(default = "Fold::folded")]
        children: Fold,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "lenient_workflow"
        )]
        workflow: Option<WorkflowStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<QuestionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_reported: Option<WaitMark>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seen_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_cwd: Option<PathBuf>,
    }

    #[test]
    fn role_round_trips_through_v0_4_0_shape() {
        let parent = SessionId::new();
        for (role, orchestrator, old_parent) in [
            (Role::Plain, false, None),
            (Role::Orchestrator, true, None),
            (
                Role::Child {
                    parent: parent.clone(),
                },
                false,
                Some(parent.clone()),
            ),
        ] {
            let mut session: Session = serde_json::from_str(V0_4_0_SESSION).unwrap();
            session.role = role.clone();
            let json = serde_json::to_string(&session).unwrap();

            let old: V040Session = serde_json::from_str(&json).unwrap();
            assert_eq!(old.orchestrator, orchestrator, "{role:?}");
            assert_eq!(old.parent, old_parent, "{role:?}");

            // And v0.4.0's rewrite, which drops `role`, still reads as the same role.
            let mut rewritten: serde_json::Value = serde_json::from_str(&json).unwrap();
            rewritten.as_object_mut().unwrap().remove("role");
            let read: Session = serde_json::from_value(rewritten).unwrap();
            assert_eq!(read, session);
        }
    }

    #[test]
    fn a_saved_session_is_v0_4_0_json_with_the_role_and_history_added_last() {
        let parent = SessionId::new();
        let mut session: Session = serde_json::from_str(V0_4_0_SESSION).unwrap();
        session.workflow = None;
        session.question = Some(QuestionId::new());
        session.seen_branch = Some("main".into());
        let marks = HistoryMarks {
            has_entries: true,
            has_history: false,
            open_permissions: vec![crate::PermissionId(3)],
        };
        for history in [None, Some(marks)] {
            for role in [
                Role::Plain,
                Role::Orchestrator,
                Role::Child {
                    parent: parent.clone(),
                },
            ] {
                session.role = role.clone();
                session.history.clone_from(&history);
                let json = serde_json::to_string(&session).unwrap();

                let old: V040Session = serde_json::from_str(&json).unwrap();
                let old_json = serde_json::to_string(&old).unwrap();
                let role_json = serde_json::to_string(&role).unwrap();
                let history_json = history.as_ref().map_or(String::new(), |marks| {
                    format!(",\"history\":{}", serde_json::to_string(marks).unwrap())
                });
                let expected = format!(
                    "{},\"role\":{role_json}{history_json}}}",
                    old_json.strip_suffix('}').unwrap()
                );
                assert_eq!(json, expected);
            }
        }
    }

    #[test]
    fn history_marks_round_trip_and_a_rewrite_without_them_reads_as_unknown() {
        let mut session: Session = serde_json::from_str(V0_4_0_SESSION).unwrap();
        assert_eq!(session.history, None, "v0.4.0 kept no marks");
        session.history = Some(HistoryMarks {
            has_entries: true,
            has_history: true,
            open_permissions: Vec::new(),
        });
        let json = serde_json::to_string(&session).unwrap();

        let read: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(read, session);

        // v0.4.0's rewrite drops them: they are unknown again, not false.
        let old: V040Session = serde_json::from_str(&json).unwrap();
        let rewritten: Session =
            serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(rewritten.history, None);
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
            existing: None,
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
        assert_eq!(choice.existing, None);
    }

    #[test]
    fn old_branch_choices_and_workspaces_load_unchanged() {
        let choice: BranchChoice =
            serde_json::from_str(r#"{"mode":"Worktree","branch":"x","base":"main","fetch":true}"#)
                .unwrap();
        assert_eq!(
            choice,
            BranchChoice {
                mode: BranchMode::Worktree,
                branch: "x".into(),
                base: Some("main".into()),
                fetch: true,
                existing: None,
            }
        );
        let written = serde_json::to_value(&choice).unwrap();
        assert_eq!(
            written,
            serde_json::json!({"mode":"Worktree","branch":"x","base":"main","fetch":true})
        );

        let workspace: Workspace = serde_json::from_str(
            r#"{"Worktree":{"branch":"x","base":"main","fetched":false,"path":"/wt/x"}}"#,
        )
        .unwrap();
        assert_eq!(
            workspace,
            Workspace::Worktree {
                branch: "x".into(),
                base: "main".into(),
                fetched: false,
                path: PathBuf::from("/wt/x"),
            }
        );

        let session = SessionId::new();
        let child = |mode: &str| {
            serde_json::from_str::<crate::backend::ApprovedChild>(&format!(
                r#"{{"session":"{session}","repository":"api","prompt":"p","title":"t","auto_allow":false{mode}}}"#
            ))
            .unwrap()
        };
        let new_branch = child(r#","mode":"NewBranch","branch":"feat/x","cwd":"/p/api""#);
        assert_eq!(new_branch.mode, BranchMode::NewBranch);
        assert_eq!(new_branch.branch.as_deref(), Some("feat/x"));
        assert_eq!(new_branch.cwd, PathBuf::from("/p/api"));
        assert_eq!(child("").mode, BranchMode::CheckedOut);
    }

    #[test]
    fn existing_choices_round_trip() {
        let choice = BranchChoice {
            mode: BranchMode::ExistingWorktree,
            branch: String::new(),
            base: None,
            fetch: true,
            existing: Some("feat/x".into()),
        };
        let json = serde_json::to_string(&choice).unwrap();
        assert_eq!(serde_json::from_str::<BranchChoice>(&json).unwrap(), choice);

        for workspace in [
            Workspace::ExistingBranch {
                branch: "feat/x".into(),
            },
            Workspace::WorktreeOnExisting {
                branch: "feat/x".into(),
                path: PathBuf::from("/wt/x"),
            },
            Workspace::ExistingWorktree {
                branch: "feat/x".into(),
                path: PathBuf::from("/wt/x"),
            },
        ] {
            let json = serde_json::to_string(&workspace).unwrap();
            assert_eq!(serde_json::from_str::<Workspace>(&json).unwrap(), workspace);
        }
    }

    #[test]
    fn modes_split_into_folder_and_kind() {
        let table = [
            (
                BranchMode::CheckedOut,
                Folder::MainCheckout,
                BranchKind::AsIs,
            ),
            (
                BranchMode::ExistingBranch,
                Folder::MainCheckout,
                BranchKind::Existing,
            ),
            (BranchMode::NewBranch, Folder::MainCheckout, BranchKind::New),
            (
                BranchMode::ExistingWorktree,
                Folder::Worktree,
                BranchKind::Existing,
            ),
            (BranchMode::Worktree, Folder::Worktree, BranchKind::New),
        ];

        for (mode, folder, kind) in table {
            assert_eq!(mode.folder(), folder, "{mode:?}");
            assert_eq!(mode.kind(), kind, "{mode:?}");
            assert_eq!(BranchMode::from_parts(folder, kind), Some(mode));
        }
        assert_eq!(
            BranchMode::from_parts(Folder::Worktree, BranchKind::AsIs),
            None
        );
    }

    #[test]
    fn existing_modes_are_remembered_as_new() {
        assert_eq!(
            BranchMode::ExistingBranch.remembered(),
            BranchMode::NewBranch
        );
        assert_eq!(
            BranchMode::ExistingWorktree.remembered(),
            BranchMode::Worktree
        );
        for mode in [
            BranchMode::CheckedOut,
            BranchMode::NewBranch,
            BranchMode::Worktree,
        ] {
            assert_eq!(mode.remembered(), mode);
        }
    }

    #[test]
    fn choosing_worktree_from_as_is_lands_on_new() {
        let mut choice = BranchChoice::default();

        choice.set_folder(Folder::Worktree);
        assert_eq!(choice.mode, BranchMode::Worktree);

        choice.set_folder(Folder::MainCheckout);
        assert_eq!(choice.mode, BranchMode::NewBranch);

        choice.mode = BranchMode::ExistingWorktree;
        choice.set_folder(Folder::MainCheckout);
        assert_eq!(choice.mode, BranchMode::ExistingBranch);
        choice.set_folder(Folder::Worktree);
        assert_eq!(choice.mode, BranchMode::ExistingWorktree);
    }

    #[test]
    fn as_is_is_refused_in_a_worktree() {
        let mut choice = BranchChoice {
            mode: BranchMode::Worktree,
            ..BranchChoice::default()
        };

        assert!(!choice.set_kind(BranchKind::AsIs));
        assert_eq!(choice.mode, BranchMode::Worktree);

        assert!(choice.set_kind(BranchKind::Existing));
        assert_eq!(choice.mode, BranchMode::ExistingWorktree);

        choice.mode = BranchMode::NewBranch;
        assert!(choice.set_kind(BranchKind::AsIs));
        assert_eq!(choice.mode, BranchMode::CheckedOut);
    }

    #[test]
    fn use_existing_keeps_the_folder() {
        let mut worktree = BranchChoice {
            mode: BranchMode::Worktree,
            ..BranchChoice::default()
        };
        worktree.use_existing("feat/x".into());
        assert_eq!(worktree.mode, BranchMode::ExistingWorktree);
        assert_eq!(worktree.existing.as_deref(), Some("feat/x"));

        let mut new_branch = BranchChoice {
            mode: BranchMode::NewBranch,
            ..BranchChoice::default()
        };
        new_branch.use_existing("feat/y".into());
        assert_eq!(new_branch.mode, BranchMode::ExistingBranch);
        assert_eq!(new_branch.existing.as_deref(), Some("feat/y"));
    }

    #[test]
    fn workspaces_say_what_they_made() {
        let path = PathBuf::from("/wt/x");
        let on_existing = Workspace::WorktreeOnExisting {
            branch: "feat/x".into(),
            path: path.clone(),
        };
        assert_eq!(on_existing.made_worktree(), Some(path.as_path()));
        assert_eq!(on_existing.made_branch(), None);
        assert_eq!(on_existing.branch(), Some("feat/x"));
        assert_eq!(on_existing.worktree(), Some(path.as_path()));
        assert_eq!(on_existing.mode(), BranchMode::ExistingWorktree);
        assert_eq!(on_existing.base(), None);
        assert_eq!(on_existing.fetched(), None);

        let reused = Workspace::ExistingWorktree {
            branch: "feat/x".into(),
            path: path.clone(),
        };
        assert_eq!(reused.made_worktree(), None);
        assert_eq!(reused.made_branch(), None);
        assert_eq!(reused.branch(), Some("feat/x"));
        assert_eq!(reused.worktree(), Some(path.as_path()));
        assert_eq!(reused.mode(), BranchMode::ExistingWorktree);
        assert_eq!(reused.base(), None);
        assert_eq!(reused.fetched(), None);

        let switched = Workspace::ExistingBranch {
            branch: "feat/x".into(),
        };
        assert_eq!(switched.made_worktree(), None);
        assert_eq!(switched.made_branch(), None);
        assert_eq!(switched.branch(), Some("feat/x"));
        assert_eq!(switched.worktree(), None);
        assert_eq!(switched.mode(), BranchMode::ExistingBranch);
        assert_eq!(switched.base(), None);
        assert_eq!(switched.fetched(), None);

        let worktree = Workspace::Worktree {
            branch: "feat/y".into(),
            base: "main".into(),
            fetched: true,
            path: path.clone(),
        };
        assert_eq!(worktree.made_worktree(), Some(path.as_path()));
        assert_eq!(worktree.made_branch(), Some("feat/y"));
        assert_eq!(worktree.worktree(), Some(path.as_path()));

        let new_branch = Workspace::NewBranch {
            branch: "feat/z".into(),
            base: "main".into(),
            fetched: false,
        };
        assert_eq!(new_branch.made_worktree(), None);
        assert_eq!(new_branch.made_branch(), Some("feat/z"));
        assert_eq!(new_branch.worktree(), None);

        assert_eq!(Workspace::CheckedOut.made_worktree(), None);
        assert_eq!(Workspace::CheckedOut.made_branch(), None);
        assert_eq!(Workspace::CheckedOut.worktree(), None);
    }
}
