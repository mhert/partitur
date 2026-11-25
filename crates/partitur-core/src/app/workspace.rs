//! The branch settings of the session being prepared, and the rules that check them against
//! what git answered about its repositories.
//!
//! A session targets one repository (a repository session, or a project session whose root is
//! the repository), every repository below the root of an orchestrator, or a plain project
//! folder that is no repository. A repository the target names is read once, in the
//! background; until the answer arrives it is loading. From the target, the wanted choice, the
//! project's settings and the other sessions follow the modes offered, the bases to start from,
//! whether fetching is possible, where a worktree would go, and the reason Create cannot be
//! pressed, if any.
//!
//! A session works in a folder, the main checkout or a worktree, on a branch: the one the main
//! checkout has (as is, which a worktree never offers), an existing local branch, or a new one.
//! A repository session is offered all of these. An orchestrator is offered no existing branch,
//! since one branch picked for every repository would mean nothing there, and a plain folder or
//! a repository git cannot read works only as it is. The existing branches are listed with those
//! checked out in a worktree first, each group by name, marking the one the main checkout has
//! and those whose only worktree is gone. A new branch named like an existing one is refused,
//! and the box then offers to work on the existing one instead; it never switches by itself.
//!
//! For an existing branch, the first reason that applies blocks Create: no branch picked; a
//! name that is no local branch; a branch whose only worktree is gone, to be pruned first; in
//! the main checkout, a branch another worktree has checked out, or another live session in the
//! main checkout when the switch would change its branch; in a worktree, the branch the main
//! checkout has, or, when the branch has no worktree yet, a folder that exists where its new
//! worktree would go; and last a repository busy with another branch change, only when git work
//! is planned. Working as it is, on the branch the main checkout has, or in the worktree a
//! branch is already checked out in, changes nothing, so the sessions already working in that
//! folder are only named in the note, archived ones included, since restoring them must find
//! their folder; only a live session refuses a switch of the main checkout.
//!
//! Create plans its git work from the settings as they are checked at that moment. For a new
//! branch that is an optional fetch, then the step that makes the branch or the worktree,
//! always last, so a failed request leaves nothing behind. For an existing branch it is the
//! switch of the main checkout to it, or a new worktree on it; reusing its worktree, or the
//! branch the main checkout has already, needs no git work. What a successful request made is
//! undone by removing the worktree, or switching the main checkout back to the head it had,
//! and deleting the new branch. Only what was made is undone: a branch that existed before is
//! never deleted, and a worktree that existed before is never removed.
//!
//! The children of a proposed split are checked the same way, each in its own repository, which
//! offers a child every mode a repository session has, and then against each other: two
//! children may not make one new branch name in one repository, nor one new worktree folder,
//! which also keeps two children from adding a worktree for one existing branch. A child that
//! switches its main checkout's branch, to a new branch or to an existing one the main checkout
//! does not have, is refused while another child works in that main checkout. Children may
//! share the worktree an existing branch has; each one's note names the other children working
//! in its folder first, then the sessions. A child that reuses a worktree plans work that
//! needs no git step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::{
    GitAnswer, GitError, GitQuery, GitStep, Head, RepoInfo, WorktreeInfo, is_valid_branch_name,
};
use crate::ids::{GitRequestId, RepositoryId};
use crate::orchestrator::orchestrated_repositories;
use crate::project::{Project, Repository};
use crate::session::{BranchChoice, BranchKind, BranchMode, Folder, Scope, Session, Workspace};
use crate::settings::{GlobalSettings, ProjectSettings};
use crate::view_model::{BranchView, ChildCheck, ExistingOption};
use crate::worktree_path::PatternValues;

/// Why an orchestrator working in its only repository's own folder offers its children no
/// `New branch`: switching that folder's branch would pull it from under the orchestrator.
pub(super) const SHARED_FOLDER_NOTE: &str =
    "children share the orchestrator's folder, so they cannot switch its branch";

/// Every mode, in the order the sheet offers them.
const ALL_MODES: [BranchMode; 5] = [
    BranchMode::Worktree,
    BranchMode::NewBranch,
    BranchMode::CheckedOut,
    BranchMode::ExistingBranch,
    BranchMode::ExistingWorktree,
];

/// The modes an orchestrator's sheet offers for its children: no existing branch, which is
/// picked per child.
const ORCHESTRATOR_MODES: [BranchMode; 3] = [
    BranchMode::Worktree,
    BranchMode::NewBranch,
    BranchMode::CheckedOut,
];

/// What git has answered about one repository so far.
#[derive(Debug, Clone)]
pub(super) enum RepoState {
    /// The request reading it has not been answered yet.
    Loading(GitRequestId),
    /// What git reported.
    Ready(RepoInfo),
    /// Why it could not be read.
    Failed(String),
}

/// What Create makes or changes in one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Make {
    /// A new branch the main checkout switches to.
    NewBranch {
        /// The branch it starts from.
        base: String,
        /// The remote fetched first, `None` when nothing is fetched.
        fetch: Option<String>,
    },
    /// A new branch in a new worktree.
    NewWorktree {
        /// The branch it starts from.
        base: String,
        /// The remote fetched first, `None` when nothing is fetched.
        fetch: Option<String>,
        /// Where the worktree goes.
        path: PathBuf,
    },
    /// A switch of the main checkout to the existing branch.
    SwitchTo,
    /// A new worktree on the existing branch.
    AddWorktree {
        /// Where the worktree goes.
        path: PathBuf,
    },
    /// The worktree the existing branch is checked out in already, used as it is.
    Reuse {
        /// Where the worktree is.
        path: PathBuf,
    },
}

impl Make {
    /// Where the worktree this makes goes, `None` when it makes none.
    pub(super) fn new_worktree(&self) -> Option<&Path> {
        match self {
            Self::NewWorktree { path, .. } | Self::AddWorktree { path } => Some(path),
            Self::NewBranch { .. } | Self::SwitchTo | Self::Reuse { .. } => None,
        }
    }
}

/// The git work Create does in one repository: the branch, what is made or changed for it,
/// and the head the main checkout had when it was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Planned {
    /// The repository the work happens in.
    pub(super) repository: Repository,
    /// The branch the session works on: the new branch, trimmed, or the existing one.
    pub(super) branch: String,
    /// What is made or changed.
    pub(super) make: Make,
    /// What the main checkout had checked out in the answer the check used.
    pub(super) before: Head,
}

impl Planned {
    /// Whether the work makes or changes something in git, which reusing a worktree does not.
    pub(super) fn changes(&self) -> bool {
        !matches!(self.make, Make::Reuse { .. })
    }

    /// Whether `other` does the same work, whatever head the main checkout had.
    pub(super) fn same_work(&self, other: &Self) -> bool {
        let rebased = Self {
            before: other.before.clone(),
            ..self.clone()
        };
        rebased == *other
    }
}

/// The steps that carry out `planned`. A new branch takes `[Fetch]` when planned, then
/// `CreateBranch` or `AddWorktree`; the step that makes something is always last. The new
/// branch starts from `<remote>/<base>` when fetching, else from `<base>`, each named by its
/// full ref (`refs/remotes/<remote>/<base>`, `refs/heads/<base>`), so a tag of the same name
/// never wins. An existing branch takes `[RestoreHead]` to switch the main checkout to it,
/// `[CheckoutWorktree]` for a new worktree on it, and nothing when its worktree is reused.
pub(super) fn create_steps(planned: &Planned) -> Vec<GitStep> {
    let repository = planned.repository.path.clone();
    let branch = planned.branch.clone();
    let start = |base: &str, fetch: &Option<String>| match fetch {
        Some(remote) => format!("refs/remotes/{remote}/{base}"),
        None => format!("refs/heads/{base}"),
    };
    let (fetch, make) = match &planned.make {
        Make::NewBranch { base, fetch } => (
            fetch,
            GitStep::CreateBranch {
                repository: repository.clone(),
                branch,
                start: start(base, fetch),
            },
        ),
        Make::NewWorktree { base, fetch, path } => (
            fetch,
            GitStep::AddWorktree {
                repository: repository.clone(),
                path: path.clone(),
                branch,
                start: start(base, fetch),
            },
        ),
        Make::SwitchTo => (
            &None,
            GitStep::RestoreHead {
                repository: repository.clone(),
                head: Head::Branch(branch),
            },
        ),
        Make::AddWorktree { path } => (
            &None,
            GitStep::CheckoutWorktree {
                repository: repository.clone(),
                path: path.clone(),
                branch,
            },
        ),
        Make::Reuse { .. } => return Vec::new(),
    };
    let fetch = fetch.iter().map(|remote| GitStep::Fetch {
        repository: repository.clone(),
        remote: remote.clone(),
    });
    fetch.chain([make]).collect()
}

/// What a session records once the steps of `planned` succeeded.
pub(super) fn workspace_of(planned: &Planned) -> Workspace {
    let branch = planned.branch.clone();
    match &planned.make {
        Make::NewBranch { base, fetch } => Workspace::NewBranch {
            branch,
            base: base.clone(),
            fetched: fetch.is_some(),
        },
        Make::NewWorktree { base, fetch, path } => Workspace::Worktree {
            branch,
            base: base.clone(),
            fetched: fetch.is_some(),
            path: path.clone(),
        },
        Make::SwitchTo => Workspace::ExistingBranch { branch },
        Make::AddWorktree { path } => Workspace::WorktreeOnExisting {
            branch,
            path: path.clone(),
        },
        Make::Reuse { path } => Workspace::ExistingWorktree {
            branch,
            path: path.clone(),
        },
    }
}

/// The steps that undo `workspace`, made in `repository` while its main checkout had `before`
/// checked out; only what was made is removed. A new worktree on a new branch is removed with
/// its branch, `[RemoveWorktree, DeleteBranch]`; a new branch is left,
/// `[RestoreHead(before), DeleteBranch]`. A switch to an existing branch is undone with
/// `[RestoreHead(before)]` and a new worktree on an existing branch with `[RemoveWorktree]`,
/// leaving the branch. A checked-out workspace or a reused worktree made nothing to undo.
pub(super) fn undo_steps(repository: &Path, workspace: &Workspace, before: &Head) -> Vec<GitStep> {
    let repository = repository.to_path_buf();
    let undo = match workspace {
        Workspace::CheckedOut | Workspace::ExistingWorktree { .. } => return Vec::new(),
        Workspace::Worktree { path, .. } | Workspace::WorktreeOnExisting { path, .. } => {
            GitStep::RemoveWorktree {
                repository: repository.clone(),
                path: path.clone(),
            }
        }
        Workspace::NewBranch { .. } | Workspace::ExistingBranch { .. } => GitStep::RestoreHead {
            repository: repository.clone(),
            head: before.clone(),
        },
    };
    let delete = workspace.made_branch().map(|branch| GitStep::DeleteBranch {
        repository,
        branch: branch.to_owned(),
    });
    [undo].into_iter().chain(delete).collect()
}

/// What a session's branch settings apply to.
#[derive(Debug)]
pub(super) enum Target {
    /// One repository, the session's folder.
    Repository {
        /// The repository.
        repository: Repository,
        /// What git answered about it.
        state: RepoState,
    },
    /// The repositories below the root, which an orchestrator's children start in.
    Orchestrator {
        /// Each repository with what git answered about it.
        repositories: Vec<(Repository, RepoState)>,
    },
    /// A project folder that is no repository.
    Folder,
}

impl Target {
    /// The target of a session in `project` with `scope`. Checked in this order: an
    /// orchestrator targets the repositories it works on (those below the root, or else the
    /// root repository); a repository session, or a
    /// project session whose root is a repository, targets that repository; any other session
    /// targets the folder. `ask` queues the reading of the repositories, given one
    /// [`GitQuery::Inspect`] per repository, and returns the state they start in.
    pub(super) fn read(
        project: &Project,
        scope: &Scope,
        orchestrator: bool,
        ask: impl FnOnce(Vec<GitQuery>) -> RepoState,
    ) -> Self {
        let repositories: Vec<Repository> = if orchestrator {
            orchestrated_repositories(project)
                .into_iter()
                .cloned()
                .collect()
        } else {
            project
                .repositories
                .iter()
                .filter(|repository| match scope {
                    Scope::Repository(id) => repository.id == *id,
                    Scope::Project => repository.is_root,
                })
                .cloned()
                .collect()
        };
        if repositories.is_empty() {
            return Self::Folder;
        }
        let queries = repositories
            .iter()
            .map(|repository| GitQuery::Inspect(repository.path.clone()))
            .collect();
        let state = ask(queries);
        if orchestrator {
            Self::Orchestrator {
                repositories: repositories
                    .into_iter()
                    .map(|repository| (repository, state.clone()))
                    .collect(),
            }
        } else {
            let mut repositories = repositories.into_iter();
            repositories
                .next()
                .map_or(Self::Folder, |repository| Self::Repository {
                    repository,
                    state,
                })
        }
    }

    /// The states of the repositories the target reads, in order.
    fn states(&self) -> Vec<&RepoState> {
        match self {
            Self::Repository { state, .. } => vec![state],
            Self::Orchestrator { repositories } => {
                repositories.iter().map(|(_, state)| state).collect()
            }
            Self::Folder => Vec::new(),
        }
    }

    /// Whether a repository of the target is still being read.
    fn loading(&self) -> bool {
        self.states()
            .into_iter()
            .any(|state| matches!(state, RepoState::Loading(_)))
    }

    /// What git answered about every repository of the target, `None` while one is loading or
    /// could not be read.
    fn infos(&self) -> Option<Vec<&RepoInfo>> {
        self.states()
            .into_iter()
            .map(|state| match state {
                RepoState::Ready(info) => Some(info),
                RepoState::Loading(_) | RepoState::Failed(_) => None,
            })
            .collect()
    }

    /// Applies the `answers` to `request` when the target waits for it; returns whether it
    /// did.
    fn answer(&mut self, request: GitRequestId, answers: Vec<Result<GitAnswer, GitError>>) -> bool {
        let mut answers = answers.into_iter();
        match self {
            Self::Repository { state, .. } if is_loading(state, request) => {
                *state = repo_state(answers.next());
                true
            }
            Self::Orchestrator { repositories }
                if repositories
                    .first()
                    .is_some_and(|(_, state)| is_loading(state, request)) =>
            {
                for (_, state) in repositories {
                    *state = repo_state(answers.next());
                }
                true
            }
            _ => false,
        }
    }

    /// The branches a new branch can start from: a repository's local branches, or those
    /// every repository of an orchestrator has; sorted, and empty until all are read.
    fn bases(&self) -> Vec<String> {
        let Some(infos) = self.infos() else {
            return Vec::new();
        };
        let mut infos = infos.into_iter();
        let first = infos.next().map(RepoInfo::branch_names).unwrap_or_default();
        let rest: Vec<&RepoInfo> = infos.collect();
        first
            .into_iter()
            .filter(|name| rest.iter().all(|info| info.has_branch(name)))
            .collect()
    }

    /// The base a new branch starts from by default: the branch checked out now; for an
    /// orchestrator the branch every repository has checked out when they agree, else the
    /// first branch they all have.
    fn start(&self) -> Option<String> {
        let bases = self.bases();
        let infos = self.infos()?;
        let mut current = infos.iter().map(|info| info.current_branch());
        let first = current.next().flatten();
        let agreed = first.filter(|name| {
            current.all(|other| other == Some(*name)) && bases.iter().any(|base| base == name)
        });
        match self {
            Self::Orchestrator { .. } => agreed
                .map(str::to_owned)
                .or_else(|| bases.into_iter().next()),
            Self::Repository { .. } | Self::Folder => first.map(str::to_owned),
        }
    }

    /// The remote fetched before starting from `base`: the one every repository of the target
    /// has `base` on; `None` when one has no remote counterpart of `base`.
    fn fetch_remote(&self, base: Option<&str>) -> Option<String> {
        let base = base?;
        let remotes: Option<Vec<&str>> = self
            .infos()?
            .into_iter()
            .map(|info| info.fetch_remote(base))
            .collect();
        remotes?.first().map(|remote| (*remote).to_owned())
    }

    /// The modes offered, in sheet order, with a note on why the others are not.
    fn modes(&self) -> (Vec<BranchMode>, Option<String>) {
        match self {
            Self::Folder => (
                vec![BranchMode::CheckedOut],
                Some("the project folder is not a repository".into()),
            ),
            Self::Repository {
                repository,
                state: RepoState::Failed(error),
            } => (
                vec![BranchMode::CheckedOut],
                Some(format!(
                    "cannot read the git state of {}: {error}",
                    repository.name
                )),
            ),
            Self::Orchestrator { repositories }
                if repositories
                    .iter()
                    .any(|(repository, _)| repository.is_root) =>
            {
                (
                    vec![BranchMode::Worktree, BranchMode::CheckedOut],
                    Some(SHARED_FOLDER_NOTE.into()),
                )
            }
            Self::Orchestrator { .. } => (ORCHESTRATOR_MODES.to_vec(), None),
            Self::Repository { .. } => (ALL_MODES.to_vec(), None),
        }
    }

    /// What git answered about the repository of a repository session, `None` while loading,
    /// when it could not be read and for any other target.
    fn repository_info(&self) -> Option<&RepoInfo> {
        match self {
            Self::Repository {
                state: RepoState::Ready(info),
                ..
            } => Some(info),
            Self::Repository { .. } | Self::Orchestrator { .. } | Self::Folder => None,
        }
    }
}

/// The linked worktrees of `info`, those other than the main checkout, that have `branch`
/// checked out, prunable ones included.
fn linked_on<'a>(info: &'a RepoInfo, branch: &'a str) -> impl Iterator<Item = &'a WorktreeInfo> {
    info.worktrees
        .iter()
        .skip(1)
        .filter(move |worktree| matches!(&worktree.head, Head::Branch(name) if name == branch))
}

/// The local branch `name` of `info` as the box offers it.
fn existing_option(info: &RepoInfo, name: &str) -> ExistingOption {
    let worktree = linked_on(info, name)
        .find(|worktree| !worktree.prunable)
        .map(|worktree| worktree.path.clone());
    let main = info.current_branch() == Some(name);
    let gone = !main && worktree.is_none() && linked_on(info, name).next().is_some();
    ExistingOption {
        name: name.to_owned(),
        worktree,
        main,
        gone,
    }
}

/// The local branches of `info` as the box offers them: those checked out in a worktree first,
/// then the rest, each group sorted by name.
fn existing_options(info: &RepoInfo) -> Vec<ExistingOption> {
    let mut options: Vec<ExistingOption> = info
        .branch_names()
        .iter()
        .map(|name| existing_option(info, name))
        .collect();
    options.sort_by_key(|option| option.worktree.is_none());
    options
}

/// The option of the existing branch `view` picked, `None` when none is picked or it is not
/// offered.
fn picked(view: &BranchView) -> Option<&ExistingOption> {
    let name = view.existing.as_deref()?;
    view.existing_options
        .iter()
        .find(|option| option.name == name)
}

/// The live sessions, those not archived, that work in `folder`.
fn users<'a>(sessions: &'a [Session], folder: &'a Path) -> impl Iterator<Item = &'a Session> {
    sessions
        .iter()
        .filter(move |session| session.archived.is_none() && works_in(session, folder))
}

/// Whether `session` works in `folder`, by its own folder or the one git resolved it to, as
/// cleanup counts it.
fn works_in(session: &Session, folder: &Path) -> bool {
    session.cwd == folder || session.resolved_cwd.as_deref() == Some(folder)
}

/// The note's end naming the sessions that work in `folder` too, archived ones included, since
/// restoring them must find their folder, as [`also_used_by`] names them.
fn sharing(sessions: &[Session], folder: &Path) -> String {
    also_used_by(titles_in(sessions, folder))
}

/// The titles of the `sessions` that work in `folder`, archived ones included.
fn titles_in<'a>(sessions: &'a [Session], folder: &'a Path) -> impl Iterator<Item = &'a str> {
    sessions
        .iter()
        .filter(move |session| works_in(session, folder))
        .map(|session| session.title.as_str())
}

/// The note's end naming the first of `titles`: ` · also used by "<title>"`, with
/// ` and <n> more` for more than one; empty for none.
fn also_used_by<'a>(mut titles: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = titles.next() else {
        return String::new();
    };
    match titles.count() {
        0 => format!(" · also used by \"{first}\""),
        more => format!(" · also used by \"{first}\" and {more} more"),
    }
}

/// The git work `view` asks for, `None` when there is none, as for Checked-out, or when what it
/// needs is missing: a base for a new branch, or the folder of a worktree.
fn make_of(view: &BranchView) -> Option<Make> {
    let fetch = view.fetch_remote.clone().filter(|_| view.fetch);
    let base = view.base.clone();
    match view.mode {
        BranchMode::NewBranch => base.map(|base| Make::NewBranch { base, fetch }),
        BranchMode::Worktree => base
            .zip(view.worktree.clone())
            .map(|(base, path)| Make::NewWorktree { base, fetch, path }),
        BranchMode::ExistingBranch => Some(Make::SwitchTo),
        BranchMode::ExistingWorktree => {
            let reuse = picked(view).is_some_and(|option| option.worktree.is_some());
            view.worktree.clone().map(|path| {
                if reuse {
                    Make::Reuse { path }
                } else {
                    Make::AddWorktree { path }
                }
            })
        }
        BranchMode::CheckedOut => None,
    }
}

/// Whether `state` waits for `request`.
pub(super) fn is_loading(state: &RepoState, request: GitRequestId) -> bool {
    matches!(state, RepoState::Loading(waiting) if *waiting == request)
}

/// The state an `answer` to an inspect leaves a repository in.
pub(super) fn repo_state(answer: Option<Result<GitAnswer, GitError>>) -> RepoState {
    match answer {
        Some(Ok(GitAnswer::Repo(info))) => RepoState::Ready(info),
        Some(Err(error)) => RepoState::Failed(error.to_string()),
        Some(Ok(_)) | None => RepoState::Failed("git did not describe the repository".into()),
    }
}

/// The sheet's label of `mode`.
fn label(mode: BranchMode) -> &'static str {
    match mode {
        BranchMode::Worktree => "Worktree",
        BranchMode::NewBranch => "New branch",
        BranchMode::CheckedOut => "Checked-out",
        BranchMode::ExistingBranch => "Existing branch",
        BranchMode::ExistingWorktree => "Existing worktree",
    }
}

/// `<what> is not offered for this session`.
fn not_offered(what: &str) -> String {
    format!("{what} is not offered for this session")
}

/// The branch settings of the session being prepared, with the view they make.
#[derive(Debug)]
pub(super) struct Branching {
    /// The project, as it was when the sheet opened.
    project: Project,
    /// What the settings apply to.
    target: Target,
    /// What the user wants; the base is set once the target is read.
    choice: BranchChoice,
    /// The project's settings, or why they could not be loaded.
    settings: Result<ProjectSettings, String>,
    /// The view, recomputed after every change.
    view: BranchView,
}

/// What the rules read besides the branch settings themselves.
pub(super) struct Context<'a> {
    /// Every session, to find another live session in a checkout.
    pub(super) sessions: &'a [Session],
    /// The global settings layer and home folder.
    pub(super) global: &'a GlobalSettings,
    /// Whether a changing git request is in flight for a repository.
    pub(super) busy: &'a dyn Fn(&Path) -> bool,
    /// Whether a folder a worktree would go to is known to exist.
    pub(super) exists: &'a dyn Fn(&Path) -> bool,
}

impl Branching {
    /// The branch settings for `target` in `project`, starting from `choice`, with the
    /// project's `settings`. The view is computed by the first [`refresh`](Self::refresh).
    pub(super) fn new(
        project: Project,
        target: Target,
        choice: BranchChoice,
        settings: Result<ProjectSettings, String>,
    ) -> Self {
        Self {
            project,
            target,
            choice,
            settings,
            view: BranchView::default(),
        }
    }

    /// What the user wants, to carry over to the settings of another target.
    pub(super) fn choice(&self) -> BranchChoice {
        self.choice.clone()
    }

    /// The view the last [`refresh`](Self::refresh) computed.
    pub(super) fn view(&self) -> &BranchView {
        &self.view
    }

    /// The git work Create does with the view the last [`refresh`](Self::refresh) computed:
    /// `None` when it does none, as for Checked-out, the existing branch the main checkout has
    /// already, an orchestrator or a plain folder.
    ///
    /// # Errors
    ///
    /// Returns the view's blocker when Create cannot be pressed, and `<repo> has no main
    /// checkout` when a repository session that should make or switch a branch has no head to
    /// return to, so it never falls back to working in the checkout it was meant to leave
    /// alone.
    pub(super) fn planned(&self) -> Result<Option<Planned>, String> {
        let view = &self.view;
        if let Some(blocker) = &view.blocker {
            return Err(blocker.clone());
        }
        let Target::Repository { repository, state } = &self.target else {
            return Ok(None);
        };
        let as_is = match view.mode {
            BranchMode::CheckedOut => true,
            BranchMode::ExistingBranch => picked(view).is_some_and(|option| option.main),
            BranchMode::Worktree | BranchMode::NewBranch | BranchMode::ExistingWorktree => false,
        };
        if as_is {
            return Ok(None);
        }
        // Without a blocker the repository is read and a base or an existing branch picked;
        // what can still be missing is a main checkout to return to.
        if let RepoState::Ready(info) = state
            && let Some(before) = info.head()
            && let Some(make) = make_of(view)
        {
            let branch = if view.mode.kind() == BranchKind::New {
                view.branch.trim().to_owned()
            } else {
                view.existing.clone().unwrap_or_default()
            };
            return Ok(Some(Planned {
                repository: repository.clone(),
                branch,
                make,
                before: before.clone(),
            }));
        }
        Err(format!("{} has no main checkout", repository.name))
    }

    /// Replaces what git said about the repository with the fresh `answers` to an inspect,
    /// keeping the choice as it is.
    ///
    /// # Errors
    ///
    /// Returns why when the repository could not be read; the earlier reading stays then.
    pub(super) fn reread(
        &mut self,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) -> Result<(), String> {
        let fresh = repo_state(answers.into_iter().next());
        if let RepoState::Failed(error) = &fresh {
            return Err(error.clone());
        }
        if let Target::Repository { state, .. } = &mut self.target {
            *state = fresh;
        }
        Ok(())
    }

    /// The branch settings an orchestrator keeps for its children: the choice, with the name
    /// trimmed and fetching only as the sheet shows it, so off when the base has no remote
    /// branch in every repository.
    pub(super) fn child_defaults(&self) -> BranchChoice {
        BranchChoice {
            branch: self.choice.branch.trim().to_owned(),
            fetch: self.view.fetch,
            ..self.choice.clone()
        }
    }

    /// Applies the `answers` to `request` when the settings wait for it, and sets the default
    /// base then.
    pub(super) fn answer(
        &mut self,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) {
        if self.target.answer(request, answers) {
            self.choice.base = self.target.start();
        }
    }

    /// Picks `mode`.
    ///
    /// # Errors
    ///
    /// Returns why when the target does not offer `mode`.
    pub(super) fn set_mode(&mut self, mode: BranchMode) -> Result<(), String> {
        self.offer(BranchChoice {
            mode,
            ..self.choice.clone()
        })
    }

    /// Moves the session to `folder`, keeping its branch kind; as is becomes a new branch in a
    /// worktree.
    ///
    /// # Errors
    ///
    /// Returns why when the target does not offer the mode that makes.
    pub(super) fn set_folder(&mut self, folder: Folder) -> Result<(), String> {
        let mut choice = self.choice.clone();
        choice.set_folder(folder);
        self.offer(choice)
    }

    /// Sets the branch kind in the current folder.
    ///
    /// # Errors
    ///
    /// Returns `As is is not offered for this session` for as is in a worktree, and why when
    /// the target does not offer the mode that makes.
    pub(super) fn set_kind(&mut self, kind: BranchKind) -> Result<(), String> {
        let mut choice = self.choice.clone();
        if !choice.set_kind(kind) {
            return Err(not_offered("As is"));
        }
        self.offer(choice)
    }

    /// Picks the existing branch `name`, keeping the mode.
    ///
    /// # Errors
    ///
    /// Returns `Existing is not offered for this session` for any target but a repository,
    /// and `<name> is not a branch of <repo>` when the box does not offer `name`, as while
    /// loading.
    pub(super) fn set_existing(&mut self, name: String) -> Result<(), String> {
        let Target::Repository { repository, .. } = &self.target else {
            return Err(not_offered("Existing"));
        };
        if !self
            .view
            .existing_options
            .iter()
            .any(|option| option.name == name)
        {
            return Err(format!("{name} is not a branch of {}", repository.name));
        }
        self.choice.existing = Some(name);
        Ok(())
    }

    /// Works on the existing branch the new branch's name names instead, in the same folder.
    ///
    /// # Errors
    ///
    /// Returns `there is no existing branch to use` when the name names none, and why when
    /// the target does not offer the mode that makes.
    pub(super) fn use_existing(&mut self) -> Result<(), String> {
        let Some(name) = self.view.use_existing.clone() else {
            return Err("there is no existing branch to use".into());
        };
        let mut choice = self.choice.clone();
        choice.use_existing(name);
        self.offer(choice)
    }

    /// Takes `choice` when the target offers its mode.
    ///
    /// # Errors
    ///
    /// Returns `<label> is not offered for this session` when it does not.
    fn offer(&mut self, choice: BranchChoice) -> Result<(), String> {
        if !self.view.modes.contains(&choice.mode) {
            return Err(not_offered(label(choice.mode)));
        }
        self.choice = choice;
        Ok(())
    }

    /// Names the new branch `name`, as typed.
    pub(super) fn set_branch(&mut self, name: String) {
        self.choice.branch = name;
    }

    /// Starts the new branch from `base`.
    ///
    /// # Errors
    ///
    /// Returns why when `base` is not among the bases offered, as while loading.
    pub(super) fn set_base(&mut self, base: String) -> Result<(), String> {
        if !self.view.bases.contains(&base) {
            return Err(format!("{base} is not a branch to start from"));
        }
        self.choice.base = Some(base);
        Ok(())
    }

    /// Sets whether the base is fetched first.
    ///
    /// # Errors
    ///
    /// Returns why when fetching is wanted but the loaded base has no remote counterpart.
    pub(super) fn set_fetch(&mut self, fetch: bool) -> Result<(), String> {
        if fetch && !self.view.loading && self.view.fetch_remote.is_none() {
            return Err("the base has no remote branch to fetch".into());
        }
        self.choice.fetch = fetch;
        Ok(())
    }

    /// Recomputes the view; a mode the target does not offer falls back to Checked-out.
    pub(super) fn refresh(&mut self, context: &Context<'_>) {
        let (modes, unreadable) = self.target.modes();
        if !modes.contains(&self.choice.mode) {
            self.choice.mode = BranchMode::CheckedOut;
        }
        let loading = self.target.loading();
        let base = self.choice.base.clone().filter(|_| !loading);
        let fetch_remote = self.target.fetch_remote(base.as_deref());
        let repository = match &self.target {
            Target::Repository { repository, .. } => Some(repository),
            Target::Orchestrator { .. } | Target::Folder => None,
        };
        let info = self.target.repository_info();
        let checked_out = info.and_then(RepoInfo::current_branch).map(str::to_owned);
        let existing_options = info.map(existing_options).unwrap_or_default();
        let mut view = BranchView {
            modes,
            mode: self.choice.mode,
            branch: self.choice.branch.clone(),
            loading,
            bases: self.target.bases(),
            base,
            fetch: self.choice.fetch && (loading || fetch_remote.is_some()),
            fetch_remote,
            checked_out,
            worktree: None,
            existing: self.choice.existing.clone(),
            existing_options,
            use_existing: None,
            note: None,
            blocker: None,
        };
        let resolved = repository.map_or(Ok(None), |repository| {
            self.worktree(&view, repository, context.global)
        });
        let unresolved = match resolved {
            Ok(worktree) => {
                view.worktree = worktree;
                None
            }
            Err(error) => Some(error),
        };
        view.note = unreadable.or_else(|| {
            repository.and_then(|repository| mode_note(&view, repository, context.sessions))
        });
        view.blocker = self.blocker(&view, context).or(unresolved);
        view.use_existing = repository.and_then(|repository| {
            offer_existing(view.blocker.as_deref(), view.branch.trim(), repository)
        });
        self.view = view;
    }

    /// The worktree the session would run in with `view` in `repository`: for a new branch
    /// where its worktree goes; for an existing branch the worktree git lists for it, else
    /// where a new one goes; `None` in the main checkout, without a name or with invalid
    /// settings.
    ///
    /// # Errors
    ///
    /// Returns why the pattern cannot be resolved.
    fn worktree(
        &self,
        view: &BranchView,
        repository: &Repository,
        global: &GlobalSettings,
    ) -> Result<Option<PathBuf>, String> {
        let branch = match view.mode {
            BranchMode::Worktree => view.branch.trim(),
            BranchMode::ExistingWorktree => {
                if let Some(path) = picked(view).and_then(|option| option.worktree.clone()) {
                    return Ok(Some(path));
                }
                view.existing.as_deref().unwrap_or_default()
            }
            BranchMode::NewBranch | BranchMode::CheckedOut | BranchMode::ExistingBranch => {
                return Ok(None);
            }
        };
        let Ok(settings) = self.settings.as_ref() else {
            return Ok(None);
        };
        worktree_at(settings, global, &self.project, repository, branch)
    }

    /// Why Create cannot be pressed with `view`, the first reason that applies.
    fn blocker(&self, view: &BranchView, context: &Context<'_>) -> Option<String> {
        if let Err(error) = &self.settings {
            return Some(error.clone());
        }
        if view.mode == BranchMode::CheckedOut {
            return None;
        }
        let name = view.branch.trim();
        match &self.target {
            Target::Repository { repository, state } => {
                let wanted = Wanted {
                    mode: view.mode,
                    name,
                    base: view.base.as_deref(),
                    worktree: view.worktree.as_deref(),
                    existing: view.existing.as_deref(),
                };
                repository_blocker(repository, state, &wanted, context)
            }
            Target::Orchestrator { .. } | Target::Folder => {
                invalid_name(name).filter(|_| !name.is_empty())
            }
        }
    }
}

/// What `view` does in `repository`, for its note: as is works in the main checkout as it is,
/// a new branch in the main checkout switches it, and an existing branch picked says what
/// [`existing_note`] says. Working as it is names the `sessions` working in that folder too.
/// `None` for a new branch in a worktree.
fn mode_note(view: &BranchView, repository: &Repository, sessions: &[Session]) -> Option<String> {
    let repo = &repository.name;
    let shared = |folder: &Path| sharing(sessions, folder);
    match view.mode {
        BranchMode::CheckedOut => Some(as_is_note(repository, &shared)),
        BranchMode::NewBranch => Some(format!("switches {repo} itself to the new branch")),
        BranchMode::ExistingBranch | BranchMode::ExistingWorktree => {
            existing_note(view.mode, picked(view), repository, &shared)
        }
        BranchMode::Worktree => None,
    }
}

/// `works in <repo> as it is now`, for `repository`'s main checkout, ended with what `shared`
/// says about the others working there.
fn as_is_note(repository: &Repository, shared: &dyn Fn(&Path) -> String) -> String {
    format!(
        "works in {} as it is now{}",
        repository.name,
        shared(&repository.path)
    )
}

/// What working on the existing branch `option` in `mode` does in `repository`, for its note:
/// its worktree is used, a worktree is added for it, the main checkout switches to it or is
/// used as it is. Working as it is and using a worktree change nothing, so they end with what
/// `shared` says about the others working in that folder. `None` for any other mode, without
/// a branch picked, and for one that is no branch or whose worktree is gone.
fn existing_note(
    mode: BranchMode,
    option: Option<&ExistingOption>,
    repository: &Repository,
    shared: &dyn Fn(&Path) -> String,
) -> Option<String> {
    let repo = &repository.name;
    let option = option.filter(|option| !option.gone)?;
    match mode {
        BranchMode::ExistingBranch if option.main => Some(as_is_note(repository, &shared)),
        BranchMode::ExistingBranch => Some(format!("switches {repo} itself to {}", option.name)),
        BranchMode::ExistingWorktree => Some(match &option.worktree {
            Some(path) => format!("uses its existing worktree{}", shared(path)),
            None => "adds a worktree for it".into(),
        }),
        BranchMode::Worktree | BranchMode::NewBranch | BranchMode::CheckedOut => None,
    }
}

/// `"<name>" is not a valid git branch name` when it is not.
fn invalid_name(name: &str) -> Option<String> {
    (!is_valid_branch_name(name)).then(|| format!("\"{name}\" is not a valid git branch name"))
}

/// Where the worktree of `repository` in `project` goes for the new branch `branch`, by the
/// most specific layer of `settings` over `global` that sets it; `None` without a name.
///
/// # Errors
///
/// Returns why the pattern cannot be resolved, as for a leading `~` without a home folder.
pub(super) fn worktree_at(
    settings: &ProjectSettings,
    global: &GlobalSettings,
    project: &Project,
    repository: &Repository,
    branch: &str,
) -> Result<Option<PathBuf>, String> {
    if branch.is_empty() {
        return Ok(None);
    }
    let values = PatternValues {
        repo_path: &repository.path,
        repo: &repository.name,
        project: &project.name,
        branch,
    };
    settings
        .worktree_path(&global.layer, &repository.id)
        .resolve(&values, global.home.as_deref())
        .map(Some)
        .map_err(|error| error.to_string())
}

/// The branch a session or child wants in one repository, as the rules check it.
struct Wanted<'a> {
    /// The folder and the kind of branch.
    mode: BranchMode,
    /// The new branch, trimmed.
    name: &'a str,
    /// The branch it starts from, `None` when there is none.
    base: Option<&'a str>,
    /// Where the worktree goes, `None` in the main checkout or without a name.
    worktree: Option<&'a Path>,
    /// The existing branch picked, `None` when none is.
    existing: Option<&'a str>,
}

/// Why the branch `wanted` cannot be made or used in `repository`, the first reason that
/// applies.
fn repository_blocker(
    repository: &Repository,
    state: &RepoState,
    wanted: &Wanted<'_>,
    context: &Context<'_>,
) -> Option<String> {
    let (repo, name) = (&repository.name, wanted.name);
    let RepoState::Ready(info) = state else {
        return Some(format!("reading the branches of {repo}…"));
    };
    if wanted.mode.kind() == BranchKind::Existing {
        return existing_blocker(repository, info, wanted, context);
    }
    if (context.busy)(&repository.path) {
        return Some(busy(repo));
    }
    if name.is_empty() {
        return Some("enter a branch name".into());
    }
    if let Some(invalid) = invalid_name(name) {
        return Some(invalid);
    }
    if info.has_branch(name) {
        return Some(already_exists(name, repository));
    }
    let Some(base) = wanted.base else {
        return Some("pick a base branch".into());
    };
    if !info.has_branch(base) {
        return Some(format!("{base} is not a branch to start from"));
    }
    if wanted.mode == BranchMode::Worktree {
        return wanted
            .worktree
            .filter(|path| (context.exists)(path))
            .map(|path| format!("{} already exists", path.display()));
    }
    users(context.sessions, &repository.path)
        .next()
        .map(|session| in_use(repo, session))
}

/// `branch <name> already exists in <repo>`, why the new branch `name` cannot be made in
/// `repository`.
fn already_exists(name: &str, repository: &Repository) -> String {
    format!("branch {name} already exists in {}", repository.name)
}

/// The branch to offer as `use existing`: the new branch `name`, when `blocker`, the reason
/// shown, is that it already exists in `repository`; `None` while any other reason is shown,
/// since using the branch would not clear that one.
fn offer_existing(blocker: Option<&str>, name: &str, repository: &Repository) -> Option<String> {
    (blocker == Some(already_exists(name, repository).as_str())).then(|| name.to_owned())
}

/// Why the existing branch `wanted` cannot be used in `repository`, which `info` describes,
/// the first reason that applies, as the [module](self) lists them.
fn existing_blocker(
    repository: &Repository,
    info: &RepoInfo,
    wanted: &Wanted<'_>,
    context: &Context<'_>,
) -> Option<String> {
    let repo = &repository.name;
    let Some(name) = wanted.existing else {
        return Some("pick a branch".into());
    };
    if !info.has_branch(name) {
        return Some(format!("{name} is not a branch of {repo}"));
    }
    let option = existing_option(info, name);
    if option.gone {
        let path = linked_on(info, name)
            .map(|worktree| worktree.path.display().to_string())
            .next()
            .unwrap_or_default();
        return Some(format!(
            "its worktree at {path} is gone; run git worktree prune"
        ));
    }
    let works = if wanted.mode == BranchMode::ExistingBranch {
        if let Some(path) = &option.worktree {
            return Some(format!(
                "{name} is checked out in {} — pick Worktree",
                path.display()
            ));
        }
        if !option.main
            && let Some(session) = users(context.sessions, &repository.path).next()
        {
            return Some(in_use(repo, session));
        }
        !option.main
    } else {
        if option.main {
            return Some(format!(
                "{name} is checked out in {repo}'s main checkout — pick Main checkout"
            ));
        }
        if option.worktree.is_none()
            && let Some(path) = wanted.worktree.filter(|path| (context.exists)(path))
        {
            return Some(format!("{} already exists", path.display()));
        }
        option.worktree.is_none()
    };
    (works && (context.busy)(&repository.path)).then(|| busy(repo))
}

/// `<repo> is busy with another branch change`.
fn busy(repo: &str) -> String {
    format!("{repo} is busy with another branch change")
}

/// `<repo> is in use by "<title>" — use a worktree`, for the live `session` in its main
/// checkout.
fn in_use(repo: &str, session: &Session) -> String {
    format!("{repo} is in use by \"{}\" — use a worktree", session.title)
}

/// One child of a proposed split as the approval card leaves it.
pub(super) struct SplitChild<'a> {
    /// The repository the child is meant for.
    pub(super) id: &'a RepositoryId,
    /// That repository, `None` when the project has none by this id.
    pub(super) repository: Option<&'a Repository>,
    /// The child's branch settings.
    pub(super) choice: &'a BranchChoice,
    /// The child's title, as the notes of the other children name it.
    pub(super) title: String,
}

/// What the check found for one child: what the card shows, and the git work approving the
/// split does for it, `None` for a child that works in its main checkout as it is or while
/// something stops the approval.
#[derive(Debug)]
pub(super) struct CheckedChild {
    /// What the card shows for the child.
    pub(super) check: ChildCheck,
    /// The git work for the child.
    pub(super) planned: Option<Planned>,
}

/// Where one child of a split works.
#[derive(Debug)]
struct Place {
    /// Its folder: the main checkout, or its worktree once that has a name; `None` when the
    /// project has no such repository.
    folder: Option<PathBuf>,
    /// Whether the folder is a worktree the approval makes.
    made: bool,
}

impl Place {
    /// The worktree the approval makes for the child, `None` when it makes none.
    fn new_worktree(&self) -> Option<&Path> {
        self.folder.as_deref().filter(|_| self.made)
    }
}

/// Checks every child of a split in its own repository, with what git answered about each in
/// `reads`, as the [module](self) describes for one repository, and against the other
/// children: a second new branch with the same name in one repository, a worktree folder
/// another child makes already, and a child that switches the branch of its repository's main
/// checkout (a new branch, or an existing one the main checkout does not have) while another
/// child works in that main checkout are refused. Children that use one existing worktree may
/// share it, and each one's note names the others.
pub(super) fn check_children(
    project: &Project,
    settings: &ProjectSettings,
    children: &[SplitChild<'_>],
    reads: &HashMap<PathBuf, RepoState>,
    context: &Context<'_>,
) -> Vec<CheckedChild> {
    let resolved: Vec<(Result<Option<PathBuf>, String>, bool)> = children
        .iter()
        .map(|child| child_worktree(project, settings, reads, context.global, child))
        .collect();
    let places: Vec<Place> = children
        .iter()
        .zip(&resolved)
        .map(
            |(child, (worktree, reused))| match child.choice.mode.folder() {
                Folder::MainCheckout => Place {
                    folder: child.repository.map(|repository| repository.path.clone()),
                    made: false,
                },
                Folder::Worktree => Place {
                    folder: worktree.clone().ok().flatten(),
                    made: !reused,
                },
            },
        )
        .collect();
    let split = Split {
        children,
        places: &places,
        reads,
        context,
    };
    children
        .iter()
        .zip(&resolved)
        .enumerate()
        .map(|(index, (child, (worktree, _)))| match worktree {
            Ok(worktree) => split.check(index, child, worktree.as_ref()),
            Err(problem) => refused(problem.clone()),
        })
        .collect()
}

/// The worktree `child` runs in, as [`BranchView::worktree`] has it, and whether it is the
/// worktree git lists for the existing branch picked, so used as it is. The worktree is why
/// the pattern cannot be resolved when it cannot.
fn child_worktree(
    project: &Project,
    settings: &ProjectSettings,
    reads: &HashMap<PathBuf, RepoState>,
    global: &GlobalSettings,
    child: &SplitChild<'_>,
) -> (Result<Option<PathBuf>, String>, bool) {
    let Some(repository) = child.repository else {
        return (Ok(None), false);
    };
    let branch = match child.choice.mode {
        BranchMode::Worktree => child.choice.branch.trim(),
        BranchMode::ExistingWorktree => {
            let name = child.choice.existing.as_deref().unwrap_or_default();
            if let Some(RepoState::Ready(info)) = reads.get(&repository.path)
                && let Some(path) = existing_option(info, name).worktree
            {
                return (Ok(Some(path)), true);
            }
            name
        }
        BranchMode::NewBranch | BranchMode::CheckedOut | BranchMode::ExistingBranch => {
            return (Ok(None), false);
        }
    };
    (
        worktree_at(settings, global, project, repository, branch),
        false,
    )
}

/// The children of a split with what the check of each reads.
struct Split<'a> {
    children: &'a [SplitChild<'a>],
    /// Where each child works.
    places: &'a [Place],
    reads: &'a HashMap<PathBuf, RepoState>,
    context: &'a Context<'a>,
}

impl Split<'_> {
    /// The check of `child`, the child at `index`, whose worktree is `worktree`.
    fn check(
        &self,
        index: usize,
        child: &SplitChild<'_>,
        worktree: Option<&PathBuf>,
    ) -> CheckedChild {
        let Some(repository) = child.repository else {
            return refused(format!("unknown repository \"{}\"", child.id));
        };
        if child.choice.mode == BranchMode::CheckedOut {
            let shared = |folder: &Path| self.sharing(index, folder);
            return CheckedChild {
                check: ChildCheck {
                    note: Some(as_is_note(repository, &shared)),
                    ..ChildCheck::default()
                },
                planned: None,
            };
        }
        let repo = &repository.name;
        let (state, info) = match self.reads.get(&repository.path) {
            Some(state @ RepoState::Ready(info)) => (state, info),
            Some(RepoState::Failed(error)) => {
                return refused(format!("cannot read the git state of {repo}: {error}"));
            }
            Some(RepoState::Loading(_)) | None => {
                return CheckedChild {
                    check: ChildCheck {
                        loading: true,
                        ..ChildCheck::default()
                    },
                    planned: None,
                };
            }
        };
        let Some(before) = info.head() else {
            return refused(format!("{repo} has no main checkout"));
        };
        let choice = child.choice;
        let base = choice
            .base
            .as_deref()
            .filter(|base| info.has_branch(base))
            .or_else(|| info.current_branch());
        let fetch_remote = base.and_then(|base| info.fetch_remote(base));
        let wanted = Wanted {
            mode: choice.mode,
            name: choice.branch.trim(),
            base,
            worktree: worktree.map(PathBuf::as_path),
            existing: choice.existing.as_deref(),
        };
        let option = wanted
            .existing
            .filter(|name| choice.mode.kind() == BranchKind::Existing && info.has_branch(name))
            .map(|name| existing_option(info, name));
        let switches = match choice.mode {
            BranchMode::NewBranch => true,
            BranchMode::ExistingBranch => option.as_ref().is_some_and(|option| !option.main),
            BranchMode::Worktree | BranchMode::CheckedOut | BranchMode::ExistingWorktree => false,
        };
        let problem = repository_blocker(repository, state, &wanted, self.context)
            .or_else(|| self.conflict(index, child, repository, &wanted, switches));
        let make = child_make(
            choice,
            base,
            fetch_remote,
            worktree,
            switches,
            option.as_ref(),
        );
        let branch = match choice.mode.kind() {
            BranchKind::New => wanted.name,
            BranchKind::Existing | BranchKind::AsIs => wanted.existing.unwrap_or_default(),
        };
        let planned = make.filter(|_| problem.is_none()).map(|make| Planned {
            repository: repository.clone(),
            branch: branch.to_owned(),
            make,
            before: before.clone(),
        });
        let shared = |folder: &Path| self.sharing(index, folder);
        CheckedChild {
            check: ChildCheck {
                loading: false,
                bases: info.branch_names(),
                base: base.map(str::to_owned),
                fetch: choice.fetch && fetch_remote.is_some(),
                fetch_remote: fetch_remote.map(str::to_owned),
                worktree: worktree.cloned(),
                existing_options: existing_options(info),
                use_existing: offer_existing(problem.as_deref(), wanted.name, repository),
                note: existing_note(choice.mode, option.as_ref(), repository, &shared),
                problem,
            },
            planned,
        }
    }

    /// The note's end naming the other children of the split that work in `folder`, then the
    /// sessions that do, as [`sharing`] names them; the child at `index` is not named.
    fn sharing(&self, index: usize, folder: &Path) -> String {
        let children = self
            .children
            .iter()
            .zip(self.places)
            .enumerate()
            .filter(|(other, (_, place))| {
                *other != index && place.folder.as_deref() == Some(folder)
            })
            .map(|(_, (other, _))| other.title.as_str());
        also_used_by(children.chain(titles_in(self.context.sessions, folder)))
    }

    /// Why the child at `index`, which wants `wanted` in `repository` and `switches` the branch
    /// of its main checkout or not, clashes with the other children, the first reason that
    /// applies.
    fn conflict(
        &self,
        index: usize,
        child: &SplitChild<'_>,
        repository: &Repository,
        wanted: &Wanted<'_>,
        switches: bool,
    ) -> Option<String> {
        let repo = &repository.name;
        let others = || {
            self.children
                .iter()
                .enumerate()
                .filter(move |(other, _)| *other != index)
        };
        let earlier = || self.children.iter().zip(self.places).take(index);
        let same_repository = |other: &SplitChild<'_>| other.id == child.id;
        let same_name = wanted.mode.kind() == BranchKind::New
            && earlier().any(|(other, _)| {
                same_repository(other)
                    && other.choice.mode.kind() == BranchKind::New
                    && other.choice.branch.trim() == wanted.name
            });
        if same_name {
            return Some(format!(
                "branch {} is also used by another child in {repo}",
                wanted.name
            ));
        }
        if let Some(path) = self.places.get(index).and_then(Place::new_worktree)
            && earlier().any(|(_, other)| other.new_worktree() == Some(path))
        {
            return Some(format!(
                "worktree {} is also used by another child",
                path.display()
            ));
        }
        // A switch to an existing branch fails while another child's worktree has it.
        if let Some(name) = wanted
            .existing
            .filter(|_| switches && wanted.mode == BranchMode::ExistingBranch)
            && let Some(path) = others()
                .filter(|(_, other)| {
                    same_repository(other)
                        && other.choice.mode == BranchMode::ExistingWorktree
                        && other.choice.existing.as_deref() == Some(name)
                })
                .find_map(|(other, _)| self.places.get(other)?.folder.as_deref())
        {
            return Some(format!(
                "{name} is checked out in {} — pick Worktree",
                path.display()
            ));
        }
        let shares_checkout = switches
            && others().any(|(_, other)| {
                same_repository(other) && other.choice.mode.folder() == Folder::MainCheckout
            });
        shares_checkout.then(|| format!("{repo} is in use by another child — use a worktree"))
    }
}

/// What approving the split makes or changes for a child that wants `choice`: from `base`,
/// fetching from `fetch_remote` when wanted, in `worktree`; on the existing branch `option`,
/// whose main checkout `switches` to it or not. `None` when nothing is, as for a child that
/// works as it is, or when what it needs is missing.
fn child_make(
    choice: &BranchChoice,
    base: Option<&str>,
    fetch_remote: Option<&str>,
    worktree: Option<&PathBuf>,
    switches: bool,
    option: Option<&ExistingOption>,
) -> Option<Make> {
    let fetch = || fetch_remote.filter(|_| choice.fetch).map(str::to_owned);
    match choice.mode {
        BranchMode::Worktree => base.zip(worktree).map(|(base, path)| Make::NewWorktree {
            base: base.to_owned(),
            fetch: fetch(),
            path: path.clone(),
        }),
        BranchMode::NewBranch => base.map(|base| Make::NewBranch {
            base: base.to_owned(),
            fetch: fetch(),
        }),
        BranchMode::ExistingBranch => switches.then_some(Make::SwitchTo),
        BranchMode::ExistingWorktree => worktree.map(|path| {
            let path = path.clone();
            if option.is_some_and(|option| option.worktree.is_some()) {
                Make::Reuse { path }
            } else {
                Make::AddWorktree { path }
            }
        }),
        BranchMode::CheckedOut => None,
    }
}

/// The check of a child the split cannot be approved with, for `problem`.
fn refused(problem: String) -> CheckedChild {
    CheckedChild {
        check: ChildCheck {
            problem: Some(problem),
            ..ChildCheck::default()
        },
        planned: None,
    }
}

#[cfg(test)]
mod tests;
