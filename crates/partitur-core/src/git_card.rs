//! The Git card of the Env tab: where a repository session works and whether it drifted.
//!
//! A [`GitCardView`] holds the workspace the session was made with, the branch drift is
//! measured against, what the latest background reading of git found, and the files the
//! session's tool calls touched outside its folder. The branch drift compares the live head
//! with the expected branch; the folder drift comes from the transcript alone and needs no git.
//!
//! A touched file counts as outside when its path is absolute and lies in none of the
//! spellings of the session's folder, in none of the ignored folders (such as the temporary
//! folder), in no hidden folder directly in the home folder (where agents keep their own
//! configuration and memory), and is no file the user attached to a prompt.

use std::path::{Path, PathBuf};

use crate::backend::SentAttachment;
use crate::git::Head;
use crate::session::Workspace;
use crate::transcript::{Entry, Transcript};

/// How many characters of a detached commit are shown.
const SHORT_SHA: usize = 7;

/// What the Env tab's Git card shows of a session that runs in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCardView {
    /// The repository's name.
    pub repository: String,
    /// The workspace the session was made with.
    pub workspace: Workspace,
    /// The branch drift is measured against, `None` while there is none to compare with.
    pub expected: Option<String>,
    /// Whether `expected` is the first branch a reading saw, rather than the one the session
    /// was made on.
    pub expected_seen: bool,
    /// What the latest reading of git found.
    pub live: LiveGit,
    /// The files the session touched outside its folder, each once, in first-touched order.
    pub outside: Vec<PathBuf>,
}

/// The state of the background reading of a session's repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveGit {
    /// No reading was asked for.
    NotChecked,
    /// A reading is in flight.
    Checking,
    /// What the reading found: the session folder's head and its uncommitted entries.
    Read {
        /// What the session's folder has checked out.
        head: Head,
        /// How many entries are uncommitted or untracked.
        uncommitted: usize,
    },
    /// The reading failed, with the reason.
    Failed(String),
}

impl GitCardView {
    /// The head the session is on now, as a label, when it differs from the expected branch:
    /// a branch by its name, a detached commit as `(detached at <short sha>)`. `None` while
    /// nothing was read or there is no expected branch.
    pub fn branch_drift(&self) -> Option<String> {
        let LiveGit::Read { head, .. } = &self.live else {
            return None;
        };
        let expected = self.expected.as_deref()?;
        match head {
            Head::Branch(name) if name == expected => None,
            head => Some(head_label(head)),
        }
    }
}

/// `head` as the card writes it: a branch by its name, a detached commit as
/// `(detached at <short sha>)`.
pub fn head_label(head: &Head) -> String {
    match head {
        Head::Branch(name) => name.clone(),
        Head::Detached(sha) => {
            let short: String = sha.chars().take(SHORT_SHA).collect();
            format!("(detached at {short})")
        }
    }
}

/// Where a session works, for telling which files it touched lie outside its folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boundary {
    /// The session's folder, in every spelling it is known by, such as as it was given and in
    /// its canonical form.
    pub inside: Vec<PathBuf>,
    /// Folders whose files never count as outside, such as the temporary folder and the
    /// session's attachment folder.
    pub ignored: Vec<PathBuf>,
    /// The home folder, whose hidden folders never count as outside.
    pub home: Option<PathBuf>,
}

impl Boundary {
    /// Whether touching `path` takes the session outside its folder.
    fn leaves(&self, path: &Path) -> bool {
        path.is_absolute()
            && !self.inside.iter().any(|folder| path.starts_with(folder))
            && !self.ignored.iter().any(|folder| path.starts_with(folder))
            && !self
                .home
                .as_deref()
                .is_some_and(|home| in_hidden_folder(path, home))
    }
}

/// Whether `path` lies in a folder directly in `home` whose name starts with a dot.
fn in_hidden_folder(path: &Path, home: &Path) -> bool {
    path.strip_prefix(home)
        .ok()
        .and_then(|rest| rest.iter().next())
        .is_some_and(|first| first.to_string_lossy().starts_with('.'))
}

/// The paths the transcript's tool calls touched outside `boundary`, each once, in the order
/// they were first touched. A file the user attached to a prompt does not count.
pub fn outside_paths(transcript: &Transcript, boundary: &Boundary) -> Vec<PathBuf> {
    let attached: Vec<&Path> = transcript
        .entries()
        .iter()
        .flat_map(|entry| match entry {
            Entry::User { attachments, .. } => attachments.as_slice(),
            _ => &[],
        })
        .filter_map(|attachment| match attachment {
            SentAttachment::File { path, .. } => Some(path.as_path()),
            SentAttachment::Image { .. } => None,
        })
        .collect();
    let mut outside: Vec<PathBuf> = Vec::new();
    let touched = transcript.entries().iter().flat_map(|entry| match entry {
        Entry::ToolCall { locations, .. } => locations.as_slice(),
        _ => &[],
    });
    for path in touched {
        if boundary.leaves(path) && !attached.contains(&path.as_path()) && !outside.contains(path) {
            outside.push(path.clone());
        }
    }
    outside
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use std::path::PathBuf;

    use super::*;
    use crate::backend::{SentAttachment, ToolKind, ToolStatus, Update};
    use crate::git::Head;
    use crate::ids::ToolCallId;
    use crate::session::Workspace;
    use crate::transcript::Transcript;

    fn touching(paths: &[&str]) -> Transcript {
        let mut transcript = Transcript::default();
        for (n, path) in paths.iter().enumerate() {
            transcript.apply(&Update::ToolCall {
                id: ToolCallId::new(format!("call-{n}")),
                title: "edit".into(),
                kind: ToolKind::Edit,
                status: ToolStatus::Completed,
                content: Vec::new(),
                locations: vec![PathBuf::from(path)],
            });
        }
        transcript
    }

    /// The boundary of a session in `/p/api`, with no other spelling, nothing ignored and no
    /// home folder.
    fn in_api() -> Boundary {
        Boundary {
            inside: vec![PathBuf::from("/p/api")],
            ignored: Vec::new(),
            home: None,
        }
    }

    #[test]
    fn a_path_in_another_spelling_of_the_folder_is_inside() {
        let transcript = touching(&["/var/home/me/api/a.rs"]);
        let mut boundary = in_api();
        boundary.inside.push(PathBuf::from("/var/home/me/api"));
        assert!(outside_paths(&transcript, &boundary).is_empty());
    }

    #[test]
    fn ignored_folders_and_hidden_folders_of_home_do_not_count() {
        let transcript = touching(&[
            "/tmp/scratch.txt",
            "/home/me/.claude/projects/p/memory/MEMORY.md",
            "/home/me/notes/todo.md",
        ]);
        let boundary = Boundary {
            ignored: vec![PathBuf::from("/tmp")],
            home: Some(PathBuf::from("/home/me")),
            ..in_api()
        };
        assert_eq!(
            outside_paths(&transcript, &boundary),
            [PathBuf::from("/home/me/notes/todo.md")]
        );
    }

    #[test]
    fn a_file_the_user_attached_does_not_count() {
        let mut transcript = touching(&["/home/me/spec.pdf", "/home/me/other.pdf"]);
        transcript.apply(&Update::UserText("read it".into()));
        transcript.apply(&Update::UserAttachments(vec![SentAttachment::File {
            name: "spec.pdf".into(),
            path: PathBuf::from("/home/me/spec.pdf"),
        }]));
        assert_eq!(
            outside_paths(&transcript, &in_api()),
            [PathBuf::from("/home/me/other.pdf")]
        );
    }

    fn card(expected: Option<&str>, live: LiveGit) -> GitCardView {
        GitCardView {
            repository: "api".into(),
            workspace: Workspace::CheckedOut,
            expected: expected.map(Into::into),
            expected_seen: false,
            live,
            outside: Vec::new(),
        }
    }

    fn read(head: Head) -> LiveGit {
        LiveGit::Read {
            head,
            uncommitted: 0,
        }
    }

    #[test]
    fn outside_paths_keeps_absolute_paths_outside_the_folder_once() {
        let transcript = touching(&[
            "/p/api/a.rs",
            "a.rs",
            "/p/web/b.rs",
            "/p/web/b.rs",
            "/p/api/sub/c.rs",
        ]);
        assert_eq!(
            outside_paths(&transcript, &in_api()),
            [PathBuf::from("/p/web/b.rs")]
        );
    }

    #[test]
    fn a_sibling_folder_with_a_shared_prefix_is_outside() {
        let transcript = touching(&["/p/api-old/x.rs"]);
        assert_eq!(
            outside_paths(&transcript, &in_api()),
            [PathBuf::from("/p/api-old/x.rs")]
        );
    }

    #[test]
    fn branch_drift_names_the_current_head_when_it_differs() {
        let main = card(Some("feat/x"), read(Head::Branch("main".into())));
        assert_eq!(main.branch_drift().as_deref(), Some("main"));
        let same = card(Some("feat/x"), read(Head::Branch("feat/x".into())));
        assert_eq!(same.branch_drift(), None);
        let detached = card(Some("feat/x"), read(Head::Detached("abc1234def".into())));
        assert_eq!(
            detached.branch_drift().as_deref(),
            Some("(detached at abc1234)")
        );
        assert_eq!(
            card(Some("feat/x"), LiveGit::NotChecked).branch_drift(),
            None
        );
        assert_eq!(
            card(None, read(Head::Branch("main".into()))).branch_drift(),
            None
        );
    }
}
