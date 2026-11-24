//! The `AGENTS.md` instruction files that apply to a session, and their injection into a
//! prompt.
//!
//! [`instruction_paths`] names the files that apply: the project root's (global) first, then
//! the repository's. [`instruction_chain`] builds the chain from what reading them found,
//! through the files port: a missing file is simply absent, and a file that cannot be read or
//! is not UTF-8 is skipped with the reason. Nothing here reads the file system itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::project::{Project, Repository};

const INSTRUCTIONS_FILE: &str = "AGENTS.md";

/// Where an instruction file applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstructionScope {
    /// The `AGENTS.md` in the project root, shared by every repository.
    Global,
    /// The `AGENTS.md` in a single repository.
    Repository,
}

impl InstructionScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Repository => "repository",
        }
    }
}

/// An instruction file that was read.
#[derive(Debug, Clone, PartialEq)]
pub struct InstructionFile {
    /// The file's location.
    pub path: PathBuf,
    /// Where the file applies.
    pub scope: InstructionScope,
    /// The file's text.
    pub content: String,
}

/// An instruction file that exists but could not be used.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedInstruction {
    /// The file's location.
    pub path: PathBuf,
    /// Where the file would have applied.
    pub scope: InstructionScope,
    /// Why the file was skipped: the read or UTF-8 error's message.
    pub reason: String,
}

/// The instruction files that apply to a prompt, in injection order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct InstructionChain {
    /// The files that were read: global first, then repository.
    pub files: Vec<InstructionFile>,
    /// The files that exist but could not be read or are not UTF-8.
    pub skipped: Vec<SkippedInstruction>,
}

/// What reading one instruction file found: its text, `None` when it does not exist, or why
/// it could not be used.
pub(crate) type InstructionRead = Result<Option<String>, String>;

/// The `AGENTS.md` files that apply to `repository` within `project`, in injection order: the
/// project root's (global) first, then the repository's.
///
/// When `repository` is the project root itself its single file is named once, with scope
/// [`InstructionScope::Global`]. The files are always named, not taken from
/// `Project::instruction_files` or `Repository::instruction_files`, so a file created after
/// discovery is still found.
#[must_use]
pub(crate) fn instruction_paths(
    project: &Project,
    repository: Option<&Repository>,
) -> Vec<(PathBuf, InstructionScope)> {
    let mut paths = vec![(
        project.root.join(INSTRUCTIONS_FILE),
        InstructionScope::Global,
    )];
    if let Some(repository) = repository.filter(|repository| !repository.is_root) {
        paths.push((
            repository.path.join(INSTRUCTIONS_FILE),
            InstructionScope::Repository,
        ));
    }
    paths
}

/// The chain of the instruction files `reads` found, in their order: each file that was read
/// is in [`InstructionChain::files`], a missing one is simply absent, and one that could not
/// be used is in [`InstructionChain::skipped`] with the reason.
#[must_use]
pub(crate) fn instruction_chain(
    reads: Vec<(PathBuf, InstructionScope, InstructionRead)>,
) -> InstructionChain {
    let mut chain = InstructionChain::default();
    for (path, scope, read) in reads {
        match read {
            Ok(Some(content)) => chain.files.push(InstructionFile {
                path,
                scope,
                content,
            }),
            Ok(None) => {}
            Err(reason) => chain.skipped.push(SkippedInstruction {
                path,
                scope,
                reason,
            }),
        }
    }
    chain
}

/// Puts the chain's files, each in an `<instructions>` block, before `prompt`.
///
/// Every file becomes one block, with trailing whitespace of its content trimmed and one blank
/// line after it. An empty chain returns `prompt` unchanged.
#[must_use]
pub(crate) fn inject(chain: &InstructionChain, prompt: &str) -> String {
    let blocks: String = chain.files.iter().map(instruction_block).collect();
    format!("{blocks}{prompt}")
}

fn instruction_block(file: &InstructionFile) -> String {
    format!(
        "<instructions source=\"{}\" scope=\"{}\">\n{}\n</instructions>\n\n",
        file.path.display(),
        file.scope.as_str(),
        file.content.trim_end(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::disallowed_methods)]
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::testing::discovered;

    /// A project folder with a repository `api`, discovered from disk.
    fn project_with_repository() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("api/.git")).unwrap();
        let project = discovered(dir.path());
        (dir, project)
    }

    fn file(path: &Path, scope: InstructionScope, content: &str) -> InstructionFile {
        InstructionFile {
            path: path.to_path_buf(),
            scope,
            content: content.to_owned(),
        }
    }

    #[test]
    fn the_paths_are_global_then_repository() {
        let (dir, project) = project_with_repository();

        assert_eq!(
            instruction_paths(&project, project.repositories.first()),
            [
                (dir.path().join("AGENTS.md"), InstructionScope::Global),
                (
                    dir.path().join("api/AGENTS.md"),
                    InstructionScope::Repository
                ),
            ]
        );
    }

    #[test]
    fn without_a_repository_only_the_global_file_applies() {
        let (dir, project) = project_with_repository();

        assert_eq!(
            instruction_paths(&project, None),
            [(dir.path().join("AGENTS.md"), InstructionScope::Global)]
        );
    }

    #[test]
    fn a_file_unknown_at_discovery_is_still_named() {
        let (dir, project) = project_with_repository();
        assert!(project.instruction_files.is_empty());
        assert!(project.repositories[0].instruction_files.is_empty());

        assert_eq!(
            instruction_paths(&project, project.repositories.first())
                .into_iter()
                .map(|(path, _)| path)
                .collect::<Vec<_>>(),
            [
                dir.path().join("AGENTS.md"),
                dir.path().join("api/AGENTS.md")
            ]
        );
    }

    #[test]
    fn a_root_repository_names_its_agents_md_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let project = discovered(dir.path());
        assert!(project.repositories[0].is_root);

        assert_eq!(
            instruction_paths(&project, project.repositories.first()),
            [(dir.path().join("AGENTS.md"), InstructionScope::Global)]
        );
    }

    #[test]
    fn the_chain_holds_what_was_read_in_order() {
        let chain = instruction_chain(vec![
            (
                PathBuf::from("/work/AGENTS.md"),
                InstructionScope::Global,
                Ok(Some("global rules".into())),
            ),
            (
                PathBuf::from("/work/api/AGENTS.md"),
                InstructionScope::Repository,
                Ok(Some("api rules".into())),
            ),
        ]);

        assert_eq!(
            chain.files,
            [
                file(
                    Path::new("/work/AGENTS.md"),
                    InstructionScope::Global,
                    "global rules"
                ),
                file(
                    Path::new("/work/api/AGENTS.md"),
                    InstructionScope::Repository,
                    "api rules"
                ),
            ]
        );
        assert!(chain.skipped.is_empty());
    }

    #[test]
    fn a_missing_file_is_simply_absent() {
        let chain = instruction_chain(vec![
            (
                PathBuf::from("/work/AGENTS.md"),
                InstructionScope::Global,
                Ok(None),
            ),
            (
                PathBuf::from("/work/api/AGENTS.md"),
                InstructionScope::Repository,
                Ok(Some("api rules".into())),
            ),
        ]);

        assert_eq!(
            chain.files,
            [file(
                Path::new("/work/api/AGENTS.md"),
                InstructionScope::Repository,
                "api rules"
            )]
        );
        assert!(chain.skipped.is_empty());
    }

    #[test]
    fn a_file_that_could_not_be_used_is_skipped_with_the_reason() {
        let chain = instruction_chain(vec![
            (
                PathBuf::from("/work/AGENTS.md"),
                InstructionScope::Global,
                Ok(Some("global rules".into())),
            ),
            (
                PathBuf::from("/work/api/AGENTS.md"),
                InstructionScope::Repository,
                Err("invalid utf-8 sequence".into()),
            ),
        ]);

        assert_eq!(chain.files.len(), 1);
        assert_eq!(
            chain.skipped,
            [SkippedInstruction {
                path: PathBuf::from("/work/api/AGENTS.md"),
                scope: InstructionScope::Repository,
                reason: "invalid utf-8 sequence".into(),
            }]
        );
    }

    #[test]
    fn inject_with_an_empty_chain_returns_the_prompt_unchanged() {
        let chain = InstructionChain {
            files: Vec::new(),
            skipped: Vec::new(),
        };

        assert_eq!(inject(&chain, "Fix the build."), "Fix the build.");
    }

    #[test]
    fn inject_wraps_each_file_in_a_delimited_block_before_the_prompt() {
        let chain = InstructionChain {
            files: vec![
                file(
                    Path::new("/work/AGENTS.md"),
                    InstructionScope::Global,
                    "Be brief.\n\n",
                ),
                file(
                    Path::new("/work/api/AGENTS.md"),
                    InstructionScope::Repository,
                    "Use cargo.\nRun tests.  \n",
                ),
            ],
            skipped: Vec::new(),
        };

        assert_eq!(
            inject(&chain, "Fix the build."),
            "<instructions source=\"/work/AGENTS.md\" scope=\"global\">\n\
             Be brief.\n\
             </instructions>\n\
             \n\
             <instructions source=\"/work/api/AGENTS.md\" scope=\"repository\">\n\
             Use cargo.\nRun tests.\n\
             </instructions>\n\
             \n\
             Fix the build."
        );
    }

    #[test]
    fn scopes_serialize_as_lowercase_words() {
        assert_eq!(
            serde_json::to_string(&InstructionScope::Global).unwrap(),
            "\"global\""
        );
        assert_eq!(
            serde_json::from_str::<InstructionScope>("\"repository\"").unwrap(),
            InstructionScope::Repository
        );
    }
}
