//! The port every agent backend implements, and the vocabulary it speaks.
//!
//! [`Backend`] is a synchronous command port: each method queues a command and returns at
//! once. Outcomes arrive later as [`BackendEvent`] values on a channel owned by the backend
//! crate, so the core never sees a future.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{
    BackendId, BackendSessionId, ConfigId, ConfigValueId, PermissionId, PermissionOptionId,
    ProposalId, RepositoryId, RequestId, SessionId, ToolCallId, ToolRequestId,
};

/// What a tool call does, used to pick an icon and a permission wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    /// Reads a file or other resource.
    Read,
    /// Changes file contents.
    Edit,
    /// Removes a file or other resource.
    Delete,
    /// Moves or renames a file.
    Move,
    /// Searches the workspace or the web.
    Search,
    /// Runs a command.
    Execute,
    /// Internal reasoning by the agent.
    Think,
    /// Fetches a remote resource.
    Fetch,
    /// Switches the agent's operating mode.
    SwitchMode,
    /// Anything else.
    Other,
}

/// How far a tool call has progressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Announced but not started, possibly awaiting permission.
    Pending,
    /// Running.
    InProgress,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

/// One piece of output produced by a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolContent {
    /// Plain text.
    Text(String),
    /// A file change.
    Diff {
        /// The file that changed.
        path: PathBuf,
        /// The previous contents, `None` when the file is new.
        old: Option<String>,
        /// The new contents.
        new: String,
    },
    /// The output of a terminal command.
    Terminal(String),
}

/// Why an agent turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The agent finished its answer.
    EndTurn,
    /// The model hit its token limit.
    MaxTokens,
    /// The agent hit its limit of model requests for one turn.
    MaxTurnRequests,
    /// The agent refused to continue.
    Refusal,
    /// The user cancelled the turn.
    Cancelled,
}

/// What choosing a [`PermissionOption`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    /// Allow this one call.
    AllowOnce,
    /// Allow this and future similar calls.
    AllowAlways,
    /// Reject this one call.
    RejectOnce,
    /// Reject this and future similar calls.
    RejectAlways,
}

/// One choice offered in a permission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Identifies the choice when answering.
    pub id: PermissionOptionId,
    /// Label shown to the user.
    pub name: String,
    /// What choosing it means.
    pub kind: PermissionKind,
}

/// What a [`ConfigOption`] controls, so the UI can group and order them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigCategory {
    /// The model in use.
    Model,
    /// How much reasoning effort to spend.
    Effort,
    /// The agent's operating mode.
    Mode,
    /// A category Partitur has no special handling for.
    Other(String),
}

/// One selectable value of a [`ConfigOption`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigChoice {
    /// Identifies the value when setting it.
    pub id: ConfigValueId,
    /// Label shown to the user.
    pub name: String,
}

/// A setting a backend lets the user change for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigOption {
    /// Identifies the option when setting it.
    pub id: ConfigId,
    /// Label shown to the user.
    pub name: String,
    /// What the option controls.
    pub category: ConfigCategory,
    /// The value currently in effect.
    pub current: ConfigValueId,
    /// The values the user can pick from.
    pub choices: Vec<ConfigChoice>,
}

/// One part of a prompt sent to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptContent {
    /// Plain text.
    Text(String),
    /// An inline image.
    Image {
        /// The image's media type, for example `image/png`.
        mime: String,
        /// The image bytes, base64 encoded.
        base64: String,
    },
    /// A reference to a file the agent can read itself.
    FileLink {
        /// The file's path.
        path: PathBuf,
    },
}

/// One child session an orchestrator proposes, before the user decides on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedChild {
    /// The repository the child works in.
    pub repository: RepositoryId,
    /// The first prompt the child receives.
    pub prompt: String,
    /// The child's display title.
    pub title: String,
}

/// One child session the user approved, as it was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedChild {
    /// The session started for the child.
    pub session: SessionId,
    /// The repository the child works in.
    pub repository: RepositoryId,
    /// The first prompt the child received.
    pub prompt: String,
    /// The child's display title.
    pub title: String,
    /// Whether the child grants permission requests without asking.
    pub auto_allow: bool,
}

/// The user's decision on a proposal of child sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// The user approved these children, which were started.
    Approved(Vec<ApprovedChild>),
    /// The user rejected the proposal.
    Rejected {
        /// What the user told the orchestrator about the rejection, if anything.
        note: Option<String>,
    },
}

/// Who started a child session's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOrigin {
    /// The user prompted the child directly.
    User,
    /// The child's orchestrator sent the prompt.
    Orchestrator,
}

/// How a child session's turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildOutcome {
    /// The turn ended for this reason.
    Ended(StopReason),
    /// The session failed with this error.
    Failed(String),
}

/// A tool an orchestrator session called, as the backend parsed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchestratorTool {
    /// Proposes child sessions for the user to approve.
    ProposeChildren(Vec<ProposedChild>),
    /// Sends a follow-up prompt to one of the orchestrator's children.
    SendToChild {
        /// The child to prompt.
        child: SessionId,
        /// The prompt to send.
        prompt: String,
    },
}

/// One incremental change to a session, in the order the agent reported it.
///
/// Updates are persisted as JSON lines and replayed through
/// [`Transcript`](crate::Transcript) to rebuild a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Update {
    /// A chunk of the user's message.
    UserText(String),
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
    /// The available configuration options changed.
    ConfigOptions(Vec<ConfigOption>),
    /// An informational message from Partitur or the backend.
    Notice(String),
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
    },
}

/// An asynchronous outcome reported by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendEvent {
    /// A [`Backend::new_session`] request succeeded.
    SessionCreated {
        /// The request being answered.
        request: RequestId,
        /// The Partitur session the request was made for.
        session: SessionId,
        /// The id the backend assigned.
        backend_session: BackendSessionId,
        /// The options the backend offers for this session.
        config: Vec<ConfigOption>,
    },
    /// A [`Backend::load_session`] request succeeded.
    SessionLoaded {
        /// The request being answered.
        request: RequestId,
        /// The Partitur session the request was made for.
        session: SessionId,
        /// The options the backend offers for this session.
        config: Vec<ConfigOption>,
    },
    /// A session changed.
    Update {
        /// The session the update belongs to.
        session: SessionId,
        /// What changed.
        update: Update,
    },
    /// The backend session ended outside a turn result, for example because the agent
    /// process exited; the session must be re-opened before the next prompt.
    SessionClosed {
        /// The Partitur session whose backend session ended.
        session: SessionId,
        /// Why it ended.
        error: String,
    },
    /// A session request failed.
    RequestFailed {
        /// The request that failed.
        request: RequestId,
        /// The Partitur session the request was made for.
        session: SessionId,
        /// What went wrong.
        error: String,
    },
    /// An orchestrator session called one of Partitur's tools; the call waits for
    /// [`Backend::answer_tool`].
    ToolCalled {
        /// Identifies the call when answering it.
        call: ToolRequestId,
        /// The orchestrator session that called the tool.
        session: SessionId,
        /// The tool and its arguments.
        tool: OrchestratorTool,
    },
}

/// Why a command could not be queued.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BackendError {
    /// The backend is not running or cannot be reached.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    /// The command names a session the backend does not know.
    #[error("unknown session {0}")]
    UnknownSession(SessionId),
    /// The command could not be handed to the backend.
    #[error("failed to send command: {0}")]
    Send(String),
    /// The answer names a tool call nothing waits for: it was never made, was already
    /// answered or timed out.
    #[error("no open tool call {0}")]
    UnknownToolCall(ToolRequestId),
}

/// A synchronous command port to an agent backend.
///
/// Every method returns as soon as the command is queued; outcomes arrive as
/// [`BackendEvent`]s on a channel the implementing crate owns. The trait is object-safe and
/// is used as `Box<dyn Backend>`.
pub trait Backend: Send {
    /// Asks the configured backend `backend` to start a new session working in `cwd`;
    /// `orchestrator` says whether the session is an orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn new_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        cwd: &Path,
        orchestrator: bool,
    ) -> Result<(), BackendError>;

    /// Asks the configured backend `backend` to resume a session it created earlier;
    /// `orchestrator` says whether the session is an orchestrator.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend: &BackendId,
        backend_session: &BackendSessionId,
        cwd: &Path,
        orchestrator: bool,
    ) -> Result<(), BackendError>;

    /// Sends a user prompt to a session.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError>;

    /// Cancels the session's running turn.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn cancel(&self, session: SessionId) -> Result<(), BackendError>;

    /// Changes one configuration option of a session.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn set_config(
        &self,
        session: SessionId,
        option: &ConfigId,
        value: &ConfigValueId,
    ) -> Result<(), BackendError>;

    /// Answers a permission request; `None` cancels it.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn respond_permission(
        &self,
        session: SessionId,
        permission: PermissionId,
        option: Option<PermissionOptionId>,
    ) -> Result<(), BackendError>;

    /// Answers the tool call `call` an orchestrator made: `Ok` text is the tool's result,
    /// `Err` text an error result.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::UnknownToolCall`] when no such call waits.
    fn answer_tool(
        &self,
        call: ToolRequestId,
        result: Result<String, String>,
    ) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::sync::{Arc, Mutex};

    use super::*;

    /// Logs the name of every command it receives.
    struct Recorder(Mutex<Vec<&'static str>>);

    impl Recorder {
        fn log(&self, command: &'static str) {
            self.0.lock().unwrap().push(command);
        }
    }

    impl Backend for Arc<Recorder> {
        fn new_session(
            &self,
            _: RequestId,
            _: SessionId,
            _: &BackendId,
            _: &Path,
            _: bool,
        ) -> Result<(), BackendError> {
            self.log("new_session");
            Ok(())
        }

        fn load_session(
            &self,
            _: RequestId,
            _: SessionId,
            _: &BackendId,
            _: &BackendSessionId,
            _: &Path,
            _: bool,
        ) -> Result<(), BackendError> {
            self.log("load_session");
            Ok(())
        }

        fn prompt(&self, _: SessionId, _: Vec<PromptContent>) -> Result<(), BackendError> {
            self.log("prompt");
            Ok(())
        }

        fn cancel(&self, _: SessionId) -> Result<(), BackendError> {
            self.log("cancel");
            Ok(())
        }

        fn set_config(
            &self,
            _: SessionId,
            _: &ConfigId,
            _: &ConfigValueId,
        ) -> Result<(), BackendError> {
            self.log("set_config");
            Ok(())
        }

        fn respond_permission(
            &self,
            _: SessionId,
            _: PermissionId,
            _: Option<PermissionOptionId>,
        ) -> Result<(), BackendError> {
            self.log("respond_permission");
            Ok(())
        }

        fn answer_tool(
            &self,
            _: ToolRequestId,
            _: Result<String, String>,
        ) -> Result<(), BackendError> {
            self.log("answer_tool");
            Ok(())
        }
    }

    fn repository(name: &str) -> RepositoryId {
        RepositoryId::try_from(name).unwrap()
    }

    fn proposed(name: &str, title: &str) -> ProposedChild {
        ProposedChild {
            repository: repository(name),
            prompt: format!("Work on {name}"),
            title: title.into(),
        }
    }

    #[test]
    fn the_backend_trait_is_object_safe_and_sendable() {
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        let backend: Box<dyn Backend> = Box::new(Arc::clone(&recorder));
        std::thread::spawn(move || {
            let session = SessionId::new();
            let cwd = Path::new("/work");
            let claude = BackendId::try_from("claude").unwrap();
            backend
                .new_session(RequestId(1), session.clone(), &claude, cwd, false)
                .unwrap();
            backend
                .load_session(
                    RequestId(2),
                    session.clone(),
                    &claude,
                    &BackendSessionId::new("b-1"),
                    cwd,
                    true,
                )
                .unwrap();
            backend.prompt(session.clone(), Vec::new()).unwrap();
            backend.cancel(session.clone()).unwrap();
            backend
                .set_config(
                    session.clone(),
                    &ConfigId::new("model"),
                    &ConfigValueId::new("fast"),
                )
                .unwrap();
            backend
                .respond_permission(session, PermissionId(3), None)
                .unwrap();
            backend
                .answer_tool(ToolRequestId(4), Ok("done".into()))
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                "new_session",
                "load_session",
                "prompt",
                "cancel",
                "set_config",
                "respond_permission",
                "answer_tool"
            ]
        );
    }

    #[test]
    fn backend_errors_describe_what_went_wrong() {
        let session = SessionId::new();
        assert_eq!(
            BackendError::Unavailable("not started".into()).to_string(),
            "backend unavailable: not started"
        );
        assert_eq!(
            BackendError::UnknownSession(session.clone()).to_string(),
            format!("unknown session {session}")
        );
        assert_eq!(
            BackendError::Send("channel closed".into()).to_string(),
            "failed to send command: channel closed"
        );
        assert_eq!(
            BackendError::UnknownToolCall(ToolRequestId(4)).to_string(),
            "no open tool call 4"
        );
    }

    #[test]
    fn updates_round_trip_through_json() {
        let mut updates = vec![
            Update::AgentText("hi".into()),
            Update::ToolCall {
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
                ],
                locations: vec![PathBuf::from("src/main.rs")],
            },
            Update::PermissionRequested {
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
            },
            Update::PermissionRequested {
                id: PermissionId(4),
                tool_call_id: ToolCallId::new("t-2"),
                title: "Propose children".into(),
                kind: ToolKind::Other,
                options: Vec::new(),
                name: Some("mcp__partitur__propose_children".into()),
            },
            Update::PermissionAnswered {
                id: PermissionId(3),
                option: None,
            },
            Update::ConfigOptions(vec![ConfigOption {
                id: ConfigId::new("model"),
                name: "Model".into(),
                category: ConfigCategory::Other("custom".into()),
                current: ConfigValueId::new("fast"),
                choices: vec![ConfigChoice {
                    id: ConfigValueId::new("fast"),
                    name: "Fast".into(),
                }],
            }]),
            Update::TurnEnded(StopReason::EndTurn),
        ];
        updates.extend(orchestration_updates());
        for update in updates {
            let json = serde_json::to_string(&update).unwrap();
            assert_eq!(serde_json::from_str::<Update>(&json).unwrap(), update);
        }

        // A permission request logged before requests carried the tool's name.
        let old = serde_json::from_str::<Update>(
            r#"{"PermissionRequested":{"id":1,"tool_call_id":"t","title":"","kind":"Read","options":[]}}"#,
        )
        .unwrap();
        assert_eq!(
            old,
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t"),
                title: String::new(),
                kind: ToolKind::Read,
                options: Vec::new(),
                name: None,
            }
        );
    }

    /// One update of each kind an orchestration adds.
    fn orchestration_updates() -> Vec<Update> {
        let proposal = ProposalId::new();
        vec![
            Update::ChildrenProposed {
                id: proposal.clone(),
                children: vec![proposed("api", "API endpoint")],
            },
            Update::ProposalDecided {
                id: proposal.clone(),
                decision: Decision::Approved(vec![ApprovedChild {
                    session: SessionId::new(),
                    repository: repository("api"),
                    prompt: "Add the endpoint".into(),
                    title: "API endpoint".into(),
                    auto_allow: true,
                }]),
            },
            Update::ProposalDecided {
                id: proposal,
                decision: Decision::Rejected {
                    note: Some("later".into()),
                },
            },
            Update::OrchestratorPrompt("do it".into()),
            Update::PartiturPrompt("children started".into()),
            Update::ChildResult {
                child: SessionId::new(),
                repository: repository("web"),
                title: "Web page".into(),
                origin: TurnOrigin::Orchestrator,
                outcome: ChildOutcome::Ended(StopReason::EndTurn),
                text: "done".into(),
            },
            Update::ChildResult {
                child: SessionId::new(),
                repository: repository("api"),
                title: "API endpoint".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Failed("boom".into()),
                text: "half done".into(),
            },
        ]
    }

    #[test]
    fn backend_events_round_trip_through_json() {
        let events = vec![
            BackendEvent::ToolCalled {
                call: ToolRequestId(1),
                session: SessionId::new(),
                tool: OrchestratorTool::ProposeChildren(vec![
                    proposed("api", "API endpoint"),
                    proposed("web", "Web page"),
                ]),
            },
            BackendEvent::ToolCalled {
                call: ToolRequestId(2),
                session: SessionId::new(),
                tool: OrchestratorTool::SendToChild {
                    child: SessionId::new(),
                    prompt: "Also add a test".into(),
                },
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<BackendEvent>(&json).unwrap(), event);
        }
    }
}
