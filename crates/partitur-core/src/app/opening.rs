//! Opening a folder as a project, in the background.
//!
//! [`App::open_project`] asks the [`Files`](crate::files::Files) port to scan the folder and
//! to resolve the folder of every project already known, and returns at once. Until the
//! project is ready the tree shows the folder as opening, by its name. A folder picked again
//! while it is being opened asks nothing more.
//!
//! When the scan arrives, a project whose folder is the scanned one, as stored or as resolved,
//! is rescanned and reopened with its id and sessions. Any other folder becomes a new project,
//! which is first given an id, and then has its `settings.toml` read through the same port; it
//! joins the tree, and the project list is saved, once the settings arrive. A scan that finds
//! the folder of a project still waiting for its settings leaves that one to finish. A scan
//! that failed ends the opening with its error, and an answer to no opening is dropped.

use std::path::{Path, PathBuf};

use super::{App, AppError};
use crate::files::{FilesAnswer, FilesError, FilesEvent, FilesQuery, FilesRequestId, ResolvedPath};
use crate::ids::ProjectId;
use crate::project::{Project, discover_project, display_name, rescan};
use crate::settings::ProjectSettings;
use crate::view_model::OpeningNode;

/// A folder being opened as a project, waiting for the files port.
#[derive(Debug)]
pub(super) struct Opening {
    /// The request in flight.
    request: FilesRequestId,
    /// The folder as it was picked.
    folder: PathBuf,
    /// What the request asked for.
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    /// The folder is being scanned, and the folders of these projects resolved, in order.
    Scanning { known: Vec<ProjectId> },
    /// The new project was found and its settings are being read.
    Reading { project: Project },
}

impl App {
    /// Starts opening the folder `root` as a project, in the background: the folder is
    /// scanned for its repositories through the files port, and the answer is applied by
    /// [`apply_files_event`](Self::apply_files_event). Until then the [`tree`](Self::tree)
    /// lists the folder among the ones being opened. A folder that is being opened already is
    /// not asked about again.
    ///
    /// The project is stored under the canonical form of `root`. A folder that is already
    /// known (compared by canonical path) is rescanned instead and keeps its id; a closed one is
    /// reopened with its sessions.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use partitur_core::testing::{
    ///     FixedClock, MemoryFiles, MemoryGit, MemoryStore, RecordingBackend, apply_files,
    ///     single_claude,
    /// };
    /// use partitur_core::{App, GlobalSettings};
    ///
    /// let files = MemoryFiles::new();
    /// files.add_folder("/work/shop/api/.git");
    /// let (backends, default) = single_claude();
    /// let store = MemoryStore::new();
    /// let mut app = App::load(
    ///     Box::new(RecordingBackend::new()),
    ///     Box::new(store.clone()),
    ///     Box::new(store),
    ///     Box::new(MemoryGit::new()),
    ///     Box::new(files.clone()),
    ///     backends,
    ///     default,
    ///     GlobalSettings::default(),
    ///     Box::new(FixedClock::default()),
    /// )?;
    ///
    /// app.open_project(Path::new("/work/shop"));
    /// assert_eq!(app.tree().opening[0].name, "shop");
    /// assert!(app.tree().projects.is_empty());
    ///
    /// // The scan, then the settings of the new project, arrive.
    /// assert!(apply_files(&mut app, &files).is_empty());
    /// assert!(app.tree().opening.is_empty());
    /// assert_eq!(app.tree().projects[0].repositories[0].name, "api");
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_project(&mut self, root: &Path) {
        if self.opening.iter().any(|opening| opening.folder == root) {
            return;
        }
        let known: Vec<ProjectId> = self
            .projects
            .iter()
            .map(|project| project.id.clone())
            .collect();
        let roots = self
            .projects
            .iter()
            .map(|project| project.root.clone())
            .collect();
        let request = self.files_request();
        self.files.query(
            request,
            vec![
                FilesQuery::Scan(root.to_path_buf()),
                FilesQuery::Resolve(roots),
            ],
        );
        self.opening.push(Opening {
            request,
            folder: root.to_path_buf(),
            stage: Stage::Scanning { known },
        });
    }

    /// Applies what the files port answered, and returns the project an opening ended with:
    /// one found again or one new, now in the [`tree`](Self::tree). The answer to the read of
    /// a session's files is kept for its prompts and its Env tab, and the prompts that waited
    /// for it go out. The paths an orchestrator's permission request names decide it once
    /// resolved. An answer to no request still waiting is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Files`] when the folder being opened could not be scanned, as when
    /// it is not a readable directory (a path that cannot be resolved is
    /// [`FilesError::NotADirectory`]); the folder is no longer being opened then. Returns
    /// [`AppError::Backend`] when a prompt that waited for a session's files, or the answer to
    /// a permission request decided on its resolved paths, cannot be queued.
    pub fn apply_files_event(&mut self, event: FilesEvent) -> Result<Option<ProjectId>, AppError> {
        let FilesEvent::Answered { request, answers } = event;
        let Some(answers) = self.context_answered(request, answers)? else {
            return Ok(None);
        };
        let Some(answers) = self.paths_resolved(request, answers)? else {
            return Ok(None);
        };
        let Some(answers) = self.cleanup_resolved(request, answers) else {
            return Ok(None);
        };
        let Some(answers) = self.folder_answered(request, answers) else {
            return Ok(None);
        };
        let Some(at) = self
            .opening
            .iter()
            .position(|opening| opening.request == request)
        else {
            return Ok(None);
        };
        let opening = self.opening.remove(at);
        let mut answers = answers.into_iter();
        match opening.stage {
            Stage::Scanning { known } => {
                let scanned = match answers.next() {
                    Some(Ok(FilesAnswer::Scanned(listing))) => listing,
                    Some(Err(error)) => return Err(AppError::Files(error)),
                    _ => return Err(unanswered()),
                };
                let resolved = match answers.next() {
                    Some(Ok(FilesAnswer::Resolved(resolved))) => resolved,
                    _ => Vec::new(),
                };
                Ok(self.scanned(opening.folder, &known, &resolved, &scanned))
            }
            Stage::Reading { project } => {
                let settings = read_settings_answer(answers.next());
                let id = project.id.clone();
                self.settings.insert(id.clone(), settings);
                self.projects.push(project);
                self.save_projects();
                Ok(Some(id))
            }
        }
    }

    /// The folders being opened, for the tree.
    pub(super) fn opening_nodes(&self) -> Vec<OpeningNode> {
        self.opening
            .iter()
            .map(|opening| OpeningNode {
                name: display_name(&opening.folder),
                folder: opening.folder.clone(),
            })
            .collect()
    }

    /// Takes the scan of a folder being opened: reopens the project it found again, or asks
    /// for the settings of the new one.
    fn scanned(
        &mut self,
        folder: PathBuf,
        known: &[ProjectId],
        resolved: &[ResolvedPath],
        listing: &crate::files::ProjectListing,
    ) -> Option<ProjectId> {
        let resolves_to_root = |project: &Project| {
            project.root == listing.root
                || known.iter().zip(resolved).any(|(id, resolved)| {
                    id == &project.id && resolved.canonical.as_ref() == Some(&listing.root)
                })
        };
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| resolves_to_root(project))
        {
            rescan(project, listing);
            project.closed = false;
            let id = project.id.clone();
            self.save_projects();
            return Some(id);
        }
        let reading = self.opening.iter().any(|opening| {
            matches!(&opening.stage, Stage::Reading { project } if project.root == listing.root)
        });
        if reading {
            return None;
        }
        let project = discover_project(listing);
        let request = self.files_request();
        self.files
            .query(request, vec![FilesQuery::Settings(project.id.clone())]);
        self.opening.push(Opening {
            request,
            folder,
            stage: Stage::Reading { project },
        });
        None
    }

    /// The id of the next request to the files port.
    pub(super) fn files_request(&mut self) -> FilesRequestId {
        self.next_files += 1;
        FilesRequestId(self.next_files)
    }
}

/// The settings a [`FilesQuery::Settings`] answered, or why they cannot be used, as the app
/// keeps them.
fn read_settings_answer(
    answer: Option<Result<FilesAnswer, FilesError>>,
) -> Result<ProjectSettings, String> {
    match answer {
        Some(Ok(FilesAnswer::Settings(settings))) => Ok(settings),
        Some(Err(error)) => Err(error.to_string()),
        _ => Err("the settings were not answered".into()),
    }
}

/// The error of an answer that does not answer what was asked, which only a broken port
/// gives.
fn unanswered() -> AppError {
    AppError::Files(FilesError::Unavailable("the scan was not answered".into()))
}
