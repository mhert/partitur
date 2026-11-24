//! In-memory stand-ins for the [`Backend`], [`Store`] and [`Git`] ports, for tests in this
//! and other crates, [`FixedClock`], and `alive`, which tells tests on Unix whether a process
//! still runs.
//! Available with the `testing` cargo feature.
//!
//! Every fake is a cheap handle to shared state: keep a clone, hand the other to
//! [`App`](crate::App), and inspect or steer the fake while the app owns it.
//!
//! [`MemoryStore`] is both the synchronous [`Store`] and the [`StoreQueue`]. Made with
//! [`immediate`](MemoryStore::immediate) (or `new`), it carries out each queued command inside
//! [`send`](StoreQueue::send); made with [`held`](MemoryStore::held), it keeps them until the
//! test calls [`release`](MemoryStore::release). Either way the events go into a list the
//! test drains with [`take_events`](MemoryStore::take_events), and [`apply_store`] feeds them
//! to an [`App`].
//!
//! [`MemoryGit`] never answers by itself. It queues each request until the test calls
//! [`MemoryGit::answer`], so a test holds a pending state simply by not answering, and it keeps
//! a model of each repository that the steps it runs change the way git would.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::app::{App, AppError};
use crate::backend::{Backend, BackendError, PromptContent};
use crate::clock::Clock;
use crate::git::{
    Git, GitAnswer, GitError, GitEvent, GitQuery, GitStep, Head, LocalBranch, RepoInfo,
    WorktreeInfo,
};
use crate::ids::{
    BackendId, BackendInfo, BackendSessionId, ConfigId, ConfigValueId, GitRequestId, PermissionId,
    PermissionOptionId, ProjectId, RequestId, SessionId,
};
use crate::project::Project;
use crate::reader::{ReadDone, TranscriptReader};
use crate::record::TranscriptRecord;
use crate::session::Session;
use crate::settings::ProjectSettings;
use crate::store::{Store, StoreCommand, StoreError, StoreEvent, StoreQueue};
use crate::tool_call::{ToolAnswer, ToolReply, ToolRole};
use crate::widget_host::{ViewId, WidgetHost, WidgetHostError, WidgetInput, WidgetPage};

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Clock`] that stands still until a test moves it. It is a cheap handle to shared time:
/// keep a clone, hand the other to [`App`], and set or advance the time while the app owns it.
#[derive(Debug, Clone)]
pub struct FixedClock(Arc<AtomicU64>);

impl Default for FixedClock {
    /// A clock showing 14 November 2023, so that times read as plausible ones.
    fn default() -> Self {
        Self::new(1_700_000_000)
    }
}

impl FixedClock {
    /// A clock showing `seconds` since the Unix epoch.
    #[must_use]
    pub fn new(seconds: u64) -> Self {
        Self(Arc::new(AtomicU64::new(seconds)))
    }

    /// Makes the clock show `seconds` since the Unix epoch.
    pub fn set(&self, seconds: u64) {
        self.0.store(seconds, Ordering::SeqCst);
    }

    /// Moves the clock `seconds` forward.
    pub fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Whether the process `pid` still exists, asked with `kill -0`: for tests that check a
/// process tree was ended.
#[cfg(unix)]
#[must_use]
pub fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The backend list and default of a setup with one backend, `claude`, running
/// `claude-agent-acp`: what [`App::load`](crate::App::load) gets when nothing else is
/// configured.
///
/// # Examples
///
/// ```
/// use partitur_core::testing::single_claude;
///
/// let (backends, default) = single_claude();
/// assert_eq!(backends.len(), 1);
/// assert_eq!(backends[0].id, default);
/// assert_eq!(default.as_str(), "claude");
/// ```
pub fn single_claude() -> (Vec<BackendInfo>, BackendId) {
    let id = BackendId::trusted("claude");
    let info = BackendInfo {
        id: id.clone(),
        name: id.to_string(),
        command: "claude-agent-acp".into(),
    };
    (vec![info], id)
}

/// One command received by a [`RecordingBackend`], mirroring the [`Backend`] methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// [`Backend::new_session`].
    NewSession {
        /// The request id.
        request: RequestId,
        /// The Partitur session.
        session: SessionId,
        /// The configured backend to run it on.
        backend: BackendId,
        /// The working directory.
        cwd: PathBuf,
        /// The tools the session is offered.
        role: ToolRole,
    },
    /// [`Backend::load_session`].
    LoadSession {
        /// The request id.
        request: RequestId,
        /// The Partitur session.
        session: SessionId,
        /// The configured backend that created it.
        backend: BackendId,
        /// The backend's id of the session to resume.
        backend_session: BackendSessionId,
        /// The working directory.
        cwd: PathBuf,
        /// The tools the session is offered.
        role: ToolRole,
    },
    /// [`Backend::prompt`].
    Prompt {
        /// The Partitur session.
        session: SessionId,
        /// The prompt content.
        content: Vec<PromptContent>,
    },
    /// [`Backend::cancel`].
    Cancel {
        /// The Partitur session.
        session: SessionId,
    },
    /// [`Backend::close_session`].
    CloseSession {
        /// The Partitur session.
        session: SessionId,
    },
    /// [`Backend::set_config`].
    SetConfig {
        /// The Partitur session.
        session: SessionId,
        /// The option being set.
        option: ConfigId,
        /// The new value.
        value: ConfigValueId,
    },
    /// [`Backend::respond_permission`].
    RespondPermission {
        /// The Partitur session.
        session: SessionId,
        /// The permission request being answered.
        permission: PermissionId,
        /// The chosen option, `None` when cancelling.
        option: Option<PermissionOptionId>,
    },
}

impl Call {
    /// The request, session and working directory when this is a [`Call::NewSession`].
    pub fn as_new_session(&self) -> Option<(RequestId, &SessionId, &Path)> {
        match self {
            Self::NewSession {
                request,
                session,
                cwd,
                ..
            } => Some((*request, session, cwd)),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct Recording {
    calls: Vec<Call>,
    /// The error a command fails with, after how many more succeed.
    fail_next: Option<(usize, BackendError)>,
}

/// A [`Backend`] that records every command instead of running it.
///
/// Clones share the same recording.
#[derive(Debug, Clone, Default)]
pub struct RecordingBackend {
    state: Arc<Mutex<Recording>>,
}

impl RecordingBackend {
    /// Creates a backend with no recorded calls.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every command received so far, oldest first, including ones that were made to fail.
    pub fn calls(&self) -> Vec<Call> {
        locked(&self.state).calls.clone()
    }

    /// Makes the next command return [`BackendError::Unavailable`]; later ones succeed again.
    pub fn fail_next_call(&self) {
        self.fail_next_call_with(BackendError::Unavailable("recording backend".into()));
    }

    /// Makes the next command return `error`; later ones succeed again.
    pub fn fail_next_call_with(&self, error: BackendError) {
        locked(&self.state).fail_next = Some((0, error));
    }

    /// Lets the next `calls` commands succeed and makes the one after them return
    /// [`BackendError::Unavailable`]; later ones succeed again.
    pub fn fail_call_after(&self, calls: usize) {
        let error = BackendError::Unavailable("recording backend".into());
        locked(&self.state).fail_next = Some((calls, error));
    }

    fn record(&self, call: Call) -> Result<(), BackendError> {
        let mut state = locked(&self.state);
        state.calls.push(call);
        match state.fail_next.take() {
            Some((0, error)) => Err(error),
            Some((left, error)) => {
                state.fail_next = Some((left - 1, error));
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl Backend for RecordingBackend {
    fn new_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        cwd: &Path,
        role: ToolRole,
    ) -> Result<(), BackendError> {
        self.record(Call::NewSession {
            request,
            session,
            backend: backend.clone(),
            cwd: cwd.to_path_buf(),
            role,
        })
    }

    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        backend_session: &BackendSessionId,
        cwd: &Path,
        role: ToolRole,
    ) -> Result<(), BackendError> {
        self.record(Call::LoadSession {
            request,
            session,
            backend: backend.clone(),
            backend_session: backend_session.clone(),
            cwd: cwd.to_path_buf(),
            role,
        })
    }

    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError> {
        self.record(Call::Prompt { session, content })
    }

    fn cancel(&self, session: SessionId) -> Result<(), BackendError> {
        self.record(Call::Cancel { session })
    }

    fn close_session(&self, session: SessionId) -> Result<(), BackendError> {
        self.record(Call::CloseSession { session })
    }

    fn set_config(
        &self,
        session: SessionId,
        option: &ConfigId,
        value: &ConfigValueId,
    ) -> Result<(), BackendError> {
        self.record(Call::SetConfig {
            session,
            option: option.clone(),
            value: value.clone(),
        })
    }

    fn respond_permission(
        &self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), BackendError> {
        self.record(Call::RespondPermission {
            session,
            permission,
            option,
        })
    }
}

#[derive(Debug, Default)]
struct Memory {
    projects: Vec<Project>,
    sessions: HashMap<ProjectId, Vec<Session>>,
    updates: HashMap<(ProjectId, SessionId), Vec<TranscriptRecord>>,
    session_saves: HashMap<ProjectId, usize>,
    update_loads: usize,
    corrupt_updates: bool,
    /// The sessions whose updates load as corrupt, whatever `corrupt_updates` says.
    corrupt_sessions: HashSet<SessionId>,
    fail_next_session_save: bool,
    fail_next_delete: bool,
    /// How many upcoming appends still fail.
    failing_appends: usize,
    settings: HashMap<ProjectId, ProjectSettings>,
    /// The settings of a project that has none of its own.
    default_settings: ProjectSettings,
    /// How many times settings were loaded.
    settings_loads: usize,
    /// How many upcoming settings loads still fail.
    failing_settings_loads: usize,
    /// The folder the sessions' attachment folders are made in, when it is a real one.
    attachment_root: Option<PathBuf>,
    /// The queued commands waiting for [`MemoryStore::release`], `None` when queued commands
    /// are carried out at once.
    held: Option<VecDeque<StoreCommand>>,
    /// What the queued commands reported, for [`MemoryStore::take_events`].
    events: Vec<StoreEvent>,
}

/// A [`Store`] that keeps everything in memory.
///
/// Clones share the same data.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    state: Arc<Mutex<Memory>>,
}

impl MemoryStore {
    /// Creates an empty store that carries out every queued command at once, as
    /// [`immediate`](Self::immediate) does.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty store that carries out every queued command inside
    /// [`send`](StoreQueue::send).
    pub fn immediate() -> Self {
        Self::default()
    }

    /// Creates an empty store that keeps every queued command until
    /// [`release`](Self::release); the synchronous [`Store`] methods still act at once.
    pub fn held() -> Self {
        let store = Self::default();
        locked(&store.state).held = Some(VecDeque::new());
        store
    }

    /// Carries out every command held so far, in the order they were queued.
    pub fn release(&self) {
        let held = locked(&self.state).held.as_mut().map(std::mem::take);
        for command in held.unwrap_or_default() {
            self.carry_out(&command);
        }
    }

    /// How many queued commands are held.
    pub fn held_commands(&self) -> usize {
        locked(&self.state).held.as_ref().map_or(0, VecDeque::len)
    }

    /// The events the queued commands reported since the last call, in order.
    pub fn take_events(&self) -> Vec<StoreEvent> {
        std::mem::take(&mut locked(&self.state).events)
    }

    /// Carries out `command` through the [`Store`] methods and records what it reports.
    fn carry_out(&self, command: &StoreCommand) {
        let failure = match command {
            StoreCommand::Append {
                project,
                session,
                record,
            } => self.append_record(project.clone(), session.clone(), record),
            StoreCommand::SaveProjects(projects) => self.save_projects(projects),
            StoreCommand::SaveSessions { project, sessions } => {
                self.save_sessions(project.clone(), sessions)
            }
            StoreCommand::Delete { project, session } => {
                self.delete_records(project.clone(), session.clone())
            }
            StoreCommand::Load {
                request,
                project,
                session,
            } => {
                let records = self
                    .load_records(project.clone(), session.clone())
                    .map_err(Arc::new);
                locked(&self.state).events.push(StoreEvent::Loaded {
                    request: *request,
                    session: session.clone(),
                    records,
                });
                return;
            }
        };
        if let Err(error) = failure {
            let event = StoreEvent::failed(command, error);
            locked(&self.state).events.push(event);
        }
    }

    /// Names the attachment folder of a session `<root>/<session>`, so a test can put real
    /// files in it; it is `/memory/<session>/attachments` until then.
    pub fn set_attachment_root(&self, root: impl Into<PathBuf>) {
        locked(&self.state).attachment_root = Some(root.into());
    }

    /// The project whose saved sessions hold `session`, for a test that reads or writes the
    /// history of a session it only knows by id; `None` when no saved list holds it, as after
    /// the session was deleted.
    pub fn project_of(&self, session: &SessionId) -> Option<ProjectId> {
        locked(&self.state)
            .sessions
            .iter()
            .find(|(_, saved)| saved.iter().any(|saved| &saved.id == session))
            .map(|(project, _)| project.clone())
    }

    /// Whether any project holds stored records of `session`, whatever its saved lists say.
    pub fn has_records(&self, session: &SessionId) -> bool {
        locked(&self.state)
            .updates
            .keys()
            .any(|(_, stored)| stored == session)
    }

    /// Makes [`load_records`](Store::load_records) report [`StoreError::Corrupt`] while `on`.
    pub fn set_corrupt_updates(&self, on: bool) {
        locked(&self.state).corrupt_updates = on;
    }

    /// Makes [`load_records`](Store::load_records) report [`StoreError::Corrupt`] for
    /// `session` from now on; the updates of other sessions still load.
    pub fn set_corrupt_updates_of(&self, session: SessionId) {
        locked(&self.state).corrupt_sessions.insert(session);
    }

    /// Makes the next [`save_sessions`](Store::save_sessions) report [`StoreError::Io`]
    /// without saving; later ones succeed again.
    pub fn fail_next_session_save(&self) {
        locked(&self.state).fail_next_session_save = true;
    }

    /// Makes the next [`append_record`](Store::append_record) report [`StoreError::Io`]
    /// without appending; later ones succeed again.
    pub fn fail_next_append(&self) {
        locked(&self.state).failing_appends = 1;
    }

    /// Makes the next [`delete_records`](Store::delete_records) report [`StoreError::Io`]
    /// without deleting; later ones succeed again.
    pub fn fail_next_delete(&self) {
        locked(&self.state).fail_next_delete = true;
    }

    /// Sets the settings [`load_settings`](Store::load_settings) returns for `project`.
    pub fn set_settings(&self, project: ProjectId, settings: ProjectSettings) {
        locked(&self.state).settings.insert(project, settings);
    }

    /// Sets the settings [`load_settings`](Store::load_settings) returns for every project
    /// without settings of its own, which suits a project whose id is not known yet.
    pub fn set_default_settings(&self, settings: ProjectSettings) {
        locked(&self.state).default_settings = settings;
    }

    /// How many times [`load_settings`](Store::load_settings) was called, failed calls included.
    pub fn settings_loads(&self) -> usize {
        locked(&self.state).settings_loads
    }

    /// Makes the next [`load_settings`](Store::load_settings) report [`StoreError::Io`];
    /// later ones succeed again.
    pub fn fail_next_settings_load(&self) {
        locked(&self.state).failing_settings_loads = 1;
    }

    /// How many times [`save_sessions`](Store::save_sessions) was called for `project`.
    pub fn session_saves(&self, project: &ProjectId) -> usize {
        locked(&self.state)
            .session_saves
            .get(project)
            .copied()
            .unwrap_or_default()
    }

    /// How many times [`load_records`](Store::load_records) was called, failed calls included.
    pub fn update_loads(&self) -> usize {
        locked(&self.state).update_loads
    }
}

impl Store for MemoryStore {
    fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        Ok(locked(&self.state).projects.clone())
    }

    fn save_projects(&self, projects: &[Project]) -> Result<(), StoreError> {
        locked(&self.state).projects = projects.to_vec();
        Ok(())
    }

    fn load_sessions(&self, project: ProjectId) -> Result<Vec<Session>, StoreError> {
        Ok(locked(&self.state)
            .sessions
            .get(&project)
            .cloned()
            .unwrap_or_default())
    }

    fn save_sessions(&self, project: ProjectId, sessions: &[Session]) -> Result<(), StoreError> {
        let mut state = locked(&self.state);
        *state.session_saves.entry(project.clone()).or_default() += 1;
        if std::mem::take(&mut state.fail_next_session_save) {
            return Err(simulated_failure(format!("memory/{project}/sessions.json")));
        }
        state.sessions.insert(project, sessions.to_vec());
        Ok(())
    }

    /// Deliberately stricter than the file store, to catch an `App` that addresses a session
    /// by the wrong project: it rejects a session that no saved session list of `project`
    /// contains.
    fn append_record(
        &self,
        project: ProjectId,
        session: SessionId,
        update: &TranscriptRecord,
    ) -> Result<(), StoreError> {
        let mut state = locked(&self.state);
        if let Some(left) = state.failing_appends.checked_sub(1) {
            state.failing_appends = left;
            return Err(simulated_failure(format!("memory/{session}.jsonl")));
        }
        let known = state
            .sessions
            .get(&project)
            .is_some_and(|saved| saved.iter().any(|saved| saved.id == session));
        if !known {
            return Err(StoreError::Corrupt {
                path: PathBuf::from("sessions.json"),
                detail: "unknown session".into(),
            });
        }
        state
            .updates
            .entry((project, session))
            .or_default()
            .push(update.clone());
        Ok(())
    }

    fn load_records(
        &self,
        project: ProjectId,
        session: SessionId,
    ) -> Result<Vec<TranscriptRecord>, StoreError> {
        let mut state = locked(&self.state);
        state.update_loads += 1;
        if state.corrupt_updates || state.corrupt_sessions.contains(&session) {
            return Err(StoreError::Corrupt {
                path: PathBuf::from(format!("memory/{session}.jsonl")),
                detail: "simulated corruption".into(),
            });
        }
        Ok(state
            .updates
            .get(&(project, session))
            .cloned()
            .unwrap_or_default())
    }

    fn delete_records(&self, project: ProjectId, session: SessionId) -> Result<(), StoreError> {
        let mut state = locked(&self.state);
        if std::mem::take(&mut state.fail_next_delete) {
            return Err(simulated_failure(format!("memory/{session}.jsonl")));
        }
        state.updates.remove(&(project, session));
        Ok(())
    }

    fn attachment_dir(
        &self,
        _project: ProjectId,
        session: SessionId,
    ) -> Result<PathBuf, StoreError> {
        Ok(match &locked(&self.state).attachment_root {
            Some(root) => root.join(session.to_string()),
            None => PathBuf::from(format!("/memory/{session}/attachments")),
        })
    }

    fn load_settings(&self, project: ProjectId) -> Result<ProjectSettings, StoreError> {
        let mut state = locked(&self.state);
        state.settings_loads += 1;
        if state.failing_settings_loads > 0 {
            state.failing_settings_loads -= 1;
            return Err(simulated_failure(format!("memory/{project}/settings.toml")));
        }
        Ok(state
            .settings
            .get(&project)
            .unwrap_or(&state.default_settings)
            .clone())
    }
}

impl StoreQueue for MemoryStore {
    fn send(&self, command: StoreCommand) {
        let mut state = locked(&self.state);
        if let Some(held) = state.held.as_mut() {
            held.push_back(command);
            return;
        }
        drop(state);
        self.carry_out(&command);
    }
}

/// Feeds every event `store` reported into `app`; returns the errors
/// [`apply_store_event`](App::apply_store_event) reported, in order.
///
/// `store` is a clone of the [`MemoryStore`] the app was loaded with.
pub fn apply_store(app: &mut App, store: &MemoryStore) -> Vec<AppError> {
    store
        .take_events()
        .into_iter()
        .filter_map(|event| app.apply_store_event(event).err())
        .collect()
}

/// The error a [`MemoryStore`] told to fail reports for `path`.
fn simulated_failure(path: String) -> StoreError {
    StoreError::Io {
        path: PathBuf::from(path),
        source: std::io::Error::other("simulated failure"),
    }
}

/// A repository at `path` with `current` checked out in its only worktree, these local
/// `branches` (without upstreams) and `remote` branches such as `origin/main`.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use partitur_core::testing::repo_info;
///
/// let info = repo_info(Path::new("/p/api"), "main", &["main", "dev"], &["origin/main"]);
/// assert_eq!(info.current_branch(), Some("main"));
/// assert_eq!(info.fetch_remote("main"), Some("origin"));
/// ```
pub fn repo_info(path: &Path, current: &str, branches: &[&str], remote: &[&str]) -> RepoInfo {
    RepoInfo {
        worktrees: vec![WorktreeInfo {
            path: path.to_path_buf(),
            head: Head::Branch(current.to_owned()),
            prunable: false,
        }],
        branches: branches
            .iter()
            .map(|name| LocalBranch {
                name: (*name).to_owned(),
                upstream_remote: None,
            })
            .collect(),
        remote_branches: remote.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// One query or step received by a [`MemoryGit`], mirroring [`GitQuery`] and [`GitStep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCall {
    /// [`GitQuery::Inspect`].
    Inspect(PathBuf),
    /// [`GitStep::Fetch`].
    Fetch {
        /// The repository to fetch into.
        repository: PathBuf,
        /// The remote to fetch.
        remote: String,
    },
    /// [`GitStep::CreateBranch`].
    CreateBranch {
        /// The repository whose main checkout switches.
        repository: PathBuf,
        /// The new branch.
        branch: String,
        /// What it starts from.
        start: String,
    },
    /// [`GitStep::AddWorktree`].
    AddWorktree {
        /// The repository the worktree belongs to.
        repository: PathBuf,
        /// Where the new worktree goes.
        path: PathBuf,
        /// The new branch checked out in it.
        branch: String,
        /// What it starts from.
        start: String,
    },
    /// [`GitStep::RestoreHead`].
    RestoreHead {
        /// The repository whose main checkout switches.
        repository: PathBuf,
        /// The branch or commit to switch to.
        head: Head,
    },
    /// [`GitStep::RemoveWorktree`].
    RemoveWorktree {
        /// The repository the worktree belongs to.
        repository: PathBuf,
        /// The worktree to remove.
        path: PathBuf,
    },
    /// [`GitStep::DeleteBranch`].
    DeleteBranch {
        /// The repository the branch lives in.
        repository: PathBuf,
        /// The branch to delete.
        branch: String,
    },
    /// [`GitQuery::Changes`].
    Changes(PathBuf),
    /// [`GitQuery::IsMerged`].
    IsMerged {
        /// The repository both branches live in.
        repository: PathBuf,
        /// The branch that may be merged.
        branch: String,
        /// The branch it may be merged into.
        base: String,
    },
    /// [`GitQuery::Toplevel`].
    Toplevel(PathBuf),
}

impl From<&GitQuery> for GitCall {
    fn from(query: &GitQuery) -> Self {
        match query.clone() {
            GitQuery::Inspect(path) => Self::Inspect(path),
            GitQuery::Changes(path) => Self::Changes(path),
            GitQuery::IsMerged {
                repository,
                branch,
                base,
            } => Self::IsMerged {
                repository,
                branch,
                base,
            },
            GitQuery::Toplevel(path) => Self::Toplevel(path),
        }
    }
}

impl From<&GitStep> for GitCall {
    fn from(step: &GitStep) -> Self {
        match step.clone() {
            GitStep::Fetch { repository, remote } => Self::Fetch { repository, remote },
            GitStep::CreateBranch {
                repository,
                branch,
                start,
            } => Self::CreateBranch {
                repository,
                branch,
                start,
            },
            GitStep::AddWorktree {
                repository,
                path,
                branch,
                start,
            } => Self::AddWorktree {
                repository,
                path,
                branch,
                start,
            },
            GitStep::RestoreHead { repository, head } => Self::RestoreHead { repository, head },
            GitStep::RemoveWorktree { repository, path } => {
                Self::RemoveWorktree { repository, path }
            }
            GitStep::DeleteBranch { repository, branch } => {
                Self::DeleteBranch { repository, branch }
            }
        }
    }
}

/// The kind of a query or step, for [`MemoryGit::fail_next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitOp {
    /// [`GitQuery::Inspect`].
    Inspect,
    /// [`GitQuery::Changes`].
    Changes,
    /// [`GitQuery::IsMerged`].
    IsMerged,
    /// [`GitQuery::Toplevel`].
    Toplevel,
    /// [`GitStep::Fetch`].
    Fetch,
    /// [`GitStep::CreateBranch`].
    CreateBranch,
    /// [`GitStep::AddWorktree`].
    AddWorktree,
    /// [`GitStep::RestoreHead`].
    RestoreHead,
    /// [`GitStep::RemoveWorktree`].
    RemoveWorktree,
    /// [`GitStep::DeleteBranch`].
    DeleteBranch,
}

impl GitOp {
    /// The git subcommand a failure of this kind names.
    fn command(self) -> &'static str {
        match self {
            Self::Inspect => "worktree list",
            Self::Changes => "status",
            Self::IsMerged => "merge-base",
            Self::Toplevel => "rev-parse",
            Self::Fetch => "fetch",
            Self::CreateBranch | Self::RestoreHead => "switch",
            Self::AddWorktree => "worktree add",
            Self::RemoveWorktree => "worktree remove",
            Self::DeleteBranch => "branch",
        }
    }
}

/// A request a [`MemoryGit`] queued and has not answered yet.
#[derive(Debug)]
enum GitRequest {
    Query(GitRequestId, Vec<GitQuery>),
    Run(GitRequestId, Vec<GitStep>),
}

#[derive(Debug, Default)]
struct GitModel {
    repositories: HashMap<PathBuf, RepoInfo>,
    changes: HashMap<PathBuf, usize>,
    toplevels: HashMap<PathBuf, PathBuf>,
    merged: HashMap<(PathBuf, String), bool>,
    failures: Vec<(GitOp, PathBuf)>,
    refuse_next: bool,
    calls: Vec<GitCall>,
    waiting: VecDeque<GitRequest>,
}

impl GitModel {
    /// Takes a failure planned for `op` in `repository`, if one is planned.
    fn take_failure(&mut self, op: GitOp, repository: &Path) -> Result<(), GitError> {
        let planned = self
            .failures
            .iter()
            .position(|(failing, path)| *failing == op && path == repository);
        match planned {
            Some(index) => {
                self.failures.remove(index);
                Err(GitError::Failed {
                    command: op.command().to_owned(),
                    stderr: "simulated failure".into(),
                })
            }
            None => Ok(()),
        }
    }

    /// Takes the planned refusal, if one is planned.
    fn refuse(&mut self) -> Result<(), GitError> {
        if std::mem::take(&mut self.refuse_next) {
            Err(GitError::Unavailable(
                "memory git refused the request".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn answer(&mut self, query: &GitQuery) -> Result<GitAnswer, GitError> {
        match query {
            GitQuery::Inspect(path) => {
                self.take_failure(GitOp::Inspect, path)?;
                self.repositories
                    .get(path)
                    .cloned()
                    .map(GitAnswer::Repo)
                    .ok_or_else(|| GitError::Failed {
                        command: GitOp::Inspect.command().to_owned(),
                        stderr: "not a git repository".into(),
                    })
            }
            GitQuery::Changes(path) => {
                self.take_failure(GitOp::Changes, path)?;
                Ok(GitAnswer::Changes(
                    self.changes.get(path).copied().unwrap_or_default(),
                ))
            }
            GitQuery::IsMerged {
                repository, branch, ..
            } => {
                self.take_failure(GitOp::IsMerged, repository)?;
                let key = (repository.clone(), branch.clone());
                Ok(GitAnswer::Merged(
                    self.merged.get(&key).copied().unwrap_or_default(),
                ))
            }
            GitQuery::Toplevel(path) => {
                self.take_failure(GitOp::Toplevel, path)?;
                Ok(GitAnswer::Folder(
                    self.toplevels
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| path.clone()),
                ))
            }
        }
    }

    fn run(&mut self, step: &GitStep) -> Result<(), GitError> {
        let op = match step {
            GitStep::Fetch { .. } => GitOp::Fetch,
            GitStep::CreateBranch { .. } => GitOp::CreateBranch,
            GitStep::AddWorktree { .. } => GitOp::AddWorktree,
            GitStep::RestoreHead { .. } => GitOp::RestoreHead,
            GitStep::RemoveWorktree { .. } => GitOp::RemoveWorktree,
            GitStep::DeleteBranch { .. } => GitOp::DeleteBranch,
        };
        self.take_failure(op, step.repository())?;
        let failed = |stderr: String| GitError::Failed {
            command: op.command().to_owned(),
            stderr,
        };
        let Some(info) = self.repositories.get_mut(step.repository()) else {
            return Err(failed("not a git repository".into()));
        };
        refusal(info, step).map_or(Ok(()), |stderr| Err(failed(stderr)))?;
        match step {
            GitStep::Fetch { .. } => {}
            GitStep::CreateBranch { branch, .. } => {
                info.branches.push(new_branch(branch));
                set_main_head(info, Head::Branch(branch.clone()));
            }
            GitStep::AddWorktree { path, branch, .. } => {
                info.branches.push(new_branch(branch));
                info.worktrees.push(WorktreeInfo {
                    path: path.clone(),
                    head: Head::Branch(branch.clone()),
                    prunable: false,
                });
            }
            GitStep::RestoreHead { head, .. } => set_main_head(info, head.clone()),
            GitStep::RemoveWorktree { path, .. } => {
                info.worktrees.retain(|worktree| worktree.path != *path);
            }
            GitStep::DeleteBranch { branch, .. } => {
                info.branches.retain(|local| local.name != *branch);
            }
        }
        Ok(())
    }

    fn respond(&mut self, request: GitRequest) -> GitEvent {
        match request {
            GitRequest::Query(request, queries) => GitEvent::Answered {
                request,
                answers: queries.iter().map(|query| self.answer(query)).collect(),
            },
            GitRequest::Run(request, steps) => {
                let mut done = 0;
                let mut result = Ok(());
                for step in &steps {
                    result = self.run(step);
                    if result.is_err() {
                        break;
                    }
                    done += 1;
                }
                GitEvent::Ran {
                    request,
                    done,
                    result,
                }
            }
        }
    }
}

/// What git says when `step` cannot run in the repository `info` describes, `None` when it
/// can.
fn refusal(info: &RepoInfo, step: &GitStep) -> Option<String> {
    let used_by = |branch: &str, worktrees: &[WorktreeInfo]| {
        worktrees
            .iter()
            .find(|worktree| matches!(&worktree.head, Head::Branch(name) if name == branch))
            .map(|worktree| worktree.path.display().to_string())
    };
    match step {
        GitStep::CreateBranch { branch, .. } | GitStep::AddWorktree { branch, .. } => info
            .has_branch(branch)
            .then(|| format!("a branch named '{branch}' already exists")),
        GitStep::RestoreHead {
            head: Head::Branch(branch),
            ..
        } => {
            if info.has_branch(branch) {
                let others = info.worktrees.get(1..).unwrap_or_default();
                used_by(branch, others)
                    .map(|path| format!("'{branch}' is already used by worktree at '{path}'"))
            } else {
                Some(format!("invalid reference: {branch}"))
            }
        }
        GitStep::Fetch { .. } | GitStep::RestoreHead { .. } => None,
        GitStep::RemoveWorktree { path, .. } => {
            let position = info
                .worktrees
                .iter()
                .position(|worktree| worktree.path == *path);
            match position {
                Some(0) => Some(format!("'{}' is a main working tree", path.display())),
                Some(_) => None,
                None => Some(format!("'{}' is not a working tree", path.display())),
            }
        }
        GitStep::DeleteBranch { branch, .. } => {
            if info.has_branch(branch) {
                used_by(branch, &info.worktrees).map(|path| {
                    format!("cannot delete branch '{branch}' used by worktree at '{path}'")
                })
            } else {
                Some(format!("branch '{branch}' not found"))
            }
        }
    }
}

/// A branch made with `--no-track`, so without an upstream.
fn new_branch(name: &str) -> LocalBranch {
    LocalBranch {
        name: name.to_owned(),
        upstream_remote: None,
    }
}

fn set_main_head(info: &mut RepoInfo, head: Head) {
    if let Some(main) = info.worktrees.first_mut() {
        main.head = head;
    }
}

/// A [`Git`] that queues every request and answers it against an in-memory model of the
/// repositories only when [`answer`](Self::answer) is called.
///
/// Clones share the same model and queue.
#[derive(Debug, Clone, Default)]
pub struct MemoryGit {
    state: Arc<Mutex<GitModel>>,
}

impl MemoryGit {
    /// Creates a fake that knows no repository.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the repository at `path` known, as `info` describes it.
    pub fn add_repository(&self, path: impl Into<PathBuf>, info: RepoInfo) {
        locked(&self.state).repositories.insert(path.into(), info);
    }

    /// The repository at `path` as the model has it now, `None` when it is unknown.
    pub fn repository(&self, path: &Path) -> Option<RepoInfo> {
        locked(&self.state).repositories.get(path).cloned()
    }

    /// Every query and step, recorded when it is queued (before it is answered).
    ///
    /// Steps are recorded when queued, so this includes steps that did not run because an
    /// earlier step of their request failed.
    pub fn calls(&self) -> Vec<GitCall> {
        locked(&self.state).calls.clone()
    }

    /// Requests queued and not answered yet.
    pub fn waiting(&self) -> usize {
        locked(&self.state).waiting.len()
    }

    /// Runs every waiting request against the model, in queue order, and returns their events.
    pub fn answer(&self) -> Vec<GitEvent> {
        let mut state = locked(&self.state);
        let waiting = std::mem::take(&mut state.waiting);
        waiting
            .into_iter()
            .map(|request| state.respond(request))
            .collect()
    }

    /// Makes the next `op` in `repository` fail with [`GitError::Failed`] without changing the
    /// model; in a [`run`](Git::run) the later steps then do not run.
    pub fn fail_next(&self, op: GitOp, repository: &Path) {
        locked(&self.state)
            .failures
            .push((op, repository.to_path_buf()));
    }

    /// The next `query` or `run` returns `Err(GitError::Unavailable)` and queues nothing.
    pub fn refuse_next_request(&self) {
        locked(&self.state).refuse_next = true;
    }

    /// Makes [`GitQuery::Changes`] for `worktree` answer `count`; unset, it answers 0.
    pub fn set_changes(&self, worktree: &Path, count: usize) {
        locked(&self.state)
            .changes
            .insert(worktree.to_path_buf(), count);
    }

    /// Makes [`GitQuery::Toplevel`] for `folder` answer `top`, as for a folder reached through
    /// a symlink; unset, it answers `folder` itself.
    pub fn set_toplevel(&self, folder: &Path, top: &Path) {
        locked(&self.state)
            .toplevels
            .insert(folder.to_path_buf(), top.to_path_buf());
    }

    /// Makes [`GitQuery::IsMerged`] for `branch` in `repository` answer `merged`, whatever
    /// the base; unset, it answers `false`.
    pub fn set_merged(&self, repository: &Path, branch: &str, merged: bool) {
        locked(&self.state)
            .merged
            .insert((repository.to_path_buf(), branch.to_owned()), merged);
    }
}

impl Git for MemoryGit {
    fn query(&self, request: GitRequestId, queries: Vec<GitQuery>) -> Result<(), GitError> {
        let mut state = locked(&self.state);
        state.refuse()?;
        state.calls.extend(queries.iter().map(GitCall::from));
        state.waiting.push_back(GitRequest::Query(request, queries));
        Ok(())
    }

    fn run(&self, request: GitRequestId, steps: Vec<GitStep>) -> Result<(), GitError> {
        let mut state = locked(&self.state);
        state.refuse()?;
        state.calls.extend(steps.iter().map(GitCall::from));
        state.waiting.push_back(GitRequest::Run(request, steps));
        Ok(())
    }
}

/// Feeds every answer `git` gives into `app` until nothing waits (undo requests included);
/// returns the errors [`apply_git_event`](App::apply_git_event) reported, in order.
///
/// `git` is a clone of the [`MemoryGit`] the app was loaded with, so the requests the app
/// queues while it applies an answer are answered in the next round.
pub fn settle(app: &mut App, git: &MemoryGit) -> Vec<AppError> {
    let mut errors = Vec::new();
    loop {
        let events = git.answer();
        if events.is_empty() {
            return errors;
        }
        errors.extend(
            events
                .into_iter()
                .filter_map(|event| app.apply_git_event(event).err()),
        );
    }
}

/// A [`TranscriptReader`] that queues every read until [`answer`](Self::answer) is called,
/// then answers it from its [`MemoryStore`].
///
/// Clones share the same queue.
#[derive(Clone, Default)]
pub struct MemoryReader {
    store: MemoryStore,
    waiting: Arc<Mutex<Vec<(ProjectId, SessionId, ReadDone)>>>,
}

impl MemoryReader {
    /// A reader with nothing queued that answers from `store`, a clone of the app's store.
    #[must_use]
    pub fn new(store: MemoryStore) -> Self {
        Self {
            store,
            waiting: Arc::default(),
        }
    }

    /// Reads queued and not answered yet.
    #[must_use]
    pub fn waiting(&self) -> usize {
        locked(&self.waiting).len()
    }

    /// Answers every queued read, in queue order, with what the store's
    /// [`load_records`](Store::load_records) gives for its session now.
    pub fn answer(&self) {
        let waiting = std::mem::take(&mut *locked(&self.waiting));
        for (project, session, done) in waiting {
            done(self.store.load_records(project, session));
        }
    }
}

impl TranscriptReader for MemoryReader {
    fn read(&self, project: ProjectId, session: SessionId, done: ReadDone) {
        locked(&self.waiting).push((project, session, done));
    }
}

/// A [`ToolReply`] that records the answer it is given, for tests.
#[derive(Debug, Default)]
pub struct RecordingReply {
    state: Mutex<ReplyState>,
}

#[derive(Debug, Default)]
struct ReplyState {
    answers: Vec<ToolAnswer>,
    gave_up: bool,
}

impl RecordingReply {
    /// A reply whose caller is waiting and that has recorded nothing.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The answers recorded so far: at most one.
    #[must_use]
    pub fn answers(&self) -> Vec<ToolAnswer> {
        locked(&self.state).answers.clone()
    }

    /// Makes the caller stop waiting.
    pub fn give_up(&self) {
        locked(&self.state).gave_up = true;
    }
}

impl ToolReply for RecordingReply {
    fn is_waiting(&self) -> bool {
        !locked(&self.state).gave_up
    }

    fn send(&self, answer: ToolAnswer) {
        let mut state = locked(&self.state);
        if state.answers.is_empty() {
            state.answers.push(answer);
        }
    }
}

/// One command received by a [`RecordingWidgetHost`], mirroring the [`WidgetHost`] methods.
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetCall {
    /// [`WidgetHost::open`].
    Open {
        /// The view.
        view: ViewId,
        /// The page to show.
        page: WidgetPage,
    },
    /// [`WidgetHost::post`].
    Post {
        /// The view.
        view: ViewId,
        /// The message, as JSON text.
        json: String,
    },
    /// [`WidgetHost::resize`].
    Resize {
        /// The view.
        view: ViewId,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// The device pixel ratio.
        scale: f32,
    },
    /// [`WidgetHost::set_visible`].
    SetVisible {
        /// The view.
        view: ViewId,
        /// Whether it is shown.
        visible: bool,
    },
    /// [`WidgetHost::input`].
    Input {
        /// The view.
        view: ViewId,
        /// The pointer event.
        input: WidgetInput,
    },
    /// [`WidgetHost::close`].
    Close {
        /// The view.
        view: ViewId,
    },
}

#[derive(Debug, Default)]
struct WidgetRecording {
    calls: Vec<WidgetCall>,
    /// The error a command fails with, after how many more succeed.
    fail_next: Option<(usize, WidgetHostError)>,
}

/// A [`WidgetHost`] that records every command instead of running it.
///
/// Clones share the same recording.
#[derive(Debug, Clone, Default)]
pub struct RecordingWidgetHost {
    state: Arc<Mutex<WidgetRecording>>,
}

impl RecordingWidgetHost {
    /// Creates a host with no recorded calls.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every command received so far, oldest first, including ones that were made to fail.
    pub fn calls(&self) -> Vec<WidgetCall> {
        locked(&self.state).calls.clone()
    }

    /// Makes the next command return [`WidgetHostError::Unavailable`]; later ones succeed again.
    pub fn fail_next_call(&self) {
        self.fail_next_call_with(WidgetHostError::Unavailable("recording widget host".into()));
    }

    /// Makes the next command return `error`; later ones succeed again.
    pub fn fail_next_call_with(&self, error: WidgetHostError) {
        locked(&self.state).fail_next = Some((0, error));
    }

    /// Lets the next `calls` commands succeed and makes the one after them return
    /// [`WidgetHostError::Unavailable`]; later ones succeed again.
    pub fn fail_call_after(&self, calls: usize) {
        let error = WidgetHostError::Unavailable("recording widget host".into());
        locked(&self.state).fail_next = Some((calls, error));
    }

    fn record(&self, call: WidgetCall) -> Result<(), WidgetHostError> {
        let mut state = locked(&self.state);
        state.calls.push(call);
        match state.fail_next.take() {
            Some((0, error)) => Err(error),
            Some((left, error)) => {
                state.fail_next = Some((left - 1, error));
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl WidgetHost for RecordingWidgetHost {
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::Open { view, page })
    }

    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::Post { view, json })
    }

    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::Resize {
            view,
            width,
            height,
            scale,
        })
    }

    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::SetVisible { view, visible })
    }

    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::Input { view, input })
    }

    fn close(&self, view: ViewId) -> Result<(), WidgetHostError> {
        self.record(WidgetCall::Close { view })
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

    use super::*;
    use crate::session::Role;
    use crate::session::{Scope, Status, Workspace};
    use crate::settings::Level;

    #[test]
    fn a_fixed_clock_shows_what_was_set_and_advanced_through_any_clone() {
        let clock = FixedClock::new(10);
        let shared = clock.clone();
        assert_eq!(clock.now(), 10);

        shared.advance(5);
        assert_eq!(clock.now(), 15);

        shared.set(3);
        assert_eq!(clock.now(), 3);
    }

    fn session(project: ProjectId) -> Session {
        Session {
            id: SessionId::new(),
            project,
            scope: Scope::Project,
            cwd: PathBuf::from("/work"),
            title: "t".into(),
            backend_session: None,
            backend: BackendId::trusted("claude"),
            model: None,
            effort: None,
            status: Status::Idle,
            role: Role::Plain,
            auto_allow: false,
            instructions_injected: false,
            created_at: 1,
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
            seen_branch: None,
            resolved_cwd: None,
        }
    }

    #[test]
    fn the_recording_backend_records_every_command_in_order() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        let backend_session = BackendSessionId::new("b1");
        let option = ConfigId::new("model");
        let value = ConfigValueId::new("fast");
        let claude = BackendId::trusted("claude");
        let work = BackendId::trusted("work");

        backend
            .new_session(
                RequestId(1),
                session.clone(),
                &claude,
                Path::new("/a"),
                ToolRole::Orchestrator,
            )
            .unwrap();
        backend
            .load_session(
                RequestId(2),
                session.clone(),
                &work,
                &backend_session,
                Path::new("/b"),
                ToolRole::Plain,
            )
            .unwrap();
        backend
            .prompt(session.clone(), vec![PromptContent::Text("hi".into())])
            .unwrap();
        backend.cancel(session.clone()).unwrap();
        backend.close_session(session.clone()).unwrap();
        backend
            .set_config(session.clone(), &option, &value)
            .unwrap();
        backend
            .respond_permission(session.clone(), PermissionId(7), None)
            .unwrap();

        assert_eq!(
            backend.calls(),
            vec![
                Call::NewSession {
                    request: RequestId(1),
                    session: session.clone(),
                    backend: claude,
                    cwd: PathBuf::from("/a"),
                    role: ToolRole::Orchestrator,
                },
                Call::LoadSession {
                    request: RequestId(2),
                    session: session.clone(),
                    backend: work,
                    backend_session,
                    cwd: PathBuf::from("/b"),
                    role: ToolRole::Plain,
                },
                Call::Prompt {
                    session: session.clone(),
                    content: vec![PromptContent::Text("hi".into())],
                },
                Call::Cancel {
                    session: session.clone()
                },
                Call::CloseSession {
                    session: session.clone()
                },
                Call::SetConfig {
                    session: session.clone(),
                    option,
                    value,
                },
                Call::RespondPermission {
                    session,
                    permission: PermissionId(7),
                    option: None,
                },
            ]
        );
    }

    #[test]
    fn only_a_new_session_call_converts_to_its_parts() {
        let session = SessionId::new();
        let new = Call::NewSession {
            request: RequestId(4),
            session: session.clone(),
            backend: BackendId::trusted("claude"),
            cwd: PathBuf::from("/a"),
            role: ToolRole::Plain,
        };

        assert_eq!(
            new.as_new_session(),
            Some((RequestId(4), &session, Path::new("/a")))
        );
        assert_eq!(Call::Cancel { session }.as_new_session(), None);
    }

    #[test]
    fn a_recording_backend_clone_shares_the_recording() {
        let backend = RecordingBackend::new();
        let clone = backend.clone();

        clone.cancel(SessionId::new()).unwrap();

        assert_eq!(backend.calls().len(), 1);
    }

    #[test]
    fn the_recording_backend_fails_exactly_the_next_call() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        backend.fail_next_call();

        let failed = backend.cancel(session.clone());
        let retried = backend.cancel(session);

        assert!(matches!(failed, Err(BackendError::Unavailable(_))));
        assert!(retried.is_ok());
        assert_eq!(backend.calls().len(), 2);
    }

    #[test]
    fn the_recording_backend_fails_the_next_call_with_the_given_error() {
        let backend = RecordingBackend::new();
        let session = SessionId::new();
        backend.fail_next_call_with(BackendError::UnknownSession(session.clone()));

        let failed = backend.cancel(session.clone());

        assert_eq!(failed, Err(BackendError::UnknownSession(session.clone())));
        assert!(backend.cancel(session).is_ok());
    }

    #[test]
    fn a_held_store_carries_out_queued_commands_in_order_on_release() {
        let store = MemoryStore::held();
        let saved = session(ProjectId::new());
        let project = saved.project.clone();
        let record = TranscriptRecord::Notice("n".into());
        store.send(StoreCommand::SaveSessions {
            project: project.clone(),
            sessions: vec![saved.clone()],
        });
        store.send(StoreCommand::Append {
            project: project.clone(),
            session: saved.id.clone(),
            record: record.clone(),
        });
        store.send(StoreCommand::Load {
            request: crate::store::LoadId(4),
            project: project.clone(),
            session: saved.id.clone(),
        });

        assert_eq!(store.held_commands(), 3);
        assert!(store.load_sessions(project.clone()).unwrap().is_empty());
        assert_eq!(store.take_events(), []);
        store.release();

        assert_eq!(store.held_commands(), 0);
        assert_eq!(
            store.take_events(),
            [StoreEvent::Loaded {
                request: crate::store::LoadId(4),
                session: saved.id.clone(),
                records: Ok(vec![record]),
            }]
        );
        assert_eq!(store.take_events(), []);
        store.send(StoreCommand::Delete {
            project: project.clone(),
            session: saved.id.clone(),
        });
        assert_eq!(store.held_commands(), 1);
    }

    #[test]
    fn an_immediate_store_reports_failed_commands_at_once() {
        let store = MemoryStore::immediate();
        let project = ProjectId::new();
        store.fail_next_session_save();
        let save = StoreCommand::SaveSessions {
            project: project.clone(),
            sessions: Vec::new(),
        };
        store.send(save.clone());
        store.fail_next_delete();
        let session = SessionId::new();
        let delete = StoreCommand::Delete {
            project: project.clone(),
            session: session.clone(),
        };
        store.send(delete.clone());
        store.send(StoreCommand::SaveProjects(Vec::new()));
        store.set_corrupt_updates(true);
        store.send(StoreCommand::Load {
            request: crate::store::LoadId(1),
            project,
            session: SessionId::new(),
        });

        assert_eq!(store.held_commands(), 0);
        let events = store.take_events();
        let failed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                StoreEvent::Failed {
                    command, session, ..
                } => Some((*command, session.clone())),
                StoreEvent::Loaded { .. } => None,
            })
            .collect();
        assert_eq!(
            failed,
            [("save sessions", None), ("delete", Some(session.clone()))]
        );
        assert!(
            matches!(
                events.as_slice(),
                [
                    _,
                    _,
                    StoreEvent::Loaded {
                        records: Err(_),
                        ..
                    }
                ]
            ),
            "{events:?}"
        );
    }

    #[test]
    fn the_memory_store_returns_nothing_before_anything_is_saved() {
        let store = MemoryStore::new();

        assert!(store.load_projects().unwrap().is_empty());
        assert!(store.load_sessions(ProjectId::new()).unwrap().is_empty());
        assert!(
            store
                .load_records(ProjectId::new(), SessionId::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_memory_store_replaces_saved_lists_and_appends_updates() {
        let store = MemoryStore::new();
        let project = Project {
            id: ProjectId::new(),
            root: PathBuf::from("/p"),
            name: "p".into(),
            repositories: Vec::new(),
            instruction_files: Vec::new(),
            closed: false,
            fold: crate::Fold::Open,
        };
        let mine = session(project.id.clone());
        let other = session(ProjectId::new());
        store.save_projects(std::slice::from_ref(&project)).unwrap();
        store
            .save_sessions(project.id.clone(), &[mine.clone(), mine.clone()])
            .unwrap();
        store
            .save_sessions(project.id.clone(), std::slice::from_ref(&mine))
            .unwrap();
        store
            .append_record(
                mine.project.clone(),
                mine.id.clone(),
                &TranscriptRecord::UserText("a".into()),
            )
            .unwrap();
        store
            .append_record(
                mine.project.clone(),
                mine.id.clone(),
                &TranscriptRecord::AgentText("b".into()),
            )
            .unwrap();

        assert_eq!(store.load_projects().unwrap(), vec![project.clone()]);
        assert_eq!(store.load_sessions(project.id).unwrap(), vec![mine.clone()]);
        assert!(
            store
                .load_sessions(other.project.clone())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .load_records(mine.project.clone(), mine.id.clone())
                .unwrap(),
            vec![
                TranscriptRecord::UserText("a".into()),
                TranscriptRecord::AgentText("b".into())
            ]
        );
        assert!(
            store
                .load_records(other.project.clone(), other.id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .load_records(other.project, mine.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_memory_store_tells_whether_any_project_holds_records_of_a_session() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let mine = session(project.clone());
        store
            .save_sessions(project.clone(), std::slice::from_ref(&mine))
            .unwrap();
        assert!(!store.has_records(&mine.id));

        store
            .append_record(
                project.clone(),
                mine.id.clone(),
                &TranscriptRecord::Notice("x".into()),
            )
            .unwrap();
        assert!(store.has_records(&mine.id));
        assert!(!store.has_records(&SessionId::new()));

        store.delete_records(project, mine.id.clone()).unwrap();
        assert!(!store.has_records(&mine.id));
    }

    #[test]
    fn the_memory_store_finds_the_project_of_a_saved_session() {
        let store = MemoryStore::new();
        let (project, elsewhere) = (ProjectId::new(), ProjectId::new());
        let (mine, other) = (session(project.clone()), session(elsewhere.clone()));
        store
            .save_sessions(project.clone(), std::slice::from_ref(&mine))
            .unwrap();
        store
            .save_sessions(elsewhere.clone(), std::slice::from_ref(&other))
            .unwrap();

        assert_eq!(store.project_of(&mine.id), Some(project));
        assert_eq!(store.project_of(&other.id), Some(elsewhere));
        assert_eq!(store.project_of(&SessionId::new()), None);
    }

    #[test]
    fn the_memory_store_rejects_updates_for_a_session_it_never_saved() {
        let store = MemoryStore::new();
        let (project, unknown) = (ProjectId::new(), SessionId::new());
        store.save_sessions(project.clone(), &[]).unwrap();

        let result = store.append_record(
            project.clone(),
            unknown.clone(),
            &TranscriptRecord::Notice("x".into()),
        );

        assert!(matches!(
            result,
            Err(StoreError::Corrupt { ref path, ref detail })
                if *path == Path::new("sessions.json") && detail == "unknown session"
        ));
        assert!(store.load_records(project, unknown).unwrap().is_empty());
    }

    #[test]
    fn the_memory_store_counts_update_loads_and_can_simulate_corruption() {
        let store = MemoryStore::new();
        let (project, session) = (ProjectId::new(), SessionId::new());
        assert_eq!(store.update_loads(), 0);

        store
            .load_records(project.clone(), session.clone())
            .unwrap();
        store.set_corrupt_updates(true);
        let corrupt = store.load_records(project.clone(), session.clone());
        store.set_corrupt_updates(false);
        let healed = store.load_records(project, session);

        assert!(matches!(corrupt, Err(StoreError::Corrupt { .. })));
        assert!(healed.is_ok());
        assert_eq!(store.update_loads(), 3);
    }

    #[test]
    fn the_memory_store_can_simulate_the_corruption_of_one_session() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let (broken, other) = (SessionId::new(), SessionId::new());

        store.set_corrupt_updates_of(broken.clone());

        assert!(matches!(
            store.load_records(project.clone(), broken),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(store.load_records(project, other).is_ok());
    }

    #[test]
    fn the_memory_store_counts_session_saves_per_project() {
        let store = MemoryStore::new();
        let saved = ProjectId::new();
        let untouched = ProjectId::new();

        store.save_sessions(saved.clone(), &[]).unwrap();
        store.save_sessions(saved.clone(), &[]).unwrap();

        assert_eq!(store.session_saves(&saved), 2);
        assert_eq!(store.session_saves(&untouched), 0);
    }

    #[test]
    fn the_memory_store_deletes_a_log_and_takes_one_never_written() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let mine = session(project.clone());
        let other = session(project.clone());
        store
            .save_sessions(project, &[mine.clone(), other.clone()])
            .unwrap();
        store
            .append_record(
                mine.project.clone(),
                mine.id.clone(),
                &TranscriptRecord::UserText("a".into()),
            )
            .unwrap();
        store
            .append_record(
                other.project.clone(),
                other.id.clone(),
                &TranscriptRecord::UserText("b".into()),
            )
            .unwrap();

        store
            .delete_records(mine.project.clone(), mine.id.clone())
            .unwrap();
        store
            .delete_records(mine.project.clone(), SessionId::new())
            .unwrap();

        assert!(
            store
                .load_records(mine.project.clone(), mine.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.load_records(other.project.clone(), other.id).unwrap(),
            [TranscriptRecord::UserText("b".into())]
        );
    }

    #[test]
    fn the_memory_store_fails_exactly_the_next_delete() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let mine = session(project.clone());
        store
            .save_sessions(project, std::slice::from_ref(&mine))
            .unwrap();
        store
            .append_record(
                mine.project.clone(),
                mine.id.clone(),
                &TranscriptRecord::UserText("a".into()),
            )
            .unwrap();
        store.fail_next_delete();

        let failed = store.delete_records(mine.project.clone(), mine.id.clone());

        assert!(matches!(failed, Err(StoreError::Io { .. })));
        assert_eq!(
            store
                .load_records(mine.project.clone(), mine.id.clone())
                .unwrap()
                .len(),
            1
        );
        store
            .delete_records(mine.project.clone(), mine.id.clone())
            .unwrap();
        assert!(
            store
                .load_records(mine.project.clone(), mine.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_memory_store_fails_exactly_the_next_session_save() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let mine = session(project.clone());
        store.fail_next_session_save();

        let failed = store.save_sessions(project.clone(), std::slice::from_ref(&mine));

        assert!(matches!(failed, Err(StoreError::Io { .. })));
        assert!(store.load_sessions(project.clone()).unwrap().is_empty());
        assert_eq!(store.session_saves(&project), 1);
        store
            .save_sessions(project.clone(), std::slice::from_ref(&mine))
            .unwrap();
        assert_eq!(store.load_sessions(project).unwrap(), [mine]);
    }

    #[test]
    fn the_memory_store_returns_set_settings_and_fails_exactly_the_next_load() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        assert_eq!(
            store.load_settings(project.clone()).unwrap(),
            ProjectSettings::default()
        );
        let mut settings = ProjectSettings::default();
        settings
            .project
            .set(Level::Project, "worktree_path", "/wt/{branch}")
            .unwrap();
        store.set_settings(project.clone(), settings.clone());
        store.fail_next_settings_load();

        let failed = store.load_settings(project.clone());

        assert!(matches!(failed, Err(StoreError::Io { .. })));
        assert_eq!(store.load_settings(project).unwrap(), settings);
        assert_eq!(
            store.load_settings(ProjectId::new()).unwrap(),
            ProjectSettings::default()
        );
        assert_eq!(store.settings_loads(), 4);
        store.set_default_settings(settings.clone());
        assert_eq!(store.load_settings(ProjectId::new()).unwrap(), settings);
    }

    #[test]
    fn a_memory_store_clone_shares_the_data() {
        let store = MemoryStore::new();
        let clone = store.clone();
        let project = ProjectId::new();

        clone
            .save_sessions(project.clone(), &[session(project.clone())])
            .unwrap();

        assert_eq!(store.load_sessions(project).unwrap().len(), 1);
    }

    fn api() -> PathBuf {
        PathBuf::from("/p/api")
    }

    fn add_worktree(repository: &Path, branch: &str) -> GitStep {
        GitStep::AddWorktree {
            repository: repository.to_path_buf(),
            path: PathBuf::from(format!("/wt/{branch}")),
            branch: branch.into(),
            start: "main".into(),
        }
    }

    fn git_with_api() -> MemoryGit {
        let git = MemoryGit::new();
        git.add_repository(
            api(),
            repo_info(&api(), "main", &["main"], &["origin/main"]),
        );
        git
    }

    fn simulated(command: &str) -> GitError {
        GitError::Failed {
            command: command.into(),
            stderr: "simulated failure".into(),
        }
    }

    fn ran(request: u64, done: usize, result: Result<(), GitError>) -> GitEvent {
        GitEvent::Ran {
            request: GitRequestId(request),
            done,
            result,
        }
    }

    #[test]
    fn repo_info_describes_a_main_checkout_with_its_branches() {
        let info = repo_info(Path::new("/r"), "dev", &["main", "dev"], &["origin/main"]);

        assert_eq!(
            info,
            RepoInfo {
                worktrees: vec![WorktreeInfo {
                    path: PathBuf::from("/r"),
                    head: Head::Branch("dev".into()),
                    prunable: false,
                }],
                branches: vec![
                    LocalBranch {
                        name: "main".into(),
                        upstream_remote: None,
                    },
                    LocalBranch {
                        name: "dev".into(),
                        upstream_remote: None,
                    },
                ],
                remote_branches: vec!["origin/main".into()],
            }
        );
    }

    #[test]
    fn memory_git_queues_without_answering() {
        let git = git_with_api();
        let before = git.repository(&api()).unwrap();

        git.query(GitRequestId(1), vec![GitQuery::Inspect(api())])
            .unwrap();
        assert_eq!(git.calls(), [GitCall::Inspect(api())]);
        assert_eq!(git.waiting(), 1);
        git.run(GitRequestId(2), vec![add_worktree(&api(), "b")])
            .unwrap();
        assert_eq!(git.waiting(), 2);
        assert_eq!(git.repository(&api()).unwrap(), before);

        let events = git.answer();

        assert_eq!(
            events,
            [
                GitEvent::Answered {
                    request: GitRequestId(1),
                    answers: vec![Ok(GitAnswer::Repo(before))],
                },
                GitEvent::Ran {
                    request: GitRequestId(2),
                    done: 1,
                    result: Ok(()),
                },
            ]
        );
        assert_eq!(git.waiting(), 0);
        assert!(git.answer().is_empty());
        let after = git.repository(&api()).unwrap();
        assert_eq!(after.checked_out_at("b"), Some(Path::new("/wt/b")));
        assert!(after.has_branch("b"));
        assert_eq!(after.current_branch(), Some("main"));
    }

    #[test]
    fn memory_git_records_every_query_and_step_as_it_is_queued() {
        let git = MemoryGit::new();
        let head = Head::Detached("abc".into());

        git.query(
            GitRequestId(1),
            vec![
                GitQuery::Inspect(api()),
                GitQuery::Changes("/wt/b".into()),
                GitQuery::IsMerged {
                    repository: api(),
                    branch: "b".into(),
                    base: "main".into(),
                },
            ],
        )
        .unwrap();
        git.run(
            GitRequestId(2),
            vec![
                GitStep::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
                GitStep::CreateBranch {
                    repository: api(),
                    branch: "c".into(),
                    start: "main".into(),
                },
                add_worktree(&api(), "b"),
                GitStep::RestoreHead {
                    repository: api(),
                    head: head.clone(),
                },
                GitStep::RemoveWorktree {
                    repository: api(),
                    path: "/wt/b".into(),
                },
                GitStep::DeleteBranch {
                    repository: api(),
                    branch: "b".into(),
                },
            ],
        )
        .unwrap();

        assert_eq!(
            git.calls(),
            [
                GitCall::Inspect(api()),
                GitCall::Changes("/wt/b".into()),
                GitCall::IsMerged {
                    repository: api(),
                    branch: "b".into(),
                    base: "main".into(),
                },
                GitCall::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
                GitCall::CreateBranch {
                    repository: api(),
                    branch: "c".into(),
                    start: "main".into(),
                },
                GitCall::AddWorktree {
                    repository: api(),
                    path: "/wt/b".into(),
                    branch: "b".into(),
                    start: "main".into(),
                },
                GitCall::RestoreHead {
                    repository: api(),
                    head,
                },
                GitCall::RemoveWorktree {
                    repository: api(),
                    path: "/wt/b".into(),
                },
                GitCall::DeleteBranch {
                    repository: api(),
                    branch: "b".into(),
                },
            ]
        );
    }

    #[test]
    fn memory_git_keeps_its_model_consistent_when_steps_run() {
        let git = git_with_api();
        let detached = Head::Detached("abc".into());

        git.run(
            GitRequestId(1),
            vec![
                GitStep::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
                GitStep::CreateBranch {
                    repository: api(),
                    branch: "c".into(),
                    start: "origin/main".into(),
                },
            ],
        )
        .unwrap();
        git.answer();
        let created = git.repository(&api()).unwrap();
        assert_eq!(created.current_branch(), Some("c"));
        assert_eq!(created.branch_names(), ["c", "main"]);

        git.run(
            GitRequestId(2),
            vec![
                add_worktree(&api(), "b"),
                GitStep::RestoreHead {
                    repository: api(),
                    head: detached.clone(),
                },
            ],
        )
        .unwrap();
        git.answer();
        let added = git.repository(&api()).unwrap();
        assert_eq!(added.head(), Some(&detached));
        assert_eq!(added.checked_out_at("b"), Some(Path::new("/wt/b")));

        git.run(
            GitRequestId(3),
            vec![
                GitStep::RemoveWorktree {
                    repository: api(),
                    path: "/wt/b".into(),
                },
                GitStep::DeleteBranch {
                    repository: api(),
                    branch: "b".into(),
                },
            ],
        )
        .unwrap();
        git.answer();
        let removed = git.repository(&api()).unwrap();
        assert_eq!(removed.worktrees.len(), 1);
        assert_eq!(removed.head(), Some(&detached));
        assert_eq!(removed.branch_names(), ["c", "main"]);
    }

    #[test]
    fn memory_git_runs_steps_in_order_and_stops_at_the_first_failure() {
        let git = git_with_api();
        git.fail_next(GitOp::AddWorktree, &api());

        git.run(
            GitRequestId(1),
            vec![
                GitStep::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
                add_worktree(&api(), "b"),
                GitStep::DeleteBranch {
                    repository: api(),
                    branch: "main".into(),
                },
            ],
        )
        .unwrap();

        assert_eq!(git.answer(), [ran(1, 1, Err(simulated("worktree add")))]);
        let after = git.repository(&api()).unwrap();
        assert!(after.has_branch("main"));
        assert!(!after.has_branch("b"));
        assert_eq!(after.worktrees.len(), 1);
    }

    #[test]
    fn memory_git_answers_every_query_even_after_a_failure() {
        let git = MemoryGit::new();
        let worktree = PathBuf::from("/wt/b");
        git.set_changes(&worktree, 3);

        git.query(
            GitRequestId(1),
            vec![
                GitQuery::Inspect("/nowhere".into()),
                GitQuery::Changes(worktree),
                GitQuery::Changes("/wt/clean".into()),
            ],
        )
        .unwrap();

        assert_eq!(
            git.answer(),
            [GitEvent::Answered {
                request: GitRequestId(1),
                answers: vec![
                    Err(GitError::Failed {
                        command: "worktree list".into(),
                        stderr: "not a git repository".into(),
                    }),
                    Ok(GitAnswer::Changes(3)),
                    Ok(GitAnswer::Changes(0)),
                ],
            }]
        );
    }

    #[test]
    fn memory_git_answers_whether_a_branch_is_merged() {
        let git = git_with_api();
        git.set_merged(&api(), "done", true);
        git.set_merged(&api(), "undone", false);
        let is_merged = |branch: &str| GitQuery::IsMerged {
            repository: api(),
            branch: branch.into(),
            base: "main".into(),
        };

        git.query(
            GitRequestId(1),
            vec![is_merged("done"), is_merged("undone"), is_merged("unknown")],
        )
        .unwrap();

        assert_eq!(
            git.answer(),
            [GitEvent::Answered {
                request: GitRequestId(1),
                answers: vec![
                    Ok(GitAnswer::Merged(true)),
                    Ok(GitAnswer::Merged(false)),
                    Ok(GitAnswer::Merged(false)),
                ],
            }]
        );
    }

    #[test]
    fn memory_git_fails_exactly_the_next_op_in_that_repository() {
        let git = git_with_api();
        let web = PathBuf::from("/p/web");
        git.add_repository(&web, repo_info(&web, "main", &["main"], &[]));
        git.fail_next(GitOp::AddWorktree, &api());

        for (request, repository) in [(1, &web), (2, &api()), (3, &api())] {
            git.run(GitRequestId(request), vec![add_worktree(repository, "b")])
                .unwrap();
        }

        assert_eq!(
            git.answer(),
            [
                ran(1, 1, Ok(())),
                ran(2, 0, Err(simulated("worktree add"))),
                ran(3, 1, Ok(())),
            ]
        );
        assert!(git.repository(&web).unwrap().has_branch("b"));
        assert!(git.repository(&api()).unwrap().has_branch("b"));
    }

    #[test]
    fn memory_git_fails_each_op_with_its_command_without_changing_the_model() {
        let git = git_with_api();
        let before = git.repository(&api()).unwrap();
        let cases = [
            (
                GitOp::Fetch,
                "fetch",
                GitStep::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
            ),
            (
                GitOp::CreateBranch,
                "switch",
                GitStep::CreateBranch {
                    repository: api(),
                    branch: "c".into(),
                    start: "main".into(),
                },
            ),
            (
                GitOp::AddWorktree,
                "worktree add",
                add_worktree(&api(), "b"),
            ),
            (
                GitOp::RestoreHead,
                "switch",
                GitStep::RestoreHead {
                    repository: api(),
                    head: Head::Detached("abc".into()),
                },
            ),
            (
                GitOp::RemoveWorktree,
                "worktree remove",
                GitStep::RemoveWorktree {
                    repository: api(),
                    path: api(),
                },
            ),
            (
                GitOp::DeleteBranch,
                "branch",
                GitStep::DeleteBranch {
                    repository: api(),
                    branch: "main".into(),
                },
            ),
        ];
        for (op, command, step) in cases {
            git.fail_next(op, &api());
            git.run(GitRequestId(1), vec![step]).unwrap();

            assert_eq!(
                git.answer(),
                [ran(1, 0, Err(simulated(command)))],
                "{command}"
            );
            assert_eq!(git.repository(&api()).unwrap(), before, "{command}");
        }
        let queries = [
            (GitOp::Inspect, "worktree list", GitQuery::Inspect(api())),
            (GitOp::Changes, "status", GitQuery::Changes(api())),
            (
                GitOp::IsMerged,
                "merge-base",
                GitQuery::IsMerged {
                    repository: api(),
                    branch: "b".into(),
                    base: "main".into(),
                },
            ),
        ];
        for (op, command, query) in queries {
            git.fail_next(op, &api());
            git.query(GitRequestId(2), vec![query]).unwrap();

            assert_eq!(
                git.answer(),
                [GitEvent::Answered {
                    request: GitRequestId(2),
                    answers: vec![Err(simulated(command))],
                }],
                "{command}"
            );
        }
    }

    #[test]
    fn memory_git_does_not_know_an_unadded_repository() {
        let git = MemoryGit::new();

        git.query(GitRequestId(1), vec![GitQuery::Inspect(api())])
            .unwrap();

        assert_eq!(git.repository(&api()), None);
        assert_eq!(
            git.answer(),
            [GitEvent::Answered {
                request: GitRequestId(1),
                answers: vec![Err(GitError::Failed {
                    command: "worktree list".into(),
                    stderr: "not a git repository".into(),
                })],
            }]
        );
    }

    #[test]
    fn memory_git_fails_steps_in_an_unadded_repository() {
        let git = MemoryGit::new();

        git.run(
            GitRequestId(1),
            vec![
                GitStep::Fetch {
                    repository: api(),
                    remote: "origin".into(),
                },
                add_worktree(&api(), "b"),
            ],
        )
        .unwrap();

        assert_eq!(
            git.answer(),
            [ran(
                1,
                0,
                Err(GitError::Failed {
                    command: "fetch".into(),
                    stderr: "not a git repository".into(),
                })
            )]
        );
        assert_eq!(git.repository(&api()), None);
    }

    /// Runs `step` alone in a fresh `api` repository that also has branch `b` checked out in
    /// the worktree `/wt/b`, and returns its events, asserting the model did not change.
    fn refused(step: GitStep) -> Vec<GitEvent> {
        let git = git_with_api();
        git.run(GitRequestId(1), vec![add_worktree(&api(), "b")])
            .unwrap();
        git.answer();
        let before = git.repository(&api()).unwrap();

        git.run(GitRequestId(2), vec![step]).unwrap();
        let events = git.answer();

        assert_eq!(git.repository(&api()).unwrap(), before);
        events
    }

    /// The events of a request 2 whose only step git refused with `stderr`.
    fn failed(command: &str, stderr: &str) -> [GitEvent; 1] {
        [ran(
            2,
            0,
            Err(GitError::Failed {
                command: command.into(),
                stderr: stderr.into(),
            }),
        )]
    }

    #[test]
    fn memory_git_refuses_to_create_a_branch_that_exists() {
        let events = refused(GitStep::CreateBranch {
            repository: api(),
            branch: "main".into(),
            start: "main".into(),
        });

        assert_eq!(
            events,
            failed("switch", "a branch named 'main' already exists")
        );
    }

    #[test]
    fn memory_git_refuses_to_add_a_worktree_for_a_branch_that_exists() {
        let mut step = add_worktree(&api(), "main");
        if let GitStep::AddWorktree { path, .. } = &mut step {
            *path = PathBuf::from("/wt/other");
        }

        assert_eq!(
            refused(step),
            failed("worktree add", "a branch named 'main' already exists")
        );
    }

    #[test]
    fn memory_git_refuses_to_delete_a_branch_checked_out_in_a_worktree() {
        let events = refused(GitStep::DeleteBranch {
            repository: api(),
            branch: "b".into(),
        });

        assert_eq!(
            events,
            failed(
                "branch",
                "cannot delete branch 'b' used by worktree at '/wt/b'"
            )
        );
    }

    #[test]
    fn memory_git_refuses_to_delete_the_branch_of_the_main_checkout() {
        let events = refused(GitStep::DeleteBranch {
            repository: api(),
            branch: "main".into(),
        });

        assert_eq!(
            events,
            failed(
                "branch",
                "cannot delete branch 'main' used by worktree at '/p/api'"
            )
        );
    }

    #[test]
    fn memory_git_refuses_to_delete_a_missing_branch() {
        let events = refused(GitStep::DeleteBranch {
            repository: api(),
            branch: "gone".into(),
        });

        assert_eq!(events, failed("branch", "branch 'gone' not found"));
    }

    #[test]
    fn memory_git_refuses_to_remove_the_main_checkout() {
        let events = refused(GitStep::RemoveWorktree {
            repository: api(),
            path: api(),
        });

        assert_eq!(
            events,
            failed("worktree remove", "'/p/api' is a main working tree")
        );
    }

    #[test]
    fn memory_git_refuses_to_remove_a_path_that_is_no_worktree() {
        let events = refused(GitStep::RemoveWorktree {
            repository: api(),
            path: "/wt/none".into(),
        });

        assert_eq!(
            events,
            failed("worktree remove", "'/wt/none' is not a working tree")
        );
    }

    #[test]
    fn memory_git_refuses_to_restore_a_missing_branch() {
        let events = refused(GitStep::RestoreHead {
            repository: api(),
            head: Head::Branch("gone".into()),
        });

        assert_eq!(events, failed("switch", "invalid reference: gone"));
    }

    #[test]
    fn memory_git_refuses_to_restore_a_branch_checked_out_in_another_worktree() {
        let events = refused(GitStep::RestoreHead {
            repository: api(),
            head: Head::Branch("b".into()),
        });

        assert_eq!(
            events,
            failed("switch", "'b' is already used by worktree at '/wt/b'")
        );
    }

    #[test]
    fn memory_git_restores_the_branch_the_main_checkout_already_has() {
        let git = git_with_api();

        git.run(
            GitRequestId(1),
            vec![GitStep::RestoreHead {
                repository: api(),
                head: Head::Branch("main".into()),
            }],
        )
        .unwrap();

        assert_eq!(git.answer(), [ran(1, 1, Ok(()))]);
        assert_eq!(
            git.repository(&api()).unwrap().current_branch(),
            Some("main")
        );
    }

    #[test]
    fn memory_git_can_refuse_to_queue() {
        let git = git_with_api();
        git.refuse_next_request();

        let refused = git.query(GitRequestId(1), vec![GitQuery::Inspect(api())]);

        assert!(matches!(refused, Err(GitError::Unavailable(_))));
        assert_eq!(git.waiting(), 0);
        assert!(git.calls().is_empty());
        git.refuse_next_request();
        assert!(matches!(
            git.run(GitRequestId(2), vec![add_worktree(&api(), "b")]),
            Err(GitError::Unavailable(_))
        ));
        assert_eq!(git.waiting(), 0);
        git.run(GitRequestId(3), vec![add_worktree(&api(), "b")])
            .unwrap();
        assert_eq!(git.waiting(), 1);
    }

    #[test]
    fn a_memory_git_clone_shares_the_model() {
        let git = MemoryGit::default();
        let clone = git.clone();

        clone.add_repository(api(), repo_info(&api(), "main", &["main"], &[]));
        clone
            .query(GitRequestId(1), vec![GitQuery::Inspect(api())])
            .unwrap();

        assert!(git.repository(&api()).is_some());
        assert_eq!(git.waiting(), 1);
    }

    #[test]
    fn a_memory_store_names_the_attachment_folder_under_its_root_once_it_has_one() {
        let store = MemoryStore::new();
        let session = SessionId::new();
        assert_eq!(
            store
                .attachment_dir(ProjectId::new(), session.clone())
                .unwrap(),
            PathBuf::from(format!("/memory/{session}/attachments"))
        );

        store.set_attachment_root("/root");

        assert_eq!(
            store
                .attachment_dir(ProjectId::new(), session.clone())
                .unwrap(),
            PathBuf::from(format!("/root/{session}"))
        );
    }

    #[test]
    fn a_memory_reader_answers_its_queued_reads_from_the_store_when_told() {
        let store = MemoryStore::new();
        let project = ProjectId::new();
        let stored = session(project.clone());
        let session = stored.id.clone();
        store.save_sessions(project.clone(), &[stored]).unwrap();
        let reader = MemoryReader::new(store.clone());
        let clone = reader.clone();
        let answers = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&answers);

        clone.read(
            project.clone(),
            session.clone(),
            Box::new(move |result| locked(&kept).push(result.unwrap())),
        );
        store
            .append_record(
                project,
                session.clone(),
                &TranscriptRecord::AgentText("hi".into()),
            )
            .unwrap();

        assert_eq!(reader.waiting(), 1);
        assert!(locked(&answers).is_empty());
        assert_eq!(store.update_loads(), 0);
        reader.answer();
        assert_eq!(reader.waiting(), 0);
        assert_eq!(
            *locked(&answers),
            [vec![TranscriptRecord::AgentText("hi".into())]]
        );
        reader.answer();
        assert_eq!(store.update_loads(), 1);
    }
}
