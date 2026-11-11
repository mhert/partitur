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

use std::path::{Path, PathBuf};

use crate::git::{GitAnswer, GitError, GitQuery, RepoInfo, is_valid_branch_name};
use crate::ids::GitRequestId;
use crate::project::{Project, Repository};
use crate::session::{BranchChoice, BranchMode, Scope, Session};
use crate::settings::{GlobalSettings, ProjectSettings};
use crate::view_model::BranchView;
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
    /// orchestrator targets its repositories below the root; a repository session, or a
    /// project session whose root is a repository, targets that repository; any other session
    /// targets the folder. `ask` queues the reading of the repositories, given one
    /// [`GitQuery::Inspect`] per repository, and returns the state they start in.
    pub(super) fn read(
        project: &Project,
        scope: &Scope,
        orchestrator: bool,
        ask: impl FnOnce(Vec<GitQuery>) -> RepoState,
    ) -> Self {
        let repositories: Vec<Repository> = project
            .repositories
            .iter()
            .filter(|repository| match scope {
                _ if orchestrator => !repository.is_root,
                Scope::Repository(id) => repository.id == *id,
                Scope::Project => repository.is_root,
            })
            .cloned()
            .collect();
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
fn is_loading(state: &RepoState, request: GitRequestId) -> bool {
    matches!(state, RepoState::Loading(waiting) if *waiting == request)
}

/// The state an `answer` to an inspect leaves a repository in.
fn repo_state(answer: Option<Result<GitAnswer, GitError>>) -> RepoState {
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

    /// The view the last [`refresh`](Self::refresh) computed.
    pub(super) fn view(&self) -> &BranchView {
        &self.view
    }

    /// Applies the `answers` to `request` when the settings wait for it, and sets the default
    /// base; returns whether they waited for it.
    pub(super) fn answer(
        &mut self,
        request: GitRequestId,
        answers: Vec<Result<GitAnswer, GitError>>,
    ) -> bool {
        let answered = self.target.answer(request, answers);
        if answered {
            self.choice.base = self.target.start();
        }
        answered
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
        let worktree = repository
            .filter(|_| self.choice.mode == BranchMode::Worktree)
            .and_then(|repository| self.worktree(repository, context.global));
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
        view.blocker = self.blocker(&view, context);
        self.view = view;
    }

    /// Where the worktree of `repository` goes for the branch named, by the most specific
    /// settings layer; `None` without a name or with invalid settings.
    fn worktree(&self, repository: &Repository, global: &GlobalSettings) -> Option<PathBuf> {
        let branch = self.choice.branch.trim();
        let settings = self.settings.as_ref().ok().filter(|_| !branch.is_empty())?;
        let values = PatternValues {
            repo_path: &repository.path,
            repo: &repository.name,
            project: &self.project.name,
            branch,
        };
        Some(
            settings
                .worktree_path(&global.layer, &repository.id)
                .resolve(&values, global.home.as_deref()),
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
                repository_blocker(repository, state, name, view, context)
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

/// Why a new branch `name` cannot be made in `repository` as `view` shows it, the first
/// reason that applies.
fn repository_blocker(
    repository: &Repository,
    state: &RepoState,
    name: &str,
    view: &BranchView,
    context: &Context<'_>,
) -> Option<String> {
    let repo = &repository.name;
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
    if view.base.is_none() {
        return Some(format!("{repo} has no branch to start from"));
    }
    if view.mode == BranchMode::Worktree {
        return view
            .worktree
            .as_ref()
            .filter(|path| path.exists())
            .map(|path| format!("{} already exists", path.display()));
    }
    context
        .sessions
        .iter()
        .find(|session| session.archived.is_none() && session.cwd == repository.path)
        .map(|session| format!("{repo} is in use by \"{}\" — use a worktree", session.title))
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
    fn every_mode_has_its_sheet_label() {
        assert_eq!(label(BranchMode::Worktree), "Worktree");
        assert_eq!(label(BranchMode::NewBranch), "New branch");
        assert_eq!(label(BranchMode::CheckedOut), "Checked-out");
    }
}
