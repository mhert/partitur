#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use super::*;
use crate::ids::{BackendId, ProjectId, SessionId};
use crate::record::TranscriptRecord;
use crate::session::Session;
use crate::workflow::superpowers_v7::{CHECKPOINT, DESIGN, Event, PLAN, State, Step};

/// A status of the superpowers workflow stored before workflows were modules: a plan of one
/// task waits for approval.
const LEGACY_STATUS: &str = r#"{"workflow":"superpowers","topic":"login","applied":6,"updated_at":6,"step":"plan","brainstorm_at":2,"gate":{"kind":"plan","since":5},"design":null,"plan":{"path":"plans/p.md","rev":1,"at":3,"approved_at":4},"tasks":{"total":1,"done":[],"current":1,"phase":null,"checkpoints":[1]},"mode":null,"outcome":null}"#;

/// The superpowers state of `status`.
fn superpowers_state(status: &WorkflowStatus) -> &State {
    match &status.state {
        ModuleState::SuperpowersV7(state) => state,
        other @ ModuleState::BmadV6_12(_) => panic!("not a superpowers state: {other:?}"),
    }
}

fn closed(gate: &str, by: GateCloser) -> WorkflowEvent {
    WorkflowEvent::GateClosed {
        module: ModuleId::SuperpowersV7,
        gate: GateKind::new(gate),
        by,
        decider: crate::Decider::User,
    }
}

fn plain_session() -> Session {
    Session {
        id: SessionId::new(),
        project: ProjectId::new(),
        scope: crate::session::Scope::Project,
        cwd: PathBuf::from("/work"),
        title: "t".into(),
        backend_session: None,
        backend: BackendId::trusted("claude"),
        model: None,
        effort: None,
        status: crate::session::Status::Idle,
        parent: None,
        auto_allow: false,
        instructions_injected: false,
        created_at: 1,
        orchestrator: false,
        archived: None,
        workspace: crate::session::Workspace::CheckedOut,
        child_defaults: None,
        first_prompt: String::new(),
        skill: None,
        title_source: crate::TitleSource::Derived,
        last_activity: 0,
        children: crate::Fold::Folded,
        workflow: None,
        question: None,
        wait_reported: None,
        seen_branch: None,
        resolved_cwd: None,
    }
}

#[test]
fn legacy_events_read_as_superpowers_v7() {
    let read = |json: &str| serde_json::from_str::<WorkflowEvent>(json).unwrap();
    assert_eq!(
        read(r#"{"superpowers":{"start_task":{"n":1,"note":null}}}"#),
        WorkflowEvent::Reported(ModuleEvent::SuperpowersV7(Event::StartTask {
            n: 1,
            note: None
        }))
    );
    assert_eq!(
        read(r#"{"superpowers":"start_brainstorming"}"#),
        WorkflowEvent::Reported(ModuleEvent::SuperpowersV7(Event::StartBrainstorming))
    );
    assert_eq!(
        read(r#"{"gate_closed":{"gate":"plan","by":"approve"}}"#),
        WorkflowEvent::GateClosed {
            module: ModuleId::SuperpowersV7,
            gate: GateKind::new(PLAN),
            by: GateCloser::Approve,
            decider: crate::Decider::User,
        }
    );
    // A whole history line of the old form reads too.
    let update: TranscriptRecord = serde_json::from_str(
        r#"{"Workflow":{"at":6,"event":{"superpowers":{"reach_checkpoint":{"after_task":1}}}}}"#,
    )
    .unwrap();
    assert_eq!(
        update,
        TranscriptRecord::Workflow {
            at: 6,
            event: Event::ReachCheckpoint { after_task: 1 }.into(),
        }
    );
}

#[test]
fn new_events_round_trip_with_the_module_tag() {
    let events: [(WorkflowEvent, &str); 3] = [
        (
            Event::StartTask { n: 1, note: None }.into(),
            r#"{"superpowers_v7":{"start_task":{"n":1,"note":null}}}"#,
        ),
        (
            Event::StartBrainstorming.into(),
            r#"{"superpowers_v7":"start_brainstorming"}"#,
        ),
        (
            closed(PLAN, GateCloser::Prompt),
            r#"{"gate_closed":{"module":"superpowers_v7","gate":"plan","by":"prompt"}}"#,
        ),
    ];
    for (event, json) in events {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<WorkflowEvent>(json).unwrap(), event);
    }
}

#[test]
fn a_legacy_status_reads_with_its_gate_and_asks_for_a_rebuild() {
    let status: WorkflowStatus = serde_json::from_str(LEGACY_STATUS).unwrap();
    assert_eq!(status.applied, 0);
    assert_eq!(status.updated_at, 6);
    assert_eq!(status.module(), ModuleId::SuperpowersV7);
    assert_eq!(
        status.gate,
        Some(OpenGate {
            kind: GateKind::new(PLAN),
            since: 5
        })
    );
    let state = superpowers_state(&status);
    assert_eq!(state.step, Some(Step::Plan));
    assert_eq!(state.topic, "login");
    assert_eq!(state.brainstorm_at, Some(2));
    assert_eq!(
        state.plan.as_ref().map(|plan| plan.approved_at),
        Some(Some(4))
    );
    assert_eq!(state.tasks.as_ref().map(|tasks| tasks.total), Some(1));
    assert!(state.planned.is_empty());

    // Saved again, it takes today's form and reads back as it is.
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains(r#""state":{"superpowers_v7":"#), "{json}");
    assert_eq!(
        serde_json::from_str::<WorkflowStatus>(&json).unwrap(),
        status
    );
}

#[test]
fn a_status_round_trips_through_json_and_a_session_without_one_omits_it() {
    let status = rebuild(
        [
            (1, Event::StartSession { topic: "x".into() }.into()),
            (2, Event::StartTask { n: 1, note: None }.into()),
        ]
        .iter()
        .map(|(at, event): &(u64, WorkflowEvent)| (*at, event)),
    )
    .unwrap();
    let json = serde_json::to_string(&status).unwrap();
    assert_eq!(
        serde_json::from_str::<WorkflowStatus>(&json).unwrap(),
        status
    );

    let mut session = plain_session();
    let value = serde_json::to_value(&session).unwrap();
    assert!(value.get("workflow").is_none());
    assert_eq!(
        serde_json::from_value::<Session>(value).unwrap().workflow,
        None
    );

    session.workflow = Some(status);
    let value = serde_json::to_value(&session).unwrap();
    assert!(value.get("workflow").is_some());
    assert_eq!(serde_json::from_value::<Session>(value).unwrap(), session);
}

#[test]
fn a_session_whose_stored_status_does_not_parse_loads_without_one() {
    let with = |workflow: serde_json::Value| {
        let mut value = serde_json::to_value(plain_session()).unwrap();
        value["workflow"] = workflow;
        serde_json::from_value::<Session>(value).unwrap().workflow
    };
    for garbage in [
        serde_json::json!("nonsense"),
        serde_json::json!(42),
        serde_json::json!(null),
        serde_json::json!({"applied": 3, "state": {"unknown_v1": {}}}),
    ] {
        assert_eq!(with(garbage.clone()), None, "{garbage}");
    }

    // A legacy status still loads with its gate.
    let legacy = with(serde_json::from_str(LEGACY_STATUS).unwrap()).unwrap();
    assert_eq!(legacy.applied, 0);
    assert_eq!(legacy.gate.unwrap().kind, GateKind::new(PLAN));
}

#[test]
fn apply_opens_and_closes_gates_from_the_modules_answer() {
    // Without a status, an event opens a fresh one of its module.
    let opened = apply(
        None,
        &Event::RequestDesignApproval {
            path: "d.md".into(),
        }
        .into(),
        7,
    );
    assert_eq!((opened.applied, opened.updated_at), (1, 7));
    assert_eq!(
        opened.gate,
        Some(OpenGate {
            kind: GateKind::new(DESIGN),
            since: 7
        })
    );

    // A report that keeps the gate leaves it open with its time.
    let kept = apply(Some(opened.clone()), &Event::StartWritingPlan.into(), 8);
    assert_eq!(kept.gate, opened.gate);
    assert_eq!((kept.applied, kept.updated_at), (2, 8));

    // A newer gate replaces the open one.
    let replaced = apply(
        Some(kept),
        &Event::ReachCheckpoint { after_task: 1 }.into(),
        9,
    );
    assert_eq!(
        replaced.gate,
        Some(OpenGate {
            kind: GateKind::new(CHECKPOINT),
            since: 9
        })
    );

    // Closing another gate than the open one leaves the open one.
    let other = apply(
        Some(replaced.clone()),
        &closed(PLAN, GateCloser::Approve),
        10,
    );
    assert_eq!(other.gate, replaced.gate);
    assert_eq!(other.applied, 4);

    // A report that closes the gate closes it.
    let finished = apply(
        Some(replaced.clone()),
        &Event::FinishSession {
            outcome: superpowers_v7::Outcome::Kept,
        }
        .into(),
        11,
    );
    assert_eq!(finished.gate, None);

    // Starting over drops the gate and the state but keeps counting.
    let fresh = apply(
        Some(replaced),
        &Event::StartSession {
            topic: "next".into(),
        }
        .into(),
        12,
    );
    assert_eq!(fresh.gate, None);
    assert_eq!(fresh.applied, 4);
    assert_eq!(superpowers_state(&fresh).topic, "next");
    assert_eq!(superpowers_state(&fresh).step, None);

    // A gate closed before any report opens a fresh status of the event's module.
    let answered = apply(None, &closed(PLAN, GateCloser::Prompt), 13);
    assert_eq!(answered.module(), ModuleId::SuperpowersV7);
    assert_eq!((answered.gate, answered.applied), (None, 1));
}

#[test]
fn unknown_gate_kinds_fall_back_to_continue() {
    let unknown = GateKind::new("unknown");
    assert_eq!(unknown.as_str(), "unknown");
    let texts = gate_texts(ModuleId::SuperpowersV7, &unknown);
    assert_eq!(texts, GateTexts::FALLBACK);
    assert_eq!(
        texts,
        GateTexts {
            approve_label: "Continue",
            approval_text: "Continue.",
            change_prefix: "Before continuing: ",
            word: "decision",
            hint: "approval is written to the transcript",
        }
    );
    assert_eq!(
        gate_texts(ModuleId::SuperpowersV7, &GateKind::new(PLAN)).word,
        "plan"
    );
}

#[test]
fn dispatch_reaches_the_module_of_the_status_and_the_event() {
    let event: ModuleEvent = Event::RequestPlanApproval {
        path: "p.md".into(),
        tasks: vec![superpowers_v7::PlannedTask {
            n: 1,
            title: "Add the parser".into(),
            files: Vec::new(),
        }],
        checkpoints: Vec::new(),
    }
    .into();
    assert_eq!(event.module(), ModuleId::SuperpowersV7);
    assert_eq!(tool_answer(&event), GATE_REQUESTED);
    assert_eq!(
        tool_answer(&Event::ReachCheckpoint { after_task: 1 }.into()),
        CHECKPOINT_REACHED
    );
    assert_eq!(tool_answer(&Event::StartWritingPlan.into()), RECORDED);
    assert_eq!(
        tool_answer(&ModuleEvent::from(
            bmad_v6_12::Event::RequestBuildPlanApproval {
                story: "1".into(),
                path: "p.md".into(),
            }
        )),
        GATE_REQUESTED
    );
    assert_eq!(ModuleId::SuperpowersV7.chip(), "superpowers 7");

    let reported = WorkflowEvent::from(event);
    let status = apply(None, &reported, 3);
    let (rail, outline, documents, gate) = view(&status);
    assert_eq!(rail.chip, "superpowers 7");
    assert_eq!(outline.len(), 5);
    assert_eq!(documents.len(), 2);
    assert_eq!(
        documents[1]
            .tasks
            .as_ref()
            .map(|tasks| tasks.rows[0].title.as_str()),
        Some("Add the parser")
    );
    assert_eq!(
        gate.map(|gate| gate.approve_label),
        Some("Approve plan".to_owned())
    );
    assert_eq!(step_cell(&status).label, "plan · gate");
    assert!(step_cell(&status).gate_waiting);

    let quiet = apply(Some(status.clone()), &closed(PLAN, GateCloser::Approve), 4);
    assert_eq!(view(&quiet).3, None);
    assert!(!step_cell(&quiet).gate_waiting);

    let start = WorkflowEvent::from(Event::StartTask { n: 1, note: None });
    let started = apply(Some(quiet), &start, 5);
    assert_eq!(
        transcript_row(&started, &start),
        Some(WorkflowRow::Task {
            label: "task 1".into(),
            title: Some("Add the parser".into()),
            files: Vec::new(),
            note: None,
        })
    );
    assert_eq!(
        transcript_row(&started, &closed(PLAN, GateCloser::Approve)),
        None
    );
    assert_eq!(
        step_note(&reported, "14:02").as_deref(),
        Some("plan ready for your approval · p.md · 1 task")
    );
    assert_eq!(
        step_note(&closed(PLAN, GateCloser::Approve), "14:02").as_deref(),
        Some("Plan approved by you at 14:02")
    );
}

#[test]
fn every_module_has_a_prefix() {
    assert_eq!(
        ModuleId::ALL,
        [ModuleId::SuperpowersV7, ModuleId::BmadV6_12]
    );
    assert_eq!(ModuleId::SuperpowersV7.prefix(), "superpowers_v7_");
    assert_eq!(ModuleId::BmadV6_12.prefix(), "bmad_v6_12_");
    assert_eq!(ModuleId::BmadV6_12.chip(), "bmad 6.12");
    assert_eq!(
        serde_json::to_value(ModuleId::BmadV6_12).unwrap(),
        serde_json::json!("bmad_v6_12")
    );
    for module in ModuleId::ALL {
        assert_eq!(ModuleState::fresh(module).module(), module);
        assert!(
            module_tools(module)
                .iter()
                .all(|tool| tool.name.starts_with(module.prefix())),
            "{module:?}"
        );
    }
}

#[test]
fn the_catalogue_lists_every_modules_tools() {
    let module: Vec<_> = SuperpowersV7::tools()
        .iter()
        .chain(BmadV6_12::tools())
        .collect();
    for orchestrator in [false, true] {
        let listed: Vec<_> = tools(orchestrator).iter().map(|tool| tool.name).collect();
        let offered: Vec<_> = module
            .iter()
            .filter(|tool| orchestrator || !tool.run_level)
            .map(|tool| tool.name)
            .collect();
        assert_eq!(listed, offered, "orchestrator {orchestrator}");
    }
    assert!(tools(false).len() < tools(true).len());
}

#[test]
fn only_a_bmad_run_report_is_run_level() {
    assert!(!ModuleEvent::from(Event::StartTask { n: 1, note: None }).run_level());
    assert!(ModuleEvent::from(bmad_v6_12::Event::StartEpic { id: "1".into() }).run_level());
    assert!(!ModuleEvent::from(bmad_v6_12::Event::FinishWorkflow { outcome: None }).run_level());
}

#[test]
fn parse_routes_a_tool_to_its_module() {
    for orchestrator in [false, true] {
        assert_eq!(
            parse(
                "superpowers_v7_start_task",
                serde_json::json!({"n": 1}),
                orchestrator
            ),
            Some(Ok(ModuleEvent::SuperpowersV7(Event::StartTask {
                n: 1,
                note: None
            })))
        );
        assert_eq!(
            parse(
                "superpowers_v7_start_task",
                serde_json::json!({}),
                orchestrator
            ),
            Some(Err("missing field `n`".to_owned()))
        );
    }
}

#[test]
fn old_tool_names_are_unknown() {
    for name in [
        "superpowers_start_task",
        "superpowers_v7_rename_session",
        "list_children",
    ] {
        for orchestrator in [false, true] {
            assert_eq!(
                parse(name, serde_json::json!({"n": 1}), orchestrator),
                None,
                "{name}"
            );
        }
    }
}

#[test]
fn the_instructions_join_every_modules_block() {
    assert_eq!(
        instructions(),
        format!(
            "{}\n{}",
            SuperpowersV7::instructions(),
            BmadV6_12::instructions()
        )
    );
}

#[test]
fn a_run_level_tool_is_offered_to_orchestrators_only() {
    let tool = |run_level| WorkflowTool {
        name: "superpowers_v7_run",
        description: "",
        schema: || serde_json::json!({}),
        run_level,
    };

    assert!(tool(true).offered(true));
    assert!(!tool(true).offered(false));
    assert!(tool(false).offered(true));
    assert!(tool(false).offered(false));
}

fn bmad_start() -> WorkflowEvent {
    bmad_v6_12::Event::StartWorkflow {
        kind: bmad_v6_12::WorkflowKind::Build,
        persona: None,
        story: None,
        epic: None,
    }
    .into()
}

#[test]
fn an_event_of_another_module_replaces_the_view_and_its_gate() {
    let plan = WorkflowEvent::from(Event::RequestPlanApproval {
        path: "p.md".into(),
        tasks: Vec::new(),
        checkpoints: Vec::new(),
    });
    let gated = apply(None, &plan, 1);
    assert_eq!(
        gated.gate.as_ref().map(|gate| gate.kind.as_str()),
        Some(PLAN)
    );

    let replaced = apply(Some(gated), &bmad_start(), 2);
    assert_eq!(replaced.module(), ModuleId::BmadV6_12);
    assert_eq!(replaced.gate, None);
    assert_eq!((replaced.applied, replaced.updated_at), (2, 2));
    assert_eq!(step_cell(&replaced).label, "build · plan");
    assert_eq!(view(&replaced).0.chip, "bmad 6.12 · build");

    // And back: starting a superpowers session replaces the BMAD view.
    let start = WorkflowEvent::from(Event::StartSession {
        topic: "login".into(),
    });
    let back = apply(Some(replaced), &start, 3);
    assert_eq!(back.module(), ModuleId::SuperpowersV7);
    assert_eq!(back.applied, 3);
    assert_eq!(back.gate, None);
    assert_eq!(superpowers_state(&back).topic, "login");
}

#[test]
fn a_run_of_another_module_switches_on_its_start() {
    let gated = apply(None, &WorkflowEvent::from(Event::StartBrainstorming), 1);
    let run = WorkflowEvent::from(bmad_v6_12::Event::StartRun {
        topic: "Invoices".into(),
        path: bmad_v6_12::RunPath::Project,
    });
    let replaced = apply(Some(gated), &run, 2);
    assert_eq!(replaced.module(), ModuleId::BmadV6_12);
    assert_eq!(view(&replaced).0.chip, "bmad 6.12");
}

#[test]
fn a_report_of_another_module_that_does_not_start_it_only_counts() {
    let run = apply(
        None,
        &WorkflowEvent::from(bmad_v6_12::Event::StartRun {
            topic: "Invoices".into(),
            path: bmad_v6_12::RunPath::Project,
        }),
        1,
    );
    let task = WorkflowEvent::from(Event::StartTask { n: 1, note: None });
    let after = apply(Some(run.clone()), &task, 2);
    assert_eq!((after.applied, after.updated_at), (2, 2));
    assert_eq!(after.state, run.state);
    assert_eq!(after.gate, run.gate);
    assert_eq!(transcript_row(&after, &task), None);

    // A BMAD workflow waiting at a gate keeps its kind, documents and gate.
    let gated = rebuild([
        (1, &bmad_start()),
        (
            2,
            &WorkflowEvent::from(bmad_v6_12::Event::RequestBuildPlanApproval {
                story: "1.1".into(),
                path: "plan.md".into(),
            }),
        ),
    ])
    .unwrap();
    assert!(gated.gate.is_some());
    let test = WorkflowEvent::from(Event::WriteFailingTest { n: 1, note: None });
    let after = apply(Some(gated.clone()), &test, 3);
    assert_eq!((after.applied, after.updated_at), (3, 3));
    assert_eq!(after.state, gated.state);
    assert_eq!(after.gate, gated.gate);
    assert_eq!(view(&after), view(&gated));
}

#[test]
fn a_gate_closed_of_another_module_only_counts() {
    let plan = WorkflowEvent::from(Event::RequestPlanApproval {
        path: "p.md".into(),
        tasks: Vec::new(),
        checkpoints: Vec::new(),
    });
    let gated = apply(None, &plan, 1);
    let foreign = WorkflowEvent::GateClosed {
        module: ModuleId::BmadV6_12,
        gate: GateKind::new(PLAN),
        by: GateCloser::Approve,
        decider: crate::Decider::User,
    };

    let after = apply(Some(gated.clone()), &foreign, 2);
    assert_eq!((after.applied, after.updated_at), (2, 2));
    assert_eq!(after.gate, gated.gate);
    assert_eq!(after.state, gated.state);
    assert_eq!(
        superpowers_state(&after).plan.as_ref().unwrap().approved_at,
        None
    );

    // The same answer from the module itself closes the gate and approves the plan.
    let own = apply(Some(gated), &closed(PLAN, GateCloser::Approve), 2);
    assert_eq!(own.gate, None);
    assert_eq!(
        superpowers_state(&own).plan.as_ref().unwrap().approved_at,
        Some(2)
    );
}

#[test]
fn a_report_of_another_module_than_the_state_changes_nothing() {
    let mut superpowers = ModuleState::fresh(ModuleId::SuperpowersV7);
    assert_eq!(
        apply_report(
            &mut superpowers,
            &ModuleEvent::from(bmad_v6_12::Event::FinishWorkflow { outcome: None }),
            1
        ),
        GateChange::Keep
    );
    assert_eq!(superpowers, ModuleState::fresh(ModuleId::SuperpowersV7));

    let mut bmad = ModuleState::fresh(ModuleId::BmadV6_12);
    assert_eq!(
        apply_report(&mut bmad, &ModuleEvent::from(Event::StartBrainstorming), 1),
        GateChange::Keep
    );
    assert_eq!(bmad, ModuleState::fresh(ModuleId::BmadV6_12));

    // Neither has a transcript row for the other's report.
    let start = WorkflowEvent::from(Event::StartTask { n: 1, note: None });
    let bmad_status = apply(None, &bmad_start(), 1);
    assert_eq!(transcript_row(&bmad_status, &start), None);
    let superpowers_status = apply(None, &start, 1);
    assert_eq!(transcript_row(&superpowers_status, &bmad_start()), None);
}

#[test]
fn the_step_note_of_a_gate_closed_is_worded_by_its_module() {
    let gate = |module| WorkflowEvent::GateClosed {
        module,
        gate: GateKind::new(PLAN),
        by: GateCloser::Approve,
        decider: crate::Decider::User,
    };
    assert_eq!(
        step_note(&gate(ModuleId::SuperpowersV7), "09:00").as_deref(),
        Some("Plan approved by you at 09:00")
    );
    // BMAD has no gate called `plan`.
    assert_eq!(step_note(&gate(ModuleId::BmadV6_12), "09:00"), None);
    assert_eq!(
        gate_texts(ModuleId::BmadV6_12, &GateKind::new(PLAN)),
        GateTexts::FALLBACK
    );
}

#[test]
fn gate_closed_without_decider_reads_as_the_user() {
    let old = r#"{"gate_closed":{"module":"superpowers_v7","gate":"plan","by":"approve"}}"#;
    let event: WorkflowEvent = serde_json::from_str(old).unwrap();
    assert!(matches!(
        event,
        WorkflowEvent::GateClosed {
            decider: Decider::User,
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&event).unwrap(), old);
}

#[test]
fn gate_closed_by_the_orchestrator_writes_its_decider() {
    let json = r#"{"gate_closed":{"module":"superpowers_v7","gate":"plan","by":"prompt","decider":"orchestrator"}}"#;
    let event = WorkflowEvent::GateClosed {
        module: ModuleId::SuperpowersV7,
        gate: GateKind::new(PLAN),
        by: GateCloser::Prompt,
        decider: Decider::Orchestrator,
    };
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<WorkflowEvent>(json).unwrap(), event);
    assert!(Decider::User.is_user());
    assert!(!Decider::Orchestrator.is_user());
}
