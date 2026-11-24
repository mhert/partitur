//! Discovery of a project folder and the git repositories inside it. A repository session
//! runs in the repository it names; a project session runs in the root folder, which is the
//! project's repository when the root itself holds `.git`.
//!
//! The project root and each repository list the agent instruction files found directly in
//! them, each by its file name, `AGENTS.md` before `CLAUDE.md`. Both are stored
//! in a shape v0.4.0 still reads: next to the list they write the `AGENTS.md` and `CLAUDE.md`
//! flags v0.4.0 reads instead, derived from the list, and a project saved by v0.4.0, which has
//! only the flags, loads with the list built from them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{ProjectId, RepositoryId};
use crate::session::{Fold, Scope};

const GIT_ENTRY: &str = ".git";
const AGENTS_MD: &str = "AGENTS.md";
const CLAUDE_MD: &str = "CLAUDE.md";
/// The agent instruction files a scan looks for, in the order a folder lists them.
const INSTRUCTION_FILES: [&str; 2] = [AGENTS_MD, CLAUDE_MD];

/// A folder the user opened, together with the repositories found in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ProjectWire", into = "ProjectWire")]
pub struct Project {
    /// Stable identity, kept across rescans.
    pub id: ProjectId,
    /// The folder the project was opened from.
    pub root: PathBuf,
    /// Display name, the root folder's name.
    pub name: String,
    /// The repositories in the project, sorted by name.
    pub repositories: Vec<Repository>,
    /// The names of the agent instruction files (`AGENTS.md`, `CLAUDE.md`) found directly in
    /// the project root, in that order.
    pub instruction_files: Vec<String>,
    /// Whether the user closed the project. A closed project stays saved, out of the tree,
    /// so opening its folder again restores it with its sessions. Project lists saved before
    /// this field existed load as open.
    pub closed: bool,
    /// Whether the project shows its sessions in the tree. Project lists saved before this
    /// field existed load as open.
    pub fold: Fold,
}

impl Project {
    /// The repository a session of `scope` runs in: the one it names, or for a project
    /// session the root folder when that is a repository itself. `None` when there is none.
    #[must_use]
    pub fn repository_of(&self, scope: &Scope) -> Option<&Repository> {
        self.repositories.iter().find(|repository| match scope {
            Scope::Repository(id) => &repository.id == id,
            Scope::Project => repository.is_root,
        })
    }
}

/// A git repository belonging to a [`Project`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RepositoryWire", into = "RepositoryWire")]
pub struct Repository {
    /// The repository's directory name.
    pub id: RepositoryId,
    /// The repository's working directory.
    pub path: PathBuf,
    /// Display name, the same as the directory name.
    pub name: String,
    /// The names of the agent instruction files (`AGENTS.md`, `CLAUDE.md`) found directly in
    /// the repository, in that order.
    pub instruction_files: Vec<String>,
    /// Whether the repository is the project root itself rather than a child folder.
    pub is_root: bool,
}

/// How a [`Project`] is stored: v0.4.0's fields, names, order and attributes, with
/// `instruction_files` added last.
///
/// Reading takes `instruction_files` when it is there, so it wins over the flags should a hand
/// edit make them disagree; v0.4.0 rewrites a project without the list, so its own writes
/// never do. Without the list, as v0.4.0 stores a project, the flags that are set name the
/// files. Writing derives the flags from the list.
#[derive(Serialize, Deserialize)]
struct ProjectWire {
    id: ProjectId,
    root: PathBuf,
    name: String,
    repositories: Vec<Repository>,
    has_global_instructions: bool,
    #[serde(default)]
    has_global_claude_md: bool,
    #[serde(default)]
    closed: bool,
    #[serde(default)]
    fold: Fold,
    #[serde(default)]
    instruction_files: Option<Vec<String>>,
}

impl From<ProjectWire> for Project {
    fn from(wire: ProjectWire) -> Self {
        Self {
            id: wire.id,
            root: wire.root,
            name: wire.name,
            repositories: wire.repositories,
            instruction_files: wire.instruction_files.unwrap_or_else(|| {
                files_from_flags(wire.has_global_instructions, wire.has_global_claude_md)
            }),
            closed: wire.closed,
            fold: wire.fold,
        }
    }
}

impl From<Project> for ProjectWire {
    fn from(project: Project) -> Self {
        Self {
            id: project.id,
            root: project.root,
            name: project.name,
            repositories: project.repositories,
            has_global_instructions: lists(&project.instruction_files, AGENTS_MD),
            has_global_claude_md: lists(&project.instruction_files, CLAUDE_MD),
            closed: project.closed,
            fold: project.fold,
            instruction_files: Some(project.instruction_files),
        }
    }
}

/// How a [`Repository`] is stored, in the manner of [`ProjectWire`].
#[derive(Serialize, Deserialize)]
struct RepositoryWire {
    id: RepositoryId,
    path: PathBuf,
    name: String,
    has_instructions: bool,
    #[serde(default)]
    has_claude_md: bool,
    is_root: bool,
    #[serde(default)]
    instruction_files: Option<Vec<String>>,
}

impl From<RepositoryWire> for Repository {
    fn from(wire: RepositoryWire) -> Self {
        Self {
            id: wire.id,
            path: wire.path,
            name: wire.name,
            instruction_files: wire
                .instruction_files
                .unwrap_or_else(|| files_from_flags(wire.has_instructions, wire.has_claude_md)),
            is_root: wire.is_root,
        }
    }
}

impl From<Repository> for RepositoryWire {
    fn from(repository: Repository) -> Self {
        Self {
            id: repository.id,
            path: repository.path,
            name: repository.name,
            has_instructions: lists(&repository.instruction_files, AGENTS_MD),
            has_claude_md: lists(&repository.instruction_files, CLAUDE_MD),
            is_root: repository.is_root,
            instruction_files: Some(repository.instruction_files),
        }
    }
}

/// The instruction files v0.4.0's flags name: `AGENTS.md` when `agents_md`, then `CLAUDE.md`
/// when `claude_md`.
fn files_from_flags(agents_md: bool, claude_md: bool) -> Vec<String> {
    [(agents_md, AGENTS_MD), (claude_md, CLAUDE_MD)]
        .into_iter()
        .filter(|(present, _)| *present)
        .map(|(_, name)| name.to_owned())
        .collect()
}

/// Whether `files` names `name`.
fn lists(files: &[String], name: &str) -> bool {
    files.iter().any(|file| file == name)
}

/// Why a project could not be discovered or rescanned.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProjectError {
    /// The project root does not exist or is not a directory.
    #[error("{} is not a directory", path.display())]
    NotADirectory {
        /// The path that was expected to be a directory.
        path: PathBuf,
    },
    /// Reading the file system failed.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The path being read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// Opens `root` as a project and finds its repositories.
///
/// A repository is a direct child directory containing `.git` (a directory, or a file as in a
/// linked worktree). When `root` itself contains `.git`, it is the project's single repository
/// and its children are not searched. Hidden child directories are skipped.
///
/// # Errors
///
/// Returns [`ProjectError::NotADirectory`] when `root` is not a directory and
/// [`ProjectError::Io`] when the root itself cannot be read (listing its entries, or checking
/// its `.git` and its instruction files). A child directory that cannot be inspected cannot be a
/// repository, so it is skipped instead of failing the whole project.
pub(crate) fn discover_project(root: &Path) -> Result<Project, ProjectError> {
    let scan = scan(root)?;
    Ok(Project {
        id: ProjectId::new(),
        root: root.to_path_buf(),
        name: display_name(root),
        repositories: scan.repositories,
        instruction_files: scan.instruction_files,
        closed: false,
        fold: Fold::Open,
    })
}

/// Recomputes the repositories and instruction files of `project` from the file system,
/// keeping its `id`, `root` and `name`.
///
/// # Errors
///
/// Returns the same errors as [`discover_project`]; the project is left unchanged on error.
pub(crate) fn rescan(project: &mut Project) -> Result<(), ProjectError> {
    let scan = scan(&project.root)?;
    project.repositories = scan.repositories;
    project.instruction_files = scan.instruction_files;
    Ok(())
}

struct Scan {
    repositories: Vec<Repository>,
    instruction_files: Vec<String>,
}

fn scan(root: &Path) -> Result<Scan, ProjectError> {
    if !root.is_dir() {
        return Err(ProjectError::NotADirectory {
            path: root.to_path_buf(),
        });
    }
    let repositories = if exists(&root.join(GIT_ENTRY))? {
        repository_at(root, true).into_iter().collect()
    } else {
        child_repositories(root)?
    };
    let mut instruction_files = Vec::new();
    for name in INSTRUCTION_FILES {
        if exists(&root.join(name))? {
            instruction_files.push(name.to_owned());
        }
    }
    Ok(Scan {
        repositories,
        instruction_files,
    })
}

fn child_repositories(root: &Path) -> Result<Vec<Repository>, ProjectError> {
    let mut repositories = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error(root))? {
        let path = entry.map_err(io_error(root))?.path();
        if !path.is_dir() || is_hidden(&path) || !path.join(GIT_ENTRY).exists() {
            continue;
        }
        repositories.extend(repository_at(&path, false));
    }
    repositories.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(repositories)
}

/// Builds the repository at `path`, or `None` when its name cannot serve as a
/// [`RepositoryId`] (for example it contains a backslash, or `path` has no file name).
fn repository_at(path: &Path, is_root: bool) -> Option<Repository> {
    let name = display_name(path);
    let id = RepositoryId::try_from(name.as_str()).ok()?;
    Some(Repository {
        id,
        path: path.to_path_buf(),
        name,
        instruction_files: INSTRUCTION_FILES
            .into_iter()
            .filter(|name| path.join(name).exists())
            .map(str::to_owned)
            .collect(),
        is_root,
    })
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn is_hidden(path: &Path) -> bool {
    display_name(path).starts_with('.')
}

fn exists(path: &Path) -> Result<bool, ProjectError> {
    path.try_exists().map_err(io_error(path))
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> ProjectError + '_ {
    move |source| ProjectError::Io {
        path: path.to_path_buf(),
        source,
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
    use std::fs;

    fn repository_ids(project: &Project) -> Vec<&str> {
        project.repositories.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn a_folder_with_two_child_repositories_discovers_both_sorted() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("b/.git")).unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/.git"), "gitdir: elsewhere").unwrap();
        fs::write(dir.path().join("a/AGENTS.md"), "rules").unwrap();
        fs::write(dir.path().join("a/CLAUDE.md"), "claude rules").unwrap();
        fs::create_dir_all(dir.path().join("c")).unwrap();
        fs::create_dir_all(dir.path().join(".hidden/.git")).unwrap();
        fs::write(dir.path().join("loose-file"), "x").unwrap();

        let project = discover_project(dir.path()).unwrap();

        assert_eq!(repository_ids(&project), ["a", "b"]);
        let a = &project.repositories[0];
        assert_eq!(a.path, dir.path().join("a"));
        assert_eq!(a.name, "a");
        assert_eq!(a.instruction_files, ["AGENTS.md", "CLAUDE.md"]);
        assert!(!a.is_root);
        assert!(project.repositories[1].instruction_files.is_empty());
        assert_eq!(project.root, dir.path());
        assert_eq!(
            project.name,
            dir.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn a_root_that_is_a_repository_is_its_own_single_repository() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("nested/.git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        let root_name = dir.path().file_name().unwrap().to_string_lossy();

        let project = discover_project(dir.path()).unwrap();

        assert_eq!(project.repositories.len(), 1);
        let repository = &project.repositories[0];
        assert!(repository.is_root);
        assert_eq!(repository.path, dir.path());
        assert_eq!(repository.id.as_str(), root_name);
        assert_eq!(repository.name, root_name);
        assert_eq!(repository.instruction_files, ["AGENTS.md"]);
    }

    #[test]
    fn a_missing_root_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");

        let error = discover_project(&missing).unwrap_err();

        assert!(matches!(error, ProjectError::NotADirectory { ref path } if *path == missing));
        assert!(error.to_string().contains("not a directory"));
    }

    #[test]
    fn a_file_root_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();

        assert!(matches!(
            discover_project(&file),
            Err(ProjectError::NotADirectory { .. })
        ));
    }

    #[test]
    fn root_instruction_files_follow_the_root_folder() {
        let dir = tempfile::tempdir().unwrap();
        let files = || discover_project(dir.path()).unwrap().instruction_files;
        assert!(files().is_empty());

        fs::write(dir.path().join("CLAUDE.md"), "rules").unwrap();
        assert_eq!(files(), ["CLAUDE.md"]);

        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        assert_eq!(files(), ["AGENTS.md", "CLAUDE.md"]);
    }

    #[test]
    fn rescan_keeps_identity_and_refreshes_repositories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/.git")).unwrap();
        let mut project = discover_project(dir.path()).unwrap();
        let (id, root, name) = (
            project.id.clone(),
            project.root.clone(),
            project.name.clone(),
        );

        fs::create_dir_all(dir.path().join("b/.git")).unwrap();
        fs::write(dir.path().join("a/AGENTS.md"), "rules").unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        fs::write(dir.path().join("a/CLAUDE.md"), "rules").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "rules").unwrap();
        rescan(&mut project).unwrap();

        assert_eq!(project.id, id);
        assert_eq!(project.root, root);
        assert_eq!(project.name, name);
        assert_eq!(repository_ids(&project), ["a", "b"]);
        assert_eq!(
            project.repositories[0].instruction_files,
            ["AGENTS.md", "CLAUDE.md"]
        );
        assert_eq!(project.instruction_files, ["AGENTS.md", "CLAUDE.md"]);
    }

    #[test]
    fn rescan_reports_a_root_that_has_disappeared() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = discover_project(dir.path()).unwrap();
        drop(dir);

        assert!(matches!(
            rescan(&mut project),
            Err(ProjectError::NotADirectory { .. })
        ));
    }

    #[test]
    fn a_project_saved_without_the_closed_flag_loads_as_open() {
        let json = r#"{"id":"0b5c3a46-3f7e-4f61-9a39-1a5d0c4b8e21","root":"/work","name":"work","repositories":[],"has_global_instructions":false}"#;

        let project: Project = serde_json::from_str(json).unwrap();

        assert!(!project.closed);
    }

    #[test]
    fn a_project_saved_without_the_claude_md_flags_loads_with_them_unset() {
        let json = r#"{"id":"0b5c3a46-3f7e-4f61-9a39-1a5d0c4b8e21","root":"/work","name":"work","repositories":[{"id":"api","path":"/work/api","name":"api","has_instructions":true,"is_root":false}],"has_global_instructions":true}"#;

        let project: Project = serde_json::from_str(json).unwrap();

        assert_eq!(project.instruction_files, ["AGENTS.md"]);
        assert_eq!(project.repositories[0].instruction_files, ["AGENTS.md"]);
    }

    /// `Project` as v0.4.0 defines it (`crates/partitur-core/src/project.rs` at the tag): its
    /// field names, order and serde attributes.
    #[derive(Debug, Serialize, Deserialize)]
    struct V040Project {
        id: ProjectId,
        root: PathBuf,
        name: String,
        repositories: Vec<V040Repository>,
        has_global_instructions: bool,
        #[serde(default)]
        has_global_claude_md: bool,
        #[serde(default)]
        closed: bool,
        #[serde(default)]
        fold: Fold,
    }

    /// `Repository` as v0.4.0 defines it.
    #[derive(Debug, Serialize, Deserialize)]
    struct V040Repository {
        id: RepositoryId,
        path: PathBuf,
        name: String,
        has_instructions: bool,
        #[serde(default)]
        has_claude_md: bool,
        is_root: bool,
    }

    /// A project list entry as v0.4.0 writes it: the root holds `CLAUDE.md`, `api` holds both
    /// files and `web` none.
    const V0_4_0_PROJECT: &str = r#"{"id":"0b5c3a46-3f7e-4f61-9a39-1a5d0c4b8e21","root":"/work","name":"work","repositories":[{"id":"api","path":"/work/api","name":"api","has_instructions":true,"has_claude_md":true,"is_root":false},{"id":"web","path":"/work/web","name":"web","has_instructions":false,"has_claude_md":false,"is_root":false}],"has_global_instructions":false,"has_global_claude_md":true,"closed":true,"fold":"Folded"}"#;

    #[test]
    fn old_project_booleans_read_as_instruction_files() {
        let project: Project = serde_json::from_str(V0_4_0_PROJECT).unwrap();

        assert_eq!(project.instruction_files, ["CLAUDE.md"]);
        assert_eq!(
            project.repositories[0].instruction_files,
            ["AGENTS.md", "CLAUDE.md"]
        );
        assert!(project.repositories[1].instruction_files.is_empty());
        assert!(project.closed);
        assert_eq!(project.fold, Fold::Folded);
    }

    #[test]
    fn instruction_files_round_trip_through_v0_4_0_shape() {
        let project: Project = serde_json::from_str(V0_4_0_PROJECT).unwrap();
        let json = serde_json::to_string(&project).unwrap();

        // v0.4.0 reads the flags back from what is saved now...
        let old: V040Project = serde_json::from_str(&json).unwrap();
        let old_json = serde_json::to_string(&old).unwrap();
        assert_eq!(old_json, V0_4_0_PROJECT);
        // ...and the bytes are v0.4.0's, with each list added last.
        let expected = V0_4_0_PROJECT
            .replace(
                r#""has_claude_md":true,"is_root":false}"#,
                r#""has_claude_md":true,"is_root":false,"instruction_files":["AGENTS.md","CLAUDE.md"]}"#,
            )
            .replace(
                r#""has_claude_md":false,"is_root":false}"#,
                r#""has_claude_md":false,"is_root":false,"instruction_files":[]}"#,
            )
            .replace(
                r#""fold":"Folded"}"#,
                r#""fold":"Folded","instruction_files":["CLAUDE.md"]}"#,
            );
        assert_eq!(json, expected);
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), project);
    }

    #[test]
    fn a_saved_list_wins_over_the_flags() {
        let json = r#"{"id":"0b5c3a46-3f7e-4f61-9a39-1a5d0c4b8e21","root":"/work","name":"work","repositories":[{"id":"api","path":"/work/api","name":"api","has_instructions":true,"has_claude_md":true,"is_root":false,"instruction_files":["CLAUDE.md"]}],"has_global_instructions":true,"has_global_claude_md":false,"instruction_files":[]}"#;

        let project: Project = serde_json::from_str(json).unwrap();

        assert!(project.instruction_files.is_empty());
        assert_eq!(project.repositories[0].instruction_files, ["CLAUDE.md"]);
    }

    #[test]
    fn display_name_falls_back_to_the_whole_path_without_a_file_name() {
        assert_eq!(display_name(Path::new("/")), "/");
        assert_eq!(display_name(Path::new("/tmp/project")), "project");
    }

    #[test]
    fn a_root_without_a_usable_name_has_no_root_repository() {
        assert_eq!(repository_at(Path::new("/"), true), None);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn set_mode(path: &Path, mode: u32) {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }

        /// Root bypasses permission bits, which would make the tests below pass vacuously.
        fn permissions_are_enforced(dir: &Path) -> bool {
            let probe = dir.join("probe");
            fs::create_dir(&probe).unwrap();
            set_mode(&probe, 0o000);
            let enforced = fs::read_dir(&probe).is_err();
            set_mode(&probe, 0o755);
            enforced
        }

        #[test]
        fn directory_names_with_backslashes_are_skipped() {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("a\\b/.git")).unwrap();
            fs::create_dir_all(dir.path().join("ok/.git")).unwrap();

            let project = discover_project(dir.path()).unwrap();

            assert_eq!(repository_ids(&project), ["ok"]);
        }

        #[test]
        fn an_unreadable_root_reports_io() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            let root = dir.path().join("root");
            fs::create_dir(&root).unwrap();
            set_mode(&root, 0o300);

            let result = discover_project(&root);
            set_mode(&root, 0o755);

            assert!(
                !enforced
                    || matches!(result, Err(ProjectError::Io { ref path, .. }) if *path == root)
            );
        }

        #[test]
        fn an_inaccessible_root_reports_io() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            let root = dir.path().join("root");
            fs::create_dir(&root).unwrap();
            set_mode(&root, 0o000);

            let result = discover_project(&root);
            set_mode(&root, 0o755);

            assert!(!enforced || matches!(result, Err(ProjectError::Io { .. })));
        }

        #[test]
        fn an_inaccessible_child_is_skipped() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            fs::create_dir_all(dir.path().join("a/.git")).unwrap();
            let child = dir.path().join("child");
            fs::create_dir_all(child.join(".git")).unwrap();
            set_mode(&child, 0o000);
            fs::create_dir_all(dir.path().join("z/.git")).unwrap();

            let project = discover_project(dir.path());
            set_mode(&child, 0o755);

            let project = project.unwrap();
            let ids = repository_ids(&project);
            assert!(ids.contains(&"a") && ids.contains(&"z"));
            assert!(!enforced || !ids.contains(&"child"));
        }
    }
}
