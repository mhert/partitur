//! Discovery of a project folder and the git repositories inside it, from what a scan of the
//! folder through the [`Files`](crate::files::Files) port listed. A repository session
//! runs in the repository it names; a project session runs in the root folder, which is the
//! project's repository when the root itself holds `.git`.
//!
//! The project root and each repository list the agent instruction files found directly in
//! them, each by its file name, `AGENTS.md` before `CLAUDE.md`. Both are stored
//! in a shape v0.4.0 still reads: next to the list they write the `AGENTS.md` and `CLAUDE.md`
//! flags v0.4.0 reads instead, derived from the list, and a project saved by v0.4.0, which has
//! only the flags, loads with the list built from them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::files::ProjectListing;
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
/// files. Writing derives the flags from the list. A v0.4.0 rewrite keeps only `AGENTS.md` and
/// `CLAUDE.md`, so any other file the list names is dropped until the next rescan.
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

/// The instruction files v0.4.0's flags name, in the order a scan lists them: `AGENTS.md`
/// when `agents_md`, `CLAUDE.md` when `claude_md`.
fn files_from_flags(agents_md: bool, claude_md: bool) -> Vec<String> {
    INSTRUCTION_FILES
        .into_iter()
        .filter(|name| (*name == AGENTS_MD && agents_md) || (*name == CLAUDE_MD && claude_md))
        .map(str::to_owned)
        .collect()
}

/// Whether `files` names `name`.
fn lists(files: &[String], name: &str) -> bool {
    files.iter().any(|file| file == name)
}

/// Builds the project a [`FilesQuery::Scan`](crate::files::FilesQuery::Scan) of its folder
/// found, with a new id, open and showing its sessions.
///
/// A repository is a child folder holding `.git` (a directory, or a file as in a linked
/// worktree). When the folder itself holds `.git`, it is the project's single repository and
/// its children are not looked at. Hidden child folders are skipped, and so is a child whose
/// name cannot serve as a [`RepositoryId`].
pub(crate) fn discover_project(listing: &ProjectListing) -> Project {
    Project {
        id: ProjectId::new(),
        root: listing.root.clone(),
        name: display_name(&listing.root),
        repositories: repositories(listing),
        instruction_files: instruction_files(&listing.entries),
        closed: false,
        fold: Fold::Open,
    }
}

/// Takes the repositories and instruction files of `project` from a new `listing` of its
/// folder, keeping its `id`, `root` and `name`.
pub(crate) fn rescan(project: &mut Project, listing: &ProjectListing) {
    project.repositories = repositories(listing);
    project.instruction_files = instruction_files(&listing.entries);
}

/// The repositories `listing` holds, sorted by name, as [`discover_project`] finds them.
fn repositories(listing: &ProjectListing) -> Vec<Repository> {
    if holds_git(&listing.entries) {
        return repository_at(&listing.root, &listing.entries, true)
            .into_iter()
            .collect();
    }
    let mut repositories: Vec<Repository> = listing
        .children
        .iter()
        .filter(|child| !is_hidden(&child.path) && holds_git(&child.entries))
        .filter_map(|child| repository_at(&child.path, &child.entries, false))
        .collect();
    repositories.sort_by(|a, b| a.name.cmp(&b.name));
    repositories
}

/// Whether a folder holding `entries` holds `.git`.
fn holds_git(entries: &[String]) -> bool {
    lists(entries, GIT_ENTRY)
}

/// The instruction files among `entries`, `AGENTS.md` before `CLAUDE.md`.
fn instruction_files(entries: &[String]) -> Vec<String> {
    INSTRUCTION_FILES
        .into_iter()
        .filter(|name| lists(entries, name))
        .map(str::to_owned)
        .collect()
}

/// Builds the repository at `path`, which holds `entries`, or `None` when its name cannot
/// serve as a [`RepositoryId`] (for example it contains a backslash, or `path` has no file
/// name).
fn repository_at(path: &Path, entries: &[String], is_root: bool) -> Option<Repository> {
    let name = display_name(path);
    let id = RepositoryId::try_from(name.as_str()).ok()?;
    Some(Repository {
        id,
        path: path.to_path_buf(),
        name,
        instruction_files: instruction_files(entries),
        is_root,
    })
}

/// The name a folder at `path` is shown by: its last component, or the whole path when it
/// has none, as `/`.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn is_hidden(path: &Path) -> bool {
    display_name(path).starts_with('.')
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
    use crate::files::{FolderListing, SCAN_ENTRIES};

    fn repository_ids(project: &Project) -> Vec<&str> {
        project.repositories.iter().map(|r| r.id.as_str()).collect()
    }

    fn names(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    fn child(path: &str, entries: &[&str]) -> FolderListing {
        FolderListing {
            path: PathBuf::from(path),
            entries: names(entries),
        }
    }

    fn listing(entries: &[&str], children: Vec<FolderListing>) -> ProjectListing {
        ProjectListing {
            root: PathBuf::from("/work/project"),
            entries: names(entries),
            children,
        }
    }

    #[test]
    fn the_scan_looks_for_the_entries_discovery_reads() {
        assert_eq!(SCAN_ENTRIES, [GIT_ENTRY, AGENTS_MD, CLAUDE_MD]);
    }

    #[test]
    fn discover_project_from_listing() {
        let listing = listing(
            &[],
            vec![
                child("/work/project/b", &[".git"]),
                child("/work/project/a", &[".git", "AGENTS.md", "CLAUDE.md"]),
                child("/work/project/c", &["AGENTS.md"]),
                child("/work/project/.hidden", &[".git"]),
                child("/work/project/a\\b", &[".git"]),
            ],
        );

        let project = discover_project(&listing);

        assert_eq!(repository_ids(&project), ["a", "b"]);
        let a = &project.repositories[0];
        assert_eq!(a.path, Path::new("/work/project/a"));
        assert_eq!(a.name, "a");
        assert_eq!(a.instruction_files, ["AGENTS.md", "CLAUDE.md"]);
        assert!(!a.is_root);
        assert!(project.repositories[1].instruction_files.is_empty());
        assert_eq!(project.root, Path::new("/work/project"));
        assert_eq!(project.name, "project");
        assert!(project.instruction_files.is_empty());
        assert!(!project.closed);
        assert_eq!(project.fold, Fold::Open);
    }

    #[test]
    fn a_root_that_is_a_repository_is_its_own_single_repository() {
        let listing = listing(
            &[".git", "AGENTS.md"],
            vec![child("/work/project/nested", &[".git"])],
        );

        let project = discover_project(&listing);

        assert_eq!(project.repositories.len(), 1);
        let repository = &project.repositories[0];
        assert!(repository.is_root);
        assert_eq!(repository.path, Path::new("/work/project"));
        assert_eq!(repository.id.as_str(), "project");
        assert_eq!(repository.name, "project");
        assert_eq!(repository.instruction_files, ["AGENTS.md"]);
        assert_eq!(project.instruction_files, ["AGENTS.md"]);
    }

    #[test]
    fn root_instruction_files_keep_their_order() {
        let files =
            |entries: &[&str]| discover_project(&listing(entries, vec![])).instruction_files;

        assert!(files(&[]).is_empty());
        assert_eq!(files(&["CLAUDE.md"]), ["CLAUDE.md"]);
        assert_eq!(
            files(&["CLAUDE.md", "AGENTS.md"]),
            ["AGENTS.md", "CLAUDE.md"]
        );
    }

    #[test]
    fn each_discovery_is_a_new_project() {
        let listing = listing(&[], vec![]);

        assert_ne!(discover_project(&listing).id, discover_project(&listing).id);
    }

    #[test]
    fn rescan_keeps_identity_and_refreshes_repositories() {
        let mut project =
            discover_project(&listing(&[], vec![child("/work/project/a", &[".git"])]));
        let (id, root, name) = (
            project.id.clone(),
            project.root.clone(),
            project.name.clone(),
        );

        rescan(
            &mut project,
            &listing(
                &["AGENTS.md", "CLAUDE.md"],
                vec![
                    child("/work/project/b", &[".git"]),
                    child("/work/project/a", &[".git", "AGENTS.md", "CLAUDE.md"]),
                ],
            ),
        );

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
        assert_eq!(repository_at(Path::new("/"), &names(&[".git"]), true), None);
    }
}
