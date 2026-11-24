//! What a session's transcript records. [`TranscriptRecord`] holds every update an agent
//! backend reports, under the same name, together with what only Partitur records: the
//! user's prompts, answered permissions, orchestration and questions, and workflow progress.
//! It is the form the store writes as JSON lines, and its JSON is the one earlier versions
//! wrote, so histories load both ways.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::{
    AgentUpdate, ChildOutcome, ChildState, ConfigOption, Decision, GateDecision, PermissionOption,
    ProposedChild, SentAttachment, StopReason, TitleProposal, ToolContent, ToolKind, ToolStatus,
    TurnOrigin, WidgetSlot,
};
use crate::ids::{
    ConfigId, ConfigValueId, PermissionId, PermissionOptionId, ProposalId, QuestionId,
    RepositoryId, SessionId, ToolCallId,
};
use crate::question::{Question, QuestionOutcome};
use crate::workflow::{Decider, WorkflowEvent};

/// One entry of a session's history: an [`AgentUpdate`] its backend reported, or something
/// only Partitur records, such as the user's prompt or a decision on a proposal.
///
/// Records are persisted as JSON lines and replayed through
/// [`Transcript`](crate::Transcript) to rebuild a session. The variants share their names and
/// fields with [`AgentUpdate`], so an update is written exactly as its record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptRecord {
    /// A chunk of the user's message.
    UserText(String),
    /// The attachments of the [`UserText`](Self::UserText) just before it.
    UserAttachments(Vec<SentAttachment>),
    /// A chunk of the agent's answer.
    AgentText(String),
    /// A chunk of the agent's reasoning.
    AgentThought(String),
    /// A tool call was announced.
    ToolCall {
        /// Identifies the call in later updates.
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
        /// The tool's input as JSON text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input: Option<String>,
        /// The MCP tool's `CallToolResult` as JSON text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_result: Option<String>,
        /// The MCP App widget the tool declares.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        widget: Option<WidgetSlot>,
    },
    /// A tool call changed; only the fields that are `Some` changed.
    ToolCallUpdate {
        /// The call being updated.
        id: ToolCallId,
        /// The new summary.
        title: Option<String>,
        /// The new kind.
        kind: Option<ToolKind>,
        /// The new status.
        status: Option<ToolStatus>,
        /// The new output, replacing the previous output: what the agent sent as content.
        content: Option<Vec<ToolContent>>,
        /// The tool's raw result as text. It only fills an entry that has no content after
        /// `content` is applied; it never replaces content such as a diff.
        #[serde(default)]
        raw_output: Option<String>,
        /// The tool's new input as JSON text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_input: Option<String>,
        /// The MCP tool's `CallToolResult` as JSON text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_result: Option<String>,
        /// The new state of the call's MCP App widget.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        widget: Option<WidgetSlot>,
    },
    /// The agent asks for permission to run a tool call.
    PermissionRequested {
        /// Identifies the request when answering.
        id: PermissionId,
        /// The call that needs permission.
        tool_call_id: ToolCallId,
        /// Human-readable summary of the call.
        title: String,
        /// What the call does.
        kind: ToolKind,
        /// The choices offered to the user.
        options: Vec<PermissionOption>,
        /// The backend's raw name of the tool, for example
        /// `mcp__partitur__propose_children`, `None` when it sent none.
        #[serde(default)]
        name: Option<String>,
        /// The paths the call is about, empty for a command or when the agent sent none.
        #[serde(default)]
        locations: Vec<PathBuf>,
        /// Whether the tool belongs to Partitur's own tool server. A request logged before
        /// the backend flagged these loads as `false`.
        #[serde(default)]
        partitur_tool: bool,
    },
    /// A permission request was settled.
    PermissionAnswered {
        /// The request that was settled.
        id: PermissionId,
        /// The chosen option, `None` when the request was cancelled.
        option: Option<PermissionOptionId>,
    },
    /// The context window usage changed.
    Usage {
        /// Tokens currently in the context.
        used: u64,
        /// Size of the context window in tokens.
        size: u64,
    },
    /// The agent's short title for the session.
    Title(String),
    /// The number of background tasks the session's agent lists as running in the background, sent
    /// whenever that number changes. It is live state: it is never stored.
    RunningBackground(usize),
    /// The available configuration options changed.
    ConfigOptions(Vec<ConfigOption>),
    /// The agent refused to change the config option `option` to `value`; the option keeps
    /// the value it had.
    ConfigRejected {
        /// The option that was to change.
        option: ConfigId,
        /// The value it was to take.
        value: ConfigValueId,
        /// Why the agent refused.
        error: String,
    },
    /// An informational message from Partitur or the backend.
    Notice(String),
    /// The agent started a turn without a prompt, for example when a background task finished.
    TurnStarted,
    /// The agent's turn ended.
    TurnEnded(StopReason),
    /// The session failed.
    Failed(String),
    /// An orchestrator proposed child sessions; the proposal waits for the user's decision.
    ChildrenProposed {
        /// Identifies the proposal when deciding it.
        id: ProposalId,
        /// The proposed children.
        children: Vec<ProposedChild>,
    },
    /// The user decided a proposal.
    ProposalDecided {
        /// The proposal that was decided.
        id: ProposalId,
        /// What the user decided.
        decision: Decision,
    },
    /// The session asked the user a question; it waits for the answer.
    QuestionAsked {
        /// Identifies the question when closing it.
        id: QuestionId,
        /// The question and its options.
        question: Question,
    },
    /// A question was answered, cancelled or replaced.
    QuestionClosed {
        /// The question that was closed.
        id: QuestionId,
        /// How it was closed.
        outcome: QuestionOutcome,
        /// Who closed it; the user when the record does not say.
        #[serde(default, skip_serializing_if = "Decider::is_user")]
        decider: Decider,
    },
    /// A prompt the child's orchestrator sent it.
    OrchestratorPrompt(String),
    /// A prompt Partitur generated for an orchestrator.
    PartiturPrompt(String),
    /// A child session's turn ended and its result was reported to its orchestrator.
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
        /// Where the child stood in its workflow; no workflow and no wait in a record made
        /// before it was kept.
        #[serde(default)]
        state: ChildState,
    },
    /// An orchestrator sent one of its children a prompt with `send_to_child`; recorded in the
    /// orchestrator's transcript, whether the prompt went out at once or waits for the child.
    ChildPrompted {
        /// The child the prompt is for.
        child: SessionId,
        /// The prompt as the orchestrator wrote it.
        prompt: String,
        /// The decision on the child's gate the prompt came with; `None` for a plain prompt
        /// and in a record made before decisions were kept.
        #[serde(default)]
        decision: Option<GateDecision>,
    },
    /// A child proposed a new title for itself with `propose_title`, and the proposal was
    /// delivered to its orchestrator; recorded in the orchestrator's transcript.
    TitleProposed(TitleProposal),
    /// The session reported progress in its workflow, or the user answered one of its gates.
    Workflow {
        /// When it happened, in seconds since the Unix epoch.
        at: u64,
        /// What happened.
        event: WorkflowEvent,
    },
}

impl From<AgentUpdate> for TranscriptRecord {
    fn from(update: AgentUpdate) -> Self {
        match update {
            AgentUpdate::AgentText(value) => Self::AgentText(value),
            AgentUpdate::AgentThought(value) => Self::AgentThought(value),
            AgentUpdate::ToolCall {
                id,
                title,
                kind,
                status,
                content,
                locations,
                raw_input,
                call_result,
                widget,
            } => Self::ToolCall {
                id,
                title,
                kind,
                status,
                content,
                locations,
                raw_input,
                call_result,
                widget,
            },
            AgentUpdate::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                raw_output,
                raw_input,
                call_result,
                widget,
            } => Self::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                raw_output,
                raw_input,
                call_result,
                widget,
            },
            AgentUpdate::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
                name,
                locations,
                partitur_tool,
            } => Self::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
                name,
                locations,
                partitur_tool,
            },
            AgentUpdate::Usage { used, size } => Self::Usage { used, size },
            AgentUpdate::Title(value) => Self::Title(value),
            AgentUpdate::RunningBackground(value) => Self::RunningBackground(value),
            AgentUpdate::ConfigOptions(value) => Self::ConfigOptions(value),
            AgentUpdate::ConfigRejected {
                option,
                value,
                error,
            } => Self::ConfigRejected {
                option,
                value,
                error,
            },
            AgentUpdate::Notice(value) => Self::Notice(value),
            AgentUpdate::TurnStarted => Self::TurnStarted,
            AgentUpdate::TurnEnded(value) => Self::TurnEnded(value),
            AgentUpdate::Failed(value) => Self::Failed(value),
        }
    }
}

impl TryFrom<TranscriptRecord> for AgentUpdate {
    type Error = TranscriptRecord;

    /// The update an agent reports for `record`.
    ///
    /// # Errors
    ///
    /// Returns `record` itself when only Partitur records it, such as the user's prompt.
    fn try_from(record: TranscriptRecord) -> Result<Self, Self::Error> {
        match record {
            TranscriptRecord::AgentText(value) => Ok(Self::AgentText(value)),
            TranscriptRecord::AgentThought(value) => Ok(Self::AgentThought(value)),
            TranscriptRecord::ToolCall {
                id,
                title,
                kind,
                status,
                content,
                locations,
                raw_input,
                call_result,
                widget,
            } => Ok(Self::ToolCall {
                id,
                title,
                kind,
                status,
                content,
                locations,
                raw_input,
                call_result,
                widget,
            }),
            TranscriptRecord::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                raw_output,
                raw_input,
                call_result,
                widget,
            } => Ok(Self::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                raw_output,
                raw_input,
                call_result,
                widget,
            }),
            TranscriptRecord::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
                name,
                locations,
                partitur_tool,
            } => Ok(Self::PermissionRequested {
                id,
                tool_call_id,
                title,
                kind,
                options,
                name,
                locations,
                partitur_tool,
            }),
            TranscriptRecord::Usage { used, size } => Ok(Self::Usage { used, size }),
            TranscriptRecord::Title(value) => Ok(Self::Title(value)),
            TranscriptRecord::RunningBackground(value) => Ok(Self::RunningBackground(value)),
            TranscriptRecord::ConfigOptions(value) => Ok(Self::ConfigOptions(value)),
            TranscriptRecord::ConfigRejected {
                option,
                value,
                error,
            } => Ok(Self::ConfigRejected {
                option,
                value,
                error,
            }),
            TranscriptRecord::Notice(value) => Ok(Self::Notice(value)),
            TranscriptRecord::TurnStarted => Ok(Self::TurnStarted),
            TranscriptRecord::TurnEnded(value) => Ok(Self::TurnEnded(value)),
            TranscriptRecord::Failed(value) => Ok(Self::Failed(value)),
            only_partitur @ (TranscriptRecord::UserText(_)
            | TranscriptRecord::UserAttachments(_)
            | TranscriptRecord::PermissionAnswered { .. }
            | TranscriptRecord::ChildrenProposed { .. }
            | TranscriptRecord::ProposalDecided { .. }
            | TranscriptRecord::QuestionAsked { .. }
            | TranscriptRecord::QuestionClosed { .. }
            | TranscriptRecord::OrchestratorPrompt(_)
            | TranscriptRecord::PartiturPrompt(_)
            | TranscriptRecord::ChildResult { .. }
            | TranscriptRecord::ChildPrompted { .. }
            | TranscriptRecord::TitleProposed(_)
            | TranscriptRecord::Workflow { .. }) => Err(only_partitur),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::SuperpowersV7Event;
    use crate::backend::{
        ApprovedChild, ChildWait, ConfigCategory, ConfigChoice, PermissionKind, Widget,
    };
    use crate::session::{BranchMode, TitleSource};

    fn repository(name: &str) -> RepositoryId {
        RepositoryId::try_from(name).unwrap()
    }

    fn proposed(name: &str, title: &str) -> ProposedChild {
        ProposedChild {
            repository: repository(name),
            prompt: format!("Work on {name}"),
            title: title.into(),
            title_source: TitleSource::Given,
            base: None,
        }
    }

    /// One history line of each kind v0.4.0 wrote, with the optional fields both set and left
    /// out.
    const V0_4_0_HISTORY: &str = include_str!("../tests/fixtures/v0_4_0_history.jsonl");

    #[test]
    fn v0_4_0_history_lines_load_as_records() {
        assert_eq!(V0_4_0_HISTORY.lines().count(), 47);
        for line in V0_4_0_HISTORY.lines() {
            let record: TranscriptRecord = serde_json::from_str(line).unwrap();
            assert_eq!(serde_json::to_string(&record).unwrap(), line);
        }
    }

    #[test]
    fn an_agent_update_is_written_as_its_record() {
        let mut updates = 0;
        for line in V0_4_0_HISTORY.lines() {
            let record: TranscriptRecord = serde_json::from_str(line).unwrap();
            let Ok(update) = serde_json::from_str::<AgentUpdate>(line) else {
                assert_eq!(AgentUpdate::try_from(record.clone()), Err(record), "{line}");
                continue;
            };
            updates += 1;
            assert_eq!(serde_json::to_string(&update).unwrap(), line);
            assert_eq!(AgentUpdate::try_from(record), Ok(update.clone()), "{line}");
            assert_eq!(
                serde_json::to_string(&TranscriptRecord::from(update)).unwrap(),
                line
            );
        }
        // Every line but those only Partitur records: the user's prompts, answers,
        // orchestration, questions and workflow progress.
        assert_eq!(updates, 22);
    }

    #[test]
    fn user_attachments_survive_json() {
        let update = TranscriptRecord::UserAttachments(vec![SentAttachment::Image {
            name: "s.png".into(),
            copy: "1-s.png".into(),
            preview: None,
        }]);
        let json = serde_json::to_string(&update).unwrap();
        assert_eq!(
            serde_json::from_str::<TranscriptRecord>(&json).unwrap(),
            update
        );
    }

    #[test]
    fn updates_round_trip_through_json() {
        let mut updates = vec![
            TranscriptRecord::AgentText("hi".into()),
            TranscriptRecord::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Edit main.rs".into(),
                kind: ToolKind::Edit,
                status: ToolStatus::InProgress,
                content: vec![
                    ToolContent::Diff {
                        path: PathBuf::from("src/main.rs"),
                        old: None,
                        new: "fn main() {}".into(),
                    },
                    ToolContent::Terminal("ok".into()),
                    ToolContent::Patch {
                        paths: vec![PathBuf::from("src/main.rs")],
                        text: "--- a/src/main.rs\n+++ b/src/main.rs\n".into(),
                    },
                ],
                locations: vec![PathBuf::from("src/main.rs")],
                raw_input: None,
                call_result: None,
                widget: None,
            },
            TranscriptRecord::PermissionRequested {
                id: PermissionId(3),
                tool_call_id: ToolCallId::new("t-1"),
                title: "Edit main.rs".into(),
                kind: ToolKind::Edit,
                options: vec![PermissionOption {
                    id: PermissionOptionId::new("allow"),
                    name: "Allow".into(),
                    kind: PermissionKind::AllowOnce,
                }],
                name: None,
                locations: Vec::new(),
                partitur_tool: false,
            },
            TranscriptRecord::PermissionRequested {
                id: PermissionId(4),
                tool_call_id: ToolCallId::new("t-2"),
                title: "Propose children".into(),
                kind: ToolKind::Other,
                options: Vec::new(),
                name: Some("mcp__partitur__propose_children".into()),
                locations: Vec::new(),
                partitur_tool: true,
            },
            TranscriptRecord::PermissionAnswered {
                id: PermissionId(3),
                option: None,
            },
            TranscriptRecord::ConfigOptions(vec![ConfigOption {
                id: ConfigId::new("model"),
                name: "Model".into(),
                category: ConfigCategory::Other("custom".into()),
                current: ConfigValueId::new("fast"),
                choices: vec![ConfigChoice {
                    id: ConfigValueId::new("fast"),
                    name: "Fast".into(),
                }],
            }]),
            TranscriptRecord::ConfigRejected {
                option: ConfigId::new("model"),
                value: ConfigValueId::new("opus"),
                error: "not offered".into(),
            },
            TranscriptRecord::TurnEnded(StopReason::EndTurn),
        ];
        updates.extend(orchestration_updates());
        updates.push(TranscriptRecord::Workflow {
            at: 7,
            event: WorkflowEvent::from(SuperpowersV7Event::StartTask {
                n: 4,
                note: Some("x".into()),
            }),
        });
        for update in updates {
            let json = serde_json::to_string(&update).unwrap();
            assert_eq!(
                serde_json::from_str::<TranscriptRecord>(&json).unwrap(),
                update
            );
        }
    }

    #[test]
    fn a_permission_request_logged_before_names_loads_without_one() {
        let old = serde_json::from_str::<TranscriptRecord>(
            r#"{"PermissionRequested":{"id":1,"tool_call_id":"t","title":"","kind":"Read","options":[]}}"#,
        )
        .unwrap();
        assert_eq!(
            old,
            TranscriptRecord::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t"),
                title: String::new(),
                kind: ToolKind::Read,
                options: Vec::new(),
                name: None,
                locations: Vec::new(),
                partitur_tool: false,
            }
        );
    }

    #[test]
    fn a_permission_request_without_the_flag_loads_as_not_partitur() {
        let old = serde_json::from_str::<TranscriptRecord>(
            r#"{"PermissionRequested":{"id":1,"tool_call_id":"t","title":"","kind":"Other","options":[],"name":"mcp__partitur__propose_children"}}"#,
        )
        .unwrap();
        assert_eq!(
            old,
            TranscriptRecord::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t"),
                title: String::new(),
                kind: ToolKind::Other,
                options: Vec::new(),
                name: Some("mcp__partitur__propose_children".into()),
                locations: Vec::new(),
                partitur_tool: false,
            }
        );
    }

    #[test]
    fn a_tool_call_logged_before_widgets_still_loads() {
        let call = serde_json::from_str::<TranscriptRecord>(
            r#"{"ToolCall":{"id":"t","title":"Read","kind":"Read","status":"Pending","content":[],"locations":[]}}"#,
        )
        .unwrap();
        assert_eq!(
            call,
            TranscriptRecord::ToolCall {
                id: ToolCallId::new("t"),
                title: "Read".into(),
                kind: ToolKind::Read,
                status: ToolStatus::Pending,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                call_result: None,
                widget: None,
            }
        );
        let change = serde_json::from_str::<TranscriptRecord>(
            r#"{"ToolCallUpdate":{"id":"t","title":null,"kind":null,"status":"Completed","content":null}}"#,
        )
        .unwrap();
        assert_eq!(
            change,
            TranscriptRecord::ToolCallUpdate {
                id: ToolCallId::new("t"),
                title: None,
                kind: None,
                status: Some(ToolStatus::Completed),
                content: None,
                raw_output: None,
                raw_input: None,
                call_result: None,
                widget: None,
            }
        );
    }

    #[test]
    fn a_tool_call_without_widget_serializes_as_before() {
        let call = TranscriptRecord::ToolCall {
            id: ToolCallId::new("t"),
            title: "Read".into(),
            kind: ToolKind::Read,
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        };
        assert_eq!(
            serde_json::to_string(&call).unwrap(),
            r#"{"ToolCall":{"id":"t","title":"Read","kind":"Read","status":"Pending","content":[],"locations":[]}}"#
        );
        let change = TranscriptRecord::ToolCallUpdate {
            id: ToolCallId::new("t"),
            title: None,
            kind: None,
            status: None,
            content: None,
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        };
        let json = serde_json::to_string(&change).unwrap();
        for key in ["raw_input", "call_result", "widget"] {
            assert!(!json.contains(key), "{json}");
        }
    }

    #[test]
    fn a_tool_call_with_widget_data_round_trips() {
        let widget = Widget {
            server: "atlassian".into(),
            uri: "ui://jira/issue".into(),
            html: "<p>issue</p>".into(),
            meta: "{}".into(),
        };
        let slots = [
            WidgetSlot::Loading {
                server: "atlassian".into(),
                uri: "ui://jira/issue".into(),
            },
            WidgetSlot::Ready(widget),
            WidgetSlot::Failed {
                server: "atlassian".into(),
                uri: "ui://jira/issue".into(),
                error: "gone".into(),
            },
        ];
        for slot in slots {
            let updates = [
                TranscriptRecord::ToolCall {
                    id: ToolCallId::new("t"),
                    title: "getJiraIssue".into(),
                    kind: ToolKind::Other,
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    locations: Vec::new(),
                    raw_input: Some(r#"{"issueKey":"PROJ-1"}"#.into()),
                    call_result: Some(r#"{"content":[]}"#.into()),
                    widget: Some(slot.clone()),
                },
                TranscriptRecord::ToolCallUpdate {
                    id: ToolCallId::new("t"),
                    title: None,
                    kind: None,
                    status: None,
                    content: None,
                    raw_output: None,
                    raw_input: Some("{}".into()),
                    call_result: Some("{}".into()),
                    widget: Some(slot),
                },
            ];
            for update in updates {
                let json = serde_json::to_string(&update).unwrap();
                assert_eq!(
                    serde_json::from_str::<TranscriptRecord>(&json).unwrap(),
                    update
                );
            }
        }
    }

    /// One update of each kind an orchestration adds.
    fn orchestration_updates() -> Vec<TranscriptRecord> {
        let proposal = ProposalId::new();
        vec![
            TranscriptRecord::ChildrenProposed {
                id: proposal.clone(),
                children: vec![proposed("api", "API endpoint")],
            },
            TranscriptRecord::ProposalDecided {
                id: proposal.clone(),
                decision: Decision::Approved(vec![ApprovedChild {
                    session: SessionId::new(),
                    repository: repository("api"),
                    prompt: "Add the endpoint".into(),
                    title: "API endpoint".into(),
                    auto_allow: true,
                    mode: BranchMode::Worktree,
                    branch: Some("feat/x".into()),
                    cwd: PathBuf::from("/p/api-wt/feat/x"),
                }]),
            },
            TranscriptRecord::ProposalDecided {
                id: proposal,
                decision: Decision::Rejected {
                    note: Some("later".into()),
                },
            },
            TranscriptRecord::OrchestratorPrompt("do it".into()),
            TranscriptRecord::PartiturPrompt("children started".into()),
            TranscriptRecord::TurnStarted,
            TranscriptRecord::ChildResult {
                child: SessionId::new(),
                repository: repository("web"),
                title: "Web page".into(),
                origin: TurnOrigin::Orchestrator,
                outcome: ChildOutcome::Ended(StopReason::EndTurn),
                text: "done".into(),
                state: ChildState::default(),
            },
            TranscriptRecord::ChildResult {
                child: SessionId::new(),
                repository: repository("api"),
                title: "API endpoint".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Failed("boom".into()),
                text: "half done".into(),
                state: ChildState {
                    step: Some("execute · 4/11".into()),
                    waits: Some(ChildWait::Gate {
                        word: "checkpoint".into(),
                    }),
                },
            },
            TranscriptRecord::ChildResult {
                child: SessionId::new(),
                repository: repository("api"),
                title: "API endpoint".into(),
                origin: TurnOrigin::Agent,
                outcome: ChildOutcome::Ended(StopReason::EndTurn),
                text: "all green".into(),
                state: ChildState {
                    step: None,
                    waits: Some(ChildWait::Question(Question {
                        text: "Merge or PR?".into(),
                        options: Vec::new(),
                    })),
                },
            },
            TranscriptRecord::ChildPrompted {
                child: SessionId::new(),
                prompt: "Also add a test".into(),
                decision: None,
            },
            TranscriptRecord::ChildPrompted {
                child: SessionId::new(),
                prompt: "Fix the route".into(),
                decision: Some(GateDecision::Changes),
            },
            TranscriptRecord::TitleProposed(TitleProposal {
                child: SessionId::new(),
                repository: repository("api"),
                title: "API endpoint".into(),
                proposed: "Auth endpoint".into(),
                by_user: false,
            }),
        ]
    }

    #[test]
    fn a_child_result_recorded_before_its_state_loads_without_one() {
        let child = SessionId::new();
        let json = format!(
            r#"{{"ChildResult":{{"child":"{child}","repository":"api","title":"t","origin":"User","outcome":{{"Failed":"boom"}},"text":"x"}}}}"#
        );

        let update: TranscriptRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(
            update,
            TranscriptRecord::ChildResult {
                child,
                repository: repository("api"),
                title: "t".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Failed("boom".into()),
                text: "x".into(),
                state: ChildState::default(),
            }
        );
    }

    #[test]
    fn a_child_prompt_recorded_before_decisions_loads_without_one() {
        let child = SessionId::new();
        let json = format!(r#"{{"ChildPrompted":{{"child":"{child}","prompt":"Go on"}}}}"#);

        let update: TranscriptRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(
            update,
            TranscriptRecord::ChildPrompted {
                child,
                prompt: "Go on".into(),
                decision: None,
            }
        );
    }
}
