//! Turns a stream of [`Update`]s into the entries a session view renders.

use std::path::PathBuf;

use crate::backend::{PermissionOption, StopReason, ToolContent, ToolKind, ToolStatus, Update};
use crate::ids::{PermissionId, PermissionOptionId, ToolCallId};

/// One rendered item of a session's transcript.
///
/// Entries are derived from updates and are never persisted; persist the updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A message from the user.
    User(String),
    /// The agent's answer.
    Agent(String),
    /// The agent's reasoning.
    Thought(String),
    /// A tool call and its latest state.
    ToolCall {
        /// Identifies the call.
        id: ToolCallId,
        /// Human-readable summary.
        title: String,
        /// What the call does.
        kind: ToolKind,
        /// How far the call has progressed.
        status: ToolStatus,
        /// Output produced so far.
        content: Vec<ToolContent>,
        /// Files the call touches.
        locations: Vec<PathBuf>,
    },
    /// A permission request and, once settled, its outcome.
    Permission {
        /// Identifies the request.
        id: PermissionId,
        /// The call that needs permission.
        tool_call_id: ToolCallId,
        /// Human-readable summary of the call.
        title: String,
        /// What the call does.
        kind: ToolKind,
        /// The choices offered to the user.
        options: Vec<PermissionOption>,
        /// `None` while unanswered; `Some(None)` when cancelled; `Some(Some(option))` when
        /// the user picked an option.
        answer: Option<Option<PermissionOptionId>>,
    },
    /// An informational message.
    Notice(String),
    /// The end of an agent turn.
    TurnEnded(StopReason),
    /// A session failure.
    Failed(String),
}

/// The renderable state of one session, rebuilt by applying [`Update`]s in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    entries: Vec<Entry>,
    usage: Option<(u64, u64)>,
    revision: u64,
}

impl Transcript {
    /// Applies one update.
    ///
    /// Consecutive agent text (or thought) chunks merge into one entry, tool call updates
    /// patch the matching entry, a permission answer settles the latest still unanswered
    /// request with its id (a backend may reuse ids, for example after a restart), and every call
    /// increments [`revision`](Self::revision).
    ///
    /// ```
    /// use partitur_core::{Entry, Transcript, Update};
    ///
    /// let mut transcript = Transcript::default();
    /// transcript.apply(&Update::AgentText("Hel".into()));
    /// transcript.apply(&Update::AgentText("lo".into()));
    /// assert_eq!(transcript.entries(), [Entry::Agent("Hello".into())]);
    /// ```
    pub fn apply(&mut self, update: &Update) {
        self.revision = self.revision.saturating_add(1);
        match update {
            Update::UserText(text) => self.entries.push(Entry::User(text.clone())),
            Update::AgentText(chunk) => {
                if let Some(Entry::Agent(text)) = self.entries.last_mut() {
                    text.push_str(chunk);
                } else {
                    self.entries.push(Entry::Agent(chunk.clone()));
                }
            }
            Update::AgentThought(chunk) => {
                if let Some(Entry::Thought(text)) = self.entries.last_mut() {
                    text.push_str(chunk);
                } else {
                    self.entries.push(Entry::Thought(chunk.clone()));
                }
            }
            Update::ToolCall {
                id,
                title,
                kind,
                status,
                content,
                locations,
            } => {
                let entry = Entry::ToolCall {
                    id: id.clone(),
                    title: title.clone(),
                    kind: *kind,
                    status: *status,
                    content: content.clone(),
                    locations: locations.clone(),
                };
                match self.tool_call_entry(id) {
                    Some(existing) => *existing = entry,
                    None => self.entries.push(entry),
                }
            }
            Update::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
            } => self.patch_tool_call(
                id,
                title.as_ref(),
                kind.as_ref(),
                status.as_ref(),
                content.as_ref(),
            ),
            Update::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
            } => self.entries.push(Entry::Permission {
                id: *id,
                tool_call_id: tool_call_id.clone(),
                title: title.clone(),
                kind: *kind,
                options: options.clone(),
                answer: None,
            }),
            Update::PermissionAnswered { id, option } => {
                let request = self.entries.iter_mut().rev().find_map(|entry| match entry {
                    Entry::Permission {
                        id: existing,
                        answer,
                        ..
                    } if existing == id && answer.is_none() => Some(answer),
                    _ => None,
                });
                if let Some(answer) = request {
                    *answer = Some(option.clone());
                }
            }
            Update::Usage { used, size } => self.usage = Some((*used, *size)),
            Update::ConfigOptions(_) => {}
            Update::Notice(text) => self.entries.push(Entry::Notice(text.clone())),
            Update::TurnEnded(reason) => self.entries.push(Entry::TurnEnded(*reason)),
            Update::Failed(text) => self.entries.push(Entry::Failed(text.clone())),
        }
    }

    /// Builds a transcript by applying `updates` in order.
    pub fn from_updates<'a>(updates: impl IntoIterator<Item = &'a Update>) -> Self {
        let mut transcript = Self::default();
        for update in updates {
            transcript.apply(update);
        }
        transcript
    }

    /// The entries to render, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The latest context usage as `(used, size)` tokens, if the agent reported any.
    pub fn usage(&self) -> Option<(u64, u64)> {
        self.usage
    }

    /// A counter that changes with every applied update, so views can tell when to redraw.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn tool_call_entry(&mut self, id: &ToolCallId) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| matches!(entry, Entry::ToolCall { id: existing, .. } if existing == id))
    }

    /// Overwrites the given fields of the tool call `id`, creating the entry with defaults
    /// for the missing fields when the call is unknown.
    fn patch_tool_call(
        &mut self,
        id: &ToolCallId,
        new_title: Option<&String>,
        new_kind: Option<&ToolKind>,
        new_status: Option<&ToolStatus>,
        new_content: Option<&Vec<ToolContent>>,
    ) {
        if let Some(Entry::ToolCall {
            title,
            kind,
            status,
            content,
            ..
        }) = self.tool_call_entry(id)
        {
            if let Some(new) = new_title {
                title.clone_from(new);
            }
            if let Some(new) = new_kind {
                *kind = *new;
            }
            if let Some(new) = new_status {
                *status = *new;
            }
            if let Some(new) = new_content {
                content.clone_from(new);
            }
        } else {
            self.entries.push(Entry::ToolCall {
                id: id.clone(),
                title: new_title.cloned().unwrap_or_default(),
                kind: new_kind.copied().unwrap_or(ToolKind::Other),
                status: new_status.copied().unwrap_or(ToolStatus::Pending),
                content: new_content.cloned().unwrap_or_default(),
                locations: Vec::new(),
            });
        }
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
    use crate::backend::PermissionKind;

    fn agent(text: &str) -> Update {
        Update::AgentText(text.into())
    }

    fn tool_call(id: &str, title: &str) -> Update {
        Update::ToolCall {
            id: ToolCallId::new(id),
            title: title.into(),
            kind: ToolKind::Read,
            status: ToolStatus::Pending,
            content: vec![ToolContent::Text("first".into())],
            locations: vec![PathBuf::from("a.rs")],
        }
    }

    fn tool_update(id: &str) -> Update {
        Update::ToolCallUpdate {
            id: ToolCallId::new(id),
            title: None,
            kind: None,
            status: None,
            content: None,
        }
    }

    fn permission(id: u64) -> Update {
        Update::PermissionRequested {
            id: PermissionId(id),
            tool_call_id: ToolCallId::new("t-1"),
            title: "Run tests".into(),
            kind: ToolKind::Execute,
            options: vec![PermissionOption {
                id: PermissionOptionId::new("allow"),
                name: "Allow".into(),
                kind: PermissionKind::AllowOnce,
            }],
        }
    }

    #[test]
    fn consecutive_agent_chunks_merge_into_one_entry() {
        let transcript = Transcript::from_updates(&[agent("Hel"), agent("lo"), agent("!")]);
        assert_eq!(transcript.entries(), [Entry::Agent("Hello!".into())]);
    }

    #[test]
    fn a_user_message_between_chunks_starts_a_new_agent_entry() {
        let transcript = Transcript::from_updates(&[
            agent("one"),
            Update::UserText("question".into()),
            agent("two"),
        ]);
        assert_eq!(
            transcript.entries(),
            [
                Entry::Agent("one".into()),
                Entry::User("question".into()),
                Entry::Agent("two".into()),
            ]
        );
    }

    #[test]
    fn thought_chunks_merge_separately_from_text() {
        let thought = |text: &str| Update::AgentThought(text.into());
        let transcript = Transcript::from_updates(&[
            thought("hm"),
            thought("m"),
            agent("answer"),
            thought("again"),
        ]);
        assert_eq!(
            transcript.entries(),
            [
                Entry::Thought("hmm".into()),
                Entry::Agent("answer".into()),
                Entry::Thought("again".into()),
            ]
        );
    }

    #[test]
    fn a_repeated_tool_call_replaces_the_entry_in_place() {
        let transcript = Transcript::from_updates(&[
            tool_call("t-1", "old"),
            Update::Notice("between".into()),
            tool_call("t-2", "other"),
            tool_call("t-1", "new"),
        ]);
        let titles: Vec<_> = transcript
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::ToolCall { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["new", "other"]);
    }

    #[test]
    fn a_tool_call_update_patches_only_the_given_fields() {
        let mut transcript = Transcript::from_updates(&[tool_call("t-1", "Read a.rs")]);
        transcript.apply(&Update::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: None,
            kind: None,
            status: Some(ToolStatus::InProgress),
            content: None,
        });
        let expected = |title: &str, kind, status, content: &str| Entry::ToolCall {
            id: ToolCallId::new("t-1"),
            title: title.into(),
            kind,
            status,
            content: vec![ToolContent::Text(content.into())],
            locations: vec![PathBuf::from("a.rs")],
        };
        assert_eq!(
            transcript.entries(),
            [expected(
                "Read a.rs",
                ToolKind::Read,
                ToolStatus::InProgress,
                "first"
            )]
        );

        transcript.apply(&Update::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: Some("Edit a.rs".into()),
            kind: Some(ToolKind::Edit),
            status: Some(ToolStatus::Completed),
            content: Some(vec![ToolContent::Text("second".into())]),
        });
        assert_eq!(
            transcript.entries(),
            [expected(
                "Edit a.rs",
                ToolKind::Edit,
                ToolStatus::Completed,
                "second"
            )]
        );
    }

    #[test]
    fn an_update_for_an_unknown_tool_call_creates_the_entry() {
        let mut transcript = Transcript::from_updates(&[agent("text")]);
        transcript.apply(&tool_update("t-9"));
        transcript.apply(&Update::ToolCallUpdate {
            id: ToolCallId::new("t-8"),
            title: Some("Run".into()),
            kind: Some(ToolKind::Execute),
            status: Some(ToolStatus::Failed),
            content: Some(vec![ToolContent::Terminal("boom".into())]),
        });
        assert_eq!(
            transcript.entries(),
            [
                Entry::Agent("text".into()),
                Entry::ToolCall {
                    id: ToolCallId::new("t-9"),
                    title: String::new(),
                    kind: ToolKind::Other,
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    locations: Vec::new(),
                },
                Entry::ToolCall {
                    id: ToolCallId::new("t-8"),
                    title: "Run".into(),
                    kind: ToolKind::Execute,
                    status: ToolStatus::Failed,
                    content: vec![ToolContent::Terminal("boom".into())],
                    locations: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn a_permission_answer_marks_the_request_and_unknown_ids_are_ignored() {
        let mut transcript = Transcript::from_updates(&[
            Update::Notice("before".into()),
            permission(1),
            permission(2),
        ]);
        let answers = |transcript: &Transcript| -> Vec<Option<Option<PermissionOptionId>>> {
            transcript
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Permission { answer, .. } => Some(answer.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(answers(&transcript), [None, None]);

        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(2),
            option: Some(PermissionOptionId::new("allow")),
        });
        assert_eq!(
            answers(&transcript),
            [None, Some(Some(PermissionOptionId::new("allow")))]
        );

        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(1),
            option: None,
        });
        assert_eq!(
            answers(&transcript),
            [Some(None), Some(Some(PermissionOptionId::new("allow")))]
        );

        let before = transcript.entries().to_vec();
        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(99),
            option: None,
        });
        assert_eq!(transcript.entries(), before);
    }

    #[test]
    fn a_repeated_permission_id_answers_the_latest_open_request() {
        let answers = |transcript: &Transcript| -> Vec<Option<Option<PermissionOptionId>>> {
            transcript
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Permission { answer, .. } => Some(answer.clone()),
                    _ => None,
                })
                .collect()
        };
        let allow = Some(PermissionOptionId::new("allow"));
        let mut transcript = Transcript::from_updates(&[
            Update::Notice("before".into()),
            permission(1),
            Update::PermissionAnswered {
                id: PermissionId(1),
                option: allow.clone(),
            },
            permission(1),
            permission(1),
        ]);

        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(1),
            option: None,
        });
        assert_eq!(
            answers(&transcript),
            [Some(allow.clone()), None, Some(None)]
        );

        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(1),
            option: allow.clone(),
        });
        assert_eq!(
            answers(&transcript),
            [Some(allow.clone()), Some(allow.clone()), Some(None)]
        );

        let settled = transcript.entries().to_vec();
        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(1),
            option: None,
        });
        assert_eq!(transcript.entries(), settled);
    }

    #[test]
    fn usage_is_recorded_and_config_options_do_not_add_entries() {
        let mut transcript = Transcript::default();
        assert_eq!(transcript.usage(), None);
        transcript.apply(&Update::Usage {
            used: 10,
            size: 100,
        });
        assert_eq!(transcript.usage(), Some((10, 100)));
        transcript.apply(&Update::Usage {
            used: 20,
            size: 100,
        });
        assert_eq!(transcript.usage(), Some((20, 100)));
        transcript.apply(&Update::ConfigOptions(Vec::new()));
        assert!(transcript.entries().is_empty());
    }

    #[test]
    fn every_applied_update_bumps_the_revision() {
        let mut transcript = Transcript::default();
        assert_eq!(transcript.revision(), 0);
        transcript.apply(&agent("a"));
        assert_eq!(transcript.revision(), 1);
        transcript.apply(&agent("b"));
        assert_eq!(transcript.revision(), 2);
        transcript.apply(&Update::ConfigOptions(Vec::new()));
        assert_eq!(transcript.revision(), 3);
        transcript.apply(&Update::PermissionAnswered {
            id: PermissionId(5),
            option: None,
        });
        assert_eq!(transcript.revision(), 4);
    }

    #[test]
    fn from_updates_replays_in_order() {
        let transcript = Transcript::from_updates(&[
            Update::UserText("hi".into()),
            Update::Notice("note".into()),
            agent("hello"),
            Update::Failed("bad".into()),
            Update::TurnEnded(StopReason::Refusal),
        ]);
        assert_eq!(
            transcript.entries(),
            [
                Entry::User("hi".into()),
                Entry::Notice("note".into()),
                Entry::Agent("hello".into()),
                Entry::Failed("bad".into()),
                Entry::TurnEnded(StopReason::Refusal),
            ]
        );
        assert_eq!(transcript.revision(), 5);
    }
}
