//! Plain data the UI renders, built by [`App`](crate::App) so views stay free of logic.
//!
//! A child session that waits for its orchestrator at a gate or with a question is marked
//! delegated: it does not need the user, so it marks neither itself nor the sessions above it
//! as needing attention, its step does not count as a gate waiting for the user, and it sorts
//! with the running sessions. The permission requests of an orchestrator's children that
//! wait for the user are part of the orchestrator's session view, so the user can answer
//! them there.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use crate::archive::Archived;
use crate::backend::{ConfigCategory, ConfigChoice, ConfigOption, PermissionOption};
use crate::ids::{
    BackendId, BackendInfo, ConfigValueId, PermissionId, ProjectId, ProposalId, RepositoryId,
    SessionId,
};
use crate::instructions::{InstructionChain, InstructionScope};
use crate::orchestrator::orchestrated_repositories;
use crate::project::{Project, Repository};
use crate::session::{BranchChoice, BranchMode, Fold, Scope, Session, Status};
use crate::transcript::{Entry, Transcript};
use crate::workflow;
use crate::workflow::display::{
    BlockMark, DocRef, GateView, OutlineBlock, OutlineLine, RailView, ReportedDoc,
};

pub use crate::workflow::display::StepCell;

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
    /// Whether `CLAUDE.md` exists in the project root.
    pub has_global_claude_md: bool,
    /// The project's live top-level sessions, those needing you first, then the running ones,
    /// then the rest.
    pub sessions: Vec<SessionNode>,
    /// The project's archived top-level sessions with the sessions below them, the latest
    /// archived first; sessions archived together keep the order of the live list (by need,
    /// then the most recently active, then the newest).
    pub archived: Vec<SessionNode>,
    /// The project's repositories other than the project root itself, as discovered.
    pub repositories: Vec<RepositoryNode>,
    /// How many repositories the project has, counting the project root when it is one: a
    /// project that is itself a single repository counts 1 while
    /// [`repositories`](Self::repositories) is empty.
    pub repository_count: usize,
    /// Whether a removal a delete asked for runs in one of the project's repositories.
    pub cleaning: bool,
    /// Whether the project's sessions are shown below it in the tree.
    pub fold: Fold,
}

/// What `App` knows of its sessions beyond what is stored: the transcripts loaded so far,
/// how many background tasks each session's agent runs, and which sessions wait
/// for their orchestrator. A session without an entry has no transcript loaded, or runs no
/// background tasks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Live<'a> {
    /// The transcripts loaded so far.
    pub(crate) transcripts: &'a HashMap<SessionId, Transcript>,
    /// The number of running background tasks of each session that runs any.
    pub(crate) background: &'a HashMap<SessionId, usize>,
    /// The sessions that wait for their orchestrator, not for the user.
    pub(crate) delegated: &'a HashSet<SessionId>,
}

impl Live<'_> {
    /// The number of background tasks `session`'s agent runs.
    fn background(&self, session: &SessionId) -> usize {
        self.background.get(session).copied().unwrap_or_default()
    }
}

/// Whether a session at `status` shows its `background` instead of its status: it does while
/// any run and its own turn is over, `Done` or `Idle`.
pub fn background_shown(status: Status, background: usize) -> bool {
    background > 0 && matches!(status, Status::Done | Status::Idle)
}

/// Whether a session at `status` running `background` is still at work: running, waiting for
/// you, or showing background tasks that run.
pub(crate) fn at_work(status: Status, background: usize) -> bool {
    matches!(status, Status::Running | Status::WaitingForYou)
        || background_shown(status, background)
}

/// One session in the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact the tree shows, not a state"
)]
pub struct SessionNode {
    /// The session's identity.
    pub id: SessionId,
    /// Display title.
    pub title: String,
    /// Where the session stands: the session's own status, except that a `Done` session with
    /// a session below it at work (showing `Running` or `WaitingForYou`, or running
    /// background tasks) shows `Running`, since the work is not over while its children still run.
    pub status: Status,
    /// How many background tasks the session's agent runs; 0 when none.
    pub background: usize,
    /// Whether this session, or a session below it, is waiting for you. A
    /// [`delegated`](Self::delegated) session does not wait for you.
    pub attention: bool,
    /// Whether the session waits at a gate or with a question for its orchestrator, which
    /// decides for it, rather than for you; its status is still `WaitingForYou`.
    pub delegated: bool,
    /// The name of the repository the session works in; empty for a session of the whole
    /// project.
    pub scope_label: String,
    /// The id of the backend the session runs on, which may no longer be configured.
    pub backend: String,
    /// Whether the session is an orchestrator, which hands its work to child sessions.
    pub orchestrator: bool,
    /// The latest context usage as `(used, size)` tokens. Known only for a session whose
    /// transcript is loaded (it was created or selected since the start) and whose agent
    /// reported a usage; `None` otherwise.
    pub usage: Option<(u64, u64)>,
    /// The prompt that started the session, empty when none was recorded.
    pub first_prompt: String,
    /// The skill the first prompt starts with, `None` when it names none.
    pub skill: Option<String>,
    /// When the session was last active, in seconds since the Unix epoch.
    pub active_at: u64,
    /// Whether the session's children are shown below it in the tree.
    pub expanded: bool,
    /// Sessions spawned from this one, those needing you first, as the project's sessions are.
    pub children: Vec<SessionNode>,
    /// When the session was archived, `None` while it is live.
    pub archived: Option<Archived>,
    /// Where the session stands in its workflow, `None` for a session that reports none.
    /// The gate of a [`delegated`](Self::delegated) session does not count as waiting.
    pub step: Option<StepCell>,
}

/// What the UI shows of a session's workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowView {
    /// The flow rail.
    pub rail: RailView,
    /// The outline's blocks.
    pub outline: Vec<OutlineBlock>,
    /// The documents the workflow works on.
    pub documents: Vec<DocRef>,
    /// The gate waiting for the user, `None` when none waits.
    pub gate: Option<GateView>,
}

impl WorkflowView {
    /// The view of the workflow `session` follows, `None` for a session that reports none.
    ///
    /// An orchestrator's documents and outline go on with every document its children in
    /// `sessions` reported, oldest child first and archived ones included: each keyed
    /// `{child id}/{key}`, labelled `{child title} · {label}`, its path joined to the
    /// child's folder, and each with an outline block headed by the child's title.
    #[must_use]
    pub fn of(session: &Session, sessions: &[Session]) -> Option<Self> {
        let (rail, mut outline, mut documents, gate) = workflow::view(session.workflow.as_ref()?);
        if session.orchestrator {
            let mut children: Vec<&Session> = sessions
                .iter()
                .filter(|child| child.parent.as_ref() == Some(&session.id))
                .collect();
            children.sort_by_key(|child| child.created_at);
            for (block, document) in children.into_iter().flat_map(child_documents) {
                outline.push(block);
                documents.push(document);
            }
        }
        Some(Self {
            rail,
            outline,
            documents,
            gate,
        })
    }

    /// The document keyed `key`, `None` when the workflow has none of that key.
    #[must_use]
    pub fn document(&self, key: &str) -> Option<&DocRef> {
        self.documents.iter().find(|document| document.key == key)
    }

    /// The key of the document shown when the user picked none: the current block's
    /// document, else the first document.
    #[must_use]
    pub fn default_document(&self) -> Option<&str> {
        self.outline
            .iter()
            .find(|block| block.current)
            .and_then(|block| block.doc.as_deref())
            .or_else(|| self.documents.first().map(|document| document.key.as_str()))
    }
}

/// The documents `child` reported, each with the outline block that opens it on its
/// orchestrator's Spec tab.
fn child_documents(child: &Session) -> Vec<(OutlineBlock, DocRef)> {
    let Some(status) = child.workflow.as_ref() else {
        return Vec::new();
    };
    let (_, _, documents, _) = workflow::view(status);
    documents
        .into_iter()
        .filter_map(|document| {
            let reported = document.reported?;
            let key = format!("{}/{}", child.id, document.key);
            let block = OutlineBlock {
                label: child.title.clone(),
                mark: BlockMark::None,
                lines: vec![OutlineLine::Note(document.label.clone())],
                doc: Some(key.clone()),
                current: false,
            };
            let document = DocRef {
                key,
                label: format!("{} · {}", child.title, document.label),
                reported: Some(ReportedDoc {
                    path: child.cwd.join(reported.path),
                    ..reported
                }),
                tasks: document.tasks,
            };
            Some((block, document))
        })
        .collect()
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
    /// Whether `CLAUDE.md` exists in the repository.
    pub has_claude_md: bool,
}

/// Everything the session view shows for the selected session.
///
/// The entries are borrowed from the session's transcript, so building the view copies none
/// of them; [`revision`](Self::revision) tells the view when they changed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact the session view shows, not a state"
)]
pub struct SessionViewModel<'a> {
    /// The session's identity.
    pub id: SessionId,
    /// The project's name, the parent session's title when there is one, and the title.
    pub breadcrumb: Vec<String>,
    /// Where the session stands.
    pub status: Status,
    /// How many background tasks the session's agent runs; 0 when none.
    pub background: usize,
    /// Whether a session below this one is waiting for you; this session's own status is in
    /// [`status`](Self::status).
    pub attention: bool,
    /// Whether the session waits at a gate or with a question for its orchestrator rather
    /// than for you, as [`SessionNode::delegated`] says.
    pub delegated: bool,
    /// Whether the session is an orchestrator, which can propose children.
    pub orchestrator: bool,
    /// The sessions spawned from this one, newest first, each with its context usage when
    /// its transcript is loaded and its agent reported one.
    pub children: Vec<SessionNode>,
    /// The project's repositories other than the project root itself, in project order, for
    /// adding a child session on the card; for an orchestrator, the repositories it works on,
    /// which are the root repository when it is the project's only one.
    pub repositories: Vec<RepositoryNode>,
    /// The transcript entries, oldest first.
    pub entries: &'a [Entry],
    /// Changes whenever the entries change.
    pub revision: u64,
    /// The latest context usage as `(used, size)` tokens, if the agent reported any.
    pub usage: Option<(u64, u64)>,
    /// The model picker, when the backend offers one.
    pub model: Option<ChoiceView>,
    /// The reasoning effort picker, when the backend offers one.
    pub effort: Option<ChoiceView>,
    /// Whether permission requests are granted without asking.
    pub auto_allow: bool,
    /// The configured backends the session can be switched to, sorted by id (the order the
    /// app was loaded with).
    pub backends: Vec<BackendInfo>,
    /// The backend the session runs on, which may be one no longer in
    /// [`backends`](Self::backends).
    pub backend: BackendId,
    /// The folder the agent runs in, which the paths of its tool calls are shown relative
    /// to.
    pub cwd: PathBuf,
    /// When the session was archived, `None` while it is live. An archived session is shown
    /// read-only.
    pub archived: Option<Archived>,
    /// For an orchestrator, the branch settings its children start from; `None` for any
    /// other session.
    pub child_defaults: Option<BranchChoice>,
    /// For an orchestrator, the approval of one of its proposals that is in flight or failed
    /// last; `None` when there is none.
    pub approval: Option<ApprovalView>,
    /// The session's workflow, `None` for a session that reports none.
    pub workflow: Option<WorkflowView>,
    /// The permission requests of the session's children that wait for the user, oldest
    /// first, as [`App::child_permissions`](crate::App::child_permissions) lists them.
    pub child_permissions: Vec<ChildPermission>,
}

/// A permission request of a child session that waits for the user, shown in its
/// orchestrator's session and answered there for the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildPermission {
    /// The child that asks.
    pub child: SessionId,
    /// The display name of the repository the child works in.
    pub repository: String,
    /// The request.
    pub permission: PermissionId,
    /// The request's title, as the child's own card shows it.
    pub title: String,
    /// The choices the request offers.
    pub options: Vec<PermissionOption>,
}

/// One configuration picker: the value in effect and the values to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceView {
    /// The value in effect.
    pub current: ConfigValueId,
    /// The current value's label, or its id when the choices do not name it.
    pub current_name: String,
    /// The values the user can pick from.
    pub choices: Vec<ConfigChoice>,
}

/// Everything the new-session sheet shows about the session being prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftView {
    /// The name of the project the session is prepared in.
    pub project: String,
    /// The repository's name for a repository session, `None` for a project session.
    pub repository: Option<String>,
    /// How many repositories the project has, the root included when it is one.
    pub repositories: usize,
    /// What the session will work on.
    pub scope: Scope,
    /// The scopes to pick from: the project first, labelled `orchestrator · all N repos` when
    /// the session would be an orchestrator and with the project's name otherwise, then each
    /// repository below the root by name, then, in a project with a single repository,
    /// `orchestrator · for this repository`.
    pub scopes: Vec<ScopeChoice>,
    /// Whether the session will be an orchestrator.
    pub orchestrator: bool,
    /// The backend the warm session runs on.
    pub backend: BackendId,
    /// The configured backends to pick from, sorted by id (the order the app was loaded
    /// with).
    pub backends: Vec<BackendInfo>,
    /// The model picker.
    pub model: DraftChoice,
    /// The reasoning effort picker.
    pub effort: DraftChoice,
    /// Whether the session will grant permission requests without asking.
    pub auto_allow: bool,
    /// How far the warm session has started.
    pub warmup: Warmup,
    /// The branch and worktree box.
    pub branch: BranchView,
    /// Whether the git work of Create is in flight.
    pub creating: bool,
    /// Why the last Create failed; cleared by the next change to the draft or Create.
    pub failure: Option<String>,
}

impl DraftView {
    /// Whether Create can be pressed: no create is in flight, the warm session did not fail
    /// and nothing in the branch box blocks it.
    pub fn can_create(&self) -> bool {
        !self.creating && !matches!(self.warmup, Warmup::Failed(_)) && self.branch.blocker.is_none()
    }
}

/// A scope the new-session sheet offers, with the label its picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeChoice {
    /// What the session would work on.
    pub scope: Scope,
    /// Whether picking this creates an orchestrator.
    pub orchestrator: bool,
    /// The label: a repository's name, followed by ` · AGENTS.md` and ` · CLAUDE.md` for the
    /// instruction files it has, or the project's or an orchestrator's label.
    pub label: String,
}

impl fmt::Display for ScopeChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

/// The branch and worktree box of the new-session sheet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchView {
    /// The modes offered, in the order Worktree, New branch, Checked-out.
    pub modes: Vec<BranchMode>,
    /// The mode picked.
    pub mode: BranchMode,
    /// The new branch's name, as typed.
    pub branch: String,
    /// Whether git has not answered about the repositories yet.
    pub loading: bool,
    /// The branches the new branch can start from, sorted; empty while loading.
    pub bases: Vec<String>,
    /// The branch it starts from; `None` on an orchestrator sheet whose repositories have no
    /// branch in common ("per child"), or while loading.
    pub base: Option<String>,
    /// Whether the base is fetched first, as it applies: wanted and possible, or while
    /// loading, wanted.
    pub fetch: bool,
    /// The remote fetched before starting from the base; `None` when the base has no remote
    /// counterpart, or while loading.
    pub fetch_remote: Option<String>,
    /// The branch the repository has checked out now; `None` when it is detached, for no
    /// single repository, or while loading.
    pub checked_out: Option<String>,
    /// Where the worktree goes, in Worktree mode of a repository session with a branch name.
    pub worktree: Option<PathBuf>,
    /// A line explaining the offered modes or what the mode does.
    pub note: Option<String>,
    /// Why Create cannot be pressed, `None` when it can.
    pub blocker: Option<String>,
}

/// How one child of a proposed split would start in its own repository, as the approval card
/// shows its branch settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildCheck {
    /// Whether git has not answered about the child's repository yet; only a Worktree or New
    /// branch child waits for it.
    pub loading: bool,
    /// The local branches of the child's repository, sorted; empty while loading and for a
    /// Checked-out child.
    pub bases: Vec<String>,
    /// The branch the child's new branch starts from: the one chosen when its repository has
    /// it, else the branch checked out there; `None` while loading and for a Checked-out child.
    pub base: Option<String>,
    /// Whether the base is fetched first: wanted, and the base has a remote counterpart.
    pub fetch: bool,
    /// The remote fetched before starting from the base; `None` when the base has no remote
    /// counterpart.
    pub fetch_remote: Option<String>,
    /// Where the child's worktree goes, in Worktree mode with a branch name.
    pub worktree: Option<PathBuf>,
    /// Why the split cannot be approved with this child, `None` when it can.
    pub problem: Option<String>,
}

/// Where the approval of an orchestrator's proposed split stands, while it is in flight or
/// after it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalView {
    /// The proposal being approved.
    pub proposal: ProposalId,
    /// Whether the git work of the approval is in flight; the card is locked meanwhile.
    pub pending: bool,
    /// Why the last approval failed; the proposal stays undecided then.
    pub error: Option<String>,
}

/// One picker of the new-session sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftChoice {
    /// The agent has not reported its options, or failed to start: the value wanted so far,
    /// if any.
    Waiting(Option<ConfigValueId>),
    /// The agent offers this option; the picker's current value is the selection.
    Offered(ChoiceView),
    /// The agent does not offer this option.
    NotOffered,
}

/// How far the warm session of the new-session sheet has started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warmup {
    /// The agent is starting.
    Loading,
    /// The agent is running and has reported its options.
    Ready,
    /// The agent could not start, for this reason.
    Failed(String),
}

/// Everything the env view shows about the selected session's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvViewModel {
    /// The same breadcrumb as the session view.
    pub breadcrumb: Vec<String>,
    /// The instruction files that apply to the session as they are on disk now: the readable
    /// ones in injection order, then the skipped ones.
    pub instruction_files: Vec<InstructionFileView>,
    /// The id of the backend the session runs on.
    pub backend: String,
    /// The command that starts the backend's adapter, `None` when the backend is no longer
    /// configured.
    pub command: Option<String>,
    /// The current model's label, when the backend offers a model option.
    pub model: Option<String>,
    /// The current reasoning effort's label, when the backend offers an effort option.
    pub effort: Option<String>,
    /// The folder the agent runs in.
    pub cwd: PathBuf,
    /// The latest context usage as `(used, size)` tokens, if the agent reported any.
    pub usage: Option<(u64, u64)>,
    /// The first prompt the session's orchestrator sent it: the brief it was started with.
    /// `None` for a session no orchestrator prompted.
    pub orchestrator_brief: Option<String>,
    /// When the session was archived, `None` while it is live.
    pub archived: Option<Archived>,
    /// The session's workflow, `None` for a session that reports none.
    pub workflow: Option<WorkflowView>,
}

/// What the confirmation of archiving or deleting a session tells about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// The session to archive or delete, a top-level one.
    pub id: SessionId,
    /// Its title.
    pub title: String,
    /// The name of its project.
    pub project: String,
    /// Whether it is an orchestrator.
    pub orchestrator: bool,
    /// How many sessions below it go with it.
    pub children: usize,
    /// Where it stands, as the tree shows it: a `Done` session with a session below it
    /// that is at work shows `Running`.
    pub status: Status,
    /// How many background tasks its agent runs; 0 when none.
    pub background: usize,
    /// Whether it stops work: the session, or a session below it, is running or waiting, or
    /// its background tasks still run.
    pub stops: bool,
    /// What deleting can also remove from git: one line per Worktree or New branch session
    /// of the family, in family order, each loading until git answers. Empty for archiving.
    pub cleanup: Vec<CleanupView>,
}

/// One line of the delete sheet: what deleting can also remove for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupView {
    /// The session the line is about.
    pub session: SessionId,
    /// Its title.
    pub title: String,
    /// The name of its repository.
    pub repository: String,
    /// Whether git has not answered yet; nothing is offered meanwhile.
    pub loading: bool,
    /// Whether the repository has a changing request in flight; nothing can be checked
    /// meanwhile.
    pub busy: bool,
    /// The session's worktree, while git still lists it.
    pub worktree: Option<WorktreeCleanup>,
    /// The session's branch, while it still exists.
    pub branch: Option<BranchCleanup>,
}

/// A worktree the delete sheet offers to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCleanup {
    /// Where it is, as git lists it.
    pub path: PathBuf,
    /// How many uncommitted or untracked entries it has, `None` when git could not tell.
    pub changes: Option<usize>,
}

/// A branch the delete sheet offers to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCleanup {
    /// The branch name.
    pub name: String,
    /// Whether it is merged into the base the session started from; a failed check counts
    /// as not merged.
    pub merged: bool,
    /// The worktree, other than the session's own, that has it checked out: it cannot be
    /// deleted then.
    pub checked_out: Option<PathBuf>,
    /// Whether the session's own worktree has it checked out: it can be deleted only
    /// together with that worktree.
    pub in_own_worktree: bool,
    /// Whether a worktree whose folder is gone, one git considers prunable, still has it
    /// checked out: git refuses to delete it until `git worktree prune` ran.
    pub prunable: bool,
}

/// One instruction file in the env view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFileView {
    /// The file's location.
    pub path: PathBuf,
    /// Where the file applies.
    pub scope: InstructionScope,
    /// Whether the file was sent to the agent with the session's first prompt.
    pub injected: bool,
    /// Why the file cannot be used, `None` when it was read.
    pub skipped_reason: Option<String>,
}

impl TreeModel {
    /// Builds the tree: the open ones of `projects` in order, each with its sessions newest
    /// first (children nested under their parent) and its repositories other than the
    /// project root itself. A session's context usage comes from its transcript in
    /// `transcripts`, which holds only the transcripts loaded so far.
    pub(crate) fn build(projects: &[Project], sessions: &[Session], live: Live<'_>) -> Self {
        Self {
            projects: projects
                .iter()
                .filter(|project| !project.closed)
                .map(|project| project_node(project, sessions, live))
                .collect(),
        }
    }
}

impl<'a> SessionViewModel<'a> {
    /// Whether a turn is in progress, running or waiting for a permission answer, so the
    /// view offers Stop instead of Send and takes no prompt.
    ///
    /// A session that waits for you only for the decision on an approval card is not in a
    /// turn: its transcript has no unanswered permission request, so it takes a prompt.
    pub fn running(&self) -> bool {
        self.status == Status::Running
            || (self.status == Status::WaitingForYou && self.awaiting_permission())
    }

    /// Whether a permission request in the transcript waits for an answer.
    fn awaiting_permission(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Permission { answer: None, .. }))
    }

    /// Builds the view of `session`, which belongs to `project`, from its transcript, the
    /// config options its backend reported and the configured `backends`. It and the sessions
    /// spawned from it take their running background tasks from `live`, and the spawned ones their
    /// context usage from its transcripts, which are only the ones loaded so far.
    pub(crate) fn build(
        project: &Project,
        sessions: &[Session],
        live: Live<'_>,
        session: &Session,
        transcript: &'a Transcript,
        config: &[ConfigOption],
        backends: &[BackendInfo],
    ) -> Self {
        let children = session_nodes(project, sessions, live, Some(&session.id));
        Self {
            id: session.id.clone(),
            breadcrumb: breadcrumb(project, sessions, session),
            status: session.status,
            background: live.background(&session.id),
            attention: children.iter().any(|child| child.attention),
            delegated: live.delegated.contains(&session.id),
            orchestrator: session.orchestrator,
            children,
            repositories: if session.orchestrator {
                orchestrated_repositories(project)
                    .into_iter()
                    .map(repository_node)
                    .collect()
            } else {
                project
                    .repositories
                    .iter()
                    .filter(|repository| !repository.is_root)
                    .map(repository_node)
                    .collect()
            },
            entries: transcript.entries(),
            revision: transcript.revision(),
            usage: transcript.usage(),
            model: choice_view(config, &ConfigCategory::Model),
            effort: choice_view(config, &ConfigCategory::Effort),
            auto_allow: session.auto_allow,
            backends: backends.to_vec(),
            backend: session.backend.clone(),
            cwd: session.cwd.clone(),
            archived: session.archived,
            child_defaults: session.child_defaults.clone(),
            approval: None,
            workflow: WorkflowView::of(session, sessions),
            child_permissions: Vec::new(),
        }
    }
}

impl EnvViewModel {
    /// Builds the env view of `session`, which belongs to `project`, from the instruction
    /// chain that applies to it now, its transcript, the config options its backend
    /// reported and the configured `backends`.
    pub(crate) fn build(
        project: &Project,
        sessions: &[Session],
        session: &Session,
        chain: InstructionChain,
        transcript: &Transcript,
        config: &[ConfigOption],
        backends: &[BackendInfo],
    ) -> Self {
        let read = chain.files.into_iter().map(|file| InstructionFileView {
            path: file.path,
            scope: file.scope,
            injected: session.instructions_injected,
            skipped_reason: None,
        });
        let skipped = chain.skipped.into_iter().map(|file| InstructionFileView {
            path: file.path,
            scope: file.scope,
            injected: false,
            skipped_reason: Some(file.reason),
        });
        Self {
            breadcrumb: breadcrumb(project, sessions, session),
            instruction_files: read.chain(skipped).collect(),
            backend: session.backend.to_string(),
            command: backends
                .iter()
                .find(|info| info.id == session.backend)
                .map(|info| info.command.clone()),
            model: choice_view(config, &ConfigCategory::Model).map(|view| view.current_name),
            effort: choice_view(config, &ConfigCategory::Effort).map(|view| view.current_name),
            cwd: session.cwd.clone(),
            usage: transcript.usage(),
            orchestrator_brief: transcript.entries().iter().find_map(|entry| match entry {
                Entry::OrchestratorPrompt(brief) => Some(brief.clone()),
                _ => None,
            }),
            archived: session.archived,
            workflow: WorkflowView::of(session, sessions),
        }
    }
}

/// The project's name, the parent session's title when the parent is known, and the title.
fn breadcrumb(project: &Project, sessions: &[Session], session: &Session) -> Vec<String> {
    let parent = session
        .parent
        .as_ref()
        .and_then(|parent| sessions.iter().find(|candidate| &candidate.id == parent));
    std::iter::once(project.name.clone())
        .chain(parent.map(|parent| parent.title.clone()))
        .chain(std::iter::once(session.title.clone()))
        .collect()
}

/// The picker for the first option of `category`, if the backend offers one.
fn choice_view(config: &[ConfigOption], category: &ConfigCategory) -> Option<ChoiceView> {
    let option = config.iter().find(|option| &option.category == category)?;
    Some(option_view(option, option.current.clone()))
}

/// The picker for `option` with `current` as the value in effect.
pub(crate) fn option_view(option: &ConfigOption, current: ConfigValueId) -> ChoiceView {
    let current_name = option
        .choices
        .iter()
        .find(|choice| choice.id == current)
        .map_or_else(|| current.to_string(), |choice| choice.name.clone());
    ChoiceView {
        current,
        current_name,
        choices: option.choices.clone(),
    }
}

fn project_node(project: &Project, sessions: &[Session], live: Live<'_>) -> ProjectNode {
    let (mut archived, sessions): (Vec<SessionNode>, Vec<SessionNode>) =
        session_nodes(project, sessions, live, None)
            .into_iter()
            .partition(|node| node.archived.is_some());
    archived.sort_by_key(|node| Reverse(node.archived.map(|mark| mark.at)));
    ProjectNode {
        id: project.id.clone(),
        name: project.name.clone(),
        root: project.root.clone(),
        has_global_instructions: project.has_global_instructions,
        has_global_claude_md: project.has_global_claude_md,
        sessions,
        archived,
        repositories: project
            .repositories
            .iter()
            .filter(|repository| !repository.is_root)
            .map(repository_node)
            .collect(),
        repository_count: project.repositories.len(),
        cleaning: false,
        fold: project.fold,
    }
}

fn repository_node(repository: &Repository) -> RepositoryNode {
    RepositoryNode {
        id: repository.id.clone(),
        name: repository.name.clone(),
        has_instructions: repository.has_instructions,
        has_claude_md: repository.has_claude_md,
    }
}

/// The sessions of `project` whose parent is `parent`, those needing you first, then the
/// running ones, then the rest, the most recently active first within each group; each with
/// the context usage of its transcript in `live` when it has one there and its running
/// background tasks. A session that shows its background tasks sorts with the running ones.
fn session_nodes(
    project: &Project,
    sessions: &[Session],
    live: Live<'_>,
    parent: Option<&SessionId>,
) -> Vec<SessionNode> {
    // Walking the sessions backwards puts later-created ones first, which the stable sort
    // keeps for equal keys.
    let siblings: Vec<&Session> = sessions
        .iter()
        .rev()
        .filter(|session| session.project == project.id && session.parent.as_ref() == parent)
        .collect();
    let mut nodes: Vec<(u64, SessionNode)> = siblings
        .into_iter()
        .map(|session| {
            let children = session_nodes(project, sessions, live, Some(&session.id));
            let below_is_active = children
                .iter()
                .any(|child| at_work(child.status, child.background));
            let delegated = live.delegated.contains(&session.id);
            let node = SessionNode {
                id: session.id.clone(),
                title: session.title.clone(),
                status: if session.status == Status::Done && below_is_active {
                    Status::Running
                } else {
                    session.status
                },
                background: live.background(&session.id),
                attention: (session.status == Status::WaitingForYou && !delegated)
                    || children.iter().any(|child| child.attention),
                delegated,
                scope_label: scope_label(project, &session.scope),
                backend: session.backend.to_string(),
                orchestrator: session.orchestrator,
                usage: live
                    .transcripts
                    .get(&session.id)
                    .and_then(Transcript::usage),
                first_prompt: session.first_prompt.clone(),
                skill: session.skill.clone(),
                active_at: session.active_at(),
                expanded: session.children == Fold::Open,
                children,
                archived: session.archived,
                step: session.workflow.as_ref().map(|status| {
                    let cell = workflow::step_cell(status);
                    StepCell {
                        gate_waiting: cell.gate_waiting && !delegated,
                        ..cell
                    }
                }),
            };
            (session.created_at, node)
        })
        .collect();
    nodes.sort_by_key(|(created_at, node)| {
        (rank(node), Reverse(node.active_at), Reverse(*created_at))
    });
    nodes.into_iter().map(|(_, node)| node).collect()
}

/// Where a session sorts among its siblings: `0` when it or a session below it needs you,
/// `1` while it shows running, shows running background tasks or waits for its orchestrator, `2`
/// otherwise.
fn rank(node: &SessionNode) -> u8 {
    if node.attention {
        0
    } else if node.status == Status::Running
        || node.delegated
        || background_shown(node.status, node.background)
    {
        1
    } else {
        2
    }
}

/// The name of the repository a session works in, empty for a session of the whole project;
/// a repository that is gone falls back to its id.
pub(crate) fn scope_label(project: &Project, scope: &Scope) -> String {
    match scope {
        Scope::Project => String::new(),
        Scope::Repository(id) => project
            .repositories
            .iter()
            .find(|repository| &repository.id == id)
            .map_or_else(|| id.to_string(), |repository| repository.name.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::sync::LazyLock;

    use super::*;
    use crate::backend::{ToolKind, Update};
    use crate::ids::{BackendSessionId, PermissionId, RepositoryId, ToolCallId};
    use crate::session::Workspace;
    use crate::workflow::{WorkflowEvent, WorkflowStatus};
    use crate::{PlannedTask, SuperpowersV7Event};

    fn project() -> Project {
        Project {
            id: ProjectId::new(),
            root: PathBuf::from("/work"),
            name: "work".into(),
            repositories: Vec::new(),
            has_global_instructions: false,
            has_global_claude_md: false,
            closed: false,
            fold: crate::Fold::Open,
        }
    }

    fn session(
        project: &Project,
        created_at: u64,
        status: Status,
        parent: Option<&Session>,
    ) -> Session {
        Session {
            id: SessionId::new(),
            project: project.id.clone(),
            scope: Scope::Project,
            cwd: project.root.clone(),
            title: format!("session {created_at}"),
            backend_session: Some(BackendSessionId::new("b")),
            backend: BackendId::try_from("claude").unwrap(),
            model: None,
            effort: None,
            status,
            parent: parent.map(|parent| parent.id.clone()),
            auto_allow: false,
            instructions_injected: false,
            created_at,
            orchestrator: false,
            archived: None,
            workspace: Workspace::CheckedOut,
            child_defaults: None,
            first_prompt: String::new(),
            skill: None,
            title_source: crate::TitleSource::Derived,
            last_activity: 0,
            children: crate::Fold::Folded,
            workflow: None,
            question: None,
            wait_reported: None,
        }
    }

    fn repository(name: &str, is_root: bool) -> Repository {
        Repository {
            id: RepositoryId::try_from(name).unwrap(),
            path: PathBuf::from("/work").join(name),
            name: name.into(),
            has_instructions: false,
            has_claude_md: false,
            is_root,
        }
    }

    /// No session runs background tasks.
    static NO_BACKGROUND: LazyLock<HashMap<SessionId, usize>> = LazyLock::new(HashMap::new);

    /// No session waits for its orchestrator.
    static NO_DELEGATED: LazyLock<HashSet<SessionId>> = LazyLock::new(HashSet::new);

    /// `transcripts` as what `App` knows live, with no session running background tasks or waiting
    /// for its orchestrator.
    fn live(transcripts: &HashMap<SessionId, Transcript>) -> Live<'_> {
        Live {
            transcripts,
            background: &NO_BACKGROUND,
            delegated: &NO_DELEGATED,
        }
    }

    fn roots(project: &Project, sessions: &[Session]) -> Vec<SessionNode> {
        session_nodes(project, sessions, live(&HashMap::new()), None)
    }

    fn archived_at(mut session: Session, at: u64) -> Session {
        session.archived = Some(Archived { at });
        session
    }

    fn titles(nodes: &[SessionNode]) -> Vec<&str> {
        nodes.iter().map(|node| node.title.as_str()).collect()
    }

    fn active(mut session: Session, last_activity: u64) -> Session {
        session.last_activity = last_activity;
        session
    }

    #[test]
    fn siblings_order_asks_you_then_running_then_by_activity() {
        let project = project();
        let done = active(session(&project, 1, Status::Done, None), 50);
        let running = active(session(&project, 2, Status::Running, None), 10);
        let waiting = active(session(&project, 3, Status::WaitingForYou, None), 5);
        let idle = active(session(&project, 4, Status::Idle, None), 80);
        let parent = active(session(&project, 5, Status::Done, None), 60);
        let below = active(
            session(&project, 6, Status::WaitingForYou, Some(&parent)),
            1,
        );

        let nodes = roots(&project, &[done, running, waiting, idle, parent, below]);

        assert_eq!(
            titles(&nodes),
            [
                "session 5",
                "session 3",
                "session 2",
                "session 4",
                "session 1"
            ]
        );
    }

    #[test]
    fn equal_activity_falls_back_to_the_newer_creation() {
        let project = project();
        let older = active(session(&project, 1, Status::Idle, None), 9);
        let newer = active(session(&project, 2, Status::Idle, None), 9);

        let nodes = roots(&project, &[older, newer]);

        assert_eq!(titles(&nodes), ["session 2", "session 1"]);
    }

    #[test]
    fn a_node_carries_the_prompt_skill_activity_and_fold_of_its_session() {
        let project = project();
        let mut open = active(session(&project, 1, Status::Idle, None), 7);
        open.first_prompt = "do it".into();
        open.skill = Some("/plan".into());
        open.children = crate::Fold::Open;

        let nodes = roots(&project, &[open]);

        assert_eq!(nodes[0].first_prompt, "do it");
        assert_eq!(nodes[0].skill.as_deref(), Some("/plan"));
        assert_eq!(nodes[0].active_at, 7);
        assert!(nodes[0].expanded);
    }

    #[test]
    fn a_project_node_carries_the_projects_fold() {
        let mut folded = project();
        folded.fold = crate::Fold::Folded;

        let tree = TreeModel::build(std::slice::from_ref(&folded), &[], live(&HashMap::new()));

        assert_eq!(tree.projects[0].fold, crate::Fold::Folded);
    }

    #[test]
    fn running_background_show_only_once_the_turn_is_over() {
        assert!(background_shown(Status::Done, 1));
        assert!(background_shown(Status::Idle, 2));
        assert!(!background_shown(Status::Done, 0));
        for status in [
            Status::Running,
            Status::WaitingForYou,
            Status::Interrupted,
            Status::Failed,
        ] {
            assert!(!background_shown(status, 1), "{status:?}");
        }
        assert!(at_work(Status::Done, 1));
        assert!(at_work(Status::Running, 0));
        assert!(at_work(Status::WaitingForYou, 0));
        assert!(!at_work(Status::Done, 0));
        assert!(!at_work(Status::Failed, 1));
    }

    #[test]
    fn archived_sessions_leave_the_live_list_with_their_children() {
        let project = project();
        let unarchived = session(&project, 3, Status::Done, None);
        let orchestrator = archived_at(session(&project, 2, Status::Done, None), 10);
        let child = archived_at(session(&project, 1, Status::Done, Some(&orchestrator)), 10);

        let tree = TreeModel::build(
            std::slice::from_ref(&project),
            &[child, orchestrator, unarchived],
            live(&HashMap::new()),
        );

        let node = &tree.projects[0];
        assert_eq!(titles(&node.sessions), ["session 3"]);
        assert_eq!(node.sessions[0].archived, None);
        assert_eq!(titles(&node.archived), ["session 2"]);
        assert_eq!(node.archived[0].archived, Some(Archived { at: 10 }));
        assert_eq!(titles(&node.archived[0].children), ["session 1"]);
    }

    #[test]
    fn the_archived_list_shows_the_latest_archive_first() {
        let project = project();
        let early = archived_at(session(&project, 3, Status::Done, None), 10);
        let late = archived_at(session(&project, 1, Status::Done, None), 20);
        let late_too = archived_at(session(&project, 2, Status::Done, None), 20);

        let tree = TreeModel::build(
            std::slice::from_ref(&project),
            &[early, late, late_too],
            live(&HashMap::new()),
        );

        assert_eq!(
            titles(&tree.projects[0].archived),
            ["session 2", "session 1", "session 3"]
        );
    }

    #[test]
    fn the_tree_leaves_out_the_root_repository() {
        let mut project = project();
        project.repositories = vec![repository("work", true), repository("api", false)];

        let tree = TreeModel::build(&[project], &[], live(&HashMap::new()));

        let names: Vec<&str> = tree.projects[0]
            .repositories
            .iter()
            .map(|repository| repository.name.as_str())
            .collect();
        assert_eq!(names, ["api"]);
    }

    #[test]
    fn the_repository_count_includes_the_root_repository() {
        let count_of = |repositories: Vec<Repository>| {
            let mut project = project();
            project.repositories = repositories;
            TreeModel::build(&[project], &[], live(&HashMap::new())).projects[0].repository_count
        };

        assert_eq!(count_of(Vec::new()), 0);
        assert_eq!(count_of(vec![repository("work", true)]), 1);
        assert_eq!(
            count_of(vec![repository("api", false), repository("web", false)]),
            2
        );
    }

    #[test]
    fn a_session_node_tells_whether_it_is_an_orchestrator() {
        let project = project();
        let mut orchestrator = session(&project, 2, Status::Idle, None);
        orchestrator.orchestrator = true;
        let plain = session(&project, 1, Status::Idle, None);

        let nodes = roots(&project, &[plain, orchestrator]);

        assert!(nodes[0].orchestrator);
        assert!(!nodes[1].orchestrator);
    }

    fn workflow_of(events: &[WorkflowEvent]) -> Option<WorkflowStatus> {
        crate::workflow::rebuild(events.iter().map(|event| (1, event)))
    }

    /// The workflow view of a session standing at `status`.
    fn view_of(status: &WorkflowStatus) -> WorkflowView {
        let mut session = session(&project(), 1, Status::Idle, None);
        session.workflow = Some(status.clone());
        WorkflowView::of(&session, &[]).unwrap()
    }

    fn planned(n: u32, title: &str) -> PlannedTask {
        PlannedTask {
            n,
            title: title.into(),
            files: Vec::new(),
        }
    }

    #[test]
    fn a_session_with_a_workflow_shows_its_step_and_one_without_shows_none() {
        let project = project();
        let mut gated = session(&project, 2, Status::Idle, None);
        gated.workflow = workflow_of(&[WorkflowEvent::from(
            SuperpowersV7Event::RequestDesignApproval {
                path: "design.md".into(),
            },
        )]);
        let plain = session(&project, 1, Status::Idle, None);

        let nodes = roots(&project, &[gated, plain]);

        assert_eq!(
            nodes[0].step,
            Some(StepCell {
                label: "design · gate".into(),
                gate_waiting: true,
            })
        );
        assert_eq!(nodes[1].step, None);
    }

    #[test]
    fn the_session_and_env_views_carry_the_workflow_with_its_task_titles() {
        let project = project();
        let events = vec![
            WorkflowEvent::from(SuperpowersV7Event::RequestPlanApproval {
                path: "plan.md".into(),
                tasks: vec![planned(1, "A"), planned(2, "B"), planned(3, "C")],
                checkpoints: Vec::new(),
            }),
            WorkflowEvent::from(SuperpowersV7Event::StartTask { n: 2, note: None }),
        ];
        let mut session = session(&project, 1, Status::Idle, None);
        session.workflow = workflow_of(&events);
        let updates: Vec<Update> = events
            .into_iter()
            .map(|event| Update::Workflow { at: 1, event })
            .collect();
        let transcript = Transcript::from_updates(&updates);

        let view = SessionViewModel::build(
            &project,
            &[],
            live(&HashMap::new()),
            &session,
            &transcript,
            &[],
            &[],
        );
        let env = EnvViewModel::build(
            &project,
            &[],
            &session,
            InstructionChain {
                files: Vec::new(),
                skipped: Vec::new(),
            },
            &transcript,
            &[],
            &[],
        );

        let workflow = view.workflow.unwrap();
        let plan = workflow.document("plan").unwrap();
        let rows = &plan.tasks.as_ref().unwrap().rows;
        let current: Vec<&str> = rows
            .iter()
            .filter(|row| row.current)
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(current, ["B"]);
        assert_eq!(Some(workflow), env.workflow);
    }

    #[test]
    fn the_workflow_view_carries_display_data_and_the_current_documents_key() {
        let tasks = vec![planned(1, "A")];
        let executing = workflow_of(&[
            WorkflowEvent::from(SuperpowersV7Event::RequestPlanApproval {
                path: "plan.md".into(),
                tasks: tasks.clone(),
                checkpoints: Vec::new(),
            }),
            WorkflowEvent::from(SuperpowersV7Event::StartExecution {
                mode: crate::ExecutionMode::Inline,
            }),
        ])
        .unwrap();
        let view = view_of(&executing);

        let (rail, outline, documents, gate) = crate::workflow::view(&executing);
        assert_eq!(view.rail, rail);
        assert_eq!(view.outline, outline);
        assert_eq!(view.documents, documents);
        assert_eq!(view.gate, gate);
        assert!(view.gate.is_some());
        assert_eq!(view.default_document(), Some("plan"));
        assert_eq!(view.document("plan"), view.documents.get(1));
        assert_eq!(view.document("design"), view.documents.first());
        assert_eq!(view.document("other"), None);

        let before = workflow_of(&[WorkflowEvent::from(SuperpowersV7Event::StartSession {
            topic: "t".into(),
        })])
        .unwrap();
        let view = view_of(&before);
        assert_eq!(view.default_document(), Some("design"));
        let empty = WorkflowView {
            documents: Vec::new(),
            ..view
        };
        assert_eq!(empty.default_document(), None);
    }

    /// A workflow whose `prd` was reported at `docs/prd.md`.
    fn prd_reported() -> Option<WorkflowStatus> {
        use crate::workflow::bmad_v6_12::{DocKind, Event};
        workflow_of(&[WorkflowEvent::from(Event::RequestDocumentApproval {
            doc: DocKind::Prd,
            path: "docs/prd.md".into(),
        })])
    }

    /// A workflow whose design was reported at `docs/design.md` and whose plan is not
    /// reported yet.
    fn design_reported() -> Option<WorkflowStatus> {
        workflow_of(&[WorkflowEvent::from(
            SuperpowersV7Event::RequestDesignApproval {
                path: "docs/design.md".into(),
            },
        )])
    }

    /// The child of `parent` created at `created_at`, titled `title`, working in `cwd`.
    fn child_in(parent: &Session, created_at: u64, title: &str, cwd: &str) -> Session {
        let mut child = session(&project(), created_at, Status::Idle, Some(parent));
        child.title = title.into();
        child.cwd = PathBuf::from(cwd);
        child
    }

    #[test]
    fn an_orchestrator_lists_its_childrens_documents_after_its_own() {
        let mut orchestrator = session(&project(), 1, Status::Idle, None);
        orchestrator.orchestrator = true;
        orchestrator.workflow = design_reported();
        let mut late = child_in(&orchestrator, 5, "Web page", "/work/web-wt");
        late.workflow = design_reported();
        let mut early = archived_at(child_in(&orchestrator, 3, "API", "/work/api-wt"), 9);
        early.workflow = prd_reported();
        let silent = child_in(&orchestrator, 4, "Silent", "/work/silent");
        let mut stranger = session(&project(), 2, Status::Idle, None);
        stranger.workflow = prd_reported();
        let sessions = [
            orchestrator.clone(),
            late.clone(),
            stranger,
            silent,
            early.clone(),
        ];

        let view = WorkflowView::of(&orchestrator, &sessions).unwrap();

        let own = WorkflowView::of(&orchestrator, &[]).unwrap();
        let keys: Vec<&str> = view.documents.iter().map(|doc| doc.key.as_str()).collect();
        let api = format!("{}/document", early.id);
        let web = format!("{}/design", late.id);
        assert_eq!(keys, ["design", "plan", api.as_str(), web.as_str()]);
        assert_eq!(view.documents[..2], own.documents[..]);
        let labels: Vec<&str> = view.documents[2..]
            .iter()
            .map(|doc| doc.label.as_str())
            .collect();
        assert_eq!(labels, ["API · prd", "Web page · design"]);
        assert_eq!(view.documents[2].reported.as_ref().unwrap().rev, 1);
        assert_eq!(view.documents[2].tasks, None);
        assert_eq!(view.rail, own.rail);
        assert_eq!(view.gate, own.gate);
        assert_eq!(view.outline[..own.outline.len()], own.outline[..]);
        assert_eq!(
            view.outline[own.outline.len()..],
            [
                OutlineBlock {
                    label: "API".into(),
                    mark: BlockMark::None,
                    lines: vec![OutlineLine::Note("prd".into())],
                    doc: Some(api.clone()),
                    current: false,
                },
                OutlineBlock {
                    label: "Web page".into(),
                    mark: BlockMark::None,
                    lines: vec![OutlineLine::Note("design".into())],
                    doc: Some(web.clone()),
                    current: false,
                },
            ]
        );
        assert_eq!(view.default_document(), own.default_document());
        assert_eq!(view.document(&api), view.documents.get(2));
    }

    #[test]
    fn childrens_documents_resolve_against_the_childs_folder() {
        let mut orchestrator = session(&project(), 1, Status::Idle, None);
        orchestrator.orchestrator = true;
        orchestrator.workflow = design_reported();
        let mut child = child_in(&orchestrator, 2, "API", "/work/api-wt");
        child.workflow = prd_reported();
        let sessions = [orchestrator.clone(), child.clone()];

        let view = WorkflowView::of(&orchestrator, &sessions).unwrap();

        let prd = view.document(&format!("{}/document", child.id)).unwrap();
        let path = &prd.reported.as_ref().unwrap().path;
        assert_eq!(path, &PathBuf::from("/work/api-wt/docs/prd.md"));
        // Joined to the orchestrator's folder, an absolute path stays as it is.
        assert_eq!(&orchestrator.cwd.join(path), path);
    }

    #[test]
    fn a_plain_session_lists_only_its_own_documents() {
        let mut parent = session(&project(), 1, Status::Idle, None);
        parent.workflow = design_reported();
        let mut child = child_in(&parent, 2, "API", "/work/api-wt");
        child.workflow = prd_reported();
        let sessions = [parent.clone(), child];

        let view = WorkflowView::of(&parent, &sessions).unwrap();

        assert_eq!(view, WorkflowView::of(&parent, &[]).unwrap());
        let keys: Vec<&str> = view.documents.iter().map(|doc| doc.key.as_str()).collect();
        assert_eq!(keys, ["design", "plan"]);
    }

    #[test]
    fn the_session_and_env_views_list_an_orchestrators_childrens_documents() {
        let project = project();
        let mut orchestrator = session(&project, 1, Status::Idle, None);
        orchestrator.orchestrator = true;
        orchestrator.workflow = design_reported();
        let mut child = child_in(&orchestrator, 2, "API", "/work/api-wt");
        child.workflow = prd_reported();
        let sessions = [orchestrator.clone(), child.clone()];
        let transcript = Transcript::default();

        let view = SessionViewModel::build(
            &project,
            &sessions,
            live(&HashMap::new()),
            &orchestrator,
            &transcript,
            &[],
            &[],
        );
        let env = EnvViewModel::build(
            &project,
            &sessions,
            &orchestrator,
            InstructionChain {
                files: Vec::new(),
                skipped: Vec::new(),
            },
            &transcript,
            &[],
            &[],
        );

        let expected = WorkflowView::of(&orchestrator, &sessions);
        assert!(
            expected
                .as_ref()
                .unwrap()
                .document(&format!("{}/document", child.id))
                .is_some()
        );
        assert_eq!(view.workflow, expected);
        assert_eq!(env.workflow, expected);
    }

    #[test]
    fn a_session_without_a_workflow_has_no_workflow_view() {
        let project = project();
        let session = session(&project, 1, Status::Idle, None);
        let transcript = Transcript::default();

        let view = SessionViewModel::build(
            &project,
            &[],
            live(&HashMap::new()),
            &session,
            &transcript,
            &[],
            &[],
        );

        assert_eq!(view.workflow, None);
    }

    #[test]
    fn no_task_row_is_current_when_no_task_runs_or_the_plan_lacks_it() {
        let status = |current| {
            workflow_of(&[
                WorkflowEvent::from(SuperpowersV7Event::RequestPlanApproval {
                    path: "plan.md".into(),
                    tasks: vec![planned(1, "A")],
                    checkpoints: Vec::new(),
                }),
                WorkflowEvent::from(SuperpowersV7Event::StartTask {
                    n: current,
                    note: None,
                }),
            ])
            .unwrap()
        };
        let idle =
            workflow_of(&[WorkflowEvent::from(SuperpowersV7Event::StartBrainstorming)]).unwrap();
        let current = |status: &WorkflowStatus| {
            let view = view_of(status);
            view.document("plan")
                .and_then(|plan| plan.tasks.clone())
                .and_then(|tasks| tasks.rows.into_iter().find(|row| row.current))
                .map(|row| row.title)
        };

        assert_eq!(current(&idle), None);
        assert_eq!(current(&status(9)), None);
        assert_eq!(current(&status(1)), Some("A".to_owned()));
    }

    #[test]
    fn a_session_node_carries_the_usage_of_its_loaded_transcript() {
        let project = project();
        let reported = session(&project, 4, Status::Idle, None);
        let child = session(&project, 3, Status::Idle, Some(&reported));
        let silent = session(&project, 2, Status::Idle, None);
        let unloaded = session(&project, 1, Status::Idle, None);
        let usage = |used| Transcript::from_updates(&[Update::Usage { used, size: 200 }]);
        let transcripts = HashMap::from([
            (reported.id.clone(), usage(41)),
            (child.id.clone(), usage(7)),
            (silent.id.clone(), Transcript::default()),
        ]);

        let tree = TreeModel::build(
            std::slice::from_ref(&project),
            &[unloaded, silent, child, reported],
            live(&transcripts),
        );

        let nodes = &tree.projects[0].sessions;
        assert_eq!(nodes[0].usage, Some((41, 200)));
        assert_eq!(nodes[0].children[0].usage, Some((7, 200)));
        assert_eq!(nodes[1].usage, None);
        assert_eq!(nodes[2].usage, None);
    }

    #[test]
    fn the_tree_carries_the_instruction_file_flags() {
        let mut project = project();
        project.has_global_instructions = true;
        project.has_global_claude_md = true;
        let mut api = repository("api", false);
        api.has_instructions = true;
        api.has_claude_md = true;
        project.repositories = vec![api, repository("web", false)];

        let tree = TreeModel::build(&[project], &[], live(&HashMap::new()));

        let node = &tree.projects[0];
        assert!(node.has_global_instructions);
        assert!(node.has_global_claude_md);
        assert!(node.repositories[0].has_instructions);
        assert!(node.repositories[0].has_claude_md);
        assert!(!node.repositories[1].has_instructions);
        assert!(!node.repositories[1].has_claude_md);
    }

    #[test]
    fn the_scope_label_names_the_repository_only() {
        let mut project = project();
        project.repositories = vec![repository("api", false)];
        let gone = RepositoryId::try_from("gone").unwrap();

        assert_eq!(scope_label(&project, &Scope::Project), "");
        assert_eq!(
            scope_label(&project, &Scope::Repository(repository("api", false).id)),
            "api"
        );
        assert_eq!(scope_label(&project, &Scope::Repository(gone)), "gone");
    }

    #[test]
    fn a_waiting_descendant_marks_every_ancestor() {
        let project = project();
        let parent = session(&project, 1, Status::Running, None);
        let child = session(&project, 2, Status::Running, Some(&parent));
        let grandchild = session(&project, 3, Status::WaitingForYou, Some(&child));
        let sibling = session(&project, 4, Status::Running, Some(&parent));

        let tree = roots(&project, &[parent, child, grandchild, sibling]);

        let parent = &tree[0];
        // The child that holds the waiting descendant sorts before the sibling.
        let (child, sibling) = (&parent.children[0], &parent.children[1]);
        assert!(parent.attention);
        assert!(child.attention);
        assert!(child.children[0].attention);
        assert!(!sibling.attention);
    }

    #[test]
    fn delegated_child_needs_no_attention() {
        let project = project();
        let parent = session(&project, 1, Status::Running, None);
        let mut delegated = session(&project, 2, Status::WaitingForYou, Some(&parent));
        delegated.workflow = workflow_of(&[WorkflowEvent::from(
            SuperpowersV7Event::RequestDesignApproval {
                path: "design.md".into(),
            },
        )]);
        let mut gated = session(&project, 3, Status::WaitingForYou, Some(&parent));
        gated.workflow = delegated.workflow.clone();
        let running = session(&project, 4, Status::Running, Some(&parent));
        let done = session(&project, 5, Status::Done, Some(&parent));
        let (delegated_id, gated_id) = (delegated.id.clone(), gated.id.clone());
        let sessions = [parent, delegated, gated, running, done];
        let marked = HashSet::from([delegated_id.clone()]);
        let tree = |sessions: &[Session]| {
            session_nodes(
                &project,
                sessions,
                Live {
                    transcripts: &HashMap::new(),
                    background: &NO_BACKGROUND,
                    delegated: &marked,
                },
                None,
            )
        };

        let nodes = tree(&sessions);
        let parent = &nodes[0];
        // The one that asks you comes first; the delegated child sorts with the running one.
        assert_eq!(
            titles(&parent.children),
            ["session 3", "session 4", "session 2", "session 5"]
        );
        let (gated, delegated) = (&parent.children[0], &parent.children[2]);
        assert!(delegated.delegated);
        assert!(!delegated.attention);
        assert_eq!(delegated.status, Status::WaitingForYou);
        assert_eq!(
            delegated.step,
            Some(StepCell {
                label: "design · gate".into(),
                gate_waiting: false,
            })
        );
        assert!(!gated.delegated);
        assert!(gated.attention);
        assert!(gated.step.as_ref().unwrap().gate_waiting);
        assert!(parent.attention);

        // Without the child that asks you, nothing above the delegated child needs you.
        let without: Vec<Session> = sessions
            .iter()
            .filter(|session| session.id != gated_id)
            .cloned()
            .collect();
        let nodes = tree(&without);
        assert!(!nodes[0].attention);
        assert_eq!(nodes[0].children[1].id, delegated_id);
    }

    #[test]
    fn a_done_parent_with_a_running_child_shows_running() {
        let project = project();
        let status_of = |parent: Status, child: Status| {
            let parent = session(&project, 1, parent, None);
            let child = session(&project, 2, child, Some(&parent));
            roots(&project, &[parent, child])[0].status
        };

        assert_eq!(status_of(Status::Done, Status::Running), Status::Running);
        assert_eq!(
            status_of(Status::Done, Status::WaitingForYou),
            Status::Running
        );
        assert_eq!(status_of(Status::Done, Status::Done), Status::Done);
        assert_eq!(status_of(Status::Idle, Status::Running), Status::Idle);
    }

    #[test]
    fn the_session_view_names_the_folder_the_agent_runs_in() {
        let project = project();
        let mut session = session(&project, 1, Status::Idle, None);
        session.cwd = PathBuf::from("/work/api");
        let transcript = Transcript::default();

        let view = SessionViewModel::build(
            &project,
            &[],
            live(&HashMap::new()),
            &session,
            &transcript,
            &[],
            &[],
        );

        assert_eq!(view.cwd, PathBuf::from("/work/api"));
    }

    #[test]
    fn the_session_view_lists_the_sessions_spawned_from_it() {
        let project = project();
        let parent = session(&project, 1, Status::Running, None);
        let older = session(&project, 2, Status::Done, Some(&parent));
        let newer = session(&project, 3, Status::WaitingForYou, Some(&parent));
        let below = session(&project, 4, Status::Running, Some(&older));
        let stranger = session(&project, 5, Status::Running, None);
        let transcripts = HashMap::from([(
            older.id.clone(),
            Transcript::from_updates(&[Update::Usage {
                used: 18,
                size: 200,
            }]),
        )]);
        let sessions = [
            parent.clone(),
            older.clone(),
            newer.clone(),
            below.clone(),
            stranger,
        ];
        let view_of = |session: &Session| {
            let transcript = Transcript::default();
            let view = SessionViewModel::build(
                &project,
                &sessions,
                live(&transcripts),
                session,
                &transcript,
                &[],
                &[],
            );
            (view.children, view.attention)
        };

        let (children, attention) = view_of(&parent);
        let ids: Vec<&SessionId> = children.iter().map(|child| &child.id).collect();
        assert_eq!(ids, [&newer.id, &older.id]);
        assert_eq!(children[0].status, Status::WaitingForYou);
        assert_eq!(children[0].usage, None);
        // The older child is done, and the session below it still runs.
        assert_eq!(children[1].status, Status::Running);
        assert_eq!(children[1].usage, Some((18, 200)));
        assert_eq!(children[1].children[0].id, below.id);
        assert!(attention);

        let (children, attention) = view_of(&older);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, below.id);
        assert!(!attention);

        let (children, attention) = view_of(&newer);
        assert!(children.is_empty());
        assert!(!attention);
    }

    #[test]
    fn the_env_view_carries_the_usage_and_the_orchestrators_first_prompt() {
        let project = project();
        let session = session(&project, 1, Status::Idle, None);
        let chain = || InstructionChain {
            files: Vec::new(),
            skipped: Vec::new(),
        };
        let env_of = |updates: &[Update]| {
            let transcript = Transcript::from_updates(updates);
            EnvViewModel::build(&project, &[], &session, chain(), &transcript, &[], &[])
        };

        let silent = env_of(&[Update::AgentText("hello".into())]);
        let briefed = env_of(&[
            Update::AgentText("hello".into()),
            Update::OrchestratorPrompt("Add the endpoint".into()),
            Update::Usage {
                used: 41,
                size: 200,
            },
            Update::OrchestratorPrompt("Also add tests".into()),
        ]);

        assert_eq!(silent.usage, None);
        assert_eq!(silent.orchestrator_brief, None);
        assert_eq!(briefed.usage, Some((41, 200)));
        assert_eq!(
            briefed.orchestrator_brief.as_deref(),
            Some("Add the endpoint")
        );
    }

    #[test]
    fn a_turn_is_in_progress_while_running_or_waiting() {
        let project = project();
        let request = Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Read main.rs".into(),
            kind: ToolKind::Read,
            options: Vec::new(),
            name: None,
            locations: Vec::new(),
            partitur_tool: false,
        };
        let answered = Update::PermissionAnswered {
            id: PermissionId(1),
            option: None,
        };
        let empty = Transcript::default();
        let asking = Transcript::from_updates(std::slice::from_ref(&request));
        let settled = Transcript::from_updates(&[request, answered]);
        let running_of = |status: Status, transcript: &Transcript| {
            let session = session(&project, 1, status, None);
            SessionViewModel::build(
                &project,
                &[],
                live(&HashMap::new()),
                &session,
                transcript,
                &[],
                &[],
            )
            .running()
        };

        assert!(running_of(Status::Running, &empty));
        assert!(running_of(Status::WaitingForYou, &asking));
        assert!(!running_of(Status::WaitingForYou, &settled));
        assert!(!running_of(Status::WaitingForYou, &empty));
        assert!(!running_of(Status::Idle, &asking));
        assert!(!running_of(Status::Done, &empty));
        assert!(!running_of(Status::Failed, &empty));
    }
}
