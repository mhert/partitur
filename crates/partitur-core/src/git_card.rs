//! The Git card of the Env tab: where a repository session works and whether it drifted.
//!
//! A [`GitCardView`] holds the workspace the session was made with, the branch drift is
//! measured against, what the latest background reading of git found, and the files the
//! session's tool calls touched outside its folder. The branch drift compares the live head
//! with the expected branch; the folder drift comes from the transcript alone and needs no git.

use std::path::{Path, PathBuf};

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

/// The absolute paths the transcript's tool calls touched that are not inside `folder`,
/// each once, in the order they were first touched. Relative paths count as inside.
pub fn outside_paths(transcript: &Transcript, folder: &Path) -> Vec<PathBuf> {
    let mut outside: Vec<PathBuf> = Vec::new();
    let touched = transcript.entries().iter().flat_map(|entry| match entry {
        Entry::ToolCall { locations, .. } => locations.as_slice(),
        _ => &[],
    });
    for path in touched {
        if path.is_absolute() && !path.starts_with(folder) && !outside.contains(path) {
            outside.push(path.clone());
        }
    }
    outside
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use std::path::{Path, PathBuf};

    use super::*;
    use crate::backend::{ToolKind, ToolStatus, Update};
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
            outside_paths(&transcript, Path::new("/p/api")),
            [PathBuf::from("/p/web/b.rs")]
        );
    }

    #[test]
    fn a_sibling_folder_with_a_shared_prefix_is_outside() {
        let transcript = touching(&["/p/api-old/x.rs"]);
        assert_eq!(
            outside_paths(&transcript, Path::new("/p/api")),
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
