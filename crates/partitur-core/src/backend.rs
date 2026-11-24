//! The port every agent backend implements, and the vocabulary it speaks.
//!
//! [`Backend`] is a synchronous command port: each method queues a command and returns at
//! once. Outcomes arrive later as [`BackendEvent`] values on a channel owned by the backend
//! crate, so the core never sees a future.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{
    BackendId, BackendSessionId, ConfigId, ConfigValueId, PermissionId, PermissionOptionId,
    RepositoryId, RequestId, SessionId, ToolCallId,
};
use crate::question::Question;
use crate::session::{BranchMode, TitleSource};
use crate::tool_call::ToolRole;

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
    /// A change given as git-patch text.
    Patch {
        /// The files the patch touches.
        paths: Vec<PathBuf>,
        /// The patch text.
        text: String,
    },
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

impl PromptContent {
    /// An inline image of the given media type, its bytes base64 encoded.
    #[must_use]
    pub fn image(mime: &str, bytes: &[u8]) -> Self {
        Self::Image {
            mime: mime.into(),
            base64: BASE64.encode(bytes),
        }
    }
}

/// One attachment of a prompt, as it was sent and as the transcript shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SentAttachment {
    /// An image kept with the session: `copy` and `preview` are file names in the session's
    /// attachment folder.
    Image {
        /// The file name the image had.
        name: String,
        /// The kept copy of the image.
        copy: PathBuf,
        /// A smaller picture of the image, when one was made.
        preview: Option<PathBuf>,
    },
    /// Any other file, sent as a link to where it was.
    File {
        /// The file name.
        name: String,
        /// Where the file was.
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
    /// Where the title comes from: [derived](TitleSource::Derived) when the orchestrator
    /// gave none and it was made from the prompt. Set when the proposal is recorded, so
    /// whatever a caller puts here is replaced. Proposals recorded before titles had a
    /// source read as [given](TitleSource::Given).
    #[serde(default = "TitleSource::given")]
    pub title_source: TitleSource,
    /// The branch a new branch for the session starts from, as the proposer suggests it; the
    /// user decides. `None` when the proposer suggested none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
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
    /// How the child uses its repository's git. Approvals recorded before children had
    /// branch settings read as Checked-out.
    #[serde(default)]
    pub mode: BranchMode,
    /// The branch the child made, `None` for a Checked-out child.
    #[serde(default)]
    pub branch: Option<String>,
    /// The folder the child works in: its worktree, or its repository. Approvals recorded
    /// before children had branch settings read as empty.
    #[serde(default)]
    pub cwd: PathBuf,
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

/// Who started a session's turn. Every turn records it; only a child session's turn
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOrigin {
    /// The user prompted the child directly.
    User,
    /// The child's orchestrator sent the prompt.
    Orchestrator,
    /// The agent resumed on its own, for example when a background task finished.
    Agent,
}

/// How a child session's turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildOutcome {
    /// The turn ended for this reason.
    Ended(StopReason),
    /// The session failed with this error.
    Failed(String),
}

/// Where a child stands in its workflow when its turn ended, as its orchestrator is told.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChildState {
    /// The step of the child's workflow in words, such as `execute · 4/11`; `None` for a
    /// child without a workflow.
    pub step: Option<String>,
    /// What the child waits for its orchestrator to decide; `None` when it waits for nothing.
    pub waits: Option<ChildWait>,
}

/// What a child waits for its orchestrator to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildWait {
    /// A gate of its workflow is open; `word` names what waits at it, such as `checkpoint`.
    Gate {
        /// The word for what waits at the gate.
        word: String,
    },
    /// The child asked this question and it is still open.
    Question(Question),
}

/// How an orchestrator decides the gate one of its children waits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    /// Approves the gate, as the user's approve button does.
    Approve,
    /// Asks for changes, with the orchestrator's prompt after the gate's change prefix.
    Changes,
}

/// A tool an orchestrator session called, as Partitur's tool server parsed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchestratorTool {
    /// Proposes child sessions for the user to approve.
    ProposeChildren(Vec<ProposedChild>),
    /// Sends a follow-up prompt to one of the orchestrator's children, or decides the gate it
    /// waits at.
    SendToChild {
        /// The child to prompt.
        child: SessionId,
        /// The prompt to send; may be blank only with an approval.
        prompt: String,
        /// The decision on the child's open gate, `None` for a plain prompt or an answer.
        #[serde(default)]
        decision: Option<GateDecision>,
    },
    /// Lists the orchestrator's children with their repository and status.
    ListChildren,
    /// Stops the current turn of one of the orchestrator's children.
    CancelChild {
        /// The child to stop.
        child: SessionId,
    },
    /// Reads the last final message and the status of one of the orchestrator's children.
    GetChildResult {
        /// The child to read.
        child: SessionId,
    },
}

/// A title a child proposed for itself with `propose_title`, as its orchestrator receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleProposal {
    /// The child that proposed it.
    pub child: SessionId,
    /// The repository the child works in.
    pub repository: RepositoryId,
    /// The child's title when it proposed the new one.
    pub title: String,
    /// The proposed title.
    pub proposed: String,
    /// Whether the user typed the child's title.
    pub by_user: bool,
}

/// An MCP App widget: the HTML of the `ui://` resource a tool declares, read from its server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Widget {
    /// The MCP server the resource was read from.
    pub server: String,
    /// The resource's `ui://` URI.
    pub uri: String,
    /// The widget's HTML document.
    pub html: String,
    /// The resource's `_meta` as JSON text, which carries the widget's CSP and permissions.
    pub meta: String,
}

/// Where a tool call's widget stands: being read, read, or failed to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetSlot {
    /// The resource is being read.
    Loading {
        /// The MCP server the resource is read from.
        server: String,
        /// The resource's `ui://` URI.
        uri: String,
    },
    /// The widget was read.
    Ready(Widget),
    /// The resource could not be read.
    Failed {
        /// The MCP server the resource was read from.
        server: String,
        /// The resource's `ui://` URI.
        uri: String,
        /// Why the read failed.
        error: String,
    },
}

/// One incremental change an agent backend reports for a session, in the order it reported it.
///
/// Every update is recorded as the [`TranscriptRecord`](crate::TranscriptRecord) of the same
/// name; what only Partitur records has no update here, so no backend can report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentUpdate {
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
        update: AgentUpdate,
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
}

/// A synchronous command port to an agent backend.
///
/// Every method returns as soon as the command is queued; outcomes arrive as
/// [`BackendEvent`]s on a channel the implementing crate owns. The trait is object-safe and
/// is used as `Box<dyn Backend>`.
pub trait Backend: Send {
    /// Asks the configured backend `backend` to start a new session working in `cwd`;
    /// `role` picks the tools the session is offered.
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
        role: ToolRole,
    ) -> Result<(), BackendError>;

    /// Asks the configured backend `backend` to resume a session it created earlier;
    /// `role` picks the tools the session is offered.
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
        role: ToolRole,
    ) -> Result<(), BackendError>;

    /// Sends a user prompt to a session.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn prompt(&self, session: SessionId, content: Vec<PromptContent>) -> Result<(), BackendError>;

    /// Cancels the session's running turn and the subagents its agent runs in the background.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn cancel(&self, session: SessionId) -> Result<(), BackendError>;

    /// Closes the session's connection and ends its agent process. Nothing the session's
    /// connection reports afterwards arrives as an event. A session the backend never opened,
    /// or closed already, is no error.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn close_session(&self, session: SessionId) -> Result<(), BackendError>;

    /// Changes one configuration option of a session. The agent answers with
    /// [`AgentUpdate::ConfigOptions`], or with [`AgentUpdate::ConfigRejected`] when it refuses the
    /// change.
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

    #[test]
    fn an_image_prompt_content_is_base64() {
        assert_eq!(
            PromptContent::image("image/png", b"hi"),
            PromptContent::Image {
                mime: "image/png".into(),
                base64: "aGk=".into()
            }
        );
    }

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
            _: ToolRole,
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
            _: ToolRole,
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

        fn close_session(&self, _: SessionId) -> Result<(), BackendError> {
            self.log("close_session");
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
    }

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

    #[test]
    fn a_proposed_child_without_base_reads_as_none() {
        let child: ProposedChild =
            serde_json::from_str(r#"{"repository":"api","prompt":"p","title":"t"}"#).unwrap();

        assert_eq!(child.base, None);
        assert!(!serde_json::to_string(&child).unwrap().contains("base"));
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
                .new_session(RequestId(1), session.clone(), &claude, cwd, ToolRole::Plain)
                .unwrap();
            backend
                .load_session(
                    RequestId(2),
                    session.clone(),
                    &claude,
                    &BackendSessionId::new("b-1"),
                    cwd,
                    ToolRole::Orchestrator,
                )
                .unwrap();
            backend.prompt(session.clone(), Vec::new()).unwrap();
            backend.cancel(session.clone()).unwrap();
            backend.close_session(session.clone()).unwrap();
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
                "close_session",
                "set_config",
                "respond_permission"
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
    }

    #[test]
    fn a_proposal_recorded_before_title_sources_loads_with_given_titles() {
        let json = r#"{"repository":"api","prompt":"p","title":"t"}"#;

        let child: ProposedChild = serde_json::from_str(json).unwrap();

        assert_eq!(child.title_source, TitleSource::Given);
    }

    #[test]
    fn an_approval_recorded_before_branches_loads() {
        let session = SessionId::new();
        let json = format!(
            r#"{{"session":"{session}","repository":"api","prompt":"p","title":"t","auto_allow":false}}"#
        );

        let child: ApprovedChild = serde_json::from_str(&json).unwrap();

        assert_eq!(child.session, session);
        assert_eq!(child.mode, BranchMode::CheckedOut);
        assert_eq!(child.branch, None);
        assert_eq!(child.cwd, PathBuf::new());
    }

    #[test]
    fn a_gate_decision_is_written_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&GateDecision::Approve).unwrap(),
            r#""approve""#
        );
        assert_eq!(
            serde_json::to_string(&GateDecision::Changes).unwrap(),
            r#""changes""#
        );
    }

    #[test]
    fn backend_events_round_trip_through_json() {
        let events = vec![
            BackendEvent::SessionClosed {
                session: SessionId::new(),
                error: "gone".into(),
            },
            BackendEvent::RequestFailed {
                request: RequestId(1),
                session: SessionId::new(),
                error: "boom".into(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<BackendEvent>(&json).unwrap(), event);
        }
    }

    #[test]
    fn orchestrator_tools_round_trip_through_json() {
        let tools = vec![
            OrchestratorTool::ProposeChildren(vec![
                proposed("api", "API endpoint"),
                proposed("web", "Web page"),
            ]),
            OrchestratorTool::SendToChild {
                child: SessionId::new(),
                prompt: "Also add a test".into(),
                decision: None,
            },
            OrchestratorTool::SendToChild {
                child: SessionId::new(),
                prompt: String::new(),
                decision: Some(GateDecision::Approve),
            },
            OrchestratorTool::ListChildren,
            OrchestratorTool::CancelChild {
                child: SessionId::new(),
            },
            OrchestratorTool::GetChildResult {
                child: SessionId::new(),
            },
        ];
        for tool in tools {
            let json = serde_json::to_string(&tool).unwrap();
            assert_eq!(
                serde_json::from_str::<OrchestratorTool>(&json).unwrap(),
                tool
            );
        }
    }
}
