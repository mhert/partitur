//! Turns a stream of [`Update`]s into the entries a session view renders.

use std::path::PathBuf;

use crate::backend::{
    ChildOutcome, Decision, PermissionOption, ProposedChild, StopReason, ToolContent, ToolKind,
    ToolStatus, TurnOrigin, Update,
};
use crate::ids::{
    PermissionId, PermissionOptionId, ProposalId, RepositoryId, SessionId, ToolCallId,
};

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
    /// An orchestrator's proposal of child sessions and, once decided, the decision.
    Proposal {
        /// Identifies the proposal.
        id: ProposalId,
        /// The proposed children.
        children: Vec<ProposedChild>,
        /// `None` while the user has not decided.
        decision: Option<Decision>,
    },
    /// A prompt the session's orchestrator sent it.
    OrchestratorPrompt(String),
    /// A prompt Partitur generated for an orchestrator.
    PartiturPrompt(String),
    /// The result of a child session's turn, as its orchestrator received it.
    ChildResult {
        /// The child session.
        child: SessionId,
        /// The repository the child works in.
        repository: RepositoryId,
        /// The child's display title.
        title: String,
        /// Who started the child's turn.
        origin: TurnOrigin,
        /// How the child's turn ended.
        outcome: ChildOutcome,
        /// The child's final answer of the turn.
        text: String,
    },
    /// A prompt the orchestrator sent one of its children with `send_to_child`.
    ChildPrompted {
        /// The child the prompt is for.
        child: SessionId,
        /// The prompt as the orchestrator wrote it.
        prompt: String,
    },
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
    /// request with its id (a backend may reuse ids, for example after a restart), a
    /// proposal's decision likewise settles the latest undecided proposal with its id, and
    /// every call increments [`revision`](Self::revision).
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
            Update::AgentText(chunk) => self.append_answer(chunk),
            Update::AgentThought(chunk) => self.append_thought(chunk),
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
                self.replace_tool_call(id, entry);
            }
            Update::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                raw_output,
            } => self.patch_tool_call(
                id,
                title.as_ref(),
                kind.as_ref(),
                status.as_ref(),
                content.as_ref(),
                raw_output.as_ref(),
            ),
            Update::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
                ..
            } => self.entries.push(Entry::Permission {
                id: *id,
                tool_call_id: tool_call_id.clone(),
                title: title.clone(),
                kind: *kind,
                options: options.clone(),
                answer: None,
            }),
            Update::PermissionAnswered { id, option } => {
                self.answer_permission(*id, option.clone());
            }
            Update::Usage { used, size } => self.usage = Some((*used, *size)),
            Update::ConfigOptions(_) => {}
            Update::Notice(text) => self.entries.push(Entry::Notice(text.clone())),
            Update::TurnEnded(reason) => self.entries.push(Entry::TurnEnded(*reason)),
            Update::Failed(text) => self.entries.push(Entry::Failed(text.clone())),
            Update::ChildrenProposed { id, children } => self.entries.push(Entry::Proposal {
                id: id.clone(),
                children: children.clone(),
                decision: None,
            }),
            Update::ProposalDecided { id, decision } => self.decide_proposal(id, decision),
            Update::OrchestratorPrompt(text) => {
                self.entries.push(Entry::OrchestratorPrompt(text.clone()));
            }
            Update::PartiturPrompt(text) => self.entries.push(Entry::PartiturPrompt(text.clone())),
            Update::ChildResult {
                child,
                repository,
                title,
                origin,
                outcome,
                text,
            } => self.entries.push(Entry::ChildResult {
                child: child.clone(),
                repository: repository.clone(),
                title: title.clone(),
                origin: *origin,
                outcome: outcome.clone(),
                text: text.clone(),
            }),
            Update::ChildPrompted { child, prompt } => self.entries.push(Entry::ChildPrompted {
                child: child.clone(),
                prompt: prompt.clone(),
            }),
        }
    }

    /// Adds `chunk` to the answer the transcript ends with, or starts a new answer with it.
    fn append_answer(&mut self, chunk: &str) {
        if let Some(Entry::Agent(text)) = self.entries.last_mut() {
            text.push_str(chunk);
        } else {
            self.entries.push(Entry::Agent(chunk.to_owned()));
        }
    }

    /// Adds `chunk` to the thought the transcript ends with, or starts a new thought with
    /// it.
    fn append_thought(&mut self, chunk: &str) {
        if let Some(Entry::Thought(text)) = self.entries.last_mut() {
            text.push_str(chunk);
        } else {
            self.entries.push(Entry::Thought(chunk.to_owned()));
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

    /// The children of the latest undecided proposal `id`, `None` when every proposal with
    /// that id is decided or there is none.
    pub fn pending_proposal(&self, id: &ProposalId) -> Option<&[ProposedChild]> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Proposal {
                id: existing,
                children,
                decision: None,
            } if existing == id => Some(children.as_slice()),
            _ => None,
        })
    }

    /// The permission requests nobody has answered yet, oldest first.
    pub fn unanswered_permissions(&self) -> impl Iterator<Item = PermissionId> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Permission {
                id, answer: None, ..
            } => Some(*id),
            _ => None,
        })
    }

    /// The proposals the user has not decided yet, oldest first, with their children.
    pub fn pending_proposals(&self) -> impl Iterator<Item = (&ProposalId, &[ProposedChild])> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Proposal {
                id,
                children,
                decision: None,
            } => Some((id, children.as_slice())),
            _ => None,
        })
    }

    /// Settles the latest unanswered permission request `id` with `option`; does nothing
    /// when none is open.
    fn answer_permission(&mut self, id: PermissionId, option: Option<PermissionOptionId>) {
        let request = self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Permission {
                id: existing,
                answer,
                ..
            } if *existing == id && answer.is_none() => Some(answer),
            _ => None,
        });
        if let Some(answer) = request {
            *answer = Some(option);
        }
    }

    /// Records `decision` on the latest undecided proposal `id`; does nothing when none is
    /// open.
    fn decide_proposal(&mut self, id: &ProposalId, decision: &Decision) {
        let open = self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Proposal {
                id: existing,
                decision,
                ..
            } if existing == id && decision.is_none() => Some(decision),
            _ => None,
        });
        if let Some(open) = open {
            *open = Some(decision.clone());
        }
    }

    fn tool_call_entry(&mut self, id: &ToolCallId) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| matches!(entry, Entry::ToolCall { id: existing, .. } if existing == id))
    }

    /// Replaces the entry of the tool call `id` with `entry`, or appends `entry` when the
    /// call is new.
    fn replace_tool_call(&mut self, id: &ToolCallId, entry: Entry) {
        match self.tool_call_entry(id) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
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
        raw_output: Option<&String>,
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
            fill_from_raw_output(content, raw_output);
        } else {
            let mut content = new_content.cloned().unwrap_or_default();
            fill_from_raw_output(&mut content, raw_output);
            self.entries.push(Entry::ToolCall {
                id: id.clone(),
                title: new_title.cloned().unwrap_or_default(),
                kind: new_kind.copied().unwrap_or(ToolKind::Other),
                status: new_status.copied().unwrap_or(ToolStatus::Pending),
                content,
                locations: Vec::new(),
            });
        }
    }
}

/// Shows a tool's raw output as its text when the entry has no content: content such as a diff
/// is never replaced by the raw result that accompanies it.
fn fill_from_raw_output(content: &mut Vec<ToolContent>, raw_output: Option<&String>) {
    if let (true, Some(text)) = (content.is_empty(), raw_output) {
        content.push(ToolContent::Text(text.clone()));
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
            raw_output: None,
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
            name: None,
        }
    }

    fn child(repository: &str, title: &str) -> ProposedChild {
        ProposedChild {
            repository: RepositoryId::try_from(repository).unwrap(),
            prompt: format!("Work on {repository}"),
            title: title.into(),
        }
    }

    #[test]
    fn a_proposal_is_decided_once_and_pending_proposal_finds_the_latest_open_one() {
        let id = ProposalId::new();
        let children = vec![child("api", "API endpoint")];
        let children2 = vec![child("api", "API endpoint"), child("web", "Web page")];
        let rejected = Decision::Rejected {
            note: Some("later".into()),
        };
        let mut transcript = Transcript::from_updates(&[
            Update::ChildrenProposed {
                id: id.clone(),
                children: children.clone(),
            },
            Update::ProposalDecided {
                id: id.clone(),
                decision: rejected.clone(),
            },
            Update::ChildrenProposed {
                id: id.clone(),
                children: children2.clone(),
            },
        ]);
        assert_eq!(
            transcript.entries(),
            [
                Entry::Proposal {
                    id: id.clone(),
                    children,
                    decision: Some(rejected.clone()),
                },
                Entry::Proposal {
                    id: id.clone(),
                    children: children2.clone(),
                    decision: None,
                },
            ]
        );
        assert_eq!(transcript.pending_proposal(&id), Some(children2.as_slice()));

        transcript.apply(&Update::ProposalDecided {
            id: id.clone(),
            decision: Decision::Approved(Vec::new()),
        });
        assert_eq!(transcript.pending_proposal(&id), None);
        assert!(matches!(
            transcript.entries(),
            [
                Entry::Proposal { decision: Some(first), .. },
                Entry::Proposal { decision: Some(Decision::Approved(_)), .. },
            ] if *first == rejected
        ));

        let decided = transcript.entries().to_vec();
        let unknown = ProposalId::new();
        transcript.apply(&Update::ProposalDecided {
            id: unknown.clone(),
            decision: Decision::Rejected { note: None },
        });
        assert_eq!(transcript.entries(), decided);
        assert_eq!(transcript.pending_proposal(&unknown), None);
    }

    #[test]
    fn pending_proposals_lists_the_undecided_ones_oldest_first() {
        let (decided, first, second) = (ProposalId::new(), ProposalId::new(), ProposalId::new());
        let transcript = Transcript::from_updates(&[
            Update::ChildrenProposed {
                id: decided.clone(),
                children: vec![child("api", "API endpoint")],
            },
            Update::ChildrenProposed {
                id: first.clone(),
                children: vec![child("web", "Web page")],
            },
            Update::ProposalDecided {
                id: decided,
                decision: Decision::Rejected { note: None },
            },
            Update::ChildrenProposed {
                id: second.clone(),
                children: vec![child("api", "API test")],
            },
        ]);

        let pending: Vec<_> = transcript.pending_proposals().collect();

        assert_eq!(
            pending,
            [
                (&first, [child("web", "Web page")].as_slice()),
                (&second, [child("api", "API test")].as_slice()),
            ]
        );
        assert_eq!(
            transcript.pending_proposal(&first),
            Some([child("web", "Web page")].as_slice())
        );
        assert_eq!(transcript.pending_proposal(&ProposalId::new()), None);
    }

    #[test]
    fn prompts_and_results_become_their_own_entries() {
        let child = SessionId::new();
        let api = RepositoryId::try_from("api").unwrap();
        let transcript = Transcript::from_updates(&[
            Update::OrchestratorPrompt("do it".into()),
            Update::PartiturPrompt("children started".into()),
            agent("working"),
            Update::ChildResult {
                child: child.clone(),
                repository: api.clone(),
                title: "API endpoint".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Failed("boom".into()),
                text: "half done".into(),
            },
            agent("next"),
            Update::ChildPrompted {
                child: child.clone(),
                prompt: "Also add tests".into(),
            },
        ]);
        assert_eq!(
            transcript.entries(),
            [
                Entry::OrchestratorPrompt("do it".into()),
                Entry::PartiturPrompt("children started".into()),
                Entry::Agent("working".into()),
                Entry::ChildResult {
                    child: child.clone(),
                    repository: api,
                    title: "API endpoint".into(),
                    origin: TurnOrigin::User,
                    outcome: ChildOutcome::Failed("boom".into()),
                    text: "half done".into(),
                },
                Entry::Agent("next".into()),
                Entry::ChildPrompted {
                    child,
                    prompt: "Also add tests".into(),
                },
            ]
        );
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
            raw_output: None,
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
            raw_output: None,
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
    fn raw_output_fills_an_empty_tool_entry_but_never_replaces_a_diff() {
        let raw = |content, raw_output: &str| Update::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: None,
            kind: None,
            status: None,
            content,
            raw_output: Some(raw_output.into()),
        };
        let entry = |title: &str, content| {
            [Entry::ToolCall {
                id: ToolCallId::new("t-1"),
                title: title.into(),
                kind: if title.is_empty() {
                    ToolKind::Other
                } else {
                    ToolKind::Edit
                },
                status: if title.is_empty() {
                    ToolStatus::Pending
                } else {
                    ToolStatus::InProgress
                },
                content,
                locations: Vec::new(),
            }]
        };
        let diff = ToolContent::Diff {
            path: PathBuf::from("a.rs"),
            old: None,
            new: "x".into(),
        };
        let call = |content| Update::ToolCall {
            id: ToolCallId::new("t-1"),
            title: "Edit".into(),
            kind: ToolKind::Edit,
            status: ToolStatus::InProgress,
            content,
            locations: Vec::new(),
        };

        let mut transcript = Transcript::from_updates(&[call(vec![diff.clone()])]);
        transcript.apply(&raw(None, "ok"));
        assert_eq!(transcript.entries(), entry("Edit", vec![diff]));

        let mut transcript = Transcript::from_updates(&[call(Vec::new())]);
        transcript.apply(&raw(None, "ok"));
        assert_eq!(
            transcript.entries(),
            entry("Edit", vec![ToolContent::Text("ok".into())])
        );

        let mut transcript = Transcript::from_updates(&[call(Vec::new())]);
        transcript.apply(&raw(Some(vec![ToolContent::Text("new".into())]), "raw"));
        assert_eq!(
            transcript.entries(),
            entry("Edit", vec![ToolContent::Text("new".into())])
        );

        let mut transcript = Transcript::default();
        transcript.apply(&raw(None, "ok"));
        assert_eq!(
            transcript.entries(),
            entry("", vec![ToolContent::Text("ok".into())])
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
            raw_output: None,
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
