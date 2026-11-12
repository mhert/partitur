//! Discovery of a project folder and the git repositories inside it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{ProjectId, RepositoryId};

const GIT_ENTRY: &str = ".git";
const INSTRUCTIONS_FILE: &str = "AGENTS.md";
const CLAUDE_FILE: &str = "CLAUDE.md";

/// A folder the user opened, together with the repositories found in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identity, kept across [`rescan`].
    pub id: ProjectId,
    /// The folder the project was opened from.
    pub root: PathBuf,
    /// Display name, the root folder's name.
    pub name: String,
    /// The repositories in the project, sorted by name.
    pub repositories: Vec<Repository>,
    /// Whether `AGENTS.md` exists directly in the project root.
    pub has_global_instructions: bool,
    /// Whether `CLAUDE.md` exists directly in the project root. Project lists saved before
    /// this field existed load with it unset until the next rescan.
    #[serde(default)]
    pub has_global_claude_md: bool,
    /// Whether the user closed the project. A closed project stays saved, out of the tree,
    /// so opening its folder again restores it with its sessions. Project lists saved before
    /// this field existed load as open.
    #[serde(default)]
    pub closed: bool,
    /// Whether the project is folded in the tree. Project lists saved before this field
    /// existed load as expanded.
    #[serde(default)]
    pub collapsed: bool,
}

/// A git repository belonging to a [`Project`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    /// The repository's directory name.
    pub id: RepositoryId,
    /// The repository's working directory.
    pub path: PathBuf,
    /// Display name, the same as the directory name.
    pub name: String,
    /// Whether `AGENTS.md` exists directly in the repository.
    pub has_instructions: bool,
    /// Whether `CLAUDE.md` exists directly in the repository. Project lists saved before this
    /// field existed load with it unset until the next rescan.
    #[serde(default)]
    pub has_claude_md: bool,
    /// Whether the repository is the project root itself rather than a child folder.
    pub is_root: bool,
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
/// its `.git`, `AGENTS.md` and `CLAUDE.md`). A child directory that cannot be inspected cannot be a
/// repository, so it is skipped instead of failing the whole project.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use partitur_core::discover_project;
///
/// let dir = tempfile::tempdir()?;
/// std::fs::create_dir_all(dir.path().join("api/.git"))?;
///
/// let project = discover_project(dir.path())?;
///
/// assert_eq!(project.repositories.len(), 1);
/// assert_eq!(project.repositories[0].name, "api");
/// # Ok(())
/// # }
/// ```
pub fn discover_project(root: &Path) -> Result<Project, ProjectError> {
    let scan = scan(root)?;
    Ok(Project {
        id: ProjectId::new(),
        root: root.to_path_buf(),
        name: display_name(root),
        repositories: scan.repositories,
        has_global_instructions: scan.has_global_instructions,
        has_global_claude_md: scan.has_global_claude_md,
        closed: false,
        collapsed: false,
    })
}

/// Recomputes the repositories and instruction file flags of `project` from the file system,
/// keeping its `id`, `root` and `name`.
///
/// # Errors
///
/// Returns the same errors as [`discover_project`]; the project is left unchanged on error.
pub fn rescan(project: &mut Project) -> Result<(), ProjectError> {
    let scan = scan(&project.root)?;
    project.repositories = scan.repositories;
    project.has_global_instructions = scan.has_global_instructions;
    project.has_global_claude_md = scan.has_global_claude_md;
    Ok(())
}

struct Scan {
    repositories: Vec<Repository>,
    has_global_instructions: bool,
    has_global_claude_md: bool,
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
    Ok(Scan {
        repositories,
        has_global_instructions: exists(&root.join(INSTRUCTIONS_FILE))?,
        has_global_claude_md: exists(&root.join(CLAUDE_FILE))?,
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
        has_instructions: path.join(INSTRUCTIONS_FILE).exists(),
        has_claude_md: path.join(CLAUDE_FILE).exists(),
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
        assert!(a.has_instructions);
        assert!(a.has_claude_md);
        assert!(!a.is_root);
        assert!(!project.repositories[1].has_instructions);
        assert!(!project.repositories[1].has_claude_md);
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
        assert!(repository.has_instructions);
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
    fn global_instructions_flag_follows_root_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !discover_project(dir.path())
                .unwrap()
                .has_global_instructions
        );

        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();
        assert!(
            discover_project(dir.path())
                .unwrap()
                .has_global_instructions
        );
    }

    #[test]
    fn global_claude_md_flag_follows_root_claude_md() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!discover_project(dir.path()).unwrap().has_global_claude_md);

        fs::write(dir.path().join("CLAUDE.md"), "rules").unwrap();
        assert!(discover_project(dir.path()).unwrap().has_global_claude_md);
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
        assert!(project.repositories[0].has_instructions);
        assert!(project.has_global_instructions);
        assert!(project.repositories[0].has_claude_md);
        assert!(project.has_global_claude_md);
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

        assert!(!project.has_global_claude_md);
        assert!(!project.repositories[0].has_claude_md);
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
