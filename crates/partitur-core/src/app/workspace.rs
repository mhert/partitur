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
//! Create plans its git work from the settings as they are checked at that moment: an
//! optional fetch, then the step that makes the branch or the worktree, always last, so a
//! failed request leaves nothing behind. What a successful request made is undone by removing
//! the worktree, or switching the main checkout back to the head it had, and deleting the new
//! branch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::{GitAnswer, GitError, GitQuery, GitStep, Head, RepoInfo, is_valid_branch_name};
use crate::ids::{GitRequestId, RepositoryId};
use crate::orchestrator::orchestrated_repositories;
use crate::project::{Project, Repository};
use crate::session::{BranchChoice, BranchMode, Scope, Session, Workspace};
use crate::settings::{GlobalSettings, ProjectSettings};
use crate::view_model::{BranchView, ChildCheck};
use crate::worktree_path::PatternValues;

/// Every mode, in the order the sheet offers them.
const ALL_MODES: [BranchMode; 3] = [
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

/// The git work Create does in one repository: what it makes, from where, and the head the
/// main checkout had when it was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Planned {
    /// The repository the work happens in.
    pub(super) repository: Repository,
    /// Worktree or New branch.
    pub(super) mode: BranchMode,
    /// The new branch, trimmed.
    pub(super) branch: String,
    /// The branch it starts from.
    pub(super) base: String,
    /// The remote fetched first, `None` when nothing is fetched.
    pub(super) fetch: Option<String>,
    /// Where the worktree goes, `None` for a new branch in the main checkout.
    pub(super) worktree: Option<PathBuf>,
    /// What the main checkout had checked out in the answer the check used.
    pub(super) before: Head,
}

impl Planned {
    /// Whether `other` does the same work, whatever head the main checkout had.
    pub(super) fn same_work(&self, other: &Self) -> bool {
        let rebased = Self {
            before: other.before.clone(),
            ..self.clone()
        };
        rebased == *other
    }
}

/// The steps that carry out `planned`: `[Fetch]` when planned, then `CreateBranch` or
/// `AddWorktree`; the step that makes something is always last. The new branch starts from
/// `<remote>/<base>` when fetching, else from `<base>`, each named by its full ref
/// (`refs/remotes/<remote>/<base>`, `refs/heads/<base>`), so a tag of the same name never
/// wins.
pub(super) fn create_steps(planned: &Planned) -> Vec<GitStep> {
    let repository = planned.repository.path.clone();
    let branch = planned.branch.clone();
    let start = match &planned.fetch {
        Some(remote) => format!("refs/remotes/{remote}/{}", planned.base),
        None => format!("refs/heads/{}", planned.base),
    };
    let make = match &planned.worktree {
        Some(path) => GitStep::AddWorktree {
            repository: repository.clone(),
            path: path.clone(),
            branch,
            start,
        },
        None => GitStep::CreateBranch {
            repository: repository.clone(),
            branch,
            start,
        },
    };
    let fetch = planned.fetch.iter().map(|remote| GitStep::Fetch {
        repository: repository.clone(),
        remote: remote.clone(),
    });
    fetch.chain([make]).collect()
}

/// What a session records once the steps of `planned` succeeded.
pub(super) fn workspace_of(planned: &Planned) -> Workspace {
    let (branch, base, fetched) = (
        planned.branch.clone(),
        planned.base.clone(),
        planned.fetch.is_some(),
    );
    match &planned.worktree {
        Some(path) => Workspace::Worktree {
            branch,
            base,
            fetched,
            path: path.clone(),
        },
        None => Workspace::NewBranch {
            branch,
            base,
            fetched,
        },
    }
}

/// The steps that undo `workspace`, made in `repository` while its main checkout had `before`
/// checked out: a worktree is removed, `[RemoveWorktree, DeleteBranch]`; a new branch is left,
/// `[RestoreHead(before), DeleteBranch]`. A checked-out workspace made nothing to undo.
pub(super) fn undo_steps(repository: &Path, workspace: &Workspace, before: &Head) -> Vec<GitStep> {
    let repository = repository.to_path_buf();
    let (undo, branch) = match workspace {
        Workspace::CheckedOut => return Vec::new(),
        Workspace::Worktree { branch, path, .. } => (
            GitStep::RemoveWorktree {
                repository: repository.clone(),
                path: path.clone(),
            },
            branch,
        ),
        Workspace::NewBranch { branch, .. } => (
            GitStep::RestoreHead {
                repository: repository.clone(),
                head: before.clone(),
            },
            branch,
        ),
    };
    vec![
        undo,
        GitStep::DeleteBranch {
            repository,
            branch: branch.clone(),
        },
    ]
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
            Self::Repository { .. } | Self::Orchestrator { .. } => (ALL_MODES.to_vec(), None),
        }
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
    }
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
    /// `None` when it does none, as for Checked-out, an orchestrator or a plain folder.
    ///
    /// # Errors
    ///
    /// Returns the view's blocker when Create cannot be pressed, and `<repo> has no main
    /// checkout` when a repository session that should make a branch has no head to return
    /// to, so it never falls back to working in the checkout it was meant to leave alone.
    pub(super) fn planned(&self) -> Result<Option<Planned>, String> {
        let view = &self.view;
        if let Some(blocker) = &view.blocker {
            return Err(blocker.clone());
        }
        let Target::Repository { repository, state } = &self.target else {
            return Ok(None);
        };
        if view.mode == BranchMode::CheckedOut {
            return Ok(None);
        }
        // Without a blocker the repository is read and a base picked; what can still be
        // missing is a main checkout to return to.
        if let RepoState::Ready(info) = state
            && let Some(base) = &view.base
            && let Some(before) = info.head()
        {
            return Ok(Some(Planned {
                repository: repository.clone(),
                mode: view.mode,
                branch: view.branch.trim().to_owned(),
                base: base.clone(),
                fetch: view.fetch_remote.clone().filter(|_| view.fetch),
                worktree: view.worktree.clone(),
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
        if !self.view.modes.contains(&mode) {
            return Err(format!("{} is not offered for this session", label(mode)));
        }
        self.choice.mode = mode;
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
        let checked_out = match &self.target {
            Target::Repository {
                state: RepoState::Ready(info),
                ..
            } => info.current_branch().map(str::to_owned),
            _ => None,
        };
        let resolved = repository
            .filter(|_| self.choice.mode == BranchMode::Worktree)
            .map_or(Ok(None), |repository| {
                self.worktree(repository, context.global)
            });
        let (worktree, unresolved) = match resolved {
            Ok(worktree) => (worktree, None),
            Err(error) => (None, Some(error)),
        };
        let note = unreadable.or_else(|| {
            repository
                .filter(|_| self.choice.mode == BranchMode::NewBranch)
                .map(|repository| format!("switches {} itself to the new branch", repository.name))
        });
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
            worktree,
            note,
            blocker: None,
        };
        view.blocker = self.blocker(&view, context).or(unresolved);
        self.view = view;
    }

    /// Where the worktree of `repository` goes for the branch named, by the most specific
    /// settings layer; `None` without a name or with invalid settings.
    ///
    /// # Errors
    ///
    /// Returns why the pattern cannot be resolved.
    fn worktree(
        &self,
        repository: &Repository,
        global: &GlobalSettings,
    ) -> Result<Option<PathBuf>, String> {
        let Ok(settings) = self.settings.as_ref() else {
            return Ok(None);
        };
        worktree_at(
            settings,
            global,
            &self.project,
            repository,
            self.choice.branch.trim(),
        )
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
                };
                repository_blocker(repository, state, &wanted, context)
            }
            Target::Orchestrator { .. } | Target::Folder => {
                invalid_name(name).filter(|_| !name.is_empty())
            }
        }
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

/// The new branch a session or child wants in one repository, as the rules check it.
struct Wanted<'a> {
    /// Worktree or New branch.
    mode: BranchMode,
    /// The new branch, trimmed.
    name: &'a str,
    /// The branch it starts from, `None` when there is none.
    base: Option<&'a str>,
    /// Where the worktree goes, `None` for a new branch in the main checkout or without a
    /// name.
    worktree: Option<&'a Path>,
}

/// Why the new branch `wanted` cannot be made in `repository`, the first reason that applies.
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
    if (context.busy)(&repository.path) {
        return Some(format!("{repo} is busy with another branch change"));
    }
    if name.is_empty() {
        return Some("enter a branch name".into());
    }
    if let Some(invalid) = invalid_name(name) {
        return Some(invalid);
    }
    if info.has_branch(name) {
        return Some(format!("branch {name} already exists in {repo}"));
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
            .filter(|path| path.exists())
            .map(|path| format!("{} already exists", path.display()));
    }
    context
        .sessions
        .iter()
        .find(|session| session.archived.is_none() && session.cwd == repository.path)
        .map(|session| format!("{repo} is in use by \"{}\" — use a worktree", session.title))
}

/// One child of a proposed split as the approval card leaves it.
pub(super) struct SplitChild<'a> {
    /// The repository the child is meant for.
    pub(super) id: &'a RepositoryId,
    /// That repository, `None` when the project has none by this id.
    pub(super) repository: Option<&'a Repository>,
    /// The child's branch settings.
    pub(super) choice: &'a BranchChoice,
}

/// What the check found for one child: what the card shows, and the git work approving the
/// split does for it, `None` for a Checked-out child or while something stops the approval.
#[derive(Debug)]
pub(super) struct CheckedChild {
    /// What the card shows for the child.
    pub(super) check: ChildCheck,
    /// The git work for the child.
    pub(super) planned: Option<Planned>,
}

/// Checks every child of a split in its own repository, with what git answered about each in
/// `reads`, as the [module](self) describes for one repository, and against the other
/// children: a second Worktree or New branch child with the same name in one repository, a
/// worktree folder another child uses already, and a New branch child in a repository whose
/// checkout another child works in (Checked-out or New branch) are refused.
pub(super) fn check_children(
    project: &Project,
    settings: &ProjectSettings,
    children: &[SplitChild<'_>],
    reads: &HashMap<PathBuf, RepoState>,
    context: &Context<'_>,
) -> Vec<CheckedChild> {
    let resolved: Vec<Result<Option<PathBuf>, String>> = children
        .iter()
        .map(|child| match child.repository {
            Some(repository) if child.choice.mode == BranchMode::Worktree => worktree_at(
                settings,
                context.global,
                project,
                repository,
                child.choice.branch.trim(),
            ),
            _ => Ok(None),
        })
        .collect();
    let worktrees: Vec<Option<PathBuf>> = resolved
        .iter()
        .map(|worktree| worktree.clone().ok().flatten())
        .collect();
    let split = Split {
        children,
        worktrees: &worktrees,
        reads,
        context,
    };
    children
        .iter()
        .zip(&resolved)
        .enumerate()
        .map(|(index, (child, worktree))| match worktree {
            Ok(worktree) => split.check(index, child, worktree.as_ref()),
            Err(problem) => refused(problem.clone()),
        })
        .collect()
}

/// The children of a split with what the check of each reads.
struct Split<'a> {
    children: &'a [SplitChild<'a>],
    /// Each child's worktree folder, `None` when it makes no worktree.
    worktrees: &'a [Option<PathBuf>],
    reads: &'a HashMap<PathBuf, RepoState>,
    context: &'a Context<'a>,
}

impl Split<'_> {
    /// The check of `child`, the child at `index`, whose worktree goes to `worktree`.
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
            return CheckedChild {
                check: ChildCheck::default(),
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
        };
        let problem = repository_blocker(repository, state, &wanted, self.context)
            .or_else(|| self.conflict(index, child, repository, &wanted));
        let planned = match (&problem, base) {
            (None, Some(base)) => Some(Planned {
                repository: repository.clone(),
                mode: choice.mode,
                branch: wanted.name.to_owned(),
                base: base.to_owned(),
                fetch: fetch_remote.filter(|_| choice.fetch).map(str::to_owned),
                worktree: worktree.cloned(),
                before: before.clone(),
            }),
            _ => None,
        };
        CheckedChild {
            check: ChildCheck {
                loading: false,
                bases: info.branch_names(),
                base: base.map(str::to_owned),
                fetch: choice.fetch && fetch_remote.is_some(),
                fetch_remote: fetch_remote.map(str::to_owned),
                worktree: worktree.cloned(),
                problem,
            },
            planned,
        }
    }

    /// Why the child at `index`, which wants `wanted` in `repository`, clashes with the other
    /// children, the first reason that applies.
    fn conflict(
        &self,
        index: usize,
        child: &SplitChild<'_>,
        repository: &Repository,
        wanted: &Wanted<'_>,
    ) -> Option<String> {
        let repo = &repository.name;
        let others = || {
            self.children
                .iter()
                .zip(self.worktrees)
                .enumerate()
                .filter(move |(other, _)| *other != index)
        };
        let earlier = || self.children.iter().zip(self.worktrees).take(index);
        let same_repository = |other: &SplitChild<'_>| other.id == child.id;
        let same_name = earlier().any(|(other, _)| {
            same_repository(other)
                && other.choice.mode != BranchMode::CheckedOut
                && other.choice.branch.trim() == wanted.name
        });
        if same_name {
            return Some(format!(
                "branch {} is also used by another child in {repo}",
                wanted.name
            ));
        }
        if let Some(path) = wanted.worktree
            && earlier().any(|(_, other)| other.as_deref() == Some(path))
        {
            return Some(format!(
                "worktree {} is also used by another child",
                path.display()
            ));
        }
        let shares_checkout = wanted.mode == BranchMode::NewBranch
            && others().any(|(_, (other, _))| {
                same_repository(other) && other.choice.mode != BranchMode::Worktree
            });
        shares_checkout.then(|| format!("{repo} is in use by another child — use a worktree"))
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
mod tests {

    use super::*;

    #[test]
    fn an_answer_that_describes_no_repository_fails_it() {
        let other = repo_state(Some(Ok(GitAnswer::Changes(0))));
        let missing = repo_state(None);

        for state in [other, missing] {
            assert!(
                matches!(&state, RepoState::Failed(error) if error == "git did not describe the repository"),
                "{state:?}"
            );
        }
    }

    #[test]
    fn a_checked_out_workspace_has_nothing_to_undo() {
        let main = Head::Branch("main".into());

        assert!(undo_steps(Path::new("/p/api"), &Workspace::CheckedOut, &main).is_empty());
    }

    #[test]
    fn every_mode_has_its_sheet_label() {
        assert_eq!(label(BranchMode::Worktree), "Worktree");
        assert_eq!(label(BranchMode::NewBranch), "New branch");
        assert_eq!(label(BranchMode::CheckedOut), "Checked-out");
    }
}
