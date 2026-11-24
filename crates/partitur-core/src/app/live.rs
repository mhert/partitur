//! What `App` keeps in memory about each session it has dealt with in this process, in one
//! place. A [`LiveSession`] holds one session's transcript as far as it is loaded, the options its backend
//! offers, whether it is attached, the prompts waiting for it, who started its turn, its
//! background tasks, the prompts Partitur deferred for it, its unanswered option changes, its
//! undecided proposals, its latest git reading and its delete sheet. A session without an
//! entry has none of these: each field's empty value means the same as no entry.
//!
//! The permission requests waiting for an answer and the unanswered backend requests are
//! kept apart, in [`Permissions`] and [`Requests`], since they are looked up by their own
//! ids. Each forgets everything it holds for a session at once, as when the session is
//! deleted.
//!
//! A session's transcript is loaded in the background, through the store's queue, the first
//! time something needs it: [`App::with_transcript`] runs what needs it at once when it is
//! loaded, and otherwise queues the load, unless one is on its way, and keeps what needs it
//! waiting, in order. While the load is on its way, every record the session gets is queued
//! for the store as always and kept aside: the load sees only the writes queued before it, so
//! the records kept aside are applied after the loaded ones, and each record shows once. When
//! the load arrives, the transcript is built, what the load itself settles is written, the
//! prompts the user sent meanwhile go out once the session is attached, and then what waited
//! runs in the order it came. A load for a session deleted meanwhile, or one a newer load
//! replaced, is dropped; the tool calls that waited for a deleted session's load are answered
//! that the session was deleted. A load that fails leaves the transcript unloaded, answers the
//! tool calls that waited with the failure and drops what else waited, as a failed load always
//! did.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::cleanup::Sheet;
use super::env_git::EnvGit;
use super::scheduler::Generated;
use super::tools::DELETED_SESSION;
use super::{App, AppError, QueuedPrompt, Request};
use crate::backend::{ChildOutcome, ConfigOption, PermissionOption, ProposedChild, TurnOrigin};
use crate::ids::{
    ConfigId, ConfigValueId, PermissionId, ProjectId, ProposalId, RequestId, SessionId,
};
use crate::record::TranscriptRecord;
use crate::store::{LoadId, StoreCommand, StoreError};
use crate::tool_call::ToolCall;
use crate::transcript::Transcript;
use crate::view_model::LiveState;

/// Where the transcript of a session stands in this process.
#[derive(Default)]
#[expect(
    clippy::large_enum_variant,
    reason = "a session's transcript is loaded for most of its life, so a box would only add a \
              step to every update"
)]
pub(super) enum TranscriptState {
    /// Not loaded: the session has not been opened, or its load failed.
    #[default]
    Unloaded,
    /// The load `request` is queued behind the store's writes and has not arrived.
    Loading {
        /// The load's id; an answer with another id is dropped.
        request: LoadId,
        /// The records the session got after the load was queued, oldest first. The load does
        /// not hold them, so they are applied after it.
        later: Vec<TranscriptRecord>,
        /// What waits for the transcript, in the order it came.
        waiting: VecDeque<AfterLoad>,
    },
    /// Loaded, and kept up to date with every record the session gets.
    Loaded(Transcript),
}

/// What waits for a session's transcript to be loaded.
pub(super) enum AfterLoad {
    /// A prompt Partitur generated for the session, delivered as
    /// [`deliver`](App::deliver) decides.
    Deliver(Generated),
    /// The end of the session's turn, reported to its parent as
    /// [`report_to_parent`](App::report_to_parent) describes.
    Report {
        /// How the turn ended.
        outcome: ChildOutcome,
        /// Who started it.
        origin: TurnOrigin,
    },
    /// A tool call whose answer reads the transcript, taken up again.
    Tool(ToolCall),
}

/// What `App` keeps in memory about one session.
#[derive(Default)]
pub(super) struct LiveSession {
    /// The transcript, as far as it is loaded.
    pub(super) transcript: TranscriptState,
    /// The options the session's backend offers.
    pub(super) config: Vec<ConfigOption>,
    /// Whether the session's backend session was created or loaded by this process, so
    /// prompts can go to it directly. A backend id restored from the store is not attached.
    pub(super) attached: bool,
    /// Prompts waiting for the session to be attached, oldest first.
    pub(super) queued: Vec<QueuedPrompt>,
    /// Who started the running turn; taken when the turn ends.
    pub(super) turn_origin: Option<TurnOrigin>,
    /// How many background tasks the session's agent runs. Kept in memory only: the
    /// background tasks end with the agent's process.
    pub(super) background: usize,
    /// Prompts Partitur generated for the session while it was running or waiting, oldest
    /// first; one goes out each time its turn ends. Kept in memory only: a restart drops them.
    pub(super) deferred: VecDeque<Generated>,
    /// Whether the model change sent when the session attached waits for the agent's answer:
    /// until the options for the new model arrive, or the agent rejects the change, the
    /// recorded effort is kept as it is and the prompts waiting for the session stay queued.
    pub(super) effort_pending: bool,
    /// For each option Partitur changed and the agent has not answered yet, the value the
    /// agent reported last: a change the agent rejects puts it back.
    pub(super) unconfirmed: HashMap<ConfigId, ConfigValueId>,
    /// The proposals the user has not decided yet, with their children: the ones made in this
    /// process and the undecided ones of the transcript loaded from the store.
    pub(super) proposals: HashMap<ProposalId, Vec<ProposedChild>>,
    /// The latest git reading of the session, once the Env tab showed it.
    pub(super) env_git: Option<EnvGit>,
    /// The cleanup lines of the latest delete sheet of the top-level session, as far as git
    /// has answered.
    pub(super) cleanup: Option<Sheet>,
}

impl LiveState for HashMap<SessionId, LiveSession> {
    fn transcript(&self, session: &SessionId) -> Option<&Transcript> {
        match &self.get(session)?.transcript {
            TranscriptState::Loaded(transcript) => Some(transcript),
            TranscriptState::Unloaded | TranscriptState::Loading { .. } => None,
        }
    }

    fn background(&self, session: &SessionId) -> usize {
        self.get(session).map_or(0, |live| live.background)
    }
}

/// The permission requests waiting for the user's answer.
#[derive(Default)]
pub(super) struct Permissions {
    /// Every waiting request, by session and request, with the options it offers.
    pub(super) waiting: HashMap<(SessionId, PermissionId), Vec<PermissionOption>>,
    /// The requests of `waiting` that wait for the user, in the order they arrived, with
    /// their titles; a request Partitur answers at once never joins them.
    pub(super) asked: Vec<(SessionId, PermissionId, String)>,
}

impl Permissions {
    /// Whether a request of `session` waits for an answer.
    pub(super) fn any_of(&self, session: &SessionId) -> bool {
        self.waiting.keys().any(|(waiting, _)| waiting == session)
    }

    /// Drops every request of `session`.
    pub(super) fn forget(&mut self, session: &SessionId) {
        self.waiting.retain(|(waiting, _), _| waiting != session);
        self.asked.retain(|(asked, _, _)| asked != session);
    }

    /// Whether anything here still mentions `session`.
    #[cfg(test)]
    pub(super) fn holds(&self, session: &SessionId) -> bool {
        self.any_of(session) || self.asked.iter().any(|(asked, _, _)| asked == session)
    }
}

/// The backend requests that have not been answered yet.
#[derive(Default)]
pub(super) struct Requests {
    /// Each request's session and what it asked for.
    pub(super) pending: HashMap<RequestId, (SessionId, Request)>,
}

impl Requests {
    /// Whether a request made for `session` is unanswered.
    pub(super) fn any_for(&self, session: &SessionId) -> bool {
        self.pending.values().any(|(made, _)| made == session)
    }

    /// Drops every request made for `session`.
    pub(super) fn forget(&mut self, session: &SessionId) {
        self.pending.retain(|_, (made, _)| made != session);
    }
}

impl App {
    /// What this process keeps about `session`, when it keeps anything.
    pub(super) fn live(&self, session: &SessionId) -> Option<&LiveSession> {
        self.live.get(session)
    }

    /// What this process keeps about `session`, to change, when it keeps anything.
    pub(super) fn live_mut(&mut self, session: &SessionId) -> Option<&mut LiveSession> {
        self.live.get_mut(session)
    }

    /// What this process keeps about `session`, made empty when it keeps nothing yet.
    pub(super) fn live_entry(&mut self, session: &SessionId) -> &mut LiveSession {
        self.live.entry(session.clone()).or_default()
    }

    /// The loaded transcript of `session`, `None` while it is not loaded.
    pub(super) fn transcript(&self, session: &SessionId) -> Option<&Transcript> {
        self.live.transcript(session)
    }

    /// Whether the load of `session`'s transcript is on its way.
    pub(super) fn loading(&self, session: &SessionId) -> bool {
        self.live(session)
            .is_some_and(|live| matches!(live.transcript, TranscriptState::Loading { .. }))
    }

    /// Runs `then` now when the transcript of `session` is loaded. Otherwise it waits for the
    /// transcript, behind what waits already, and the load is queued unless it is on its way.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists, and what `then`
    /// returns when it runs now.
    pub(super) fn with_transcript(
        &mut self,
        session: &SessionId,
        then: AfterLoad,
    ) -> Result<(), AppError> {
        if self.transcript(session).is_some() {
            return self.run_after_load(session, then);
        }
        self.start_load(session)?;
        if let TranscriptState::Loading { waiting, .. } = &mut self.live_entry(session).transcript {
            waiting.push_back(then);
        }
        Ok(())
    }

    /// Queues the load of the transcript of `session`, unless it is loaded or on its way.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists.
    pub(super) fn start_load(&mut self, session: &SessionId) -> Result<(), AppError> {
        let project = self.session(session)?.project.clone();
        if self
            .live(session)
            .is_some_and(|live| !matches!(live.transcript, TranscriptState::Unloaded))
        {
            return Ok(());
        }
        let request = self.queue_load(project, session.clone());
        self.live_entry(session).transcript = TranscriptState::Loading {
            request,
            later: Vec::new(),
            waiting: VecDeque::new(),
        };
        Ok(())
    }

    /// Queues the load of the records of `session`, a session of `project`, and returns its id.
    pub(super) fn queue_load(&mut self, project: ProjectId, session: SessionId) -> LoadId {
        self.next_load = self.next_load.saturating_add(1);
        let request = LoadId(self.next_load);
        self.queue.send(StoreCommand::Load {
            request,
            project,
            session,
        });
        request
    }

    /// Takes up the answer to the load `request` of the transcript of `session`, as the module
    /// describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when the load failed, and the first error of what waited
    /// for the transcript; what comes after a failure runs all the same.
    pub(super) fn transcript_loaded(
        &mut self,
        request: LoadId,
        session: &SessionId,
        records: Result<Vec<TranscriptRecord>, Arc<StoreError>>,
    ) -> Result<(), AppError> {
        let Some(live) = self.live.get_mut(session) else {
            return Ok(());
        };
        let (later, waiting) = match std::mem::take(&mut live.transcript) {
            TranscriptState::Loading {
                request: expected,
                later,
                waiting,
            } if expected == request => (later, waiting),
            other => {
                live.transcript = other;
                return Ok(());
            }
        };
        let mut records = match records {
            Ok(records) => records,
            Err(error) => {
                let error = owned(error);
                for then in waiting {
                    if let AfterLoad::Tool(call) = then {
                        call.reply.send(Err(error.to_string()));
                    }
                }
                // The prompts sent meanwhile are stored; a later load shows them.
                return self
                    .send_after_load(session)
                    .and(Err(AppError::Store(error)));
            }
        };
        records.extend(later);
        self.load_transcript(session, &records)?;
        let mut first = self.send_after_load(session).err();
        for then in waiting {
            if let Err(error) = self.run_after_load(session, then) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Sends the prompts the user sent to `session` while its transcript loaded, once it is
    /// attached and no model change waits for the agent's answer.
    ///
    /// # Errors
    ///
    /// As [`send_queued`](Self::send_queued).
    fn send_after_load(&mut self, session: &SessionId) -> Result<(), AppError> {
        if self.attached(session) && !self.effort_pending(session) {
            return self.send_queued(session);
        }
        Ok(())
    }

    /// Runs `then`, which waited for the transcript of `session`.
    ///
    /// # Errors
    ///
    /// What the delivery, the report or the tool call returns.
    fn run_after_load(&mut self, session: &SessionId, then: AfterLoad) -> Result<(), AppError> {
        match then {
            AfterLoad::Deliver(generated) => self.deliver(session, generated).map(drop),
            AfterLoad::Report { outcome, origin } => self.report_loaded(session, outcome, origin),
            AfterLoad::Tool(call) => self.tool_call_received(call),
        }
    }

    /// Whether `session`'s backend session is attached, without checking it exists.
    pub(super) fn attached(&self, session: &SessionId) -> bool {
        self.live(session).is_some_and(|live| live.attached)
    }

    /// Whether prompts wait for `session` to be attached.
    pub(super) fn has_queued(&self, session: &SessionId) -> bool {
        self.live(session)
            .is_some_and(|live| !live.queued.is_empty())
    }

    /// Whether a model change of `session` waits for the agent's answer.
    pub(super) fn effort_pending(&self, session: &SessionId) -> bool {
        self.live(session).is_some_and(|live| live.effort_pending)
    }

    /// The prompts Partitur deferred for `session`, oldest first; `None` when there are none.
    pub(super) fn deferred_of(&self, session: &SessionId) -> Option<&VecDeque<Generated>> {
        self.live(session)
            .map(|live| &live.deferred)
            .filter(|queue| !queue.is_empty())
    }

    /// Drops everything this process keeps about `session`, its waiting permission requests
    /// and its unanswered backend requests among them.
    ///
    /// The tool calls that waited for its transcript are answered that the session was deleted.
    pub(super) fn forget_live(&mut self, session: &SessionId) {
        if let Some(LiveSession {
            transcript: TranscriptState::Loading { waiting, .. },
            ..
        }) = self.live.remove(session)
        {
            for then in waiting {
                if let AfterLoad::Tool(call) = then {
                    call.reply.send(Err(DELETED_SESSION.to_owned()));
                }
            }
        }
        self.permissions.forget(session);
        self.requests.forget(session);
    }
}

#[cfg(test)]
mod tests;

/// `error` as the app's own: an event the UI cloned shares its error, whose text is what the
/// user sees.
pub(super) fn owned(error: Arc<StoreError>) -> StoreError {
    Arc::try_unwrap(error).unwrap_or_else(|shared| StoreError::Unavailable(shared.to_string()))
}
