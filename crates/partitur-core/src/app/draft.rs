//! The session the new-session sheet prepares before it exists.
//!
//! Opening the sheet starts a warm session: a backend session on the seeded backend, in the
//! folder and with the orchestrator flag the real session will have, so the agent reports the
//! models and efforts it offers before anything is created. The newest session of the project
//! seeds the backend, model, effort and auto-allow; a seed whose backend is no longer
//! configured gives only its auto-allow, and a project without sessions starts on the default
//! backend with the agent's own choices and auto-allow off. While the sheet is open the warm
//! session lives outside the session list and the store. Picking a model sets it on the warm
//! session at once, effort and auto-allow are only recorded, and picking another backend
//! closes the warm session and warms up a fresh one under a new id.
//!
//! Create adopts the warm session as the real one under the same id: it is saved, selected and,
//! once the agent has answered, attached with its options and the updates it reported so far.
//! The model and effort it records are the wanted ones where the agent lists them, else the
//! agent's current ones. Cancel closes the warm session and leaves nothing behind: nothing is
//! saved and every event it still reports is ignored.

use std::path::PathBuf;

use super::orchestration::UNKNOWN_SESSION;
use super::{App, AppError, NEW_SESSION_TITLE, Request, unix_seconds};
use crate::backend::{BackendEvent, ConfigCategory, ConfigOption, Update};
use crate::ids::{
    BackendId, BackendSessionId, ConfigValueId, ProjectId, RequestId, SessionId, ToolRequestId,
};
use crate::orchestrator::is_orchestrator;
use crate::session::{Scope, Session, Status};
use crate::transcript::Transcript;
use crate::view_model::{DraftChoice, DraftView, Warmup, option_view};

/// The session being prepared: what the sheet has chosen and how far its warm session is.
pub(super) struct Draft {
    /// The warm session's id, which the created session keeps.
    id: SessionId,
    /// The request that started the warm session.
    request: RequestId,
    /// What the created session will be.
    settings: Settings,
    /// How far the warm session has started.
    phase: Phase,
    /// The updates the warm session reported, in arrival order, applied on create.
    held: Vec<Update>,
}

/// What the sheet has chosen for the session, which carries over to a fresh warm-up.
struct Settings {
    project: ProjectId,
    scope: Scope,
    cwd: PathBuf,
    orchestrator: bool,
    backend: BackendId,
    /// The model wanted, kept where the agent lists it.
    model: Option<ConfigValueId>,
    /// The effort wanted, kept where the agent lists it.
    effort: Option<ConfigValueId>,
    auto_allow: bool,
}

/// How far a warm session has started.
enum Phase {
    /// The create is on its way.
    Loading,
    /// The agent answered with its backend id and options.
    Ready {
        backend_session: BackendSessionId,
        config: Vec<ConfigOption>,
    },
    /// The agent could not start, for this reason.
    Failed(String),
}

impl App {
    /// Prepares a session in `project` for the new-session sheet and starts its warm
    /// session, as the [module](self) describes. A session being prepared already is
    /// discarded first, as [`discard_draft`](Self::discard_draft) does.
    ///
    /// The warm session works in the project root or the repository's folder, and is an
    /// orchestrator when the session will be one. A warm-up the backend cannot queue does not
    /// fail the call: the draft shows the failure instead.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownProject`] or [`AppError::UnknownRepository`] when `project`
    /// or the scope's repository does not exist; nothing changes then.
    pub fn prepare_session(&mut self, project: ProjectId, scope: Scope) -> Result<(), AppError> {
        let cwd = self.working_directory(&project, &scope)?;
        let orchestrator = is_orchestrator(self.project(&project)?, &scope);
        // The open draft is gone even when its close cannot be queued; only the outcome of
        // the new warm-up is reported, in the draft.
        drop(self.discard_draft());
        let mut settings = Settings {
            project,
            scope,
            cwd,
            orchestrator,
            backend: self.default.clone(),
            model: None,
            effort: None,
            auto_allow: false,
        };
        self.seed(&mut settings);
        self.warm_up(settings);
        Ok(())
    }

    /// The session being prepared, or `None` when the sheet is closed.
    pub fn draft(&self) -> Option<DraftView> {
        let draft = self.draft.as_ref()?;
        let settings = &draft.settings;
        let project = self.project(&settings.project).ok()?;
        let repository = match &settings.scope {
            Scope::Project => None,
            Scope::Repository(id) => project
                .repositories
                .iter()
                .find(|repository| &repository.id == id)
                .map(|repository| repository.name.clone()),
        };
        Some(DraftView {
            project: project.name.clone(),
            repository,
            repositories: project.repositories.len(),
            orchestrator: settings.orchestrator,
            backend: settings.backend.clone(),
            backends: self.backends.clone(),
            model: draft.choice(&ConfigCategory::Model, settings.model.as_ref()),
            effort: draft.choice(&ConfigCategory::Effort, settings.effort.as_ref()),
            auto_allow: settings.auto_allow,
            warmup: draft.warmup(),
        })
    }

    /// Moves the session being prepared to the configured backend `backend`: the warm
    /// session is closed and a fresh one, under a new id, starts on `backend`. The wanted
    /// model, effort and auto-allow stay, and the pickers wait for the new agent. Picking the
    /// backend the warm session runs on changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::UnknownBackend`] when `backend` is not configured and
    /// [`AppError::Backend`] when the warm session cannot be closed; nothing changes then.
    pub fn draft_backend(&mut self, backend: BackendId) -> Result<(), AppError> {
        let draft = self.draft.as_ref().ok_or(AppError::NoDraft)?;
        if !self.is_configured(&backend) {
            return Err(AppError::UnknownBackend(backend));
        }
        if draft.settings.backend == backend {
            return Ok(());
        }
        self.backend.close_session(draft.id.clone())?;
        if let Some(old) = self.draft.take() {
            self.deleted.insert(old.id);
            self.warm_up(Settings {
                backend,
                ..old.settings
            });
        }
        Ok(())
    }

    /// Picks the model `value` for the session being prepared and sets it on the warm
    /// session at once, so the agent can report the efforts that model offers.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::NoSuchConfig`] when its agent has not answered yet or offers no model,
    /// and [`AppError::Backend`] when the backend cannot queue the change; nothing changes
    /// then.
    pub fn draft_model(&mut self, value: ConfigValueId) -> Result<(), AppError> {
        let draft = self.draft.as_mut().ok_or(AppError::NoDraft)?;
        let option = match &mut draft.phase {
            Phase::Ready { config, .. } => config
                .iter_mut()
                .find(|option| option.category == ConfigCategory::Model),
            Phase::Loading | Phase::Failed(_) => None,
        }
        .ok_or(AppError::NoSuchConfig(ConfigCategory::Model))?;
        self.backend
            .set_config(draft.id.clone(), &option.id, &value)?;
        option.current = value.clone();
        draft.settings.model = Some(value);
        Ok(())
    }

    /// Picks the reasoning effort `value` for the session being prepared; it is sent when
    /// the session is created.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared.
    pub fn draft_effort(&mut self, value: ConfigValueId) -> Result<(), AppError> {
        self.draft_settings()?.effort = Some(value);
        Ok(())
    }

    /// Sets whether the session being prepared will grant permission requests without
    /// asking.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared.
    pub fn draft_auto_allow(&mut self, enabled: bool) -> Result<(), AppError> {
        self.draft_settings()?.auto_allow = enabled;
        Ok(())
    }

    /// Creates the session being prepared, adopting its warm session, and selects it.
    ///
    /// The session keeps the warm session's id, is titled "New session" and starts
    /// [`Idle`](Status::Idle) with the chosen backend and auto-allow. Once the agent has
    /// answered, the session is attached with the agent's backend id and options, records
    /// the wanted model and effort where the agent lists them (else the agent's current
    /// ones), gets the updates the agent reported so far, and the values that differ from
    /// the agent's current ones are sent. Before the agent has answered, the session records
    /// the wanted values as they are, and they are applied when the answer arrives.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared,
    /// [`AppError::DraftFailed`] when its agent could not start, [`AppError::Store`] when the
    /// session list cannot be saved (the session is not created and the draft stays then),
    /// and afterwards [`AppError::Store`] or [`AppError::Backend`] when a held update cannot
    /// be saved or a choice cannot be sent.
    pub fn create_from_draft(&mut self) -> Result<SessionId, AppError> {
        let session = self.draft.as_ref().ok_or(AppError::NoDraft)?.session()?;
        let id = session.id.clone();
        let project = session.project.clone();
        self.sessions.push(session);
        if let Err(error) = self.save_sessions(&project) {
            self.sessions.pop();
            return Err(error);
        }
        self.draft
            .take()
            .map_or(Ok(()), |draft| self.adopt(draft))?;
        self.current = Some(id.clone());
        Ok(id)
    }

    /// Discards the session being prepared: the draft is gone, every event its warm
    /// session still reports is ignored, and the warm session is closed.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared and
    /// [`AppError::Backend`] when the close cannot be queued; the draft is gone either way.
    pub fn discard_draft(&mut self) -> Result<(), AppError> {
        let draft = self.draft.take().ok_or(AppError::NoDraft)?;
        self.deleted.insert(draft.id.clone());
        self.backend.close_session(draft.id)?;
        Ok(())
    }

    /// Whether `session` is the warm session of the session being prepared.
    pub(super) fn is_draft(&self, session: &SessionId) -> bool {
        self.draft
            .as_ref()
            .is_some_and(|draft| &draft.id == session)
    }

    /// Applies `event` of the warm session, as [`apply_event`](Self::apply_event) describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when the answer to a tool call cannot be queued.
    pub(super) fn draft_event(&mut self, event: BackendEvent) -> Result<(), AppError> {
        let refused = self.draft.as_mut().and_then(|draft| draft.apply(event));
        refused.map_or(Ok(()), |call| self.refuse_tool(call, UNKNOWN_SESSION))
    }

    /// Takes the backend, wanted model and effort, and auto-allow from the newest session of
    /// the project into `settings`, as the [module](self) describes. Of sessions created in
    /// the same second, the later one counts as newer.
    fn seed(&self, settings: &mut Settings) {
        let Some(newest) = self
            .sessions
            .iter()
            .filter(|session| session.project == settings.project)
            .max_by_key(|session| session.created_at)
        else {
            return;
        };
        settings.auto_allow = newest.auto_allow;
        if self.is_configured(&newest.backend) {
            settings.backend = newest.backend.clone();
            settings.model = newest.model.clone();
            settings.effort = newest.effort.clone();
        }
    }

    /// Starts a warm session under a new id for `settings` and makes it the draft. A
    /// create the backend cannot queue fails the draft.
    fn warm_up(&mut self, settings: Settings) {
        let id = SessionId::new();
        let request = self.next_request();
        let queued = self.backend.new_session(
            request,
            id.clone(),
            &settings.backend,
            &settings.cwd,
            settings.orchestrator,
        );
        let phase = match queued {
            Ok(()) => Phase::Loading,
            Err(error) => Phase::Failed(error.to_string()),
        };
        self.draft = Some(Draft {
            id,
            request,
            settings,
            phase,
            held: Vec::new(),
        });
    }

    /// The settings of the session being prepared.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::NoDraft`] when no session is being prepared.
    fn draft_settings(&mut self) -> Result<&mut Settings, AppError> {
        let draft = self.draft.as_mut().ok_or(AppError::NoDraft)?;
        Ok(&mut draft.settings)
    }

    /// Takes over the created session's warm session, as
    /// [`create_from_draft`](Self::create_from_draft) describes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] when a held update cannot be saved and
    /// [`AppError::Backend`] when a choice cannot be sent.
    fn adopt(&mut self, draft: Draft) -> Result<(), AppError> {
        let id = draft.id;
        self.transcripts.insert(id.clone(), Transcript::default());
        match draft.phase {
            Phase::Ready { config, .. } => {
                self.config.insert(id.clone(), config);
                self.attached.insert(id.clone());
                for update in &draft.held {
                    self.apply_update(&id, update)?;
                }
                self.reapply_choices(&id)
            }
            Phase::Loading | Phase::Failed(_) => {
                self.pending.insert(draft.request, (id, Request::Create));
                Ok(())
            }
        }
    }
}

impl Draft {
    /// Applies `event` of the warm session, as [`App::apply_event`] describes, and returns
    /// the tool call to refuse when the event is one.
    fn apply(&mut self, event: BackendEvent) -> Option<ToolRequestId> {
        match event {
            BackendEvent::SessionCreated {
                request,
                backend_session,
                config,
                ..
            } if request == self.request => {
                self.phase = Phase::Ready {
                    backend_session,
                    config,
                };
            }
            BackendEvent::RequestFailed { request, error, .. } if request == self.request => {
                self.phase = Phase::Failed(error);
            }
            BackendEvent::SessionClosed { error, .. } => {
                self.phase = Phase::Failed(format!("connection to the agent closed: {error}"));
            }
            BackendEvent::Update {
                update: Update::ConfigOptions(options),
                ..
            } => {
                if let Phase::Ready { config, .. } = &mut self.phase {
                    *config = options;
                }
            }
            BackendEvent::Update { update, .. } => self.held.push(update),
            BackendEvent::ToolCalled { call, .. } => return Some(call),
            BackendEvent::SessionCreated { .. }
            | BackendEvent::RequestFailed { .. }
            | BackendEvent::SessionLoaded { .. } => {}
        }
        None
    }

    /// The session creating this draft makes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::DraftFailed`] when the warm session could not start.
    fn session(&self) -> Result<Session, AppError> {
        let settings = &self.settings;
        let (backend_session, model, effort) = match &self.phase {
            Phase::Failed(_) => return Err(AppError::DraftFailed),
            Phase::Loading => (None, settings.model.clone(), settings.effort.clone()),
            Phase::Ready {
                backend_session,
                config,
            } => (
                Some(backend_session.clone()),
                selection(config, &ConfigCategory::Model, settings.model.as_ref()),
                selection(config, &ConfigCategory::Effort, settings.effort.as_ref()),
            ),
        };
        Ok(Session {
            id: self.id.clone(),
            project: settings.project.clone(),
            scope: settings.scope.clone(),
            cwd: settings.cwd.clone(),
            title: NEW_SESSION_TITLE.into(),
            backend_session,
            backend: settings.backend.clone(),
            model,
            effort,
            status: Status::Idle,
            parent: None,
            auto_allow: settings.auto_allow,
            instructions_injected: false,
            created_at: unix_seconds(),
            orchestrator: settings.orchestrator,
            archived: None,
        })
    }

    /// The picker of `category`, showing `wanted` where the agent lists it.
    fn choice(&self, category: &ConfigCategory, wanted: Option<&ConfigValueId>) -> DraftChoice {
        let Phase::Ready { config, .. } = &self.phase else {
            return DraftChoice::Waiting(wanted.cloned());
        };
        let option = config.iter().find(|option| &option.category == category);
        match (option, selection(config, category, wanted)) {
            (Some(option), Some(current)) => DraftChoice::Offered(option_view(option, current)),
            _ => DraftChoice::NotOffered,
        }
    }

    /// How far the warm session has started.
    fn warmup(&self) -> Warmup {
        match &self.phase {
            Phase::Loading => Warmup::Loading,
            Phase::Ready { .. } => Warmup::Ready,
            Phase::Failed(error) => Warmup::Failed(error.clone()),
        }
    }
}

/// The value of the first option of `category` in `config` a session records: `wanted` when
/// the option lists it among its choices, else the option's current value. `None` when no
/// option has the category.
fn selection(
    config: &[ConfigOption],
    category: &ConfigCategory,
    wanted: Option<&ConfigValueId>,
) -> Option<ConfigValueId> {
    let option = config.iter().find(|option| &option.category == category)?;
    let listed = wanted.filter(|wanted| option.choices.iter().any(|choice| choice.id == **wanted));
    Some(listed.unwrap_or(&option.current).clone())
}

#[cfg(test)]
mod tests;
