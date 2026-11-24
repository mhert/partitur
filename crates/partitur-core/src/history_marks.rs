//! What a session's meta keeps of its history, so that the app can answer a few questions about
//! a transcript it has not loaded without reading the history file.
//!
//! [`HistoryMarks`] says whether the transcript has any entry, whether it has an entry other
//! than a notice, and which permission requests in it nobody answered. Each mark is the answer
//! the replayed [`Transcript`](crate::Transcript) gives: applying a record to the marks changes
//! them exactly as applying it to the transcript changes those answers. Entries are only ever
//! added or replaced by an entry of the same kind, so the first two marks only ever turn true.
//! A permission request opens until an answer with its id settles it, as the transcript settles
//! the latest unanswered request with that id.

use serde::{Deserialize, Serialize};

use crate::ids::PermissionId;
use crate::record::TranscriptRecord;

/// The marks of one session's history; see the [module](self).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMarks {
    /// Whether the transcript has at least one entry, a notice included.
    pub has_entries: bool,
    /// Whether the transcript has an entry other than a notice: a prompt, an answer or another
    /// sign of a conversation.
    pub has_history: bool,
    /// The permission requests nobody answered, one id per open request, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_permissions: Vec<PermissionId>,
}

/// What a record adds to the transcript's entries.
enum Shown {
    /// No entry: it changes an existing one, or nothing the entries show.
    Nothing,
    /// A notice.
    Notice,
    /// An entry other than a notice.
    Conversation,
}

impl HistoryMarks {
    /// The marks of a history holding `records`, in order.
    pub fn of<'a>(records: impl IntoIterator<Item = &'a TranscriptRecord>) -> Self {
        let mut marks = Self::default();
        for record in records {
            marks.apply(record);
        }
        marks
    }

    /// Applies one more record of the history. Returns whether the marks changed.
    pub fn apply(&mut self, record: &TranscriptRecord) -> bool {
        let before = self.clone();
        match shown(record) {
            Shown::Nothing => {}
            Shown::Notice => self.has_entries = true,
            Shown::Conversation => {
                self.has_entries = true;
                self.has_history = true;
            }
        }
        match record {
            TranscriptRecord::PermissionRequested { id, .. } => self.open_permissions.push(*id),
            TranscriptRecord::PermissionAnswered { id, .. } => {
                if let Some(open) = self.open_permissions.iter().rposition(|open| open == id) {
                    self.open_permissions.remove(open);
                }
            }
            _ => {}
        }
        *self != before
    }

    /// Whether a permission request in the history waits for an answer.
    pub fn has_open_permission(&self) -> bool {
        !self.open_permissions.is_empty()
    }
}

/// What `record` adds to the entries of a transcript it is applied to, as
/// [`Transcript::apply`](crate::Transcript::apply) does.
fn shown(record: &TranscriptRecord) -> Shown {
    match record {
        TranscriptRecord::PermissionAnswered { .. }
        | TranscriptRecord::Usage { .. }
        | TranscriptRecord::Title(_)
        | TranscriptRecord::RunningBackground(_)
        | TranscriptRecord::ConfigOptions(_)
        | TranscriptRecord::ProposalDecided { .. }
        | TranscriptRecord::QuestionClosed { .. } => Shown::Nothing,
        TranscriptRecord::Notice(_) | TranscriptRecord::ConfigRejected { .. } => Shown::Notice,
        TranscriptRecord::UserText(_)
        | TranscriptRecord::UserAttachments(_)
        | TranscriptRecord::AgentText(_)
        | TranscriptRecord::AgentThought(_)
        | TranscriptRecord::ToolCall { .. }
        | TranscriptRecord::ToolCallUpdate { .. }
        | TranscriptRecord::PermissionRequested { .. }
        | TranscriptRecord::TurnStarted
        | TranscriptRecord::TurnEnded(_)
        | TranscriptRecord::Failed(_)
        | TranscriptRecord::ChildrenProposed { .. }
        | TranscriptRecord::QuestionAsked { .. }
        | TranscriptRecord::OrchestratorPrompt(_)
        | TranscriptRecord::PartiturPrompt(_)
        | TranscriptRecord::ChildResult { .. }
        | TranscriptRecord::ChildPrompted { .. }
        | TranscriptRecord::TitleProposed(_)
        | TranscriptRecord::Workflow { .. } => Shown::Conversation,
    }
}

#[cfg(test)]
mod tests;
