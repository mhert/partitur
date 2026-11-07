//! How Partitur talks to an orchestrator on its own: a child's finished turn reaches its
//! parent as a prompt, and prompts Partitur generates for a busy session wait for its turn to
//! end.

use std::collections::VecDeque;

use super::{App, AppError};
use crate::backend::{ChildOutcome, TurnOrigin, Update};
use crate::ids::{RepositoryId, SessionId};
use crate::orchestrator::child_result_prompt;
use crate::session::{Scope, Status};
use crate::transcript::{Entry, Transcript};

/// What a child reports when its turn ended without an answer from the agent.
const NO_FINAL_MESSAGE: &str = "(no final message)";

/// A prompt Partitur writes to a session on its own, not typed by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Generated {
    /// Wakes an orchestrator, for example with the decision on its proposal.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "nothing wakes an orchestrator until proposals are decided"
        )
    )]
    Wake(String),
    /// A prompt an orchestrator sends one of its children.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "orchestrators cannot prompt their children until send_to_child is handled"
        )
    )]
    FromOrchestrator(String),
    /// A child's finished turn, reported to its orchestrator.
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

impl Generated {
    /// The update that records the prompt in the receiving session's transcript.
    fn record(&self) -> Update {
        match self {
            Self::Wake(text) => Update::PartiturPrompt(text.clone()),
            Self::FromOrchestrator(text) => Update::OrchestratorPrompt(text.clone()),
            Self::ChildResult {
                child,
                repository,
                title,
                origin,
                outcome,
                text,
            } => Update::ChildResult {
                child: child.clone(),
                repository: repository.clone(),
                title: title.clone(),
                origin: *origin,
                outcome: outcome.clone(),
                text: text.clone(),
            },
        }
    }

    /// The text the agent is prompted with.
    fn text(&self) -> String {
        match self {
            Self::Wake(text) | Self::FromOrchestrator(text) => text.clone(),
            Self::ChildResult {
                child,
                repository,
                title,
                origin,
                outcome,
                text,
            } => child_result_prompt(child, repository, title, *origin, outcome, text),
        }
    }
}

impl App {
    /// Starts a turn with `generated` when the session is neither
    /// [`Running`](Status::Running) nor [`WaitingForYou`](Status::WaitingForYou), else queues
    /// it behind the prompts already deferred for the session; returns whether it went out
    /// now.
    ///
    /// A generated turn counts as started by the orchestrator: a prompt from an orchestrator
    /// is, and the others go to orchestrators, which have no parent to report the origin to.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn deliver(
        &mut self,
        session: &SessionId,
        generated: Generated,
    ) -> Result<bool, AppError> {
        if matches!(
            self.session(session)?.status,
            Status::Running | Status::WaitingForYou
        ) {
            self.deferred
                .entry(session.clone())
                .or_default()
                .push_back(generated);
            return Ok(false);
        }
        self.start_generated(session, &generated)?;
        Ok(true)
    }

    /// Sends the oldest prompt deferred for `session`, if any.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists,
    /// [`AppError::Backend`] when the backend cannot queue the prompt and [`AppError::Store`]
    /// when the change cannot be saved.
    pub(super) fn deliver_next(&mut self, session: &SessionId) -> Result<(), AppError> {
        let Some(generated) = self.deferred.get_mut(session).and_then(VecDeque::pop_front) else {
            return Ok(());
        };
        self.start_generated(session, &generated)
    }

    /// A child's turn that `origin` started ended with `outcome`: its final message goes to
    /// its parent. A session without a parent, or whose parent no longer exists, reports to
    /// nobody, and so does a project-scoped session, which is never a child.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when `child` does not exist,
    /// [`AppError::Store`] when its transcript cannot be loaded or the change cannot be saved
    /// and [`AppError::Backend`] when the backend cannot queue the prompt.
    pub(super) fn report_to_parent(
        &mut self,
        child: &SessionId,
        outcome: ChildOutcome,
        origin: TurnOrigin,
    ) -> Result<(), AppError> {
        let session = self.session(child)?;
        let (Some(parent), Scope::Repository(repository)) = (&session.parent, &session.scope)
        else {
            return Ok(());
        };
        if self.session(parent).is_err() {
            return Ok(());
        }
        let (parent, repository, title) =
            (parent.clone(), repository.clone(), session.title.clone());
        let result = Generated::ChildResult {
            child: child.clone(),
            repository,
            title,
            origin,
            outcome,
            text: final_message(self.ensure_transcript(child)?),
        };
        self.deliver(&parent, result)?;
        Ok(())
    }

    /// The session's transcript, loaded from the store when it is not loaded yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when no such session exists and
    /// [`AppError::Store`] when its updates cannot be loaded.
    fn ensure_transcript(&mut self, id: &SessionId) -> Result<&Transcript, AppError> {
        self.session(id)?;
        if !self.transcripts.contains_key(id) {
            let updates = self.store.load_updates(id.clone())?;
            self.transcripts
                .insert(id.clone(), Transcript::from_updates(&updates));
        }
        self.transcripts
            .get(id)
            .ok_or_else(|| AppError::UnknownSession(id.clone()))
    }

    /// Starts a turn of `session` with `generated`, as [`deliver`](Self::deliver) describes.
    fn start_generated(
        &mut self,
        session: &SessionId,
        generated: &Generated,
    ) -> Result<(), AppError> {
        self.start_turn(
            session,
            &generated.record(),
            generated.text(),
            Vec::new(),
            TurnOrigin::Orchestrator,
        )
    }
}

/// The agent's last answer in the latest turn of `transcript`, or `(no final message)` when
/// the agent said nothing since the turn's prompt.
pub(super) fn final_message(transcript: &Transcript) -> String {
    transcript
        .entries()
        .iter()
        .rev()
        .take_while(|entry| !starts_turn(entry))
        .find_map(|entry| match entry {
            Entry::Agent(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| NO_FINAL_MESSAGE.to_owned())
}

/// Whether `entry` is the prompt a turn starts with.
fn starts_turn(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::User(_)
            | Entry::OrchestratorPrompt(_)
            | Entry::PartiturPrompt(_)
            | Entry::ChildResult { .. }
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::backend::{
        BackendEvent, ChildOutcome, PermissionKind, PermissionOption, PromptContent, StopReason,
        ToolKind, ToolStatus, TurnOrigin, Update,
    };
    use crate::ids::{
        BackendSessionId, PermissionId, PermissionOptionId, ProjectId, RepositoryId, SessionId,
        ToolCallId,
    };
    use crate::orchestrator::child_result_prompt;
    use crate::session::{Scope, Status};
    use crate::store::Store as _;
    use crate::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use crate::transcript::Entry;

    struct Fixture {
        app: App,
        backend: RecordingBackend,
        store: MemoryStore,
    }

    fn fixture_with(store: MemoryStore) -> Fixture {
        let (backends, default) = single_claude();
        let backend = RecordingBackend::new();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            backends,
            default,
        )
        .unwrap();
        Fixture {
            app,
            backend,
            store,
        }
    }

    /// A project folder whose children named in `repositories` are git repositories.
    fn project_dir(repositories: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for name in repositories {
            fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        dir
    }

    fn repository(name: &str) -> RepositoryId {
        RepositoryId::try_from(name).unwrap()
    }

    /// Creates a session in `scope` and attaches it, as though its instructions were already
    /// sent, so its prompts reach the backend as given.
    fn attached_session(fixture: &mut Fixture, project: &ProjectId, scope: Scope) -> SessionId {
        let session = fixture.app.new_session(project.clone(), scope).unwrap();
        let calls = fixture.backend.calls();
        let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
        fixture
            .app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new(format!("b-{session}")),
                config: Vec::new(),
            })
            .unwrap();
        fixture
            .app
            .change_session(session.clone(), |attached| {
                attached.instructions_injected = true;
            })
            .unwrap();
        session
    }

    /// An attached child of `parent` working in `repository`, titled `title`.
    fn attached_child(
        fixture: &mut Fixture,
        project: &ProjectId,
        parent: &SessionId,
        name: &str,
        title: &str,
    ) -> SessionId {
        let child = attached_session(fixture, project, Scope::Repository(repository(name)));
        fixture
            .app
            .change_session(child.clone(), |spawned| {
                spawned.parent = Some(parent.clone());
                spawned.title = title.into();
            })
            .unwrap();
        child
    }

    /// An open two-repository project with an attached, idle orchestrator and an attached
    /// child of it in `api` titled "API endpoint".
    struct Family {
        _dir: TempDir,
        fixture: Fixture,
        project: ProjectId,
        parent: SessionId,
        child: SessionId,
    }

    fn family() -> Family {
        let dir = project_dir(&["api", "web"]);
        let mut fixture = fixture_with(MemoryStore::new());
        let project = fixture.app.open_project(dir.path()).unwrap();
        let parent = attached_session(&mut fixture, &project, Scope::Project);
        assert!(fixture.app.session(&parent).unwrap().orchestrator);
        let child = attached_child(&mut fixture, &project, &parent, "api", "API endpoint");
        Family {
            _dir: dir,
            fixture,
            project,
            parent,
            child,
        }
    }

    fn send(fixture: &mut Fixture, session: &SessionId, update: Update) {
        fixture
            .app
            .apply_event(BackendEvent::Update {
                session: session.clone(),
                update,
            })
            .unwrap();
    }

    /// Starts a turn of `child` as its orchestrator would.
    fn orchestrate(fixture: &mut Fixture, child: &SessionId) {
        fixture
            .app
            .deliver(
                child,
                Generated::FromOrchestrator("Add the endpoint".into()),
            )
            .unwrap();
    }

    fn status(app: &App, session: &SessionId) -> Status {
        app.session(session).unwrap().status
    }

    /// The texts of the prompts `session` was sent, oldest first.
    fn prompts(backend: &RecordingBackend, session: &SessionId) -> Vec<String> {
        backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt {
                    session: prompted,
                    content,
                } if &prompted == session => Some(
                    content
                        .into_iter()
                        .filter_map(|part| match part {
                            PromptContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    fn last_prompt(backend: &RecordingBackend, session: &SessionId) -> String {
        prompts(backend, session).pop().expect("no prompt was sent")
    }

    fn result_text(
        child: &SessionId,
        origin: TurnOrigin,
        outcome: &ChildOutcome,
        text: &str,
    ) -> String {
        child_result_prompt(
            child,
            &repository("api"),
            "API endpoint",
            origin,
            outcome,
            text,
        )
    }

    #[test]
    fn generated_prompts_are_recorded_by_their_kind_and_sent_as_written() {
        let wake = Generated::Wake("children started".into());
        let prompt = Generated::FromOrchestrator("Add the endpoint".into());

        assert_eq!(
            wake.record(),
            Update::PartiturPrompt("children started".into())
        );
        assert_eq!(wake.text(), "children started");
        assert_eq!(
            prompt.record(),
            Update::OrchestratorPrompt("Add the endpoint".into())
        );
        assert_eq!(prompt.text(), "Add the endpoint");
    }

    #[test]
    fn the_final_message_is_the_last_answer_of_the_latest_turn() {
        let answered = Transcript::from_updates(&[
            Update::UserText("go".into()),
            Update::AgentText("first".into()),
            Update::Notice("note".into()),
            Update::AgentText("last".into()),
            Update::TurnEnded(StopReason::EndTurn),
        ]);
        let silent = Transcript::from_updates(&[
            Update::UserText("go".into()),
            Update::AgentText("earlier turn".into()),
            Update::TurnEnded(StopReason::EndTurn),
            Update::OrchestratorPrompt("again".into()),
            Update::TurnEnded(StopReason::EndTurn),
        ]);

        assert_eq!(final_message(&answered), "last");
        assert_eq!(final_message(&silent), "(no final message)");
    }

    #[test]
    fn a_child_turn_end_prompts_its_idle_parent_with_the_result() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(&mut fixture, &child, Update::AgentText("half".into()));
        send(&mut fixture, &child, Update::AgentText(" done".into()));
        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        let outcome = ChildOutcome::Ended(StopReason::EndTurn);
        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::Prompt {
                session: parent.clone(),
                content: vec![PromptContent::Text(result_text(
                    &child,
                    TurnOrigin::Orchestrator,
                    &outcome,
                    "half done"
                ))],
            })
        );
        assert_eq!(
            fixture.app.transcripts[&parent].entries().last(),
            Some(&Entry::ChildResult {
                child: child.clone(),
                repository: repository("api"),
                title: "API endpoint".into(),
                origin: TurnOrigin::Orchestrator,
                outcome,
                text: "half done".into(),
            })
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);
        assert_eq!(status(&fixture.app, &child), Status::Done);
    }

    #[test]
    fn a_user_started_child_turn_is_marked_as_such() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        fixture.app.select_session(child.clone()).unwrap();
        fixture
            .app
            .submit_prompt("Also add a test", Vec::new())
            .unwrap();

        send(&mut fixture, &child, Update::AgentText("added".into()));
        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(turn started by the user)"), "{text}");
        assert_eq!(
            text,
            result_text(
                &child,
                TurnOrigin::User,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "added"
            )
        );
    }

    #[test]
    fn a_failed_child_turn_is_marked_failed_with_the_error() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(&mut fixture, &child, Update::Failed("boom".into()));

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(failed: boom)"), "{text}");
        assert_eq!(status(&fixture.app, &child), Status::Failed);
    }

    #[test]
    fn a_cancelled_child_turn_is_marked_with_its_stop_reason() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(
            &mut fixture,
            &child,
            Update::TurnEnded(StopReason::Cancelled),
        );

        let text = last_prompt(&fixture.backend, &parent);
        assert!(text.contains("(turn ended: cancelled)"), "{text}");
    }

    #[test]
    fn a_child_blocked_on_a_permission_produces_no_result() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);

        send(
            &mut fixture,
            &child,
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-1"),
                title: "Run cargo test".into(),
                kind: ToolKind::Execute,
                options: vec![PermissionOption {
                    id: PermissionOptionId::new("allow"),
                    name: "Allow".into(),
                    kind: PermissionKind::AllowOnce,
                }],
                name: None,
            },
        );

        assert!(prompts(&fixture.backend, &parent).is_empty());
        assert_eq!(status(&fixture.app, &child), Status::WaitingForYou);
        assert_eq!(status(&fixture.app, &parent), Status::Idle);
    }

    #[test]
    fn results_queue_while_the_parent_is_mid_turn_and_go_out_one_per_turn() {
        let Family {
            mut fixture,
            project,
            parent,
            child,
            ..
        } = family();
        let web = attached_child(&mut fixture, &project, &parent, "web", "Web page");
        fixture.app.select_session(parent.clone()).unwrap();
        fixture.app.submit_prompt("Plan it", Vec::new()).unwrap();
        for (session, answer) in [(&child, "api done"), (&web, "web done")] {
            orchestrate(&mut fixture, session);
            send(&mut fixture, session, Update::AgentText(answer.into()));
            send(
                &mut fixture,
                session,
                Update::TurnEnded(StopReason::EndTurn),
            );
        }

        assert_eq!(prompts(&fixture.backend, &parent), ["Plan it"]);

        let ended = ChildOutcome::Ended(StopReason::EndTurn);
        let web_result = child_result_prompt(
            &web,
            &repository("web"),
            "Web page",
            TurnOrigin::Orchestrator,
            &ended,
            "web done",
        );
        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(&child, TurnOrigin::Orchestrator, &ended, "api done")
        );
        assert_eq!(status(&fixture.app, &parent), Status::Running);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(last_prompt(&fixture.backend, &parent), web_result);
        assert_eq!(status(&fixture.app, &parent), Status::Running);

        send(
            &mut fixture,
            &parent,
            Update::TurnEnded(StopReason::EndTurn),
        );
        assert_eq!(prompts(&fixture.backend, &parent).len(), 3);
        assert_eq!(status(&fixture.app, &parent), Status::Done);
    }

    #[test]
    fn a_child_result_for_a_detached_parent_waits_for_the_reattach() {
        let Family {
            _dir,
            fixture,
            parent,
            child,
            ..
        } = family();
        let mut restarted = fixture_with(fixture.store.clone());
        drop(fixture);
        restarted.app.select_session(child.clone()).unwrap();
        restarted
            .app
            .submit_prompt("Add the endpoint", Vec::new())
            .unwrap();
        let Some(Call::LoadSession { request, .. }) = restarted.backend.calls().last().cloned()
        else {
            panic!("the child was not loaded");
        };
        restarted
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: child.clone(),
                config: Vec::new(),
            })
            .unwrap();
        send(&mut restarted, &child, Update::AgentText("done".into()));

        send(
            &mut restarted,
            &child,
            Update::TurnEnded(StopReason::EndTurn),
        );

        let Some(Call::LoadSession {
            request,
            session,
            orchestrator: true,
            ..
        }) = restarted.backend.calls().last().cloned()
        else {
            panic!("the parent was not loaded as an orchestrator");
        };
        assert_eq!(session, parent);
        assert!(prompts(&restarted.backend, &parent).is_empty());

        restarted
            .app
            .apply_event(BackendEvent::SessionLoaded {
                request,
                session: parent.clone(),
                config: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            prompts(&restarted.backend, &parent),
            [result_text(
                &child,
                TurnOrigin::User,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "done"
            )]
        );
        assert_eq!(status(&restarted.app, &parent), Status::Running);
    }

    #[test]
    fn a_child_without_a_final_message_reports_so() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(
            &mut fixture,
            &child,
            Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Read main.rs".into(),
                kind: ToolKind::Read,
                status: ToolStatus::Completed,
                content: Vec::new(),
                locations: Vec::new(),
            },
        );

        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "(no final message)"
            )
        );
    }

    #[test]
    fn a_session_without_a_parent_reports_to_nobody() {
        let Family {
            mut fixture,
            project,
            ..
        } = family();
        let lone = attached_session(&mut fixture, &project, Scope::Repository(repository("web")));
        fixture
            .app
            .submit_prompt("Fix the build", Vec::new())
            .unwrap();
        send(&mut fixture, &lone, Update::AgentText("fixed".into()));
        let calls = fixture.backend.calls();

        send(&mut fixture, &lone, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(fixture.backend.calls(), calls);
        assert_eq!(status(&fixture.app, &lone), Status::Done);
    }

    #[test]
    fn a_child_whose_transcript_is_not_loaded_is_read_from_the_store() {
        let Family {
            mut fixture,
            parent,
            child,
            ..
        } = family();
        orchestrate(&mut fixture, &child);
        send(
            &mut fixture,
            &child,
            Update::AgentText("from the store".into()),
        );
        fixture.app.transcripts.remove(&child);
        assert!(
            !fixture
                .store
                .load_updates(child.clone())
                .unwrap()
                .is_empty()
        );

        send(&mut fixture, &child, Update::TurnEnded(StopReason::EndTurn));

        assert_eq!(
            last_prompt(&fixture.backend, &parent),
            result_text(
                &child,
                TurnOrigin::Orchestrator,
                &ChildOutcome::Ended(StopReason::EndTurn),
                "from the store"
            )
        );
    }
}
