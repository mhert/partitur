//! The port every agent backend implements, and the vocabulary it speaks.
//!
//! [`Backend`] is a synchronous command port: each method queues a command and returns at
//! once. Outcomes arrive later as [`BackendEvent`] values on a channel owned by the backend
//! crate, so the core never sees a future.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{
    BackendSessionId, ConfigId, ConfigValueId, PermissionId, PermissionOptionId, RequestId,
    SessionId, ToolCallId,
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
        /// The new output, replacing the previous output.
        content: Option<Vec<ToolContent>>,
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
    /// Asks the backend to start a new session working in `cwd`.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn new_session(
        &self,
        request: RequestId,
        session: SessionId,
        cwd: &Path,
    ) -> Result<(), BackendError>;

    /// Asks the backend to resume a session it created earlier.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the command could not be queued.
    fn load_session(
        &self,
        request: RequestId,
        session: SessionId,
        backend_session: &BackendSessionId,
        cwd: &Path,
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
        fn new_session(&self, _: RequestId, _: SessionId, _: &Path) -> Result<(), BackendError> {
            self.log("new_session");
            Ok(())
        }

        fn load_session(
            &self,
            _: RequestId,
            _: SessionId,
            _: &BackendSessionId,
            _: &Path,
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
    }

    #[test]
    fn the_backend_trait_is_object_safe_and_sendable() {
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        let backend: Box<dyn Backend> = Box::new(Arc::clone(&recorder));
        std::thread::spawn(move || {
            let session = SessionId::new();
            let cwd = Path::new("/work");
            backend
                .new_session(RequestId(1), session.clone(), cwd)
                .unwrap();
            backend
                .load_session(
                    RequestId(2),
                    session.clone(),
                    &BackendSessionId::new("b-1"),
                    cwd,
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
    fn updates_round_trip_through_json() {
        let updates = vec![
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
        for update in updates {
            let json = serde_json::to_string(&update).unwrap();
            assert_eq!(serde_json::from_str::<Update>(&json).unwrap(), update);
        }
    }
}
