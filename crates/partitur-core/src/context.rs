//! What a session draws on from the file system when its first prompt goes out and when its
//! Env tab shows it, and how one read through the files port becomes a [`SessionContext`].
//!
//! [`context_reads`] names the files to read for a session: the `AGENTS.md` instruction files
//! that apply to it, and for an orchestrator also the `AGENTS.md` of every repository it
//! orchestrates and the configuration file of every workflow module in its working folder.
//! Each file is asked for with a [`FilesQuery::Read`], and [`context_from`] turns the answers
//! into the session's context: the instruction chain, the repositories' instruction texts and
//! the workflow modules' configurations. A file that is missing is absent from it; one that
//! could not be read is skipped from the chain with the reason and absent from the rest.

use std::path::{Path, PathBuf};

use crate::files::{FilesAnswer, FilesError, FilesQuery};
use crate::instructions::{
    InstructionChain, InstructionRead, InstructionScope, instruction_chain, instruction_paths,
};
use crate::orchestrator::{INSTRUCTIONS_FILE, orchestrated_repositories};
use crate::project::Project;
use crate::session::{Scope, Session};
use crate::workflow::ModuleId;

/// The files a session's first prompt and its Env tab draw on, as one read found them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionContext {
    /// The instruction files that apply to the session.
    pub(crate) chain: InstructionChain,
    /// The text of each orchestrated repository's `AGENTS.md`, by the repository's path; only
    /// an orchestrator's context has them.
    pub(crate) agents_md: Vec<(PathBuf, String)>,
    /// The text of each workflow module's configuration file in the session's working folder;
    /// only an orchestrator's context has them.
    pub(crate) workflow_config: Vec<(ModuleId, String)>,
}

impl SessionContext {
    /// The text of the `AGENTS.md` of the repository at `repository`, when it was read.
    pub(crate) fn agents_md_of(&self, repository: &Path) -> Option<&str> {
        self.agents_md
            .iter()
            .find(|(path, _)| path == repository)
            .map(|(_, text)| text.as_str())
    }

    /// The text of `module`'s configuration file, when it was read.
    pub(crate) fn config_of(&self, module: ModuleId) -> Option<&str> {
        self.workflow_config
            .iter()
            .find(|(read, _)| *read == module)
            .map(|(_, text)| text.as_str())
    }
}

/// What a file of a session's context is read for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wanted {
    /// An instruction file of the chain, applying with this scope.
    Instructions(InstructionScope),
    /// The `AGENTS.md` of the repository at this path, for an orchestrator's preamble.
    RepositoryNote(PathBuf),
    /// This workflow module's configuration file.
    WorkflowConfig(ModuleId),
}

/// The files to read for `session` of `project`, each with what it is read for, in the order
/// the context takes them.
#[must_use]
pub(crate) fn context_reads(project: &Project, session: &Session) -> Vec<(PathBuf, Wanted)> {
    let repository = match &session.scope {
        Scope::Project => None,
        Scope::Repository(id) => project
            .repositories
            .iter()
            .find(|repository| &repository.id == id),
    };
    let mut reads: Vec<(PathBuf, Wanted)> = instruction_paths(project, repository)
        .into_iter()
        .map(|(path, scope)| (path, Wanted::Instructions(scope)))
        .collect();
    if session.role.is_orchestrator() {
        reads.extend(
            orchestrated_repositories(project)
                .into_iter()
                .map(|repository| {
                    (
                        repository.path.join(INSTRUCTIONS_FILE),
                        Wanted::RepositoryNote(repository.path.clone()),
                    )
                }),
        );
        reads.extend(ModuleId::ALL.into_iter().filter_map(|module| {
            let file = module.config_file()?;
            Some((session.cwd.join(file), Wanted::WorkflowConfig(module)))
        }));
    }
    reads
}

/// The queries that read `reads`, one [`FilesQuery::Read`] per file, in order.
#[must_use]
pub(crate) fn context_queries(reads: &[(PathBuf, Wanted)]) -> Vec<FilesQuery> {
    reads
        .iter()
        .map(|(path, _)| FilesQuery::Read(path.clone()))
        .collect()
}

/// The context the `answers` to [`context_queries`] of `reads` make, one answer per read.
#[must_use]
pub(crate) fn context_from(
    reads: Vec<(PathBuf, Wanted)>,
    answers: Vec<Result<FilesAnswer, FilesError>>,
) -> SessionContext {
    let mut instructions = Vec::new();
    let mut context = SessionContext::default();
    for ((path, wanted), answer) in reads.into_iter().zip(answers) {
        let read = text(answer);
        match wanted {
            Wanted::Instructions(scope) => instructions.push((path, scope, read)),
            Wanted::RepositoryNote(repository) => {
                if let Ok(Some(text)) = read {
                    context.agents_md.push((repository, text));
                }
            }
            Wanted::WorkflowConfig(module) => {
                if let Ok(Some(text)) = read {
                    context.workflow_config.push((module, text));
                }
            }
        }
    }
    context.chain = instruction_chain(instructions);
    context
}

/// What one answer to a [`FilesQuery::Read`] found: the text, `None` for a missing file, or
/// why it could not be used, which for a failed read is the operating system's message.
fn text(answer: Result<FilesAnswer, FilesError>) -> InstructionRead {
    match answer {
        Ok(FilesAnswer::Read(text)) => Ok(text),
        Ok(_) => Err(UNEXPECTED_ANSWER.to_owned()),
        Err(FilesError::Io { message, .. }) => Err(message),
        Err(error) => Err(error.to_string()),
    }
}

/// Why a file is skipped whose read the files port answered as another query.
const UNEXPECTED_ANSWER: &str = "the files port answered another query";

#[cfg(test)]
mod tests {
    #![allow(clippy::disallowed_methods)]
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::files::ProjectListing;
    use crate::instructions::{InstructionFile, SkippedInstruction};
    use crate::testing::discovered;

    fn project_session(project: &Project, scope: Scope) -> Session {
        let mut session = crate::app::test_support::stored_session(project, "s", 1);
        session.scope = scope;
        session
    }

    fn orchestrated() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        for name in ["api", "web"] {
            std::fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        let project = discovered(dir.path());
        (dir, project)
    }

    #[test]
    fn a_plain_session_reads_only_its_instruction_files() {
        let (dir, project) = orchestrated();
        let api = project.repositories[0].id.clone();
        let session = project_session(&project, Scope::Repository(api));

        assert_eq!(
            context_reads(&project, &session),
            [
                (
                    dir.path().join("AGENTS.md"),
                    Wanted::Instructions(InstructionScope::Global)
                ),
                (
                    dir.path().join("api/AGENTS.md"),
                    Wanted::Instructions(InstructionScope::Repository)
                ),
            ]
        );
    }

    #[test]
    fn an_orchestrator_also_reads_its_repositories_and_workflow_configuration() {
        let (dir, project) = orchestrated();
        let mut session = project_session(&project, Scope::Project);
        session.role = crate::session::Role::Orchestrator;
        session.cwd = PathBuf::from("/cwd");

        let reads = context_reads(&project, &session);

        assert_eq!(
            reads,
            [
                (
                    dir.path().join("AGENTS.md"),
                    Wanted::Instructions(InstructionScope::Global)
                ),
                (
                    dir.path().join("api/AGENTS.md"),
                    Wanted::RepositoryNote(dir.path().join("api"))
                ),
                (
                    dir.path().join("web/AGENTS.md"),
                    Wanted::RepositoryNote(dir.path().join("web"))
                ),
                (
                    PathBuf::from("/cwd/_bmad/config.toml"),
                    Wanted::WorkflowConfig(ModuleId::BmadV6_12)
                ),
            ]
        );
        assert_eq!(
            context_queries(&reads)[3],
            FilesQuery::Read(PathBuf::from("/cwd/_bmad/config.toml"))
        );
    }

    #[test]
    fn the_answers_make_the_context() {
        let reads = vec![
            (
                PathBuf::from("/w/AGENTS.md"),
                Wanted::Instructions(InstructionScope::Global),
            ),
            (
                PathBuf::from("/w/x/AGENTS.md"),
                Wanted::Instructions(InstructionScope::Repository),
            ),
            (
                PathBuf::from("/w/api/AGENTS.md"),
                Wanted::RepositoryNote(PathBuf::from("/w/api")),
            ),
            (
                PathBuf::from("/w/web/AGENTS.md"),
                Wanted::RepositoryNote(PathBuf::from("/w/web")),
            ),
            (
                PathBuf::from("/w/_bmad/config.toml"),
                Wanted::WorkflowConfig(ModuleId::BmadV6_12),
            ),
        ];
        let answers = vec![
            Ok(FilesAnswer::Read(Some("rules".into()))),
            Err(FilesError::Io {
                path: PathBuf::from("/w/x/AGENTS.md"),
                message: "denied".into(),
            }),
            Ok(FilesAnswer::Read(Some("Keep it.".into()))),
            Ok(FilesAnswer::Read(None)),
            Ok(FilesAnswer::Read(Some("[core]".into()))),
        ];

        let context = context_from(reads, answers);

        assert_eq!(
            context.chain.files,
            [InstructionFile {
                path: PathBuf::from("/w/AGENTS.md"),
                scope: InstructionScope::Global,
                content: "rules".into(),
            }]
        );
        assert_eq!(
            context.chain.skipped,
            [SkippedInstruction {
                path: PathBuf::from("/w/x/AGENTS.md"),
                scope: InstructionScope::Repository,
                reason: "denied".into(),
            }]
        );
        assert_eq!(context.agents_md_of(Path::new("/w/api")), Some("Keep it."));
        assert_eq!(context.agents_md_of(Path::new("/w/web")), None);
        assert_eq!(context.config_of(ModuleId::BmadV6_12), Some("[core]"));
        assert_eq!(context.config_of(ModuleId::SuperpowersV7), None);
    }

    #[test]
    fn an_unusable_answer_is_skipped_or_absent() {
        let listing = FilesAnswer::Scanned(ProjectListing {
            root: PathBuf::from("/w"),
            entries: Vec::new(),
            children: Vec::new(),
        });
        let reads = vec![
            (
                PathBuf::from("/w/AGENTS.md"),
                Wanted::Instructions(InstructionScope::Global),
            ),
            (
                PathBuf::from("/w/x/AGENTS.md"),
                Wanted::Instructions(InstructionScope::Repository),
            ),
            (
                PathBuf::from("/w/api/AGENTS.md"),
                Wanted::RepositoryNote(PathBuf::from("/w/api")),
            ),
            (
                PathBuf::from("/w/_bmad/config.toml"),
                Wanted::WorkflowConfig(ModuleId::BmadV6_12),
            ),
        ];
        let answers = vec![
            Ok(listing),
            Err(FilesError::Unavailable("gone".into())),
            Err(FilesError::Unavailable("gone".into())),
            Err(FilesError::Unavailable("gone".into())),
        ];

        let context = context_from(reads, answers);

        assert!(context.chain.files.is_empty());
        let reasons: Vec<&str> = context
            .chain
            .skipped
            .iter()
            .map(|file| file.reason.as_str())
            .collect();
        assert_eq!(
            reasons,
            [UNEXPECTED_ANSWER, "the file system is unavailable: gone"]
        );
        assert!(context.agents_md.is_empty());
        assert!(context.workflow_config.is_empty());
    }
}
