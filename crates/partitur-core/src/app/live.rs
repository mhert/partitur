//! What `App` keeps in memory about each session it has dealt with in this process, in one
//! place. A [`LiveSession`] holds one session's loaded transcript, the options its backend
//! offers, whether it is attached, the prompts waiting for it, who started its turn, its
//! background tasks, the prompts Partitur deferred for it, its unanswered option changes, its
//! undecided proposals, its latest git reading and its delete sheet. A session without an
//! entry has none of these: each field's empty value means the same as no entry.
//!
//! The permission requests waiting for an answer and the unanswered backend requests are
//! kept apart, in [`Permissions`] and [`Requests`], since they are looked up by their own
//! ids. Each forgets everything it holds for a session at once, as when the session is
//! deleted.

use std::collections::{HashMap, VecDeque};

use super::cleanup::Sheet;
use super::env_git::EnvGit;
use super::scheduler::Generated;
use super::{App, QueuedPrompt, Request};
use crate::backend::{ConfigOption, PermissionOption, ProposedChild, TurnOrigin};
use crate::ids::{ConfigId, ConfigValueId, PermissionId, ProposalId, RequestId, SessionId};
use crate::transcript::Transcript;
use crate::view_model::LiveState;

/// What `App` keeps in memory about one session.
#[derive(Default)]
pub(super) struct LiveSession {
    /// The transcript, once loaded; `None` while the session has not been opened.
    pub(super) transcript: Option<Transcript>,
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
        self.get(session)?.transcript.as_ref()
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

    /// The loaded transcript of `session`, `None` while it has not been opened.
    pub(super) fn transcript(&self, session: &SessionId) -> Option<&Transcript> {
        self.live(session)?.transcript.as_ref()
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
    pub(super) fn forget_live(&mut self, session: &SessionId) {
        self.live.remove(session);
        self.permissions.forget(session);
        self.requests.forget(session);
    }
}
