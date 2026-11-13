//! What the user does about the current session's workflow: approving the gate it waits
//! at, or asking for changes instead.
//!
//! Approving sends the gate's fixed approval prompt, from the rail's button or with Enter
//! on the Session tab. It is offered only while the session waits at a gate, is not in a
//! turn and is not archived; otherwise Enter does what it does without a gate. Asking for
//! changes shows the Session tab with the prompt box holding the start of the request, such
//! as `Changes to the plan: `, the cursor at its end and the focus in the box. The gate
//! closes once that prompt is sent, as it does for any prompt the session is sent.

use iced::Task;
use iced::widget::{operation, text_editor};
use partitur_core::{GateKind, SessionViewModel};

use crate::app::{self, Message, State, Tab};
use crate::session_view;

/// An interaction with the current session's workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowMessage {
    /// The user approved the gate the session waits at.
    Approve,
    /// The user asked for changes to what the gate offers instead of approving it.
    RequestChanges,
}

/// Applies `message` to the current session and returns the follow-up work: approving
/// scrolls the transcript to the prompt it sent, asking for changes focuses the prompt box.
/// An approval the core refuses is shown in the error bar.
pub fn update(state: &mut State, message: FlowMessage) -> Task<Message> {
    match message {
        FlowMessage::Approve => approve(state),
        FlowMessage::RequestChanges => request_changes(state),
    }
}

/// What Enter does on the Session tab when no widget took the key: it approves the gate
/// when it can be approved, and otherwise does what Enter does on the tab without a gate.
pub(crate) fn press_enter(state: &mut State) -> Task<Message> {
    let gated = state
        .app
        .session_view()
        .is_some_and(|model| approvable(&model).is_some());
    if gated {
        approve(state)
    } else {
        session_view::press_enter(state)
    }
}

/// The gate the session `model` waits at, when it can be approved now: the session is not
/// in a turn and not archived.
pub(crate) fn approvable(model: &SessionViewModel<'_>) -> Option<GateKind> {
    let gate = model.workflow.as_ref()?.status.gate.as_ref()?;
    (!model.running() && model.archived.is_none()).then_some(gate.kind)
}

/// Approves the current session's gate, then shows the transcript's end.
fn approve(state: &mut State) -> Task<Message> {
    let Some(session) = state.app.current_session() else {
        return Task::none();
    };
    if app::report(&mut state.error, state.app.approve_gate(session)).is_none() {
        return Task::none();
    }
    session_view::to_the_end(&mut state.session)
}

/// Shows the Session tab with the start of a request for changes to the current session's
/// gate in the prompt box, its cursor at the end and the focus in the box. Without a gate
/// nothing changes.
fn request_changes(state: &mut State) -> Task<Message> {
    let kind = state.app.session_view().and_then(|model| {
        let gate = model.workflow.as_ref()?.status.gate.as_ref()?;
        Some(gate.kind)
    });
    let Some(kind) = kind else {
        return Task::none();
    };
    state.tab = Tab::Session;
    let mut prompt = text_editor::Content::with_text(kind.change_prefix());
    prompt.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    state.session.prompt = prompt;
    operation::focus(session_view::PROMPT)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::PathBuf;

    use iced_test::simulator;
    use partitur_core::testing::Call;
    use partitur_core::{
        BackendEvent, PartiturTool, PlannedTask, PromptContent, ProposalId, ProposedChild,
        RepositoryId, SessionId, SuperpowersEvent, Update,
    };

    use super::*;
    use crate::app::{BareKey, update as apply, view};
    use crate::test_support::{Plain, deliver, deliver_tool, end_turn, orchestrated, plain};

    /// Feeds the superpowers report `event` of `session` through the application.
    fn report(state: &mut State, session: &SessionId, event: SuperpowersEvent) {
        deliver_tool(state, session.clone(), PartiturTool::Superpowers(event));
    }

    /// The plan of `count` tasks, task `n` titled `Task n`.
    fn tasks(count: u32) -> Vec<PlannedTask> {
        (1..=count)
            .map(|n| PlannedTask {
                n,
                title: format!("Task {n}"),
                files: Vec::new(),
            })
            .collect()
    }

    /// Asks for approval of a plan of eleven tasks.
    fn request_plan(state: &mut State, session: &SessionId) {
        report(
            state,
            session,
            SuperpowersEvent::RequestPlanApproval {
                path: PathBuf::from("plans/p.md"),
                tasks: tasks(11),
                checkpoints: vec![3, 6],
            },
        );
    }

    /// The texts of every prompt `backend` was sent.
    fn prompts(fixture: &Plain) -> Vec<String> {
        fixture
            .backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt { content, .. } => Some(content),
                _ => None,
            })
            .map(|content| {
                content
                    .into_iter()
                    .filter_map(|part| match part {
                        PromptContent::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .collect()
    }

    /// A plain session waiting at the plan's gate, its turn ended, on the Session tab.
    fn waiting_at_the_plan() -> Plain {
        let mut fixture = plain();
        let session = fixture.session.clone();
        request_plan(&mut fixture.state, &session);
        end_turn(&mut fixture.state, &session);
        fixture.state.tab = Tab::Session;
        fixture
    }

    #[test]
    fn the_rail_shows_the_steps_and_the_current_task() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        request_plan(state, &session);
        end_turn(state, &session);
        let _task = update(state, FlowMessage::Approve);
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Subagent,
            },
        );
        report(
            state,
            &session,
            SuperpowersEvent::StartTask { n: 4, note: None },
        );

        let mut ui = simulator(view(state));
        ui.find("superpowers").unwrap();
        for step in ["brainstorm", "design", "plan", "execute", "finish"] {
            ui.find(step).unwrap();
        }
        ui.find("task 4 of 11").unwrap();
        ui.find("Task 4").unwrap();
        assert!(ui.find("tdd green").is_err());

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::MakeTestPass { n: 4, note: None },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd green").unwrap();
    }

    #[test]
    fn the_rail_names_the_phase_of_the_current_task() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        report(
            state,
            &session,
            SuperpowersEvent::WriteFailingTest { n: 2, note: None },
        );
        // Before the plan is executed, the rail names no task's phase.
        let mut ui = simulator(view(state));
        assert!(ui.find("tdd red").is_err());
        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Inline,
            },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd red").unwrap();

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::StartRefactoring { n: 2, note: None },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd refactor").unwrap();
    }

    #[test]
    fn the_rail_sums_up_the_plan_and_tells_the_outcome() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        report(state, &session, SuperpowersEvent::StartWritingPlan);
        request_plan(state, &session);
        // The gate's detail stands in for the plan's summary while it waits.
        let mut ui = simulator(view(state));
        assert!(ui.find("11 tasks · 2 checkpoints").is_err());
        drop(ui);
        end_turn(state, &session);
        let _task = update(state, FlowMessage::RequestChanges);
        state.app.submit_prompt("Fewer tasks", Vec::new()).unwrap();
        let mut ui = simulator(view(state));
        ui.find("11 tasks · 2 checkpoints").unwrap();
        assert!(ui.find("· waiting for your approval").is_err());

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::FinishSession {
                outcome: partitur_core::Outcome::PrOpened,
            },
        );
        let mut ui = simulator(view(state));
        ui.find("PR opened").unwrap();
    }

    #[test]
    fn a_session_without_a_workflow_has_no_rail() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let mut ui = simulator(view(&fixture.state));

        assert!(ui.find("superpowers").is_err());
        drop(ui);
        fixture.state.tab = Tab::Env;
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("superpowers").is_err());
    }

    #[test]
    fn a_waiting_gate_offers_approve_and_enter_sends_it() {
        let mut fixture = waiting_at_the_plan();

        let mut ui = simulator(view(&fixture.state));
        ui.find("· waiting for your approval").unwrap();
        ui.find("Enter").unwrap();
        ui.click("Approve plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Approve)]);
        for message in messages {
            let _task = apply(&mut fixture.state, message);
        }
        assert_eq!(
            prompts(&fixture).last().unwrap(),
            "Plan approved. Execute it."
        );
        assert_eq!(fixture.state.error, None);
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Approve plan").is_err());

        let mut fixture = waiting_at_the_plan();
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));
        assert_eq!(
            prompts(&fixture).last().unwrap(),
            "Plan approved. Execute it."
        );
    }

    #[test]
    fn approve_is_disabled_while_the_turn_runs() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        fixture.state.tab = Tab::Session;
        report(
            &mut fixture.state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        let before = fixture.backend.calls();

        let mut ui = simulator(view(&fixture.state));
        ui.find("· waiting for your approval").unwrap();
        ui.click("Approve design").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.backend.calls(), before);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn enter_prefers_the_gate_over_an_undecided_split() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        let id = ProposalId::new();
        deliver(
            state,
            BackendEvent::Update {
                session: orchestrator.clone(),
                update: Update::ChildrenProposed {
                    id: id.clone(),
                    children: vec![ProposedChild {
                        repository: RepositoryId::try_from("web").unwrap(),
                        prompt: "Add the page".into(),
                        title: "Web page".into(),
                        title_source: partitur_core::TitleSource::Given,
                    }],
                },
            },
        );
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &orchestrator);

        let _task = apply(state, Message::Key(BareKey::Enter));

        let model = state.app.session_view().unwrap();
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::Proposal { id: shown, decision: None, .. } if *shown == id
        )));
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::User(text)
                if text == "Design approved. Write the implementation plan."
        )));
    }

    #[test]
    fn request_changes_prefills_and_focuses_the_prompt() {
        let mut fixture = waiting_at_the_plan();
        fixture.state.tab = Tab::Env;

        let _task = apply(
            &mut fixture.state,
            Message::Flow(FlowMessage::RequestChanges),
        );

        assert_eq!(fixture.state.tab, Tab::Session);
        assert_eq!(fixture.state.session.prompt.text(), "Changes to the plan: ");
        assert_eq!(
            fixture.state.session.prompt.cursor().position,
            text_editor::Position {
                line: 0,
                column: "Changes to the plan: ".len()
            }
        );
    }

    #[test]
    fn request_changes_without_a_gate_changes_nothing() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Env;

        let _task = update(&mut fixture.state, FlowMessage::RequestChanges);

        assert_eq!(fixture.state.tab, Tab::Env);
        assert_eq!(fixture.state.session.prompt.text(), "");
    }

    #[test]
    fn the_env_tab_shows_the_rail_too() {
        let mut fixture = waiting_at_the_plan();
        fixture.state.tab = Tab::Env;

        let mut ui = simulator(view(&fixture.state));

        ui.find("superpowers").unwrap();
        ui.find("· waiting for your approval").unwrap();
        ui.click("Approve plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Approve)]);
    }

    #[test]
    fn an_archived_session_cannot_be_approved() {
        let mut fixture = waiting_at_the_plan();
        let session = fixture.session.clone();
        fixture.state.app.archive(session).unwrap();
        let before = fixture.backend.calls();

        let mut ui = simulator(view(&fixture.state));
        ui.click("Approve plan").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.backend.calls(), before);
    }

    #[test]
    fn a_refused_approval_shows_in_the_error_bar() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let _task = update(&mut fixture.state, FlowMessage::Approve);

        assert!(fixture.state.error.is_some());
    }
}
