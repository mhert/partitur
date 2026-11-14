#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use super::*;
use crate::workflow::{
    GateKind, ModuleState, WorkflowEvent, WorkflowStatus, apply, gate_texts, rebuild, step_note,
};

fn sp(event: Event) -> WorkflowEvent {
    event.into()
}

fn fold(events: &[(u64, WorkflowEvent)]) -> WorkflowStatus {
    let mut status = None;
    for (at, event) in events {
        status = Some(apply(status, event, *at));
    }
    status.unwrap()
}

fn task(n: u32) -> PlannedTask {
    PlannedTask {
        n,
        title: format!("Task {n}"),
        files: vec!["src/a.rs".into()],
    }
}

fn plan_request(count: u32, checkpoints: Vec<u32>) -> WorkflowEvent {
    sp(Event::RequestPlanApproval {
        path: "plan.md".into(),
        tasks: (1..=count).map(task).collect(),
        checkpoints,
    })
}

fn progress(total: u32, current: Option<u32>) -> TaskProgress {
    TaskProgress {
        total,
        done: Vec::new(),
        current,
        phase: None,
        checkpoints: Vec::new(),
    }
}

/// The superpowers state of `status`.
fn st(status: &WorkflowStatus) -> &State {
    let ModuleState::SuperpowersV7(state) = &status.state;
    state
}

fn state_with(step: Option<Step>) -> State {
    State {
        topic: "t".into(),
        step,
        ..State::default()
    }
}

fn closed(gate: &str, by: GateCloser) -> WorkflowEvent {
    WorkflowEvent::GateClosed {
        module: ModuleId::SuperpowersV7,
        gate: GateKind::new(gate),
        by,
    }
}

#[test]
fn an_event_before_start_session_opens_a_workflow_with_an_empty_topic() {
    let status = apply(None, &sp(Event::StartBrainstorming), 10);
    assert_eq!(
        (
            st(&status).topic.as_str(),
            st(&status).step,
            st(&status).brainstorm_at,
            status.applied,
            status.updated_at
        ),
        ("", Some(Step::Brainstorm), Some(10), 1, 10)
    );
}

#[test]
fn start_session_replaces_the_view_but_keeps_counting() {
    let status = fold(&[
        (1, sp(Event::StartBrainstorming)),
        (
            2,
            sp(Event::RequestDesignApproval {
                path: "d.md".into(),
            }),
        ),
        (3, sp(Event::StartSession { topic: "x".into() })),
    ]);
    assert_eq!(status.applied, 3);
    assert_eq!(status.updated_at, 3);
    assert_eq!(st(&status).step, None);
    assert_eq!(st(&status).topic, "x");
    assert_eq!(st(&status).design, None);
    assert_eq!(status.gate, None);
    assert_eq!(st(&status).brainstorm_at, None);
}

#[test]
fn a_repeated_design_request_bumps_the_revision_and_reopens_the_gate() {
    let request = sp(Event::RequestDesignApproval {
        path: "d.md".into(),
    });
    let status = fold(&[
        (5, request.clone()),
        (6, closed(DESIGN, GateCloser::Approve)),
        (9, request),
    ]);
    assert_eq!(
        st(&status).design,
        Some(Document {
            path: "d.md".into(),
            rev: 2,
            at: 9,
            approved_at: None
        })
    );
    assert_eq!(
        status.gate,
        Some(OpenGate {
            kind: GateKind::new(DESIGN),
            since: 9
        })
    );
    assert_eq!(st(&status).step, Some(Step::Design));
}

#[test]
fn plan_approval_replaces_the_task_list() {
    let mut status = fold(&[(4, plan_request(11, vec![3, 6, 9]))]);
    assert_eq!(
        st(&status).tasks,
        Some(TaskProgress {
            total: 11,
            done: Vec::new(),
            current: None,
            phase: None,
            checkpoints: vec![3, 6, 9]
        })
    );
    assert_eq!(
        status.gate,
        Some(OpenGate {
            kind: GateKind::new(PLAN),
            since: 4
        })
    );
    assert_eq!(st(&status).step, Some(Step::Plan));
    assert_eq!(st(&status).plan.as_ref().map(|plan| plan.rev), Some(1));
    status = apply(Some(status), &sp(Event::StartTask { n: 1, note: None }), 5);
    status = apply(Some(status), &sp(Event::FinishTask { n: 1, note: None }), 6);
    let status = apply(Some(status), &plan_request(2, Vec::new()), 7);
    let tasks = st(&status).tasks.clone().unwrap();
    assert_eq!(
        (tasks.total, tasks.done, tasks.current),
        (2, Vec::new(), None)
    );
    assert_eq!(st(&status).plan.clone().map(|plan| plan.rev), Some(2));
}

#[test]
fn tasks_and_phases_move_the_current_task() {
    let mut status = None;
    let mut run = |at: u64, event: Event| {
        status = Some(apply(status.take(), &sp(event), at));
        st(status.as_ref().unwrap()).tasks.clone().unwrap()
    };
    let tasks = run(1, Event::StartTask { n: 4, note: None });
    assert_eq!(
        (tasks.total, tasks.current, tasks.phase),
        (0, Some(4), None)
    );
    let tasks = run(2, Event::WriteFailingTest { n: 4, note: None });
    assert_eq!((tasks.current, tasks.phase), (Some(4), Some(TddPhase::Red)));
    let tasks = run(3, Event::MakeTestPass { n: 4, note: None });
    assert_eq!(tasks.phase, Some(TddPhase::Green));
    let tasks = run(4, Event::StartRefactoring { n: 4, note: None });
    assert_eq!(tasks.phase, Some(TddPhase::Refactor));
    let tasks = run(5, Event::FinishTask { n: 4, note: None });
    assert_eq!((tasks.done.clone(), tasks.phase), (vec![4], None));
    let tasks = run(6, Event::FinishTask { n: 4, note: None });
    assert_eq!(tasks.done, vec![4]);
    let tasks = run(7, Event::FinishTask { n: 2, note: None });
    assert_eq!(tasks.done, vec![2, 4]);
}

#[test]
fn approving_closes_only_the_matching_gate_and_stamps_the_document() {
    let design = sp(Event::RequestDesignApproval {
        path: "d.md".into(),
    });
    let status = fold(&[(1, design.clone()), (2, closed(PLAN, GateCloser::Approve))]);
    assert_eq!(
        status.gate.as_ref().map(|gate| &gate.kind),
        Some(&GateKind::new(DESIGN))
    );
    assert_eq!(st(&status).plan, None);

    let status = fold(&[
        (1, design.clone()),
        (50, closed(DESIGN, GateCloser::Approve)),
    ]);
    assert_eq!(status.gate, None);
    assert_eq!(st(&status).design.clone().unwrap().approved_at, Some(50));

    let status = fold(&[(1, design), (50, closed(DESIGN, GateCloser::Prompt))]);
    assert_eq!(status.gate, None);
    assert_eq!(st(&status).design.clone().unwrap().approved_at, None);

    let status = fold(&[
        (1, sp(Event::StartWritingPlan)),
        (2, sp(Event::ReachCheckpoint { after_task: 1 })),
        (3, closed(CHECKPOINT, GateCloser::Approve)),
    ]);
    assert_eq!(st(&status).step, Some(Step::Plan));
    assert_eq!(status.gate, None);

    let status = fold(&[
        (1, plan_request(2, Vec::new())),
        (60, closed(PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(st(&status).plan.clone().unwrap().approved_at, Some(60));
    assert_eq!(status.gate, None);
}

#[test]
fn a_checkpoint_opens_a_gate_and_finish_session_closes_it() {
    let status = fold(&[
        (
            1,
            sp(Event::StartExecution {
                mode: ExecutionMode::Inline,
            }),
        ),
        (2, sp(Event::ReachCheckpoint { after_task: 3 })),
    ]);
    assert_eq!(st(&status).mode, Some(ExecutionMode::Inline));
    assert_eq!(st(&status).step, Some(Step::Execute));
    assert_eq!(
        status.gate,
        Some(OpenGate {
            kind: GateKind::new(CHECKPOINT),
            since: 2
        })
    );
    let status = apply(Some(status), &sp(Event::StartFinishingBranch), 3);
    assert_eq!(st(&status).step, Some(Step::Finish));
    assert!(status.gate.is_some());
    let status = apply(
        Some(status),
        &sp(Event::FinishSession {
            outcome: Outcome::Merged,
        }),
        4,
    );
    assert_eq!(st(&status).outcome, Some(Outcome::Merged));
    assert_eq!(status.gate, None);
    assert_eq!(st(&status).step, Some(Step::Finish));
}

#[test]
fn rebuild_folds_every_event_and_nothing_yields_none() {
    assert_eq!(rebuild([]), None);
    let events = [
        (1, sp(Event::StartSession { topic: "x".into() })),
        (2, sp(Event::StartBrainstorming)),
        (3, plan_request(3, vec![2])),
    ];
    let rebuilt = rebuild(events.iter().map(|(at, event)| (*at, event)));
    assert_eq!(rebuilt, Some(fold(&events)));
    assert_eq!(rebuilt.unwrap().applied, 3);
}

#[test]
fn step_labels_follow_the_tree_wording() {
    let label = |state: &State| step_label(state, None);
    assert_eq!(label(&state_with(None)), "superpowers");
    assert_eq!(label(&state_with(Some(Step::Brainstorm))), "brainstorm");
    assert_eq!(label(&state_with(Some(Step::Design))), "design");
    assert_eq!(label(&state_with(Some(Step::Plan))), "plan");
    assert_eq!(label(&state_with(Some(Step::Execute))), "execute");
    assert_eq!(label(&state_with(Some(Step::Finish))), "finish");

    let mut state = state_with(Some(Step::Execute));
    state.tasks = Some(progress(11, Some(4)));
    assert_eq!(label(&state), "execute · 4/11");
    state.tasks = Some(progress(0, Some(4)));
    assert_eq!(label(&state), "execute · 4");
    state.tasks = Some(progress(11, None));
    assert_eq!(label(&state), "execute");

    let gated = |state: &State, kind: &str| {
        let gate = OpenGate {
            kind: GateKind::new(kind),
            since: 1,
        };
        step_label(state, Some(&gate))
    };
    let mut state = state_with(Some(Step::Design));
    assert_eq!(gated(&state, DESIGN), "design · gate");
    assert_eq!(gated(&state, PLAN), "plan · gate");
    assert_eq!(gated(&state, CHECKPOINT), "execute · checkpoint");
    assert_eq!(gated(&state, "unknown"), "gate");

    state.outcome = Some(Outcome::PrOpened);
    assert_eq!(gated(&state, CHECKPOINT), "finish · PR opened");
    state.outcome = Some(Outcome::Kept);
    assert_eq!(label(&state), "finish · kept");
    state.outcome = Some(Outcome::Discarded);
    assert_eq!(label(&state), "finish · discarded");
    state.outcome = Some(Outcome::Merged);
    assert_eq!(label(&state), "finish · merged");
}

#[test]
fn summaries_count_tasks_and_checkpoints() {
    let with = |total, checkpoints: Vec<u32>| TaskProgress {
        checkpoints,
        ..progress(total, None)
    };
    assert_eq!(plan_summary(&with(1, vec![])), "1 task");
    assert_eq!(plan_summary(&with(12, vec![])), "12 tasks");
    assert_eq!(
        plan_summary(&with(11, vec![3, 6, 9])),
        "11 tasks · 3 checkpoints"
    );
    assert_eq!(plan_summary(&with(2, vec![1])), "2 tasks · 1 checkpoint");

    assert_eq!(
        execute_detail(&progress(11, Some(4))).as_deref(),
        Some("task 4 of 11")
    );
    assert_eq!(execute_detail(&progress(11, None)), None);
    assert_eq!(
        execute_detail(&progress(0, Some(4))).as_deref(),
        Some("task 4")
    );

    assert_eq!(checkpoints_line(&[]), None);
    assert_eq!(
        checkpoints_line(&[3, 6, 9]).as_deref(),
        Some("checkpoints after task 3, 6, 9")
    );
    assert_eq!(
        checkpoints_line(&[3]).as_deref(),
        Some("checkpoints after task 3")
    );
}

#[test]
fn step_notes_word_every_event() {
    let note = |event: Event| step_note(&sp(event), "14:02");
    let some = |text: &str| Some(text.to_owned());
    assert_eq!(
        note(Event::StartSession {
            topic: "login".into()
        }),
        some("superpowers · login")
    );
    assert_eq!(note(Event::StartBrainstorming), some("brainstorming"));
    assert_eq!(
        note(Event::RequestDesignApproval {
            path: "d.md".into()
        }),
        some("design ready for your approval · d.md")
    );
    assert_eq!(note(Event::StartWritingPlan), some("writing the plan"));
    assert_eq!(
        note(Event::RequestPlanApproval {
            path: "p.md".into(),
            tasks: vec![task(1), task(2)],
            checkpoints: vec![1],
        }),
        some("plan ready for your approval · p.md · 2 tasks · 1 checkpoint")
    );
    assert_eq!(
        note(Event::StartExecution {
            mode: ExecutionMode::Subagent
        }),
        some("executing with subagents")
    );
    assert_eq!(
        note(Event::StartExecution {
            mode: ExecutionMode::Inline
        }),
        some("executing inline")
    );
    assert_eq!(
        note(Event::FinishTask { n: 4, note: None }),
        some("task 4 done")
    );
    assert_eq!(
        note(Event::FinishTask {
            n: 4,
            note: Some("all green".into())
        }),
        some("task 4 done · all green")
    );
    assert_eq!(
        note(Event::ReachCheckpoint { after_task: 3 }),
        some("checkpoint after task 3 · waiting for your approval")
    );
    assert_eq!(
        note(Event::StartFinishingBranch),
        some("finishing the branch")
    );
    assert_eq!(
        note(Event::FinishSession {
            outcome: Outcome::PrOpened
        }),
        some("finished · PR opened")
    );
    assert_eq!(note(Event::StartTask { n: 1, note: None }), None);
    assert_eq!(note(Event::WriteFailingTest { n: 1, note: None }), None);
    assert_eq!(note(Event::MakeTestPass { n: 1, note: None }), None);
    assert_eq!(note(Event::StartRefactoring { n: 1, note: None }), None);
}

#[test]
fn step_notes_word_the_users_answers_with_the_given_time() {
    let some = |text: &str| Some(text.to_owned());
    let closed = |gate, by| step_note(&closed(gate, by), "14:02");
    let (approve, prompt) = (GateCloser::Approve, GateCloser::Prompt);
    assert_eq!(
        closed(DESIGN, approve),
        some("Design approved by you at 14:02")
    );
    assert_eq!(closed(PLAN, approve), some("Plan approved by you at 14:02"));
    assert_eq!(
        closed(CHECKPOINT, approve),
        some("Checkpoint passed by you at 14:02")
    );
    assert_eq!(
        closed(DESIGN, prompt),
        some("Changes to the design requested at 14:02")
    );
    assert_eq!(
        closed(PLAN, prompt),
        some("Changes to the plan requested at 14:02")
    );
    assert_eq!(
        closed(CHECKPOINT, prompt),
        some("Checkpoint answered at 14:02")
    );
    assert_eq!(closed("unknown", approve), None);
}

#[test]
fn gate_texts_are_the_fixed_copy() {
    let texts = |kind: &str| gate_texts(ModuleId::SuperpowersV7, &GateKind::new(kind));
    assert_eq!(
        texts(DESIGN),
        GateTexts {
            approve_label: "Approve design",
            approval_text: "Design approved. Write the implementation plan.",
            change_prefix: "Changes to the design: ",
            word: "design",
            hint: "approval is written to the transcript · the plan is written next",
        }
    );
    assert_eq!(
        texts(PLAN),
        GateTexts {
            approve_label: "Approve plan",
            approval_text: "Plan approved. Execute it.",
            change_prefix: "Changes to the plan: ",
            word: "plan",
            hint: "approval is written to the transcript · execution starts with task 1",
        }
    );
    assert_eq!(
        texts(CHECKPOINT),
        GateTexts {
            approve_label: "Continue",
            approval_text: "Continue.",
            change_prefix: "Before continuing: ",
            word: "checkpoint",
            hint: "approval is written to the transcript · execution continues",
        }
    );
}

#[test]
fn tool_names_carry_the_superpowers_prefix() {
    use Event as E;
    let tools = [
        (
            E::StartSession {
                topic: String::new(),
            },
            "superpowers_start_session",
        ),
        (E::StartBrainstorming, "superpowers_start_brainstorming"),
        (
            E::RequestDesignApproval { path: "d".into() },
            "superpowers_request_design_approval",
        ),
        (E::StartWritingPlan, "superpowers_start_writing_plan"),
        (
            E::RequestPlanApproval {
                path: "p".into(),
                tasks: Vec::new(),
                checkpoints: Vec::new(),
            },
            "superpowers_request_plan_approval",
        ),
        (
            E::StartExecution {
                mode: ExecutionMode::Subagent,
            },
            "superpowers_start_execution",
        ),
        (E::StartTask { n: 1, note: None }, "superpowers_start_task"),
        (
            E::WriteFailingTest { n: 1, note: None },
            "superpowers_write_failing_test",
        ),
        (
            E::MakeTestPass { n: 1, note: None },
            "superpowers_make_test_pass",
        ),
        (
            E::StartRefactoring { n: 1, note: None },
            "superpowers_start_refactoring",
        ),
        (
            E::FinishTask { n: 1, note: None },
            "superpowers_finish_task",
        ),
        (
            E::ReachCheckpoint { after_task: 1 },
            "superpowers_reach_checkpoint",
        ),
        (
            E::StartFinishingBranch,
            "superpowers_start_finishing_branch",
        ),
        (
            E::FinishSession {
                outcome: Outcome::Kept,
            },
            "superpowers_finish_session",
        ),
    ];
    for (event, name) in &tools {
        assert_eq!(event.tool_name(), *name);
    }
    let catalogued: Vec<_> = SUPERPOWERS_TOOLS.iter().map(|spec| spec.name).collect();
    let reported: Vec<_> = tools.iter().map(|(_, name)| *name).collect();
    assert_eq!(catalogued, reported);
}

#[test]
fn the_instructions_name_every_gate_tool_and_the_server() {
    for name in [
        "superpowers_request_design_approval",
        "superpowers_request_plan_approval",
        "superpowers_reach_checkpoint",
        "partitur",
    ] {
        assert!(WORKFLOW_INSTRUCTIONS.contains(name), "{name}");
    }
    for spec in SUPERPOWERS_TOOLS
        .iter()
        .filter(|s| s.name.contains("request_"))
    {
        assert!(WORKFLOW_INSTRUCTIONS.contains(spec.name), "{}", spec.name);
    }
}

#[test]
fn every_superpowers_tool_signature_starts_with_its_name() {
    for spec in SUPERPOWERS_TOOLS {
        assert!(
            spec.signature.starts_with(&format!("{}(", spec.name)),
            "{}",
            spec.signature
        );
    }
}

#[test]
fn labels_name_steps_outcomes_and_phases() {
    let labels: Vec<_> = Step::ALL.iter().map(|step| step.label()).collect();
    assert_eq!(
        labels,
        ["brainstorm", "design", "plan", "execute", "finish"]
    );
    assert_eq!(Outcome::Merged.label(), "merged");
    assert_eq!(TddPhase::Red.label(), "red");
    assert_eq!(TddPhase::Green.label(), "green");
    assert_eq!(TddPhase::Refactor.label(), "refactor");
}
