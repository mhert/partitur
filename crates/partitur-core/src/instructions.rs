//! Resolution of `AGENTS.md` instruction files and their injection into a prompt.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
#[derive(Debug, Clone, PartialEq)]
pub struct InstructionChain {
    /// The files that were read: global first, then repository.
    pub files: Vec<InstructionFile>,
    /// The files that exist but could not be read or are not UTF-8.
    pub skipped: Vec<SkippedInstruction>,
}

/// Reads the `AGENTS.md` files that apply to `repository` within `project`: the project root's
/// (global) first, then the repository's.
///
/// A missing file is simply absent. A file that cannot be read, or is not UTF-8, is listed in
/// [`InstructionChain::skipped`]. When `repository` is the project root itself its single file
/// is listed once, with scope [`InstructionScope::Global`]. The files are always probed, not
/// taken from `Project::has_global_instructions` or `Repository::has_instructions`, so a file
/// created after discovery is still found.
#[must_use]
pub fn instruction_chain(project: &Project, repository: Option<&Repository>) -> InstructionChain {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut load = |path: PathBuf, scope| match read_instructions(&path) {
        Ok(Some(content)) => files.push(InstructionFile {
            path,
            scope,
            content,
        }),
        Ok(None) => {}
        Err(reason) => skipped.push(SkippedInstruction {
            path,
            scope,
            reason,
        }),
    };

    load(
        project.root.join(INSTRUCTIONS_FILE),
        InstructionScope::Global,
    );
    if let Some(repository) = repository.filter(|repository| !repository.is_root) {
        load(
            repository.path.join(INSTRUCTIONS_FILE),
            InstructionScope::Repository,
        );
    }
    InstructionChain { files, skipped }
}

/// Reads `path` as text: `Ok(None)` when it does not exist, `Err` with the reason when it
/// exists but cannot be used.
fn read_instructions(path: &Path) -> Result<Option<String>, String> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Puts the chain's files, each in an `<instructions>` block, before `prompt`.
///
/// Every file becomes one block, with trailing whitespace of its content trimmed and one blank
/// line after it. An empty chain returns `prompt` unchanged.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use partitur_core::{InstructionChain, InstructionFile, InstructionScope, inject};
///
/// let chain = InstructionChain {
///     files: vec![InstructionFile {
///         path: PathBuf::from("/work/AGENTS.md"),
///         scope: InstructionScope::Global,
///         content: "Be brief.\n".to_owned(),
///     }],
///     skipped: Vec::new(),
/// };
///
/// assert_eq!(
///     inject(&chain, "Fix the build."),
///     "<instructions source=\"/work/AGENTS.md\" scope=\"global\">\nBe brief.\n</instructions>\n\nFix the build."
/// );
/// ```
#[must_use]
pub fn inject(chain: &InstructionChain, prompt: &str) -> String {
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
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::project::discover_project;

    /// A project folder with a repository `api`, discovered from disk.
    fn project_with_repository() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("api/.git")).unwrap();
        let project = discover_project(dir.path()).unwrap();
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
    fn the_chain_is_global_then_repository() {
        let (dir, project) = project_with_repository();
        fs::write(dir.path().join("AGENTS.md"), "global rules").unwrap();
        fs::write(dir.path().join("api/AGENTS.md"), "api rules").unwrap();

        let chain = instruction_chain(&project, project.repositories.first());

        assert_eq!(
            chain.files,
            [
                file(
                    &dir.path().join("AGENTS.md"),
                    InstructionScope::Global,
                    "global rules"
                ),
                file(
                    &dir.path().join("api/AGENTS.md"),
                    InstructionScope::Repository,
                    "api rules"
                ),
            ]
        );
        assert!(chain.skipped.is_empty());
    }

    #[test]
    fn without_a_repository_only_the_global_file_applies() {
        let (dir, project) = project_with_repository();
        fs::write(dir.path().join("AGENTS.md"), "global rules").unwrap();
        fs::write(dir.path().join("api/AGENTS.md"), "api rules").unwrap();

        let chain = instruction_chain(&project, None);

        assert_eq!(
            chain.files,
            [file(
                &dir.path().join("AGENTS.md"),
                InstructionScope::Global,
                "global rules"
            )]
        );
    }

    #[test]
    fn a_file_created_after_discovery_is_still_picked_up() {
        let (dir, project) = project_with_repository();
        assert!(!project.has_global_instructions);
        fs::write(dir.path().join("AGENTS.md"), "late rules").unwrap();

        let chain = instruction_chain(&project, None);

        assert_eq!(chain.files.len(), 1);
    }

    #[test]
    fn a_missing_file_is_simply_absent() {
        let (dir, project) = project_with_repository();
        fs::write(dir.path().join("api/AGENTS.md"), "api rules").unwrap();

        let chain = instruction_chain(&project, project.repositories.first());

        assert_eq!(
            chain.files,
            [file(
                &dir.path().join("api/AGENTS.md"),
                InstructionScope::Repository,
                "api rules"
            )]
        );
        assert!(chain.skipped.is_empty());

        fs::remove_file(dir.path().join("api/AGENTS.md")).unwrap();
        let chain = instruction_chain(&project, project.repositories.first());
        assert!(chain.files.is_empty());
        assert!(chain.skipped.is_empty());
    }

    #[test]
    fn an_unreadable_instruction_file_is_skipped_and_named() {
        let (dir, project) = project_with_repository();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        let chain = instruction_chain(&project, None);

        assert!(chain.files.is_empty());
        assert_eq!(chain.skipped.len(), 1);
        assert_eq!(chain.skipped[0].path, path);
        assert_eq!(chain.skipped[0].scope, InstructionScope::Global);
        assert!(chain.skipped[0].reason.contains("utf-8"));
    }

    #[test]
    fn an_instruction_file_that_cannot_be_read_is_skipped_with_the_io_error() {
        let (dir, project) = project_with_repository();
        let path = dir.path().join("api/AGENTS.md");
        fs::create_dir(&path).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "global rules").unwrap();

        let chain = instruction_chain(&project, project.repositories.first());

        assert_eq!(chain.files.len(), 1);
        assert_eq!(chain.skipped.len(), 1);
        assert_eq!(chain.skipped[0].path, path);
        assert_eq!(chain.skipped[0].scope, InstructionScope::Repository);
        assert!(!chain.skipped[0].reason.is_empty());
    }

    #[test]
    fn a_root_repository_injects_its_agents_md_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        let project = discover_project(dir.path()).unwrap();
        assert!(project.repositories[0].is_root);

        let chain = instruction_chain(&project, project.repositories.first());

        assert_eq!(
            chain.files,
            [file(
                &dir.path().join("AGENTS.md"),
                InstructionScope::Global,
                "rules"
            )]
        );
        assert!(chain.skipped.is_empty());
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
