//! [`MemoryFiles`], the stand-in for the [`Files`] port, and the helpers that feed its answers
//! to an [`App`].
//!
//! The fake answers from an in-memory tree of folders and text files, or, made with
//! [`MemoryFiles::on_disk`], from the real file system, for tests whose project folders are
//! real temporary folders. Either way it queues every request until the test calls
//! [`MemoryFiles::answer`], so a test holds a pending state simply by not answering;
//! [`open_project`] answers at once. The settings a [`FilesQuery::Settings`] asks for come
//! from the [`MemoryStore`] given to [`MemoryFiles::with_settings_from`], and are the defaults
//! without one.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{MemoryStore, locked};
use crate::app::{App, AppError};
use crate::files::{
    Files, FilesAnswer, FilesError, FilesEvent, FilesQuery, FilesRequestId, FolderListing,
    ProjectListing, ResolvedPath, SCAN_ENTRIES,
};
use crate::ids::ProjectId;
use crate::settings::ProjectSettings;
use crate::store::Store as _;

/// A [`Files`] that queues every request and answers it, from an in-memory tree or the real
/// file system, only when [`answer`](Self::answer) is called.
///
/// Clones share the same tree and queue.
#[derive(Debug, Clone, Default)]
pub struct MemoryFiles {
    state: Arc<Mutex<FilesModel>>,
}

#[derive(Debug, Default)]
struct FilesModel {
    /// Whether the answers come from the real file system rather than `tree`.
    on_disk: bool,
    /// Every folder and file, by path; a folder is `None`, a file its text.
    tree: BTreeMap<PathBuf, Option<String>>,
    /// Where the settings come from.
    settings: Option<MemoryStore>,
    /// Every query, recorded when it is queued.
    queries: Vec<FilesQuery>,
    /// The requests queued and not answered yet.
    waiting: VecDeque<(FilesRequestId, Vec<FilesQuery>)>,
}

impl MemoryFiles {
    /// Creates a fake over an empty in-memory tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a fake that answers from the real file system.
    pub fn on_disk() -> Self {
        let files = Self::default();
        locked(&files.state).on_disk = true;
        files
    }

    /// Takes the settings a [`FilesQuery::Settings`] answers from `store`'s
    /// [`load_settings`](crate::Store::load_settings), its failures included.
    #[must_use]
    pub fn with_settings_from(self, store: MemoryStore) -> Self {
        locked(&self.state).settings = Some(store);
        self
    }

    /// Adds the folder at `path` to the in-memory tree, with every folder above it.
    pub fn add_folder(&self, path: impl Into<PathBuf>) {
        let path = path.into();
        let mut state = locked(&self.state);
        for folder in path.ancestors() {
            state.tree.entry(folder.to_path_buf()).or_insert(None);
        }
    }

    /// Adds the text file at `path` to the in-memory tree, with every folder above it.
    pub fn add_file(&self, path: impl Into<PathBuf>, text: impl Into<String>) {
        let path = path.into();
        if let Some(parent) = path.parent() {
            self.add_folder(parent);
        }
        locked(&self.state).tree.insert(path, Some(text.into()));
    }

    /// Every query, recorded when it is queued (before it is answered).
    pub fn queries(&self) -> Vec<FilesQuery> {
        locked(&self.state).queries.clone()
    }

    /// Requests queued and not answered yet.
    pub fn waiting(&self) -> usize {
        locked(&self.state).waiting.len()
    }

    /// Answers every waiting request, in queue order, and returns their events.
    pub fn answer(&self) -> Vec<FilesEvent> {
        let mut state = locked(&self.state);
        let waiting = std::mem::take(&mut state.waiting);
        waiting
            .into_iter()
            .map(|(request, queries)| FilesEvent::Answered {
                request,
                answers: queries.iter().map(|query| state.respond(query)).collect(),
            })
            .collect()
    }
}

impl Files for MemoryFiles {
    fn query(&self, request: FilesRequestId, queries: Vec<FilesQuery>) {
        let mut state = locked(&self.state);
        state.queries.extend(queries.iter().cloned());
        state.waiting.push_back((request, queries));
    }
}

impl FilesModel {
    fn respond(&self, query: &FilesQuery) -> Result<FilesAnswer, FilesError> {
        match query {
            FilesQuery::Scan(root) => self.scan(root).map(FilesAnswer::Scanned),
            FilesQuery::Read(path) => match self.read(path) {
                Ok(text) => Ok(FilesAnswer::Read(Some(text))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Ok(FilesAnswer::Read(None))
                }
                Err(error) => Err(io_error(path, &error)),
            },
            FilesQuery::Resolve(paths) => Ok(FilesAnswer::Resolved(
                paths.iter().map(|path| self.resolve(path)).collect(),
            )),
            FilesQuery::Settings(project) => self.settings(project),
        }
    }

    fn scan(&self, given: &Path) -> Result<ProjectListing, FilesError> {
        let not_a_directory = |path: &Path| FilesError::NotADirectory {
            path: path.to_path_buf(),
        };
        let root = self
            .canonical(given)
            .ok_or_else(|| not_a_directory(given))?;
        if !self.is_dir(&root) {
            return Err(not_a_directory(&root));
        }
        let entries = self
            .entries(&root)
            .map_err(|error| io_error(&root, &error))?;
        let children = if entries.iter().any(|entry| entry == SCAN_ENTRIES[0]) {
            Vec::new()
        } else {
            self.children(&root)
                .map_err(|error| io_error(&root, &error))?
                .into_iter()
                .filter(|path| self.is_dir(path))
                .map(|path| FolderListing {
                    entries: self.entries(&path).unwrap_or_default(),
                    path,
                })
                .collect()
        };
        Ok(ProjectListing {
            root,
            entries,
            children,
        })
    }

    fn settings(&self, project: &ProjectId) -> Result<FilesAnswer, FilesError> {
        self.settings
            .as_ref()
            .map_or_else(
                || Ok(ProjectSettings::default()),
                |store| store.load_settings(project.clone()),
            )
            .map(FilesAnswer::Settings)
            .map_err(|error| FilesError::Settings(error.to_string()))
    }

    fn entries(&self, folder: &Path) -> io::Result<Vec<String>> {
        let mut found = Vec::new();
        for name in SCAN_ENTRIES {
            if self.exists(&folder.join(name))? {
                found.push(name.to_owned());
            }
        }
        Ok(found)
    }

    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        if self.on_disk {
            crate::canonical(path).ok()
        } else {
            self.tree.contains_key(path).then(|| path.to_path_buf())
        }
    }

    fn is_dir(&self, path: &Path) -> bool {
        if self.on_disk {
            path.is_dir()
        } else {
            matches!(self.tree.get(path), Some(None))
        }
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        if self.on_disk {
            path.try_exists()
        } else {
            Ok(self.tree.contains_key(path))
        }
    }

    fn children(&self, folder: &Path) -> io::Result<Vec<PathBuf>> {
        if self.on_disk {
            std::fs::read_dir(folder)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect()
        } else {
            Ok(self
                .tree
                .keys()
                .filter(|path| path.parent() == Some(folder))
                .cloned()
                .collect())
        }
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        if self.on_disk {
            read_text(path)
        } else {
            match self.tree.get(path) {
                Some(Some(text)) => Ok(text.clone()),
                Some(None) => Err(io::Error::other("is a directory")),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
    }

    fn resolve(&self, path: &Path) -> ResolvedPath {
        let (is_symlink, present) = if self.on_disk {
            let meta = path.symlink_metadata();
            (
                meta.as_ref().is_ok_and(std::fs::Metadata::is_symlink),
                meta.is_ok(),
            )
        } else {
            (false, self.tree.contains_key(path))
        };
        ResolvedPath {
            given: path.to_path_buf(),
            canonical: self.canonical(path),
            is_symlink,
            present,
        }
    }
}

/// The text of the file at `path`, read as the files port's adapter reads it: a file that is
/// not UTF-8 fails with the conversion's own message.
fn read_text(path: &Path) -> io::Result<String> {
    String::from_utf8(std::fs::read(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn io_error(path: &Path, error: &io::Error) -> FilesError {
    FilesError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// Feeds every answer `files` gives into `app` until nothing waits; returns the errors
/// [`apply_files_event`](App::apply_files_event) reported, in order.
///
/// `files` is a clone of the [`MemoryFiles`] the app was loaded with, so the requests the app
/// queues while it applies an answer are answered in the next round.
pub fn apply_files(app: &mut App, files: &MemoryFiles) -> Vec<AppError> {
    let mut errors = Vec::new();
    loop {
        let events = files.answer();
        if events.is_empty() {
            return errors;
        }
        errors.extend(
            events
                .into_iter()
                .filter_map(|event| app.apply_files_event(event).err()),
        );
    }
}

/// Opens `root` as a project in `app` and answers what that asks of `files` at once, as if
/// the scan had come back before anything else happened; returns the project's id.
///
/// # Errors
///
/// Returns the first error [`apply_files_event`](App::apply_files_event) reported, as for a
/// folder that cannot be scanned, and [`FilesError::Unavailable`] when the answers opened no
/// project, which only a request answered elsewhere leaves.
pub fn open_project(
    app: &mut App,
    files: &MemoryFiles,
    root: &Path,
) -> Result<ProjectId, AppError> {
    app.open_project(root);
    let mut opened = None;
    loop {
        let events = files.answer();
        if events.is_empty() {
            break;
        }
        for event in events {
            if let Some(project) = app.apply_files_event(event)? {
                opened = Some(project);
            }
        }
    }
    opened.ok_or_else(|| {
        AppError::Files(FilesError::Unavailable(
            "no answer opened the project".into(),
        ))
    })
}

/// The project the real folder `root` makes, as a scan of it finds it; for tests in this
/// crate that store a project without opening it.
///
/// # Panics
///
/// When `root` cannot be scanned.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn discovered(root: &Path) -> crate::project::Project {
    let model = FilesModel {
        on_disk: true,
        ..FilesModel::default()
    };
    crate::project::discover_project(&model.scan(root).expect("a folder that can be scanned"))
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

    fn answers(
        files: &MemoryFiles,
        queries: Vec<FilesQuery>,
    ) -> Vec<Result<FilesAnswer, FilesError>> {
        files.query(FilesRequestId(1), queries);
        assert_eq!(files.waiting(), 1);
        let mut events = files.answer();
        assert_eq!(files.waiting(), 0);
        let Some(FilesEvent::Answered { request, answers }) = events.pop() else {
            panic!("one event");
        };
        assert_eq!(request, FilesRequestId(1));
        answers
    }

    #[test]
    fn the_tree_answers_a_scan() {
        let files = MemoryFiles::new();
        files.add_folder("/work/api/.git");
        files.add_file("/work/api/AGENTS.md", "rules");
        files.add_file("/work/notes.txt", "x");
        files.add_folder("/work/docs");

        let answers = answers(&files, vec![FilesQuery::Scan("/work".into())]);

        let Ok(FilesAnswer::Scanned(listing)) = &answers[0] else {
            panic!("a listing: {answers:?}");
        };
        assert_eq!(listing.root, Path::new("/work"));
        assert!(listing.entries.is_empty());
        assert_eq!(
            listing.children,
            [
                FolderListing {
                    path: "/work/api".into(),
                    entries: vec![".git".into(), "AGENTS.md".into()],
                },
                FolderListing {
                    path: "/work/docs".into(),
                    entries: vec![],
                },
            ]
        );
        assert_eq!(files.queries(), [FilesQuery::Scan("/work".into())]);
    }

    #[test]
    fn a_repository_root_lists_no_children() {
        let files = MemoryFiles::new();
        files.add_folder("/work/.git");
        files.add_folder("/work/nested/.git");

        let answers = answers(&files, vec![FilesQuery::Scan("/work".into())]);

        let Ok(FilesAnswer::Scanned(listing)) = &answers[0] else {
            panic!("a listing: {answers:?}");
        };
        assert_eq!(listing.entries, [".git"]);
        assert!(listing.children.is_empty());
    }

    #[test]
    fn a_missing_or_file_root_is_not_a_directory() {
        let files = MemoryFiles::new();
        files.add_file("/work/file", "x");

        let answers = answers(
            &files,
            vec![
                FilesQuery::Scan("/missing".into()),
                FilesQuery::Scan("/work/file".into()),
            ],
        );

        assert_eq!(
            answers,
            [
                Err(FilesError::NotADirectory {
                    path: "/missing".into()
                }),
                Err(FilesError::NotADirectory {
                    path: "/work/file".into()
                }),
            ]
        );
    }

    #[test]
    fn reads_and_resolves_from_the_tree() {
        let files = MemoryFiles::new();
        files.add_file("/work/AGENTS.md", "rules");

        let answers = answers(
            &files,
            vec![
                FilesQuery::Read("/work/AGENTS.md".into()),
                FilesQuery::Read("/work/CLAUDE.md".into()),
                FilesQuery::Read("/work".into()),
                FilesQuery::Resolve(vec!["/work".into(), "/gone".into()]),
            ],
        );

        assert_eq!(answers[0], Ok(FilesAnswer::Read(Some("rules".into()))));
        assert_eq!(answers[1], Ok(FilesAnswer::Read(None)));
        assert!(matches!(answers[2], Err(FilesError::Io { .. })));
        assert_eq!(
            answers[3],
            Ok(FilesAnswer::Resolved(vec![
                ResolvedPath {
                    given: "/work".into(),
                    canonical: Some("/work".into()),
                    is_symlink: false,
                    present: true,
                },
                ResolvedPath {
                    given: "/gone".into(),
                    canonical: None,
                    is_symlink: false,
                    present: false,
                },
            ]))
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_tree_knows_no_symbolic_links() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        let files = MemoryFiles::new();
        files.add_folder(&link);

        let answers = answers(&files, vec![FilesQuery::Resolve(vec![link.clone()])]);

        assert_eq!(
            answers,
            [Ok(FilesAnswer::Resolved(vec![ResolvedPath {
                given: link.clone(),
                canonical: Some(link),
                is_symlink: false,
                present: true,
            }]))]
        );
    }

    #[test]
    fn settings_come_from_the_store_or_are_the_defaults() {
        let project = ProjectId::new();
        let defaults = answers(
            &MemoryFiles::new(),
            vec![FilesQuery::Settings(project.clone())],
        );
        assert_eq!(
            defaults,
            [Ok(FilesAnswer::Settings(ProjectSettings::default()))]
        );

        let store = MemoryStore::new();
        store.fail_next_settings_load();
        let files = MemoryFiles::new().with_settings_from(store.clone());
        let failed = answers(&files, vec![FilesQuery::Settings(project)]);
        assert!(
            matches!(&failed[0], Err(FilesError::Settings(text)) if text.contains("settings.toml"))
        );
        assert_eq!(store.settings_loads(), 1);
    }

    #[test]
    fn opening_reports_answers_that_opened_nothing() {
        let files = MemoryFiles::new();
        files.add_folder("/work/shop");
        let (backends, default) = crate::testing::single_claude();
        let store = MemoryStore::new();
        let mut app = App::load(
            Box::new(crate::testing::RecordingBackend::new()),
            Box::new(store.clone()),
            Box::new(store),
            Box::new(crate::testing::MemoryGit::new()),
            Box::new(files.clone()),
            backends,
            default,
            crate::settings::GlobalSettings::default(),
            Box::new(crate::testing::FixedClock::default()),
        )
        .unwrap();
        app.open_project(Path::new("/work/shop"));
        // Answered, but never applied: the folder stays being opened.
        assert_eq!(files.answer().len(), 1);

        let error = open_project(&mut app, &files, Path::new("/work/shop")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "the file system is unavailable: no answer opened the project"
        );
    }

    #[test]
    fn the_disk_answers_what_the_tree_would() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::canonical(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("api/.git")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "rules").unwrap();
        std::fs::write(root.join("loose"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("api"), root.join("link")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).unwrap();
        let files = MemoryFiles::on_disk();

        let answers = answers(
            &files,
            vec![
                FilesQuery::Scan(root.clone()),
                FilesQuery::Read(root.join("CLAUDE.md")),
                FilesQuery::Read(root.join("AGENTS.md")),
                FilesQuery::Read(root.join("api")),
                FilesQuery::Resolve(vec![
                    root.join("link"),
                    root.join("dangling"),
                    root.join("missing"),
                ]),
                FilesQuery::Scan(root.join("missing")),
                FilesQuery::Scan(root.join("loose")),
                FilesQuery::Scan(root.join("api")),
            ],
        );

        let Ok(FilesAnswer::Scanned(listing)) = &answers[0] else {
            panic!("a listing: {answers:?}");
        };
        assert_eq!(listing.entries, ["CLAUDE.md"]);
        assert!(listing.children.contains(&FolderListing {
            path: root.join("api"),
            entries: vec![".git".into()],
        }));
        assert_eq!(answers[1], Ok(FilesAnswer::Read(Some("rules".into()))));
        assert_eq!(answers[2], Ok(FilesAnswer::Read(None)));
        assert!(matches!(answers[3], Err(FilesError::Io { .. })));
        #[cfg(unix)]
        assert_eq!(
            answers[4],
            Ok(FilesAnswer::Resolved(vec![
                ResolvedPath {
                    given: root.join("link"),
                    canonical: Some(root.join("api")),
                    is_symlink: true,
                    present: true,
                },
                ResolvedPath {
                    given: root.join("dangling"),
                    canonical: None,
                    is_symlink: true,
                    present: true,
                },
                ResolvedPath {
                    given: root.join("missing"),
                    canonical: None,
                    is_symlink: false,
                    present: false,
                },
            ]))
        );
        assert!(matches!(answers[5], Err(FilesError::NotADirectory { .. })));
        assert!(matches!(answers[6], Err(FilesError::NotADirectory { .. })));
        assert!(matches!(answers[7], Ok(FilesAnswer::Scanned(_))));
    }
}
