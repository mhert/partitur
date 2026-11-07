//! Plain data the UI renders, built by [`App`](crate::App) so views stay free of logic.

use std::cmp::Reverse;
use std::path::PathBuf;

use crate::backend::{ConfigCategory, ConfigChoice, ConfigOption};
use crate::ids::{BackendId, BackendInfo, ConfigValueId, ProjectId, RepositoryId, SessionId};
use crate::instructions::{InstructionChain, InstructionScope};
use crate::project::Project;
use crate::session::{Scope, Session, Status};
use crate::transcript::{Entry, Transcript};

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
    /// The id of the backend the session runs on, which may no longer be configured.
    pub backend: String,
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

/// Everything the session view shows for the selected session.
///
/// The entries are borrowed from the session's transcript, so building the view copies none
/// of them; [`revision`](Self::revision) tells the view when they changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionViewModel<'a> {
    /// The session's identity.
    pub id: SessionId,
    /// The project's name, the parent session's title when there is one, and the title.
    pub breadcrumb: Vec<String>,
    /// Where the session stands.
    pub status: Status,
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
    /// Whether a turn is in progress, running or waiting for a permission answer, so the
    /// view offers Stop instead of Send and takes no prompt.
    pub running: bool,
    /// The configured backends the session can be switched to, sorted by id (the order the
    /// app was loaded with).
    pub backends: Vec<BackendInfo>,
    /// The backend the session runs on, which may be one no longer in
    /// [`backends`](Self::backends).
    pub backend: BackendId,
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
    /// first (children nested under their parent) and its repositories as discovered.
    pub(crate) fn build(projects: &[Project], sessions: &[Session]) -> Self {
        Self {
            projects: projects
                .iter()
                .filter(|project| !project.closed)
                .map(|project| project_node(project, sessions))
                .collect(),
        }
    }
}

impl<'a> SessionViewModel<'a> {
    /// Builds the view of `session`, which belongs to `project`, from its transcript, the
    /// config options its backend reported and the configured `backends`.
    pub(crate) fn build(
        project: &Project,
        sessions: &[Session],
        session: &Session,
        transcript: &'a Transcript,
        config: &[ConfigOption],
        backends: &[BackendInfo],
    ) -> Self {
        Self {
            id: session.id.clone(),
            breadcrumb: breadcrumb(project, sessions, session),
            status: session.status,
            entries: transcript.entries(),
            revision: transcript.revision(),
            usage: transcript.usage(),
            model: choice_view(config, &ConfigCategory::Model),
            effort: choice_view(config, &ConfigCategory::Effort),
            auto_allow: session.auto_allow,
            running: matches!(session.status, Status::Running | Status::WaitingForYou),
            backends: backends.to_vec(),
            backend: session.backend.clone(),
        }
    }
}

impl EnvViewModel {
    /// Builds the env view of `session`, which belongs to `project`, from the instruction
    /// chain that applies to it now, the config options its backend reported and the
    /// configured `backends`.
    pub(crate) fn build(
        project: &Project,
        sessions: &[Session],
        session: &Session,
        chain: InstructionChain,
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
    let current_name = option
        .choices
        .iter()
        .find(|choice| choice.id == option.current)
        .map_or_else(|| option.current.to_string(), |choice| choice.name.clone());
    Some(ChoiceView {
        current: option.current.clone(),
        current_name,
        choices: option.choices.clone(),
    })
}

fn project_node(project: &Project, sessions: &[Session]) -> ProjectNode {
    ProjectNode {
        id: project.id.clone(),
        name: project.name.clone(),
        root: project.root.clone(),
        has_global_instructions: project.has_global_instructions,
        sessions: session_nodes(project, sessions, None),
        repositories: project
            .repositories
            .iter()
            .map(|repository| RepositoryNode {
                id: repository.id.clone(),
                name: repository.name.clone(),
                has_instructions: repository.has_instructions,
            })
            .collect(),
    }
}

/// The sessions of `project` whose parent is `parent`, newest first.
fn session_nodes(
    project: &Project,
    sessions: &[Session],
    parent: Option<&SessionId>,
) -> Vec<SessionNode> {
    // Walking the sessions backwards puts later-created ones first, which the stable sort
    // keeps for equal timestamps.
    let mut siblings: Vec<&Session> = sessions
        .iter()
        .rev()
        .filter(|session| session.project == project.id && session.parent.as_ref() == parent)
        .collect();
    siblings.sort_by_key(|session| Reverse(session.created_at));
    siblings
        .into_iter()
        .map(|session| SessionNode {
            id: session.id.clone(),
            title: session.title.clone(),
            status: session.status,
            scope_label: scope_label(project, &session.scope),
            backend: session.backend.to_string(),
            children: session_nodes(project, sessions, Some(&session.id)),
        })
        .collect()
}

/// The name of what a session works on; a repository that is gone falls back to its id.
fn scope_label(project: &Project, scope: &Scope) -> String {
    match scope {
        Scope::Project => project.name.clone(),
        Scope::Repository(id) => project
            .repositories
            .iter()
            .find(|repository| &repository.id == id)
            .map_or_else(|| id.to_string(), |repository| repository.name.clone()),
    }
}
