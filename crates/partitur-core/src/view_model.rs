//! Plain data the UI renders, built by [`App`](crate::App) so views stay free of logic.

use std::cmp::Reverse;
use std::path::PathBuf;

use crate::backend::{ConfigCategory, ConfigChoice, ConfigOption};
use crate::ids::{BackendId, BackendInfo, ConfigValueId, ProjectId, RepositoryId, SessionId};
use crate::instructions::{InstructionChain, InstructionScope};
use crate::project::{Project, Repository};
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
    /// The project's repositories other than the project root itself, as discovered.
    pub repositories: Vec<RepositoryNode>,
}

/// One session in the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNode {
    /// The session's identity.
    pub id: SessionId,
    /// Display title.
    pub title: String,
    /// Where the session stands: the session's own status, except that a `Done` session with
    /// a session below it that shows `Running` or `WaitingForYou` shows `Running`, since the
    /// work is not over while its children still run.
    pub status: Status,
    /// Whether this session, or a session below it, is waiting for you.
    pub attention: bool,
    /// The name of the repository the session works in; empty for a session of the whole
    /// project.
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
    /// Whether a session below this one is waiting for you; this session's own status is in
    /// [`status`](Self::status).
    pub attention: bool,
    /// Whether the session is an orchestrator, which can propose children.
    pub orchestrator: bool,
    /// The project's repositories other than the project root itself, in project order, for
    /// adding a child session on the card.
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
    /// first (children nested under their parent) and its repositories other than the
    /// project root itself.
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
            attention: session_nodes(project, sessions, Some(&session.id))
                .iter()
                .any(|child| child.attention),
            orchestrator: session.orchestrator,
            repositories: project
                .repositories
                .iter()
                .filter(|repository| !repository.is_root)
                .map(repository_node)
                .collect(),
            entries: transcript.entries(),
            revision: transcript.revision(),
            usage: transcript.usage(),
            model: choice_view(config, &ConfigCategory::Model),
            effort: choice_view(config, &ConfigCategory::Effort),
            auto_allow: session.auto_allow,
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
            .filter(|repository| !repository.is_root)
            .map(repository_node)
            .collect(),
    }
}

fn repository_node(repository: &Repository) -> RepositoryNode {
    RepositoryNode {
        id: repository.id.clone(),
        name: repository.name.clone(),
        has_instructions: repository.has_instructions,
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
        .map(|session| {
            let children = session_nodes(project, sessions, Some(&session.id));
            let below_is_active = children
                .iter()
                .any(|child| matches!(child.status, Status::Running | Status::WaitingForYou));
            SessionNode {
                id: session.id.clone(),
                title: session.title.clone(),
                status: if session.status == Status::Done && below_is_active {
                    Status::Running
                } else {
                    session.status
                },
                attention: session.status == Status::WaitingForYou
                    || children.iter().any(|child| child.attention),
                scope_label: scope_label(project, &session.scope),
                backend: session.backend.to_string(),
                children,
            }
        })
        .collect()
}

/// The name of the repository a session works in, empty for a session of the whole project;
/// a repository that is gone falls back to its id.
fn scope_label(project: &Project, scope: &Scope) -> String {
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

    use super::*;
    use crate::backend::{ToolKind, Update};
    use crate::ids::{BackendSessionId, PermissionId, RepositoryId, ToolCallId};

    fn project() -> Project {
        Project {
            id: ProjectId::new(),
            root: PathBuf::from("/work"),
            name: "work".into(),
            repositories: Vec::new(),
            has_global_instructions: false,
            closed: false,
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
        }
    }

    fn repository(name: &str, is_root: bool) -> Repository {
        Repository {
            id: RepositoryId::try_from(name).unwrap(),
            path: PathBuf::from("/work").join(name),
            name: name.into(),
            has_instructions: false,
            is_root,
        }
    }

    fn roots(project: &Project, sessions: &[Session]) -> Vec<SessionNode> {
        session_nodes(project, sessions, None)
    }

    #[test]
    fn the_tree_leaves_out_the_root_repository() {
        let mut project = project();
        project.repositories = vec![repository("work", true), repository("api", false)];

        let tree = TreeModel::build(&[project], &[]);

        let names: Vec<&str> = tree.projects[0]
            .repositories
            .iter()
            .map(|repository| repository.name.as_str())
            .collect();
        assert_eq!(names, ["api"]);
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
        // Children are newest first: the sibling, then the child.
        let (sibling, child) = (&parent.children[0], &parent.children[1]);
        assert!(parent.attention);
        assert!(child.attention);
        assert!(child.children[0].attention);
        assert!(!sibling.attention);
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
    fn a_turn_is_in_progress_while_running_or_waiting() {
        let project = project();
        let request = Update::PermissionRequested {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Read main.rs".into(),
            kind: ToolKind::Read,
            options: Vec::new(),
            name: None,
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
            SessionViewModel::build(&project, &[], &session, transcript, &[], &[]).running()
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
