//! The port Partitur reads the file system through, and the vocabulary it speaks.
//!
//! [`Files`] is a command port like [`Git`](crate::Git): [`Files::query`] queues a request of
//! read-only [`FilesQuery`]s and returns at once. Every query of the request runs, and the
//! outcome arrives later as one [`FilesEvent`] naming the same request, with one answer per
//! query in order, so the core never waits on the disk.
//!
//! A [`FilesQuery::Scan`] lists what a project folder holds: its canonical path, which of the
//! [`SCAN_ENTRIES`] it holds, and, unless it holds `.git` itself, every child folder with the
//! entries it holds. Which of them are repositories is the core's to decide. A
//! [`FilesQuery::Read`] reads a whole text file, a missing file being no error and one that is
//! not UTF-8 failing with the conversion's message, and a
//! [`FilesQuery::Resolve`] tells for each path where it resolves to, whether it is a symbolic
//! link and whether anything is there at all. A [`FilesQuery::Settings`] reads the `settings.toml` of a project from the
//! data folder; it is the one query that may write, since a project without the file gets the
//! commented template written there, as the store has always done when it first read them.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::ids::ProjectId;
use crate::settings::ProjectSettings;

/// The entries a [`FilesQuery::Scan`] looks for in the project folder and in each child
/// folder: the git entry, a folder or a file as in a linked worktree, and the agent
/// instruction files.
pub const SCAN_ENTRIES: [&str; 3] = [".git", "AGENTS.md", "CLAUDE.md"];

/// A command port to the file system, like [`Git`](crate::Git): [`query`](Self::query) queues
/// a request and returns at once; the outcome arrives later as one [`FilesEvent`] naming the
/// same `request`.
pub trait Files: Send {
    /// Queues `queries`. All of them run; [`FilesEvent::Answered`] carries one answer per
    /// query, in order. A request that cannot run is answered all the same, with
    /// [`FilesError::Unavailable`] for each query.
    fn query(&self, request: FilesRequestId, queries: Vec<FilesQuery>);
}

/// Correlates a request queued through the [`Files`] port with the event that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FilesRequestId(pub u64);

impl fmt::Display for FilesRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "files-{}", self.0)
    }
}

/// A question about the file system. Every query only reads, but for
/// [`Settings`](Self::Settings), which writes the commented template for a project that has no
/// `settings.toml` yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesQuery {
    /// What the project folder at this path holds, answered with [`FilesAnswer::Scanned`].
    Scan(PathBuf),
    /// The whole text file at this path, answered with [`FilesAnswer::Read`].
    Read(PathBuf),
    /// Where each of these paths resolves to, answered with [`FilesAnswer::Resolved`].
    Resolve(Vec<PathBuf>),
    /// The settings of this project from its `settings.toml`, answered with
    /// [`FilesAnswer::Settings`]. A project without the file gets the commented template
    /// written and the defaults.
    Settings(ProjectId),
}

/// The answer to one [`FilesQuery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesAnswer {
    /// What a [`FilesQuery::Scan`] found.
    Scanned(ProjectListing),
    /// The text of a [`FilesQuery::Read`], `None` when there is no such file.
    Read(Option<String>),
    /// One fact per path of a [`FilesQuery::Resolve`], in order.
    Resolved(Vec<ResolvedPath>),
    /// The settings a [`FilesQuery::Settings`] read.
    Settings(ProjectSettings),
}

/// What a project folder holds, as a [`FilesQuery::Scan`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectListing {
    /// The folder's canonical path.
    pub root: PathBuf,
    /// Which of the [`SCAN_ENTRIES`] the folder holds, in that order.
    pub entries: Vec<String>,
    /// Every child folder, in no particular order, hidden ones included; none when the
    /// folder holds `.git`, since a repository's children are not searched. A child that
    /// cannot be inspected is listed as holding nothing.
    pub children: Vec<FolderListing>,
}

/// A child folder of a [`ProjectListing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderListing {
    /// The child's path: the listing's root joined with its name.
    pub path: PathBuf,
    /// Which of the [`SCAN_ENTRIES`] it holds, in that order.
    pub entries: Vec<String>,
}

/// What a [`FilesQuery::Resolve`] learnt about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The path as it was asked about.
    pub given: PathBuf,
    /// Its canonical form, with every symbolic link resolved; `None` when it cannot be
    /// resolved, as when it does not exist.
    pub canonical: Option<PathBuf>,
    /// Whether the path itself is a symbolic link.
    pub is_symlink: bool,
    /// Whether anything is at the path itself, without following a symbolic link there: a
    /// link that leads nowhere is present, a path below a missing folder is not.
    pub present: bool,
}

/// The outcome of one request queued through [`Files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesEvent {
    /// The answers to a [`Files::query`] request, one per query, in order.
    Answered {
        /// The request being answered.
        request: FilesRequestId,
        /// One answer or error per query.
        answers: Vec<Result<FilesAnswer, FilesError>>,
    },
}

/// Why a query failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FilesError {
    /// The path does not exist or is not a directory.
    #[error("{} is not a directory", path.display())]
    NotADirectory {
        /// The path that was expected to be a directory.
        path: PathBuf,
    },
    /// Reading the file system failed.
    #[error("cannot read {}: {message}", path.display())]
    Io {
        /// The path being read.
        path: PathBuf,
        /// What the operating system reported.
        message: String,
    },
    /// The project's settings could not be read or are invalid; the text says why.
    #[error("{0}")]
    Settings(String),
    /// The port could not run the request.
    #[error("the file system is unavailable: {0}")]
    Unavailable(String),
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

    #[test]
    fn a_request_is_named_after_its_number() {
        assert_eq!(FilesRequestId(7).to_string(), "files-7");
    }

    #[test]
    fn errors_read_as_the_project_errors_did() {
        let path = PathBuf::from("/work");
        assert_eq!(
            FilesError::NotADirectory { path: path.clone() }.to_string(),
            "/work is not a directory"
        );
        assert_eq!(
            FilesError::Io {
                path,
                message: "denied".into()
            }
            .to_string(),
            "cannot read /work: denied"
        );
        assert_eq!(FilesError::Settings("bad".into()).to_string(), "bad");
        assert_eq!(
            FilesError::Unavailable("gone".into()).to_string(),
            "the file system is unavailable: gone"
        );
    }
}
