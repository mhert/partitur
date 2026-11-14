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
fn superpowers_state(status: &WorkflowStatus) -> &State {
    match &status.state {
        ModuleState::SuperpowersV7(state) => state,
        other @ ModuleState::BmadV6_12(_) => panic!("not a superpowers state: {other:?}"),
    }
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
            superpowers_state(&status).topic.as_str(),
            superpowers_state(&status).step,
            superpowers_state(&status).brainstorm_at,
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
    assert_eq!(superpowers_state(&status).step, None);
    assert_eq!(superpowers_state(&status).topic, "x");
    assert_eq!(superpowers_state(&status).design, None);
    assert_eq!(status.gate, None);
    assert_eq!(superpowers_state(&status).brainstorm_at, None);
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
        superpowers_state(&status).design,
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
    assert_eq!(superpowers_state(&status).step, Some(Step::Design));
}

#[test]
fn plan_approval_replaces_the_task_list() {
    let mut status = fold(&[(4, plan_request(11, vec![3, 6, 9]))]);
    assert_eq!(
        superpowers_state(&status).tasks,
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
    assert_eq!(superpowers_state(&status).step, Some(Step::Plan));
    assert_eq!(
        superpowers_state(&status)
            .plan
            .as_ref()
            .map(|plan| plan.rev),
        Some(1)
    );
    status = apply(Some(status), &sp(Event::StartTask { n: 1, note: None }), 5);
    status = apply(Some(status), &sp(Event::FinishTask { n: 1, note: None }), 6);
    let status = apply(Some(status), &plan_request(2, Vec::new()), 7);
    let tasks = superpowers_state(&status).tasks.clone().unwrap();
    assert_eq!(
        (tasks.total, tasks.done, tasks.current),
        (2, Vec::new(), None)
    );
    assert_eq!(
        superpowers_state(&status).plan.clone().map(|plan| plan.rev),
        Some(2)
    );
}

#[test]
fn tasks_and_phases_move_the_current_task() {
    let mut status = None;
    let mut run = |at: u64, event: Event| {
        status = Some(apply(status.take(), &sp(event), at));
        superpowers_state(status.as_ref().unwrap())
            .tasks
            .clone()
            .unwrap()
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
    assert_eq!(superpowers_state(&status).plan, None);

    let status = fold(&[
        (1, design.clone()),
        (50, closed(DESIGN, GateCloser::Approve)),
    ]);
    assert_eq!(status.gate, None);
    assert_eq!(
        superpowers_state(&status)
            .design
            .clone()
            .unwrap()
            .approved_at,
        Some(50)
    );

    let status = fold(&[(1, design), (50, closed(DESIGN, GateCloser::Prompt))]);
    assert_eq!(status.gate, None);
    assert_eq!(
        superpowers_state(&status)
            .design
            .clone()
            .unwrap()
            .approved_at,
        None
    );

    let status = fold(&[
        (1, sp(Event::StartWritingPlan)),
        (2, sp(Event::ReachCheckpoint { after_task: 1 })),
        (3, closed(CHECKPOINT, GateCloser::Approve)),
    ]);
    assert_eq!(superpowers_state(&status).step, Some(Step::Plan));
    assert_eq!(status.gate, None);

    let status = fold(&[
        (1, plan_request(2, Vec::new())),
        (60, closed(PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(
        superpowers_state(&status).plan.clone().unwrap().approved_at,
        Some(60)
    );
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
    assert_eq!(superpowers_state(&status).mode, Some(ExecutionMode::Inline));
    assert_eq!(superpowers_state(&status).step, Some(Step::Execute));
    assert_eq!(
        status.gate,
        Some(OpenGate {
            kind: GateKind::new(CHECKPOINT),
            since: 2
        })
    );
    let status = apply(Some(status), &sp(Event::StartFinishingBranch), 3);
    assert_eq!(superpowers_state(&status).step, Some(Step::Finish));
    assert!(status.gate.is_some());
    let status = apply(
        Some(status),
        &sp(Event::FinishSession {
            outcome: Outcome::Merged,
        }),
        4,
    );
    assert_eq!(superpowers_state(&status).outcome, Some(Outcome::Merged));
    assert_eq!(status.gate, None);
    assert_eq!(superpowers_state(&status).step, Some(Step::Finish));
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
    assert_eq!(label(&state_with(None)), "superpowers 7");
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

/// The catalogue's tool names, in the order of the workflow's stages.
const TOOL_NAMES: [&str; 14] = [
    "superpowers_v7_start_session",
    "superpowers_v7_start_brainstorming",
    "superpowers_v7_request_design_approval",
    "superpowers_v7_start_writing_plan",
    "superpowers_v7_request_plan_approval",
    "superpowers_v7_start_execution",
    "superpowers_v7_start_task",
    "superpowers_v7_write_failing_test",
    "superpowers_v7_make_test_pass",
    "superpowers_v7_start_refactoring",
    "superpowers_v7_finish_task",
    "superpowers_v7_reach_checkpoint",
    "superpowers_v7_start_finishing_branch",
    "superpowers_v7_finish_session",
];

/// A minimal valid call of each tool, with the event it must parse into.
fn parse_cases() -> Vec<(&'static str, serde_json::Value, Event)> {
    stage_cases().into_iter().chain(task_cases()).collect()
}

/// A minimal valid call of each tool that reports a workflow stage, with its event.
fn stage_cases() -> Vec<(&'static str, serde_json::Value, Event)> {
    use Event as E;
    use serde_json::json;
    vec![
        (
            "superpowers_v7_start_session",
            json!({"topic": "Billing"}),
            E::StartSession {
                topic: "Billing".into(),
            },
        ),
        (
            "superpowers_v7_start_brainstorming",
            json!({}),
            E::StartBrainstorming,
        ),
        (
            "superpowers_v7_request_design_approval",
            json!({"path": "docs/design.md"}),
            E::RequestDesignApproval {
                path: "docs/design.md".into(),
            },
        ),
        (
            "superpowers_v7_start_writing_plan",
            json!({}),
            E::StartWritingPlan,
        ),
        (
            "superpowers_v7_request_plan_approval",
            json!({
                "path": "plans/p.md",
                "tasks": [{"n": 1, "title": "A", "files": ["a.rs"]}],
                "checkpoints": [3],
            }),
            E::RequestPlanApproval {
                path: "plans/p.md".into(),
                tasks: vec![PlannedTask {
                    n: 1,
                    title: "A".into(),
                    files: vec!["a.rs".into()],
                }],
                checkpoints: vec![3],
            },
        ),
        (
            "superpowers_v7_start_execution",
            json!({"mode": "subagent"}),
            E::StartExecution {
                mode: ExecutionMode::Subagent,
            },
        ),
        (
            "superpowers_v7_start_execution",
            json!({"mode": "inline"}),
            E::StartExecution {
                mode: ExecutionMode::Inline,
            },
        ),
    ]
}

/// A minimal valid call of each tool that reports a task's progress, with its event, and the
/// checkpoint and the end of the session.
fn task_cases() -> Vec<(&'static str, serde_json::Value, Event)> {
    use Event as E;
    use serde_json::json;
    let n = json!({"n": 2});
    let noted = json!({"n": 2, "note": "why"});
    vec![
        (
            "superpowers_v7_start_task",
            n.clone(),
            E::StartTask { n: 2, note: None },
        ),
        (
            "superpowers_v7_write_failing_test",
            noted.clone(),
            E::WriteFailingTest {
                n: 2,
                note: Some("why".into()),
            },
        ),
        (
            "superpowers_v7_make_test_pass",
            n,
            E::MakeTestPass { n: 2, note: None },
        ),
        (
            "superpowers_v7_start_refactoring",
            noted.clone(),
            E::StartRefactoring {
                n: 2,
                note: Some("why".into()),
            },
        ),
        (
            "superpowers_v7_finish_task",
            noted,
            E::FinishTask {
                n: 2,
                note: Some("why".into()),
            },
        ),
        (
            "superpowers_v7_reach_checkpoint",
            json!({"after_task": 3}),
            E::ReachCheckpoint { after_task: 3 },
        ),
        (
            "superpowers_v7_start_finishing_branch",
            json!({}),
            E::StartFinishingBranch,
        ),
        (
            "superpowers_v7_finish_session",
            json!({"outcome": "pr_opened"}),
            E::FinishSession {
                outcome: Outcome::PrOpened,
            },
        ),
    ]
}

#[test]
fn tool_names_carry_the_versioned_prefix() {
    let names: Vec<_> = SuperpowersV7::tools()
        .iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, TOOL_NAMES);
    assert_eq!(SuperpowersV7::ID.prefix(), "superpowers_v7_");
    for tool in SuperpowersV7::tools() {
        assert!(tool.name.starts_with("superpowers_v7_"), "{}", tool.name);
        assert!(!tool.run_level, "{}", tool.name);
    }
    for (_, _, event) in parse_cases() {
        assert!(names.contains(&event.tool_name()), "{event:?}");
    }
}

#[test]
fn parse_reads_each_tool_into_its_event() {
    let cases = parse_cases();
    let covered: std::collections::BTreeSet<_> = cases.iter().map(|case| case.0).collect();
    assert_eq!(covered, TOOL_NAMES.into_iter().collect());

    for (name, arguments, event) in cases {
        assert_eq!(event.tool_name(), name);
        assert_eq!(
            SuperpowersV7::parse(name, arguments),
            Ok(event),
            "for {name}"
        );
    }
}

#[test]
fn parse_reports_serdes_message_for_bad_arguments() {
    use serde_json::json;
    let cases = [
        (
            "superpowers_v7_start_execution",
            json!({"mode": "batch"}),
            "unknown variant `batch`, expected `subagent` or `inline`",
        ),
        (
            "superpowers_v7_request_design_approval",
            json!({}),
            "missing field `path`",
        ),
        (
            "superpowers_v7_start_task",
            json!({"n": -1}),
            "invalid value: integer `-1`, expected u32",
        ),
        (
            "superpowers_v7_finish_session",
            json!({"outcome": "lost"}),
            "unknown variant `lost`, expected one of `merged`, `pr_opened`, `kept`, `discarded`",
        ),
        (
            "superpowers_v7_rename_session",
            json!({}),
            "unknown tool superpowers_v7_rename_session",
        ),
    ];

    for (name, arguments, message) in cases {
        assert_eq!(
            SuperpowersV7::parse(name, arguments),
            Err(message.to_owned()),
            "for {name}"
        );
    }
}

#[test]
fn every_tool_schema_is_an_object_requiring_its_arguments() {
    let required = [
        vec!["topic"],
        vec![],
        vec!["path"],
        vec![],
        vec!["path", "tasks", "checkpoints"],
        vec!["mode"],
        vec!["n"],
        vec!["n"],
        vec!["n"],
        vec!["n"],
        vec!["n"],
        vec!["after_task"],
        vec![],
        vec!["outcome"],
    ];
    assert_eq!(SuperpowersV7::tools().len(), required.len());
    for (tool, required) in SuperpowersV7::tools().iter().zip(required) {
        let schema = (tool.schema)();
        assert_eq!(schema["type"], "object", "{}", tool.name);
        assert!(schema["properties"].is_object(), "{}", tool.name);
        let listed: Vec<&str> = schema["required"]
            .as_array()
            .map(|names| names.iter().map(|name| name.as_str().unwrap()).collect())
            .unwrap_or_default();
        assert_eq!(listed, required, "{}", tool.name);
    }
}

#[test]
fn the_plan_schema_describes_its_tasks_and_checkpoints() {
    let plan = SuperpowersV7::tools()
        .iter()
        .find(|tool| tool.name == "superpowers_v7_request_plan_approval")
        .unwrap();
    let schema = (plan.schema)();
    assert_eq!(
        schema["properties"]["tasks"]["items"]["required"],
        serde_json::json!(["n", "title", "files"])
    );
    assert_eq!(
        schema["properties"]["tasks"]["items"]["properties"]["n"],
        serde_json::json!({
            "type": "integer",
            "minimum": 1,
            "description": "The task's number in the plan",
        })
    );
    assert_eq!(
        schema["properties"]["checkpoints"]["items"],
        serde_json::json!({"type": "integer"})
    );
}

#[test]
fn the_instructions_name_every_gate_tool_and_the_server() {
    let instructions = SuperpowersV7::instructions();
    for name in [
        "superpowers_v7_request_design_approval",
        "superpowers_v7_request_plan_approval",
        "superpowers_v7_reach_checkpoint",
        "superpowers_v7_*",
        "\"partitur\"",
    ] {
        assert!(instructions.contains(name), "{name}");
    }
    assert!(instructions.starts_with("<partitur-workflow>"));
    assert!(instructions.ends_with("</partitur-workflow>"));
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
