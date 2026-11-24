//! The [`Files`] command port over the local file system.
//!
//! [`DiskFiles`] answers every request on a thread of its own, named after the request
//! (`files-<n>`), so a slow folder, such as one on a network mount, never holds up another
//! request and the caller never waits on the disk. The thread runs the request's queries one
//! after the other and sends its one [`FilesEvent`] to the receiver [`DiskFiles::start`]
//! returned; when that receiver is gone the event is dropped. A request whose thread cannot be
//! started is answered at once with [`FilesError::Unavailable`] for each query.
//!
//! A scan resolves the folder to its canonical path, checks it is a directory, and looks for
//! each of the [`SCAN_ENTRIES`] in it; a check that fails is [`FilesError::Io`]. Unless the
//! folder holds `.git`, it then lists the folder's child directories, each with the entries it
//! holds; a child that cannot be inspected is listed as holding nothing. A settings query reads
//! the project's `settings.toml` through [`FileStore::load_settings`], which writes the
//! commented template when the file is missing.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use partitur_core::files::SCAN_ENTRIES;
use partitur_core::{
    Files, FilesAnswer, FilesError, FilesEvent, FilesQuery, FilesRequestId, FolderListing,
    ProjectListing, ResolvedPath, Store as _,
};

use crate::FileStore;

/// The [`Files`] port, reading the local file system on one thread per request.
#[derive(Debug, Clone)]
pub struct DiskFiles {
    store: Arc<FileStore>,
    events: UnboundedSender<FilesEvent>,
}

impl DiskFiles {
    /// Starts the port, reading the projects' settings through `store`; every outcome arrives
    /// on the returned receiver.
    pub fn start(store: FileStore) -> (Self, UnboundedReceiver<FilesEvent>) {
        let (events, receiver) = unbounded();
        let port = Self {
            store: Arc::new(store),
            events,
        };
        (port, receiver)
    }
}

impl Files for DiskFiles {
    fn query(&self, request: FilesRequestId, queries: Vec<FilesQuery>) {
        let store = Arc::clone(&self.store);
        let events = self.events.clone();
        let count = queries.len();
        let spawned = std::thread::Builder::new()
            .name(request.to_string())
            .spawn(move || {
                let answers = queries.iter().map(|query| answer(&store, query)).collect();
                // A receiver that is gone wants no more outcomes.
                let _ = events.unbounded_send(FilesEvent::Answered { request, answers });
            });
        answer_unstarted(&self.events, request, count, spawned.map(drop));
    }
}

/// Answers a request of `count` queries whose thread did not start, with
/// [`FilesError::Unavailable`] for each; a request whose thread started is left to it.
fn answer_unstarted(
    events: &UnboundedSender<FilesEvent>,
    request: FilesRequestId,
    count: usize,
    spawned: io::Result<()>,
) {
    if let Err(error) = spawned {
        let answers = (0..count)
            .map(|_| Err(FilesError::Unavailable(error.to_string())))
            .collect();
        // A receiver that is gone wants no more outcomes.
        let _ = events.unbounded_send(FilesEvent::Answered { request, answers });
    }
}

fn answer(store: &FileStore, query: &FilesQuery) -> Result<FilesAnswer, FilesError> {
    match query {
        FilesQuery::Scan(root) => scan(root).map(FilesAnswer::Scanned),
        FilesQuery::Read(path) => match read_text(path) {
            Ok(text) => Ok(FilesAnswer::Read(Some(text))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(FilesAnswer::Read(None)),
            Err(error) => Err(io_error(path, &error)),
        },
        FilesQuery::Resolve(paths) => Ok(FilesAnswer::Resolved(
            paths.iter().map(|path| resolve(path)).collect(),
        )),
        FilesQuery::Settings(project) => store
            .load_settings(project.clone())
            .map(FilesAnswer::Settings)
            .map_err(|error| FilesError::Settings(error.to_string())),
    }
}

/// The text of the file at `path`; a file that is not UTF-8 fails with the conversion's own
/// message, which names the offending bytes.
fn read_text(path: &Path) -> io::Result<String> {
    String::from_utf8(std::fs::read(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn scan(given: &Path) -> Result<ProjectListing, FilesError> {
    let root = partitur_core::canonical(given).map_err(|_| FilesError::NotADirectory {
        path: given.to_path_buf(),
    })?;
    if !root.is_dir() {
        return Err(FilesError::NotADirectory { path: root });
    }
    let entries = entries(&root).map_err(|error| io_error(&root, &error))?;
    let children = if entries.iter().any(|entry| entry == SCAN_ENTRIES[0]) {
        Vec::new()
    } else {
        children(&root).map_err(|error| io_error(&root, &error))?
    };
    Ok(ProjectListing {
        root,
        entries,
        children,
    })
}

/// Which of the [`SCAN_ENTRIES`] `folder` holds, in that order.
fn entries(folder: &Path) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    for name in SCAN_ENTRIES {
        if folder.join(name).try_exists()? {
            found.push(name.to_owned());
        }
    }
    Ok(found)
}

/// Every child directory of `root`, with the entries it holds.
fn children(root: &Path) -> io::Result<Vec<FolderListing>> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            children.push(FolderListing {
                entries: entries(&path).unwrap_or_default(),
                path,
            });
        }
    }
    Ok(children)
}

fn resolve(path: &Path) -> ResolvedPath {
    ResolvedPath {
        given: path.to_path_buf(),
        canonical: partitur_core::canonical(path).ok(),
        is_symlink: path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink()),
    }
}

fn io_error(path: &Path, error: &io::Error) -> FilesError {
    FilesError::Io {
        path: PathBuf::from(path),
        message: error.to_string(),
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

    use std::fs;

    use futures::StreamExt;
    use futures::executor::block_on;
    use partitur_core::ProjectId;

    use super::*;
    use crate::StatePaths;

    /// The next event `events` brings, failing the test once none came for ten seconds.
    fn next_event(events: &mut UnboundedReceiver<FilesEvent>) -> FilesEvent {
        for _ in 0..1000 {
            if let Ok(event) = events.try_recv() {
                return event;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no answer came");
    }

    fn port(data: &Path) -> (DiskFiles, UnboundedReceiver<FilesEvent>) {
        DiskFiles::start(FileStore::new(StatePaths::under(data)))
    }

    /// The answers of one request of `queries`, as the port sends them.
    fn answers(queries: Vec<FilesQuery>) -> Vec<Result<FilesAnswer, FilesError>> {
        let data = tempfile::tempdir().unwrap();
        let (files, mut events) = port(data.path());
        files.query(FilesRequestId(4), queries);
        let FilesEvent::Answered { request, answers } = next_event(&mut events);
        assert_eq!(request, FilesRequestId(4));
        answers
    }

    fn scanned(root: &Path) -> ProjectListing {
        match answers(vec![FilesQuery::Scan(root.to_path_buf())]).remove(0) {
            Ok(FilesAnswer::Scanned(listing)) => listing,
            other => panic!("not a listing: {other:?}"),
        }
    }

    #[test]
    fn scan_lists_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let root = partitur_core::canonical(dir.path()).unwrap();
        fs::create_dir_all(root.join("b/.git")).unwrap();
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/.git"), "gitdir: elsewhere").unwrap();
        fs::write(root.join("a/AGENTS.md"), "rules").unwrap();
        fs::write(root.join("a/CLAUDE.md"), "claude rules").unwrap();
        fs::create_dir_all(root.join("c")).unwrap();
        fs::write(root.join("CLAUDE.md"), "rules").unwrap();
        fs::write(root.join("loose-file"), "x").unwrap();

        // Through a spelling that is not canonical.
        let mut listing = scanned(&root.join("c/.."));

        listing.children.sort_by(|x, y| x.path.cmp(&y.path));
        assert_eq!(listing.root, root);
        assert_eq!(listing.entries, ["CLAUDE.md"]);
        assert_eq!(
            listing.children,
            [
                FolderListing {
                    path: root.join("a"),
                    entries: vec![".git".into(), "AGENTS.md".into(), "CLAUDE.md".into()],
                },
                FolderListing {
                    path: root.join("b"),
                    entries: vec![".git".into()],
                },
                FolderListing {
                    path: root.join("c"),
                    entries: vec![],
                },
            ]
        );
    }

    #[test]
    fn a_repository_root_lists_no_children() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("nested/.git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();

        let listing = scanned(dir.path());

        assert_eq!(listing.entries, [".git", "AGENTS.md"]);
        assert!(listing.children.is_empty());
    }

    #[test]
    fn a_missing_or_file_root_is_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = partitur_core::canonical(dir.path()).unwrap();
        let (missing, file) = (root.join("missing"), root.join("file"));
        fs::write(&file, "x").unwrap();

        let answers = answers(vec![
            FilesQuery::Scan(missing.clone()),
            FilesQuery::Scan(file.clone()),
        ]);

        assert_eq!(
            answers,
            [
                Err(FilesError::NotADirectory { path: missing }),
                Err(FilesError::NotADirectory { path: file }),
            ]
        );
        assert!(
            answers[0]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
    }

    #[test]
    fn reads_a_file_and_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "rules").unwrap();

        let answers = answers(vec![
            FilesQuery::Read(dir.path().join("AGENTS.md")),
            FilesQuery::Read(dir.path().join("CLAUDE.md")),
            FilesQuery::Read(dir.path().to_path_buf()),
        ]);

        assert_eq!(answers[0], Ok(FilesAnswer::Read(Some("rules".into()))));
        assert_eq!(answers[1], Ok(FilesAnswer::Read(None)));
        assert!(
            matches!(&answers[2], Err(FilesError::Io { path, .. }) if path == dir.path()),
            "{answers:?}"
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_fails_with_the_conversions_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        let answers = answers(vec![FilesQuery::Read(path.clone())]);

        let expected = String::from_utf8(vec![0xff, 0xfe, 0xfd])
            .unwrap_err()
            .to_string();
        assert_eq!(
            answers[0],
            Err(FilesError::Io {
                path,
                message: expected
            })
        );
    }

    #[test]
    fn resolves_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = partitur_core::canonical(dir.path()).unwrap();
        fs::create_dir(root.join("real")).unwrap();

        let answers = answers(vec![FilesQuery::Resolve(vec![
            root.join("real/../real"),
            root.join("missing"),
        ])]);

        assert_eq!(
            answers,
            [Ok(FilesAnswer::Resolved(vec![
                ResolvedPath {
                    given: root.join("real/../real"),
                    canonical: Some(root.join("real")),
                    is_symlink: false,
                },
                ResolvedPath {
                    given: root.join("missing"),
                    canonical: None,
                    is_symlink: false,
                },
            ]))]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let root = partitur_core::canonical(dir.path()).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        let answers = answers(vec![FilesQuery::Resolve(vec![root.join("link")])]);

        assert_eq!(
            answers,
            [Ok(FilesAnswer::Resolved(vec![ResolvedPath {
                given: root.join("link"),
                canonical: Some(root.join("real")),
                is_symlink: true,
            }]))]
        );
    }

    #[test]
    fn settings_are_read_and_a_missing_file_gets_the_template() {
        let data = tempfile::tempdir().unwrap();
        let (files, mut events) = port(data.path());
        let project = ProjectId::new();
        let path = StatePaths::under(data.path()).settings_file(&project);

        files.query(
            FilesRequestId(1),
            vec![FilesQuery::Settings(project.clone())],
        );
        let FilesEvent::Answered { answers, .. } = next_event(&mut events);
        assert_eq!(
            answers,
            [Ok(FilesAnswer::Settings(
                partitur_core::ProjectSettings::default()
            ))]
        );
        assert!(path.is_file());

        fs::write(&path, "not = [toml").unwrap();
        files.query(FilesRequestId(2), vec![FilesQuery::Settings(project)]);
        let FilesEvent::Answered { answers, .. } = next_event(&mut events);
        assert!(
            matches!(&answers[0], Err(FilesError::Settings(text)) if text.contains("settings.toml")),
            "{answers:?}"
        );
    }

    #[test]
    fn a_request_whose_thread_did_not_start_fails_each_query() {
        let (sender, mut events) = unbounded();

        answer_unstarted(&sender, FilesRequestId(3), 2, Ok(()));
        answer_unstarted(
            &sender,
            FilesRequestId(3),
            2,
            Err(io::Error::other("no threads")),
        );
        drop(sender);

        let FilesEvent::Answered { request, answers } = next_event(&mut events);
        assert!(block_on(events.next()).is_none());
        assert_eq!(request, FilesRequestId(3));
        assert_eq!(
            answers,
            [
                Err(FilesError::Unavailable("no threads".into())),
                Err(FilesError::Unavailable("no threads".into())),
            ]
        );
    }

    #[test]
    fn an_answer_nobody_waits_for_is_dropped() {
        let data = tempfile::tempdir().unwrap();
        let (files, events) = port(data.path());
        drop(events);

        files.query(
            FilesRequestId(5),
            vec![FilesQuery::Read(data.path().join("x"))],
        );
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

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
        fn an_unreadable_root_reports_io() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            let root = partitur_core::canonical(dir.path()).unwrap().join("root");
            fs::create_dir(&root).unwrap();
            set_mode(&root, 0o300);

            let result = answers(vec![FilesQuery::Scan(root.clone())]);
            set_mode(&root, 0o755);

            assert!(
                !enforced
                    || matches!(&result[0], Err(FilesError::Io { path, .. }) if *path == root),
                "{result:?}"
            );
        }

        #[test]
        fn an_inaccessible_root_reports_io() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            let root = partitur_core::canonical(dir.path()).unwrap().join("root");
            fs::create_dir(&root).unwrap();
            set_mode(&root, 0o000);

            let result = answers(vec![FilesQuery::Scan(root.clone())]);
            set_mode(&root, 0o755);

            assert!(
                !enforced || matches!(result[0], Err(FilesError::Io { .. })),
                "{result:?}"
            );
        }

        #[test]
        fn an_inaccessible_child_holds_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let enforced = permissions_are_enforced(dir.path());
            let root = partitur_core::canonical(dir.path()).unwrap();
            fs::create_dir_all(root.join("a/.git")).unwrap();
            let child = root.join("child");
            fs::create_dir_all(child.join(".git")).unwrap();
            set_mode(&child, 0o000);

            let listing = scanned(&root);
            set_mode(&child, 0o755);

            let held = listing
                .children
                .iter()
                .find(|listed| listed.path == child)
                .map(|listed| listed.entries.clone());
            assert!(!enforced || held == Some(Vec::new()), "{listing:?}");
        }
    }
}
