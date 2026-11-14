#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::json;

use super::*;
use crate::workflow::display::{
    BlockMark, DocRef, GateView, OutlineBlock, OutlineLine, RailRight, RailStep, RailView,
    ReportedDoc, StepCell, StepState, TaskList, TaskRowView, WorkflowRow,
};
use crate::workflow::{
    GateKind, ModuleEvent, ModuleState, OpenGate, WorkflowEvent, WorkflowStatus, apply, gate_texts,
    rebuild, step_cell, step_note, transcript_row, view,
};

fn bm(event: Event) -> WorkflowEvent {
    event.into()
}

fn fold(events: &[(u64, WorkflowEvent)]) -> WorkflowStatus {
    rebuild(events.iter().map(|(at, event)| (*at, event))).unwrap()
}

/// The BMAD state of `status`.
fn bmad_state(status: &WorkflowStatus) -> &State {
    match &status.state {
        ModuleState::BmadV6_12(state) => state,
        other @ ModuleState::SuperpowersV7(_) => panic!("not a BMAD state: {other:?}"),
    }
}

/// The workflow `status` runs.
fn run(status: &WorkflowStatus) -> &WorkflowRun {
    bmad_state(status).workflow.as_ref().unwrap()
}

fn start(kind: WorkflowKind) -> WorkflowEvent {
    bm(Event::StartWorkflow {
        kind,
        persona: Some("Amelia · dev".into()),
        story: Some("1.2".into()),
        epic: Some("1".into()),
    })
}

fn closed(gate: &str, by: GateCloser) -> WorkflowEvent {
    WorkflowEvent::GateClosed {
        module: ModuleId::BmadV6_12,
        gate: GateKind::new(gate),
        by,
    }
}

fn story(id: &str) -> StoryRef {
    StoryRef {
        id: id.into(),
        title: format!("Story {id}"),
    }
}

fn prd_request(path: &str) -> WorkflowEvent {
    bm(Event::RequestDocumentApproval {
        doc: DocKind::Prd,
        path: path.into(),
    })
}

/// Every gate request with the kind it belongs to, its gate id and its step's index.
fn requests() -> Vec<(WorkflowKind, Event, &'static str, usize)> {
    vec![
        (
            WorkflowKind::Prd,
            Event::RequestDocumentApproval {
                doc: DocKind::Prd,
                path: "prd.md".into(),
            },
            DOCUMENT,
            1,
        ),
        (
            WorkflowKind::EpicsAndStories,
            Event::RequestStoryOrderApproval {
                path: "epics.md".into(),
                stories: vec![story("1.1"), story("1.2")],
            },
            STORY_ORDER,
            1,
        ),
        (
            WorkflowKind::SprintPlanning,
            Event::RequestReadinessDecision {
                verdict: Readiness::Concerns,
                path: "readiness.md".into(),
            },
            READINESS,
            1,
        ),
        (
            WorkflowKind::Build,
            Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            },
            BUILD_PLAN,
            0,
        ),
        (
            WorkflowKind::Build,
            Event::RequestBuildReview {
                story: "1.2".into(),
                path: Some("review.md".into()),
            },
            BUILD_REVIEW,
            2,
        ),
        (
            WorkflowKind::CodeReview,
            Event::RequestReviewVerdict {
                story: "1.2".into(),
                verdict: ReviewVerdict::ChangesRequested,
                path: "review.md".into(),
            },
            REVIEW_VERDICT,
            1,
        ),
        (
            WorkflowKind::Retrospective,
            Event::RequestRetrospectiveVerdict {
                epic: "1".into(),
                verdict: RetroVerdict::AcceptedWithOpenItems,
                path: "retro.md".into(),
            },
            RETROSPECTIVE_VERDICT,
            1,
        ),
    ]
}

#[test]
fn start_workflow_opens_the_kinds_first_step() {
    let status = fold(&[(1, start(WorkflowKind::Build))]);

    assert_eq!(status.module(), ModuleId::BmadV6_12);
    assert_eq!(
        run(&status),
        &WorkflowRun {
            kind: WorkflowKind::Build,
            persona: Some("Amelia · dev".into()),
            story: Some("1.2".into()),
            epic: Some("1".into()),
            step: 0,
            finished: None,
            documents: Vec::new(),
        }
    );
    assert_eq!(WorkflowKind::Build.steps()[run(&status).step], "plan");
    assert!(BmadV6_12::starts_over(&Event::StartWorkflow {
        kind: WorkflowKind::Spec,
        persona: None,
        story: None,
        epic: None,
    }));
    assert!(!BmadV6_12::starts_over(&Event::FinishWorkflow {
        outcome: None
    }));

    // Starting over drops the open gate and the documents, and keeps counting.
    let again = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (3, start(WorkflowKind::Build)),
    ]);
    assert_eq!(again.gate, None);
    assert_eq!(again.applied, 3);
    assert_eq!(run(&again).kind, WorkflowKind::Build);
    assert!(run(&again).documents.is_empty());
}

#[test]
fn every_kind_has_its_label_and_steps() {
    let kinds = [
        (WorkflowKind::Brief, "brief", &["draft", "approve"][..]),
        (WorkflowKind::Prfaq, "prfaq", &["draft", "approve"]),
        (WorkflowKind::Prd, "prd", &["draft", "approve"]),
        (WorkflowKind::Ux, "ux", &["draft", "approve"]),
        (
            WorkflowKind::Architecture,
            "architecture",
            &["draft", "approve"],
        ),
        (
            WorkflowKind::EpicsAndStories,
            "epics",
            &["draft", "story order"],
        ),
        (
            WorkflowKind::SprintPlanning,
            "sprint planning",
            &["plan", "readiness"],
        ),
        (WorkflowKind::Spec, "spec", &["draft", "approve"]),
        (WorkflowKind::Build, "build", &["plan", "build", "review"]),
        (
            WorkflowKind::CodeReview,
            "code review",
            &["review", "verdict"],
        ),
        (WorkflowKind::Retrospective, "retro", &["retro", "verdict"]),
    ];
    for (kind, label, steps) in kinds {
        assert_eq!((kind.label(), kind.steps()), (label, steps), "{kind:?}");
    }
    let docs = [
        (DocKind::Brief, "brief"),
        (DocKind::Prfaq, "prfaq"),
        (DocKind::Prd, "prd"),
        (DocKind::Ux, "ux"),
        (DocKind::Architecture, "architecture"),
        (DocKind::Spec, "spec"),
    ];
    for (doc, label) in docs {
        assert_eq!(doc.label(), label);
    }
}

#[test]
fn each_request_opens_its_gate_on_its_step() {
    for (kind, request, gate, step) in requests() {
        let status = fold(&[(1, start(kind)), (5, bm(request.clone()))]);

        assert_eq!(
            status.gate,
            Some(OpenGate {
                kind: GateKind::new(gate),
                since: 5
            }),
            "{gate}"
        );
        assert_eq!(run(&status).step, step, "{gate}");
        let document = &run(&status).documents[0];
        assert_eq!(
            (
                document.key.as_str(),
                document.rev,
                document.at,
                document.approved_at
            ),
            (gate, 1, 5, None),
            "{gate}"
        );
        assert_eq!(
            BmadV6_12::tool_answer(&request),
            crate::workflow::GATE_REQUESTED,
            "{gate}"
        );
    }
}

#[test]
fn a_request_records_its_path_label_and_stories() {
    let labels: Vec<_> = requests()
        .into_iter()
        .map(|(kind, request, _, _)| {
            let status = fold(&[(1, start(kind)), (2, bm(request))]);
            let document = run(&status).documents[0].clone();
            (
                document.label,
                document.path.map(|path| path.display().to_string()),
                document.stories.len(),
            )
        })
        .collect();
    let expected = [
        ("prd", Some("prd.md"), 0),
        ("story order", Some("epics.md"), 2),
        ("readiness verdict", Some("readiness.md"), 0),
        ("build plan", Some("plan.md"), 0),
        ("build", Some("review.md"), 0),
        ("review verdict", Some("review.md"), 0),
        ("retrospective verdict", Some("retro.md"), 0),
    ]
    .map(|(label, path, stories)| (label.to_owned(), path.map(str::to_owned), stories));
    assert_eq!(labels, expected);

    let unreported = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            }),
        ),
    ]);
    assert_eq!(run(&unreported).documents[0].path, None);
}

#[test]
fn a_request_before_any_start_opens_the_workflow_its_gate_belongs_to() {
    let kinds: Vec<_> = requests()
        .into_iter()
        .map(|(_, request, _, _)| run(&fold(&[(1, bm(request))])).kind)
        .collect();
    assert_eq!(
        kinds,
        [
            WorkflowKind::Prd,
            WorkflowKind::EpicsAndStories,
            WorkflowKind::SprintPlanning,
            WorkflowKind::Build,
            WorkflowKind::Build,
            WorkflowKind::CodeReview,
            WorkflowKind::Retrospective,
        ]
    );
    let docs = [
        (DocKind::Brief, WorkflowKind::Brief),
        (DocKind::Prfaq, WorkflowKind::Prfaq),
        (DocKind::Prd, WorkflowKind::Prd),
        (DocKind::Ux, WorkflowKind::Ux),
        (DocKind::Architecture, WorkflowKind::Architecture),
        (DocKind::Spec, WorkflowKind::Spec),
    ];
    for (doc, kind) in docs {
        let request = bm(Event::RequestDocumentApproval {
            doc,
            path: "d.md".into(),
        });
        let status = fold(&[(1, request)]);
        assert_eq!((run(&status).kind, run(&status).step), (kind, 1));
    }
}

#[test]
fn a_request_of_another_kind_keeps_the_step() {
    let status = fold(&[(1, start(WorkflowKind::Build)), (2, prd_request("prd.md"))]);
    assert_eq!(run(&status).kind, WorkflowKind::Build);
    assert_eq!(run(&status).step, 0);
    assert_eq!(
        status.gate.map(|gate| gate.kind),
        Some(GateKind::new(DOCUMENT))
    );
}

#[test]
fn approving_advances_to_the_next_step_and_stamps_the_document() {
    let status = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
        (3, closed(BUILD_PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(status.gate, None);
    assert_eq!(run(&status).step, 1);
    assert_eq!(run(&status).documents[0].approved_at, Some(3));

    // The last step has no next one.
    let review = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            }),
        ),
        (3, closed(BUILD_REVIEW, GateCloser::Approve)),
    ]);
    assert_eq!(run(&review).step, 2);
    assert_eq!(run(&review).documents[0].approved_at, Some(3));

    // A gate the module does not know neither advances nor stamps.
    let unknown = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
        (3, closed("unknown", GateCloser::Approve)),
    ]);
    assert_eq!(run(&unknown).step, 0);
    assert_eq!(run(&unknown).documents[0].approved_at, None);

    // Approving another gate than the document's stamps no document.
    let other = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (3, closed(STORY_ORDER, GateCloser::Approve)),
    ]);
    assert_eq!(run(&other).documents[0].approved_at, None);

    // Answering a gate before any workflow is a no-op on the state.
    let nothing = fold(&[(1, closed(DOCUMENT, GateCloser::Approve))]);
    assert_eq!(bmad_state(&nothing), &State::default());
}

#[test]
fn approving_advances_from_the_gates_own_step_only_while_it_waits() {
    // A gate whose step the kind lacks leaves the step where it is.
    let foreign = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestReviewVerdict {
                story: "1.2".into(),
                verdict: ReviewVerdict::Approved,
                path: "review.md".into(),
            }),
        ),
        (3, closed(REVIEW_VERDICT, GateCloser::Approve)),
    ]);
    assert_eq!(run(&foreign).step, 0);
    assert_eq!(run(&foreign).documents[0].approved_at, Some(3));

    // An approval of a gate that no longer waits changes nothing.
    let answered = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
        (3, closed(BUILD_PLAN, GateCloser::Prompt)),
        (4, closed(BUILD_PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(run(&answered).step, 0);
    assert_eq!(run(&answered).documents[0].approved_at, None);

    // An approval replaced by a later request no longer counts either.
    let replaced = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
        (
            3,
            bm(Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            }),
        ),
        (4, closed(BUILD_PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(run(&replaced).step, 2);
    assert_eq!(run(&replaced).documents[0].approved_at, None);

    // The step after the gate's own, not after the current one.
    let mut state = bmad_state(&fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
    ]))
    .clone();
    state.workflow.as_mut().unwrap().step = 2;
    BmadV6_12::gate_closed(
        &mut state,
        &GateKind::new(BUILD_PLAN),
        GateCloser::Approve,
        3,
        true,
    );
    assert_eq!(state.workflow.as_ref().unwrap().step, 1);

    // A finished workflow has no gate waiting.
    let finished = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (3, bm(Event::FinishWorkflow { outcome: None })),
        (4, closed(DOCUMENT, GateCloser::Approve)),
    ]);
    assert_eq!(run(&finished).documents[0].approved_at, None);
}

#[test]
fn starting_and_finishing_ask_the_user_nothing() {
    assert!(request(&Event::FinishWorkflow { outcome: None }).is_none());
    assert!(
        request(&Event::StartWorkflow {
            kind: WorkflowKind::Build,
            persona: None,
            story: None,
            epic: None,
        })
        .is_none()
    );
}

#[test]
fn a_prompt_closes_the_gate_without_approving() {
    let status = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (3, closed(DOCUMENT, GateCloser::Prompt)),
    ]);
    assert_eq!(status.gate, None);
    assert_eq!(run(&status).step, 1);
    assert_eq!(run(&status).documents[0].approved_at, None);
}

#[test]
fn a_repeated_document_request_bumps_the_revision() {
    let status = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (3, closed(DOCUMENT, GateCloser::Approve)),
        (4, prd_request("prd-2.md")),
    ]);
    assert_eq!(run(&status).documents.len(), 1);
    let document = &run(&status).documents[0];
    assert_eq!(document.rev, 2);
    assert_eq!(document.at, 4);
    assert_eq!(document.approved_at, None);
    assert_eq!(document.path, Some("prd-2.md".into()));

    // Another gate's request is a document of its own.
    let both = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            }),
        ),
        (
            3,
            bm(Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            }),
        ),
    ]);
    let keys: Vec<_> = run(&both)
        .documents
        .iter()
        .map(|document| (document.key.as_str(), document.rev))
        .collect();
    assert_eq!(keys, [(BUILD_PLAN, 1), (BUILD_REVIEW, 1)]);
}

#[test]
fn finish_workflow_closes_an_open_gate() {
    let status = fold(&[
        (1, start(WorkflowKind::Prd)),
        (2, prd_request("prd.md")),
        (
            3,
            bm(Event::FinishWorkflow {
                outcome: Some("done".into()),
            }),
        ),
    ]);
    assert_eq!(status.gate, None);
    assert_eq!(
        run(&status).finished,
        Some(Finished {
            outcome: Some("done".into())
        })
    );
    assert_eq!(
        BmadV6_12::tool_answer(&Event::FinishWorkflow { outcome: None }),
        crate::workflow::RECORDED
    );
    assert_eq!(
        BmadV6_12::tool_answer(&Event::StartWorkflow {
            kind: WorkflowKind::Prd,
            persona: None,
            story: None,
            epic: None,
        }),
        crate::workflow::RECORDED
    );

    // Finishing without a started workflow records nothing.
    let alone = fold(&[(1, bm(Event::FinishWorkflow { outcome: None }))]);
    assert_eq!(bmad_state(&alone).workflow, None);
}

#[test]
fn a_finished_workflow_without_an_outcome_round_trips() {
    for outcome in [None, Some("done".to_owned())] {
        let status = fold(&[
            (1, start(WorkflowKind::Spec)),
            (
                2,
                bm(Event::FinishWorkflow {
                    outcome: outcome.clone(),
                }),
            ),
        ]);
        let json = serde_json::to_string(&status).unwrap();
        let back: WorkflowStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(run(&back).finished, Some(Finished { outcome }));
    }
    let open = fold(&[(1, start(WorkflowKind::Spec))]);
    let json = serde_json::to_string(&open).unwrap();
    let back: WorkflowStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(run(&back).finished, None);
}

#[test]
fn a_verdict_is_stored_tagged_by_its_gate() {
    let status = fold(&[(
        1,
        bm(Event::RequestReadinessDecision {
            verdict: Readiness::Concerns,
            path: "readiness.md".into(),
        }),
    )]);
    let stored = serde_json::to_value(&status).unwrap();
    assert_eq!(
        stored["state"]["bmad_v6_12"]["workflow"]["documents"][0]["verdict"],
        json!({"readiness": "concerns"})
    );
    let back: WorkflowStatus = serde_json::from_value(stored).unwrap();
    assert_eq!(
        run(&back).documents[0].verdict,
        Some(Verdict::Readiness(Readiness::Concerns))
    );
}

#[test]
fn events_round_trip_with_the_module_tag() {
    for (_, request, _, _) in requests() {
        let event = bm(request);
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("bmad_v6_12").is_some(), "{json}");
        let back: WorkflowEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }
}

#[test]
fn gate_texts_are_the_fixed_copy() {
    let table = [
        (
            DOCUMENT,
            "Approve document",
            "Document approved. Continue.",
            "Changes to the document: ",
            "document",
        ),
        (
            STORY_ORDER,
            "Approve story order",
            "Story order approved. Continue.",
            "Changes to the story order: ",
            "story order",
        ),
        (
            READINESS,
            "Accept verdict",
            "Readiness verdict accepted. Continue.",
            "Before continuing: ",
            "readiness verdict",
        ),
        (
            BUILD_PLAN,
            "Approve plan",
            "Plan approved. Build it.",
            "Changes to the plan: ",
            "build plan",
        ),
        (
            BUILD_REVIEW,
            "Approve build",
            "Build approved. Open the pull request.",
            "Changes to the build: ",
            "build",
        ),
        (
            REVIEW_VERDICT,
            "Accept verdict",
            "Verdict accepted. Continue.",
            "Changes to the review: ",
            "review verdict",
        ),
        (
            RETROSPECTIVE_VERDICT,
            "Accept verdict",
            "Verdict accepted. Continue.",
            "Changes to the retrospective: ",
            "retrospective verdict",
        ),
    ];
    for (gate, approve_label, approval_text, change_prefix, word) in table {
        let texts = gate_texts(ModuleId::BmadV6_12, &GateKind::new(gate));
        assert_eq!(
            (
                texts.approve_label,
                texts.approval_text,
                texts.change_prefix,
                texts.word
            ),
            (approve_label, approval_text, change_prefix, word),
            "{gate}"
        );
        let next = texts
            .hint
            .strip_prefix("approval is written to the transcript · ")
            .unwrap_or_default();
        assert!(!next.is_empty(), "{gate}: {}", texts.hint);
    }
    assert_eq!(
        gate_texts(ModuleId::BmadV6_12, &GateKind::new("unknown")),
        crate::workflow::GateTexts::FALLBACK
    );
}

#[test]
fn gate_notes_name_the_gate_and_the_time() {
    let notes: Vec<_> = requests()
        .into_iter()
        .map(|(_, _, gate, _)| {
            (
                step_note(&closed(gate, GateCloser::Approve), "14:02").unwrap(),
                step_note(&closed(gate, GateCloser::Prompt), "14:02").unwrap(),
            )
        })
        .collect();
    let expected = [
        (
            "Document approved by you at 14:02",
            "Changes to the document requested at 14:02",
        ),
        (
            "Story order approved by you at 14:02",
            "Changes to the story order requested at 14:02",
        ),
        (
            "Readiness verdict accepted by you at 14:02",
            "Changes to the readiness verdict requested at 14:02",
        ),
        (
            "Build plan approved by you at 14:02",
            "Changes to the build plan requested at 14:02",
        ),
        (
            "Build approved by you at 14:02",
            "Changes to the build requested at 14:02",
        ),
        (
            "Review verdict accepted by you at 14:02",
            "Changes to the review verdict requested at 14:02",
        ),
        (
            "Retrospective verdict accepted by you at 14:02",
            "Changes to the retrospective verdict requested at 14:02",
        ),
    ]
    .map(|(approve, prompt)| (approve.to_owned(), prompt.to_owned()));
    assert_eq!(notes, expected);
    assert_eq!(
        step_note(&closed("unknown", GateCloser::Approve), "14:02"),
        None
    );
    assert_eq!(step_note(&start(WorkflowKind::Build), "14:02"), None);
}

/// A plain rail step.
fn step(label: &str, state: StepState, doc: Option<&str>) -> RailStep {
    RailStep::Plain {
        label: label.into(),
        state,
        detail: None,
        doc: doc.map(str::to_owned),
    }
}

fn build_plan() -> WorkflowEvent {
    bm(Event::RequestBuildPlanApproval {
        story: "1.2".into(),
        path: "plan.md".into(),
    })
}

#[test]
fn a_build_child_rail_reads_plan_build_review_with_its_chip() {
    let started = fold(&[(1, start(WorkflowKind::Build))]);
    let (rail, ..) = view(&started);
    assert_eq!(
        rail,
        RailView {
            chip: "bmad 6.12 · build".into(),
            steps: vec![
                step("plan", StepState::Current, None),
                step("build", StepState::Upcoming, None),
                step("review", StepState::Upcoming, None),
            ],
            right: RailRight::Unit {
                persona: Some("Amelia · dev".into()),
                title: Some("story 1.2".into()),
                pill: None,
                link: None,
            },
        }
    );

    let approved = fold(&[
        (1, start(WorkflowKind::Build)),
        (2, build_plan()),
        (3, closed(BUILD_PLAN, GateCloser::Approve)),
    ]);
    assert_eq!(
        view(&approved).0.steps,
        [
            step("plan", StepState::Done, Some(BUILD_PLAN)),
            step("build", StepState::Current, None),
            step("review", StepState::Upcoming, None),
        ]
    );

    let finished = apply(
        Some(approved),
        &bm(Event::FinishWorkflow { outcome: None }),
        4,
    );
    assert_eq!(
        view(&finished).0.steps,
        [
            step("plan", StepState::Done, Some(BUILD_PLAN)),
            step("build", StepState::Done, None),
            step("review", StepState::Done, None),
        ]
    );

    // Without a story the rail names the epic, and without either it names nothing.
    let epic = fold(&[(
        1,
        bm(Event::StartWorkflow {
            kind: WorkflowKind::Retrospective,
            persona: None,
            story: None,
            epic: Some("2".into()),
        }),
    )]);
    assert_eq!(
        view(&epic).0.right,
        RailRight::Unit {
            persona: None,
            title: Some("epic 2".into()),
            pill: None,
            link: None,
        }
    );
    let neither = fold(&[(
        1,
        bm(Event::StartWorkflow {
            kind: WorkflowKind::Prd,
            persona: None,
            story: None,
            epic: None,
        }),
    )]);
    assert_eq!(
        view(&neither).0.right,
        RailRight::Unit {
            persona: None,
            title: None,
            pill: None,
            link: None,
        }
    );

    // Before any workflow the rail holds only the chip.
    let empty = fold(&[(1, closed(DOCUMENT, GateCloser::Prompt))]);
    let (rail, outline, documents, _) = view(&empty);
    assert_eq!(rail.chip, "bmad 6.12");
    assert!(rail.steps.is_empty());
    assert!(outline.is_empty());
    assert!(documents.is_empty());
}

#[test]
fn a_step_names_the_latest_document_of_its_gate() {
    // A retrospective that was also asked a review verdict: both gates sit on `verdict`, and
    // the step names the one asked about last.
    let status = fold(&[
        (1, start(WorkflowKind::Retrospective)),
        (
            2,
            bm(Event::RequestRetrospectiveVerdict {
                epic: "1".into(),
                verdict: RetroVerdict::Accepted,
                path: "retro.md".into(),
            }),
        ),
        (
            3,
            bm(Event::RequestReviewVerdict {
                story: "1.2".into(),
                verdict: ReviewVerdict::Approved,
                path: "review.md".into(),
            }),
        ),
    ]);
    assert_eq!(
        view(&status).0.steps[1],
        step("verdict", StepState::Current, Some(REVIEW_VERDICT))
    );
    assert_eq!(view(&status).1[1].doc, Some(REVIEW_VERDICT.into()));
}

#[test]
fn the_build_plan_gate_puts_approve_plan_on_the_rail() {
    let status = fold(&[(1, start(WorkflowKind::Build)), (2, build_plan())]);
    let (rail, _, _, gate) = view(&status);
    assert_eq!(
        rail.right,
        RailRight::Gate {
            approve_label: "Approve plan".into()
        }
    );
    assert_eq!(
        rail.steps[0],
        step("plan", StepState::Current, Some(BUILD_PLAN))
    );
    assert_eq!(
        gate,
        Some(GateView {
            approve_label: "Approve plan".into(),
            change_prefix: "Changes to the plan: ".into(),
            hint: gate_texts(ModuleId::BmadV6_12, &GateKind::new(BUILD_PLAN))
                .hint
                .into(),
            doc: Some(BUILD_PLAN.into()),
        })
    );

    // A gate without a document of its own names none, and one the module does not word
    // gets the fallback words.
    let gate = OpenGate {
        kind: GateKind::new(STORY_ORDER),
        since: 2,
    };
    assert_eq!(BmadV6_12::gate_view(bmad_state(&status), &gate).doc, None);
    let unknown = OpenGate {
        kind: GateKind::new("unknown"),
        since: 2,
    };
    assert_eq!(
        BmadV6_12::rail(bmad_state(&status), Some(&unknown)).right,
        RailRight::Gate {
            approve_label: crate::workflow::GateTexts::FALLBACK.approve_label.into()
        }
    );
}

#[test]
fn child_step_cells_name_kind_and_gate() {
    let cell = |events: &[(u64, WorkflowEvent)]| step_cell(&fold(events));
    assert_eq!(
        cell(&[(1, start(WorkflowKind::Build)), (2, build_plan())]),
        StepCell {
            label: "build · plan gate".into(),
            gate_waiting: true,
        }
    );
    let waiting: Vec<_> = requests()
        .into_iter()
        .map(|(kind, request, _, _)| cell(&[(1, start(kind)), (2, bm(request))]).label)
        .collect();
    assert_eq!(
        waiting,
        [
            "prd · approval",
            "epics · story order",
            "sprint planning · readiness",
            "build · plan gate",
            "build · review gate",
            "code review · verdict",
            "retro · verdict",
        ]
    );
    let built = [
        (1, start(WorkflowKind::Build)),
        (2, build_plan()),
        (3, closed(BUILD_PLAN, GateCloser::Approve)),
    ];
    assert_eq!(
        cell(&built),
        StepCell {
            label: "build · build".into(),
            gate_waiting: false,
        }
    );
    let mut finished = built.to_vec();
    finished.push((4, bm(Event::FinishWorkflow { outcome: None })));
    assert_eq!(cell(&finished).label, "build · done");
    assert_eq!(
        cell(&[(1, closed(DOCUMENT, GateCloser::Prompt))]).label,
        "bmad 6.12"
    );

    // A gate the module does not know is named a gate.
    let status = fold(&[(1, start(WorkflowKind::Build))]);
    let unknown = OpenGate {
        kind: GateKind::new("unknown"),
        since: 1,
    };
    assert_eq!(
        BmadV6_12::step_cell(bmad_state(&status), Some(&unknown)).label,
        "build · gate"
    );
}

#[test]
fn the_readiness_verdict_shows_under_the_readiness_step() {
    let events = [
        (1, start(WorkflowKind::SprintPlanning)),
        (
            2,
            bm(Event::RequestReadinessDecision {
                verdict: Readiness::Concerns,
                path: "readiness.md".into(),
            }),
        ),
    ];
    let status = fold(&events);
    let readiness = |mark| OutlineBlock {
        label: "READINESS".into(),
        mark,
        lines: vec![
            OutlineLine::Path("readiness.md".into()),
            OutlineLine::Note("concerns".into()),
        ],
        doc: Some(READINESS.into()),
        current: true,
    };
    assert_eq!(
        view(&status).1,
        [
            OutlineBlock {
                label: "PLAN".into(),
                mark: BlockMark::None,
                lines: Vec::new(),
                doc: None,
                current: false,
            },
            readiness(BlockMark::Gate),
        ]
    );
    let approved = apply(Some(status), &closed(READINESS, GateCloser::Approve), 3);
    assert_eq!(view(&approved).1[1], readiness(BlockMark::Approved));
}

#[test]
fn every_verdict_shows_its_word_and_other_gates_only_their_path() {
    let notes: Vec<_> = [
        bm(Event::RequestReadinessDecision {
            verdict: Readiness::Pass,
            path: "r.md".into(),
        }),
        bm(Event::RequestReadinessDecision {
            verdict: Readiness::Fail,
            path: "r.md".into(),
        }),
        bm(Event::RequestReviewVerdict {
            story: "1.2".into(),
            verdict: ReviewVerdict::Approved,
            path: "r.md".into(),
        }),
        bm(Event::RequestReviewVerdict {
            story: "1.2".into(),
            verdict: ReviewVerdict::ChangesRequested,
            path: "r.md".into(),
        }),
        bm(Event::RequestRetrospectiveVerdict {
            epic: "1".into(),
            verdict: RetroVerdict::Accepted,
            path: "r.md".into(),
        }),
        bm(Event::RequestRetrospectiveVerdict {
            epic: "1".into(),
            verdict: RetroVerdict::AcceptedWithOpenItems,
            path: "r.md".into(),
        }),
        bm(Event::RequestRetrospectiveVerdict {
            epic: "1".into(),
            verdict: RetroVerdict::Rejected,
            path: "r.md".into(),
        }),
    ]
    .into_iter()
    .map(|request| {
        let status = fold(&[(1, request)]);
        let outline = view(&status).1;
        let block = outline.into_iter().find(|block| block.current).unwrap();
        assert_eq!(block.mark, BlockMark::Gate);
        assert_eq!(block.lines[0], OutlineLine::Path("r.md".into()));
        match &block.lines[1] {
            OutlineLine::Note(word) => word.clone(),
            other => panic!("not a note: {other:?}"),
        }
    })
    .collect();
    assert_eq!(
        notes,
        [
            "pass",
            "fail",
            "approved",
            "changes requested",
            "accepted",
            "accepted with open items",
            "rejected",
        ]
    );
    let prd = fold(&[(1, start(WorkflowKind::Prd)), (2, prd_request("prd.md"))]);
    assert_eq!(view(&prd).1[1].lines, [OutlineLine::Path("prd.md".into())]);
    let bare = fold(&[
        (1, start(WorkflowKind::Build)),
        (
            2,
            bm(Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            }),
        ),
    ]);
    let outline = view(&bare).1;
    assert!(outline[2].lines.is_empty());
    assert_eq!(outline[2].doc, Some(BUILD_REVIEW.into()));
}

#[test]
fn story_order_documents_list_their_stories() {
    let status = fold(&[
        (1, start(WorkflowKind::EpicsAndStories)),
        (
            2,
            bm(Event::RequestStoryOrderApproval {
                path: "epics.md".into(),
                stories: vec![story("1.1"), story("1.2")],
            }),
        ),
        (3, prd_request("prd.md")),
    ]);
    let row = |id: &str| TaskRowView {
        n: id.into(),
        title: format!("Story {id}"),
        files: Vec::new(),
        done: false,
        current: false,
    };
    assert_eq!(
        view(&status).2,
        [
            DocRef {
                key: STORY_ORDER.into(),
                label: "story order".into(),
                reported: Some(ReportedDoc {
                    path: "epics.md".into(),
                    rev: 1,
                    at: 2,
                }),
                tasks: Some(TaskList {
                    rows: vec![row("1.1"), row("1.2")],
                    footnote: None,
                }),
            },
            DocRef {
                key: DOCUMENT.into(),
                label: "prd".into(),
                reported: Some(ReportedDoc {
                    path: "prd.md".into(),
                    rev: 1,
                    at: 3,
                }),
                tasks: None,
            },
        ]
    );

    // A story order without stories lists none, and a build review without a file has no
    // reported revision.
    let empty = fold(&[(
        1,
        bm(Event::RequestStoryOrderApproval {
            path: "epics.md".into(),
            stories: Vec::new(),
        }),
    )]);
    assert_eq!(view(&empty).2[0].tasks, None);
    let bare = fold(&[(
        1,
        bm(Event::RequestBuildReview {
            story: "1.2".into(),
            path: None,
        }),
    )]);
    assert_eq!(view(&bare).2[0].reported, None);
}

#[test]
fn a_persona_start_leaves_a_persona_row() {
    let status = fold(&[(1, start(WorkflowKind::Build))]);
    assert_eq!(
        transcript_row(&status, &start(WorkflowKind::Build)),
        Some(WorkflowRow::Persona {
            persona: "Amelia · dev".into(),
            text: "build · story 1.2".into(),
            path: None,
        })
    );
    let storyless = bm(Event::StartWorkflow {
        kind: WorkflowKind::Prd,
        persona: Some("John · pm".into()),
        story: None,
        epic: Some("1".into()),
    });
    assert_eq!(
        transcript_row(&fold(&[(1, storyless.clone())]), &storyless),
        Some(WorkflowRow::Persona {
            persona: "John · pm".into(),
            text: "prd".into(),
            path: None,
        })
    );
    let anonymous = bm(Event::StartWorkflow {
        kind: WorkflowKind::Prd,
        persona: None,
        story: None,
        epic: None,
    });
    assert_eq!(
        transcript_row(&fold(&[(1, anonymous.clone())]), &anonymous),
        None
    );
    for (_, request, _, _) in requests() {
        let request = bm(request);
        assert_eq!(transcript_row(&status, &request), None);
    }
    assert_eq!(
        transcript_row(&status, &bm(Event::FinishWorkflow { outcome: None })),
        None
    );
}

#[test]
fn bmad_step_and_gate_notes_follow_the_pattern() {
    let note = |event: WorkflowEvent| step_note(&event, "14:02");
    assert_eq!(note(start(WorkflowKind::Build)), None);
    assert_eq!(
        note(bm(Event::StartWorkflow {
            kind: WorkflowKind::CodeReview,
            persona: None,
            story: Some("1.2".into()),
            epic: None,
        })),
        Some("bmad · code review".into())
    );
    let requested: Vec<_> = requests()
        .into_iter()
        .map(|(_, request, _, _)| note(bm(request)).unwrap())
        .collect();
    assert_eq!(
        requested,
        [
            "prd ready for your approval · prd.md",
            "story order ready for your approval · epics.md",
            "readiness verdict ready for your approval · readiness.md",
            "build plan ready for your approval · plan.md",
            "build ready for your approval · review.md",
            "review verdict ready for your approval · review.md",
            "retrospective verdict ready for your approval · retro.md",
        ]
    );
    assert_eq!(
        note(bm(Event::RequestBuildReview {
            story: "1.2".into(),
            path: None,
        })),
        Some("build ready for your review".into())
    );
    assert_eq!(
        note(bm(Event::FinishWorkflow { outcome: None })),
        Some("finished".into())
    );
    assert_eq!(
        note(bm(Event::FinishWorkflow {
            outcome: Some("merged".into())
        })),
        Some("finished · merged".into())
    );
    assert_eq!(
        note(closed(BUILD_PLAN, GateCloser::Approve)),
        Some("Build plan approved by you at 14:02".into())
    );
    assert_eq!(
        note(closed(BUILD_PLAN, GateCloser::Prompt)),
        Some("Changes to the build plan requested at 14:02".into())
    );
}

#[test]
fn parse_reads_each_workflow_tool() {
    let calls = [
        (
            "bmad_v6_12_start_workflow",
            json!({"kind": "epics_and_stories", "persona": "John · pm", "epic": "2"}),
            Event::StartWorkflow {
                kind: WorkflowKind::EpicsAndStories,
                persona: Some("John · pm".into()),
                story: None,
                epic: Some("2".into()),
            },
        ),
        (
            "bmad_v6_12_request_document_approval",
            json!({"doc": "architecture", "path": "arch.md"}),
            Event::RequestDocumentApproval {
                doc: DocKind::Architecture,
                path: "arch.md".into(),
            },
        ),
        (
            "bmad_v6_12_request_story_order_approval",
            json!({"path": "epics.md", "stories": [{"id": "1.1", "title": "Story 1.1"}]}),
            Event::RequestStoryOrderApproval {
                path: "epics.md".into(),
                stories: vec![story("1.1")],
            },
        ),
        (
            "bmad_v6_12_request_readiness_decision",
            json!({"verdict": "fail", "path": "ready.md"}),
            Event::RequestReadinessDecision {
                verdict: Readiness::Fail,
                path: "ready.md".into(),
            },
        ),
        (
            "bmad_v6_12_request_build_plan_approval",
            json!({"story": "1.2", "path": "plan.md"}),
            Event::RequestBuildPlanApproval {
                story: "1.2".into(),
                path: "plan.md".into(),
            },
        ),
        (
            "bmad_v6_12_request_build_review",
            json!({"story": "1.2"}),
            Event::RequestBuildReview {
                story: "1.2".into(),
                path: None,
            },
        ),
        (
            "bmad_v6_12_request_review_verdict",
            json!({"story": "1.2", "verdict": "approved", "path": "review.md"}),
            Event::RequestReviewVerdict {
                story: "1.2".into(),
                verdict: ReviewVerdict::Approved,
                path: "review.md".into(),
            },
        ),
        (
            "bmad_v6_12_request_retrospective_verdict",
            json!({"epic": "1", "verdict": "rejected", "path": "retro.md"}),
            Event::RequestRetrospectiveVerdict {
                epic: "1".into(),
                verdict: RetroVerdict::Rejected,
                path: "retro.md".into(),
            },
        ),
        (
            "bmad_v6_12_finish_workflow",
            json!({}),
            Event::FinishWorkflow { outcome: None },
        ),
    ];
    let names: Vec<_> = BmadV6_12::tools()
        .iter()
        .filter(|tool| !tool.run_level)
        .map(|tool| tool.name)
        .collect();
    assert_eq!(
        names,
        calls.iter().map(|(name, _, _)| *name).collect::<Vec<_>>()
    );
    for (name, arguments, event) in calls {
        assert_eq!(event.tool_name(), name);
        assert!(!event.run_level(), "{name}");
        assert_eq!(BmadV6_12::parse(name, arguments.clone()), Ok(event.clone()));
        for orchestrator in [false, true] {
            assert_eq!(
                crate::workflow::parse(name, arguments.clone(), orchestrator),
                Some(Ok(ModuleEvent::BmadV6_12(event.clone()))),
                "{name}"
            );
        }
    }
    assert_eq!(
        BmadV6_12::parse("bmad_v6_12_start_sprint", json!({})),
        Err("unknown tool bmad_v6_12_start_sprint".to_owned())
    );
}

/// Every run-level tool with arguments and the event they read as.
fn run_calls() -> [(&'static str, serde_json::Value, Event); 10] {
    [
        (
            "bmad_v6_12_start_run",
            json!({"topic": "Invoices", "path": "project"}),
            Event::StartRun {
                topic: "Invoices".into(),
                path: RunPath::Project,
            },
        ),
        (
            "bmad_v6_12_start_planning",
            json!({"doc": "ux", "persona": "Sally · ux"}),
            Event::StartPlanning {
                doc: PlanningDoc::Ux,
                persona: Some("Sally · ux".into()),
            },
        ),
        (
            "bmad_v6_12_plan_stories",
            json!({"epics": [{"id": "1", "title": "Billing", "stories": [
                {"id": "1.1", "title": "Story 1.1"}
            ]}]}),
            Event::PlanStories {
                epics: vec![EpicPlan {
                    id: "1".into(),
                    title: "Billing".into(),
                    stories: vec![story("1.1")],
                }],
            },
        ),
        (
            "bmad_v6_12_start_sprint_planning",
            json!({"persona": "Bob · sm"}),
            Event::StartSprintPlanning {
                persona: Some("Bob · sm".into()),
            },
        ),
        (
            "bmad_v6_12_start_epic",
            json!({"id": "2"}),
            Event::StartEpic { id: "2".into() },
        ),
        (
            "bmad_v6_12_start_story",
            json!({"id": "2.1", "path": "stories/2.1.md", "persona": "Amelia · dev"}),
            Event::StartStory {
                id: "2.1".into(),
                path: Some("stories/2.1.md".into()),
                persona: Some("Amelia · dev".into()),
            },
        ),
        (
            "bmad_v6_12_start_story_review",
            json!({"id": "2.1"}),
            Event::StartStoryReview {
                id: "2.1".into(),
                persona: None,
            },
        ),
        (
            "bmad_v6_12_finish_story",
            json!({"id": "2.1", "outcome": "changes_requested"}),
            Event::FinishStory {
                id: "2.1".into(),
                outcome: StoryOutcome::ChangesRequested,
            },
        ),
        (
            "bmad_v6_12_start_retrospective",
            json!({"epic": "2", "persona": "Bob · sm"}),
            Event::StartRetrospective {
                epic: "2".into(),
                persona: Some("Bob · sm".into()),
            },
        ),
        (
            "bmad_v6_12_finish_run",
            json!({"outcome": "abandoned"}),
            Event::FinishRun {
                outcome: RunOutcome::Abandoned,
            },
        ),
    ]
}

#[test]
fn parse_reads_each_run_tool() {
    let calls = run_calls();
    let names: Vec<_> = BmadV6_12::tools()
        .iter()
        .filter(|tool| tool.run_level)
        .map(|tool| tool.name)
        .collect();
    assert_eq!(
        names,
        calls.iter().map(|(name, _, _)| *name).collect::<Vec<_>>()
    );
    for (name, arguments, event) in calls {
        assert_eq!(event.tool_name(), name);
        assert!(event.run_level(), "{name}");
        assert!(ModuleEvent::from(event.clone()).run_level(), "{name}");
        assert_eq!(BmadV6_12::parse(name, arguments.clone()), Ok(event.clone()));
        assert_eq!(
            crate::workflow::parse(name, arguments.clone(), true),
            Some(Ok(ModuleEvent::BmadV6_12(event.clone()))),
            "{name}"
        );
        assert_eq!(
            crate::workflow::parse(name, arguments, false),
            None,
            "{name}"
        );
    }
    assert_eq!(
        BmadV6_12::parse("bmad_v6_12_finish_run", json!({})),
        Err("missing field `outcome`".to_owned())
    );
}

#[test]
fn the_run_schemas_follow_the_arguments() {
    let schema = |name: &str| {
        let tool = BmadV6_12::tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap();
        (tool.schema)()
    };
    let required = [
        ("bmad_v6_12_start_run", json!(["topic", "path"])),
        ("bmad_v6_12_start_planning", json!(["doc"])),
        ("bmad_v6_12_plan_stories", json!(["epics"])),
        ("bmad_v6_12_start_epic", json!(["id"])),
        ("bmad_v6_12_start_story", json!(["id"])),
        ("bmad_v6_12_start_story_review", json!(["id"])),
        ("bmad_v6_12_finish_story", json!(["id", "outcome"])),
        ("bmad_v6_12_start_retrospective", json!(["epic"])),
        ("bmad_v6_12_finish_run", json!(["outcome"])),
    ];
    for (name, required) in required {
        assert_eq!(schema(name)["type"], "object", "{name}");
        assert_eq!(schema(name)["required"], required, "{name}");
    }
    assert_eq!(
        schema("bmad_v6_12_start_run")["properties"]["path"]["enum"],
        json!(["single", "epic", "project"])
    );
    assert_eq!(
        schema("bmad_v6_12_start_planning")["properties"]["doc"]["enum"],
        json!(["brief", "prfaq", "prd", "ux", "architecture"])
    );
    assert!(schema("bmad_v6_12_start_planning")["properties"]["persona"].is_object());
    let epics = &schema("bmad_v6_12_plan_stories")["properties"]["epics"];
    assert_eq!(
        epics["items"]["required"],
        json!(["id", "title", "stories"])
    );
    assert_eq!(
        epics["items"]["properties"]["stories"]["items"]["required"],
        json!(["id", "title"])
    );
    let sprint = schema("bmad_v6_12_start_sprint_planning");
    assert!(sprint["properties"]["persona"].is_object());
    assert!(sprint.get("required").is_none());
    let story = schema("bmad_v6_12_start_story");
    for property in ["id", "path", "persona"] {
        assert!(story["properties"][property].is_object(), "{property}");
    }
    assert!(schema("bmad_v6_12_start_story_review")["properties"]["persona"].is_object());
    assert!(schema("bmad_v6_12_start_retrospective")["properties"]["persona"].is_object());
    assert!(schema("bmad_v6_12_start_epic")["properties"]["id"].is_object());
    assert_eq!(
        schema("bmad_v6_12_finish_story")["properties"]["outcome"]["enum"],
        json!(["done", "changes_requested"])
    );
    assert_eq!(
        schema("bmad_v6_12_finish_run")["properties"]["outcome"]["enum"],
        json!(["completed", "abandoned"])
    );
}

#[test]
fn parse_rejects_an_unknown_kind_with_serdes_message() {
    let error = BmadV6_12::parse("bmad_v6_12_start_workflow", json!({"kind": "epic"})).unwrap_err();
    assert!(
        error.starts_with("unknown variant `epic`, expected one of `brief`, `prfaq`"),
        "{error}"
    );
    assert_eq!(
        BmadV6_12::parse(
            "bmad_v6_12_request_build_plan_approval",
            json!({"story": "1"})
        ),
        Err("missing field `path`".to_owned())
    );
}

#[test]
fn the_schemas_follow_the_arguments() {
    let schema = |name: &str| {
        let tool = BmadV6_12::tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap();
        assert!(!tool.run_level, "{name}");
        (tool.schema)()
    };
    let kinds = schema("bmad_v6_12_start_workflow");
    assert_eq!(
        kinds["properties"]["kind"]["enum"],
        json!([
            "brief",
            "prfaq",
            "prd",
            "ux",
            "architecture",
            "epics_and_stories",
            "sprint_planning",
            "spec",
            "build",
            "code_review",
            "retrospective"
        ])
    );
    assert_eq!(kinds["required"], json!(["kind"]));
    assert_eq!(
        schema("bmad_v6_12_request_document_approval")["properties"]["doc"]["enum"],
        json!(["brief", "prfaq", "prd", "ux", "architecture", "spec"])
    );
    let stories = schema("bmad_v6_12_request_story_order_approval");
    assert_eq!(
        stories["properties"]["stories"]["items"]["required"],
        json!(["id", "title"])
    );
    assert_eq!(
        schema("bmad_v6_12_request_readiness_decision")["properties"]["verdict"]["enum"],
        json!(["pass", "concerns", "fail"])
    );
    for tool in BmadV6_12::tools()
        .iter()
        .filter(|tool| tool.name.starts_with("bmad_v6_12_request_"))
    {
        let path = &(tool.schema)()["properties"]["path"];
        assert_eq!(path["type"], "string", "{}", tool.name);
    }
    assert_eq!(
        schema("bmad_v6_12_request_build_plan_approval")["required"],
        json!(["story", "path"])
    );
    assert_eq!(
        schema("bmad_v6_12_request_build_review")["required"],
        json!(["story"])
    );
    assert_eq!(
        schema("bmad_v6_12_request_review_verdict")["properties"]["verdict"]["enum"],
        json!(["approved", "changes_requested"])
    );
    assert_eq!(
        schema("bmad_v6_12_request_retrospective_verdict")["properties"]["verdict"]["enum"],
        json!(["accepted", "accepted_with_open_items", "rejected"])
    );
    let finish = schema("bmad_v6_12_finish_workflow");
    assert!(finish["properties"]["outcome"].is_object());
    assert!(finish.get("required").is_none());
}

#[test]
fn the_instructions_name_every_gate_tool_and_the_server() {
    let instructions = BmadV6_12::instructions();
    assert!(instructions.contains("\"partitur\""));
    assert!(instructions.contains("fresh chat"));
    assert!(instructions.contains("end your turn"));
    let gates: Vec<_> = BmadV6_12::tools()
        .iter()
        .filter(|tool| tool.name.starts_with("bmad_v6_12_request_"))
        .map(|tool| tool.name)
        .collect();
    assert_eq!(gates.len(), 7);
    for gate in gates {
        assert!(instructions.contains(gate), "{gate}");
    }
    assert!(instructions.contains("orchestrator only"));
    assert!(instructions.contains("one child per BMAD workflow"));
    for tool in BmadV6_12::tools().iter().filter(|tool| tool.run_level) {
        assert!(instructions.contains(tool.name), "{}", tool.name);
    }
}

fn start_run(path: RunPath) -> WorkflowEvent {
    bm(Event::StartRun {
        topic: "Invoices".into(),
        path,
    })
}

#[test]
fn a_run_shows_its_steps_by_name_on_its_rail() {
    let status = fold(&[
        (1, start_run(RunPath::Project)),
        (
            2,
            bm(Event::StartPlanning {
                doc: PlanningDoc::Brief,
                persona: Some("Mary · analyst".into()),
            }),
        ),
        (
            3,
            bm(Event::StartPlanning {
                doc: PlanningDoc::Ux,
                persona: None,
            }),
        ),
    ]);
    let plain = |label: &str, state| step(label, state, None);
    let (rail, outline, documents, gate) = view(&status);
    assert_eq!(
        rail,
        RailView {
            chip: "bmad 6.12".into(),
            steps: vec![
                plain("brief", StepState::Done),
                plain("prd", StepState::Done),
                plain("ux", StepState::Current),
                plain("architecture", StepState::Upcoming),
                plain("epics", StepState::Upcoming),
                plain("sprint", StepState::Upcoming),
                RailStep::Loop {
                    label: "stories".into(),
                    phases: vec!["build".into(), "review".into()],
                    active: None,
                    state: StepState::Upcoming,
                },
                plain("retro", StepState::Upcoming),
            ],
            right: RailRight::Unit {
                persona: Some("Mary · analyst".into()),
                title: None,
                pill: None,
                link: None,
            },
        }
    );
    let labels: Vec<_> = outline.iter().map(|block| block.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "BRIEF",
            "PRD",
            "UX",
            "ARCHITECTURE",
            "EPICS",
            "SPRINT",
            "STORIES",
            "RETRO"
        ]
    );
    assert_eq!(
        outline[2],
        OutlineBlock {
            label: "UX".into(),
            mark: BlockMark::None,
            lines: Vec::new(),
            doc: None,
            current: true,
        }
    );
    assert!(outline.iter().filter(|block| block.current).count() == 1);
    assert_eq!(documents, Vec::new());
    assert_eq!(gate, None);
    assert_eq!(
        step_cell(&status),
        StepCell {
            label: "ux".into(),
            gate_waiting: false,
        }
    );

    let single = fold(&[(1, start_run(RunPath::Single))]);
    assert_eq!(
        view(&single).0.steps,
        [
            plain("spec", StepState::Current),
            plain("build", StepState::Upcoming)
        ]
    );
    assert_eq!(step_cell(&single).label, "spec");
}

#[test]
fn every_run_step_has_its_name() {
    let names: Vec<_> = [RunPath::Single, RunPath::Epic]
        .into_iter()
        .flat_map(|path| {
            let mut events = vec![(1, start_run(path))];
            events.push((
                2,
                bm(Event::StartStory {
                    id: "1".into(),
                    path: None,
                    persona: None,
                }),
            ));
            events.push((
                3,
                bm(Event::StartRetrospective {
                    epic: "1".into(),
                    persona: None,
                }),
            ));
            events
                .iter()
                .map(|(at, _)| step_cell(&fold(&events[..usize::try_from(*at).unwrap()])).label)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        names,
        ["spec", "build", "retro", "spec", "story 1 · build", "retro"]
    );
    let project = fold(&[
        (1, start_run(RunPath::Project)),
        (
            2,
            bm(Event::StartPlanning {
                doc: PlanningDoc::Prfaq,
                persona: None,
            }),
        ),
    ]);
    assert_eq!(step_cell(&project).label, "prfaq");
    let architecture = fold(&[
        (1, start_run(RunPath::Project)),
        (
            2,
            bm(Event::StartPlanning {
                doc: PlanningDoc::Architecture,
                persona: None,
            }),
        ),
    ]);
    assert_eq!(step_cell(&architecture).label, "architecture");
    let sprint = fold(&[
        (1, start_run(RunPath::Project)),
        (2, bm(Event::PlanStories { epics: Vec::new() })),
    ]);
    assert_eq!(step_cell(&sprint).label, "epics");
    let sprint = fold(&[
        (1, start_run(RunPath::Project)),
        (2, bm(Event::StartSprintPlanning { persona: None })),
    ]);
    assert_eq!(step_cell(&sprint).label, "sprint");
}

#[test]
fn a_finished_run_has_every_step_done() {
    let status = fold(&[
        (1, start_run(RunPath::Epic)),
        (
            2,
            bm(Event::FinishRun {
                outcome: RunOutcome::Completed,
            }),
        ),
    ]);
    let step_states: Vec<_> = view(&status)
        .0
        .steps
        .into_iter()
        .map(|step| match step {
            RailStep::Plain { state, .. } | RailStep::Loop { state, .. } => state,
        })
        .collect();
    assert_eq!(step_states, [StepState::Done; 3]);
    assert_eq!(step_cell(&status).label, "run · completed");
}

#[test]
fn a_run_at_no_step_marks_none_current() {
    let mut state = bmad_state(&fold(&[(1, start_run(RunPath::Epic))])).clone();
    state.run.as_mut().unwrap().current = None;
    let rail = BmadV6_12::rail(&state, None);
    assert!(rail.steps.iter().all(|step| matches!(
        step,
        RailStep::Plain {
            state: StepState::Upcoming,
            ..
        } | RailStep::Loop {
            state: StepState::Upcoming,
            ..
        }
    )));
    assert!(
        BmadV6_12::outline(&state, None)
            .iter()
            .all(|block| !block.current)
    );
    // A run standing at no step at all names only the module.
    assert_eq!(BmadV6_12::step_cell(&state, None).label, "bmad 6.12");
}

#[test]
fn a_gate_waiting_in_a_run_puts_its_button_on_the_rail() {
    let status = fold(&[(1, start_run(RunPath::Single)), (2, prd_request("prd.md"))]);
    assert!(bmad_state(&status).run.is_some());
    assert_eq!(
        view(&status).0.right,
        RailRight::Gate {
            approve_label: "Approve document".into(),
        }
    );
    assert!(step_cell(&status).gate_waiting);
    assert_eq!(step_cell(&status).label, "spec");
}

/// The story plan of one epic per entry of `sizes`, epic `i` titled `Epic i` with that many
/// stories `i.j`, each titled `Story i.j`.
fn planned(sizes: &[usize]) -> WorkflowEvent {
    bm(planned_event(sizes))
}

/// The report of the story plan [`planned`] describes.
fn planned_event(sizes: &[usize]) -> Event {
    Event::PlanStories {
        epics: (1..)
            .zip(sizes)
            .map(|(epic, size)| EpicPlan {
                id: epic.to_string(),
                title: format!("Epic {epic}"),
                stories: (1..=*size).map(|n| story(&format!("{epic}.{n}"))).collect(),
            })
            .collect(),
    }
}

fn start_story(id: &str, path: Option<&str>, persona: Option<&str>) -> WorkflowEvent {
    bm(Event::StartStory {
        id: id.into(),
        path: path.map(PathBuf::from),
        persona: persona.map(str::to_owned),
    })
}

fn finish_story(id: &str, outcome: StoryOutcome) -> WorkflowEvent {
    bm(Event::FinishStory {
        id: id.into(),
        outcome,
    })
}

/// A project of four epics, the second of eight stories, its third story in review.
fn project_in_review() -> Vec<(u64, WorkflowEvent)> {
    vec![
        (1, start_run(RunPath::Project)),
        (2, planned(&[2, 8, 1, 1])),
        (
            3,
            bm(Event::StartSprintPlanning {
                persona: Some("Bob · sm".into()),
            }),
        ),
        (
            4,
            start_story("2.3", Some("stories/2.3.md"), Some("Amelia · dev")),
        ),
        (
            5,
            bm(Event::StartStoryReview {
                id: "2.3".into(),
                persona: Some("Quinn · qa".into()),
            }),
        ),
    ]
}

/// The story loop on a run's rail.
fn story_loop(label: &str, active: Option<usize>, state: StepState) -> RailStep {
    RailStep::Loop {
        label: label.into(),
        phases: vec!["build".into(), "review".into()],
        active,
        state,
    }
}

/// The story loop on the rail of `status`.
fn the_loop(status: &WorkflowStatus) -> RailStep {
    view(status)
        .0
        .steps
        .into_iter()
        .find(|step| matches!(step, RailStep::Loop { .. }))
        .unwrap()
}

#[test]
fn the_project_rail_shows_the_loop_with_epic_and_story_counts() {
    let events = project_in_review();
    let plain = |label: &str| step(label, StepState::Done, None);
    assert_eq!(
        view(&fold(&events)).0.steps,
        [
            plain("prd"),
            plain("architecture"),
            plain("epics"),
            plain("sprint"),
            story_loop("epic 2 of 4 · story 3 of 8", Some(1), StepState::Current),
            step("retro", StepState::Upcoming, None),
        ]
    );
    assert_eq!(
        the_loop(&fold(&events[..4])),
        story_loop("epic 2 of 4 · story 3 of 8", Some(0), StepState::Current)
    );
    let mut done = events.clone();
    done.push((6, finish_story("2.3", StoryOutcome::Done)));
    assert_eq!(
        the_loop(&fold(&done)),
        story_loop("epic 2 of 4 · story 3 of 8", None, StepState::Current)
    );

    // Before any story the loop counts what is planned.
    assert_eq!(
        the_loop(&fold(&events[..1])),
        story_loop("stories", None, StepState::Upcoming)
    );
    assert_eq!(
        the_loop(&fold(&events[..2])),
        story_loop("story ? of 12", None, StepState::Upcoming)
    );
    let mut epic = events[..2].to_vec();
    epic.push((3, bm(Event::StartEpic { id: "2".into() })));
    assert_eq!(
        the_loop(&fold(&epic)),
        story_loop("epic 2 of 4 · story ? of 8", None, StepState::Upcoming)
    );
}

#[test]
fn the_epic_rail_counts_all_stories() {
    let status = fold(&[
        (1, start_run(RunPath::Epic)),
        (2, planned(&[2, 1])),
        (3, start_story("2.1", None, None)),
    ]);
    assert_eq!(
        view(&status).0.steps,
        [
            step("spec", StepState::Done, None),
            story_loop("story 3 of 3", Some(0), StepState::Current),
            step("retro", StepState::Upcoming, None),
        ]
    );
    let first = fold(&[
        (1, start_run(RunPath::Epic)),
        (2, planned(&[2, 1])),
        (3, start_story("1.1", None, None)),
    ]);
    assert_eq!(
        the_loop(&first),
        story_loop("story 1 of 3", Some(0), StepState::Current)
    );
}

#[test]
fn the_rail_right_names_persona_story_and_open_story() {
    let events = project_in_review();
    assert_eq!(
        view(&fold(&events)).0.right,
        RailRight::Unit {
            persona: Some("Quinn · qa".into()),
            title: Some("Story 2.3".into()),
            pill: None,
            link: Some(("open story".into(), "story".into())),
        }
    );
    assert_eq!(
        view(&fold(&events[..3])).0.right,
        RailRight::Unit {
            persona: Some("Bob · sm".into()),
            title: None,
            pill: None,
            link: None,
        }
    );
    let pathless = fold(&[
        (1, start_run(RunPath::Epic)),
        (2, planned(&[1])),
        (3, start_story("1.1", None, None)),
    ]);
    assert_eq!(
        view(&pathless).0.right,
        RailRight::Unit {
            persona: None,
            title: Some("Story 1.1".into()),
            pill: None,
            link: None,
        }
    );
}

#[test]
fn orchestrator_step_cells() {
    let events = project_in_review();
    let cell = |events: &[(u64, WorkflowEvent)]| step_cell(&fold(events)).label;
    assert_eq!(cell(&events[..1]), "prd");
    assert_eq!(cell(&events[..2]), "epics");
    assert_eq!(cell(&events[..3]), "sprint");
    assert_eq!(cell(&events[..4]), "story 3/8 · build");
    assert_eq!(cell(&events), "story 3/8 · review");
    let mut events = events;
    events.push((6, finish_story("2.3", StoryOutcome::Done)));
    assert_eq!(cell(&events), "story 3/8");
    events.push((
        7,
        bm(Event::StartRetrospective {
            epic: "2".into(),
            persona: None,
        }),
    ));
    assert_eq!(cell(&events), "retro");
    for (outcome, word) in [
        (RunOutcome::Completed, "run · completed"),
        (RunOutcome::Abandoned, "run · abandoned"),
    ] {
        let mut finished = events.clone();
        finished.push((8, bm(Event::FinishRun { outcome })));
        assert_eq!(cell(&finished), word);
    }
    let epic = [
        (1, start_run(RunPath::Epic)),
        (2, planned(&[2, 1])),
        (3, start_story("2.1", None, None)),
    ];
    assert_eq!(cell(&epic), "story 3/3 · build");
    assert_eq!(cell(&epic[..2]), "spec");
}

#[test]
fn the_story_list_document_marks_done_and_current() {
    let events = [
        (1, start_run(RunPath::Project)),
        (2, planned(&[2, 8, 1, 1])),
        (3, start_story("1.1", None, None)),
        (4, finish_story("1.1", StoryOutcome::Done)),
        (5, start_story("2.3", Some("stories/2.3.md"), None)),
    ];
    let (_, outline, documents, _) = view(&fold(&events));
    assert_eq!(documents.len(), 2);
    let stories = &documents[0];
    assert_eq!(
        (
            stories.key.as_str(),
            stories.label.as_str(),
            &stories.reported
        ),
        ("stories", "stories", &None)
    );
    let tasks = stories.tasks.as_ref().unwrap();
    assert_eq!(tasks.rows.len(), 12);
    assert_eq!(tasks.footnote, None);
    assert_eq!(
        tasks.rows[0],
        TaskRowView {
            n: "1.1".into(),
            title: "Story 1.1".into(),
            files: Vec::new(),
            done: true,
            current: false,
        }
    );
    assert_eq!(
        tasks.rows[4],
        TaskRowView {
            n: "2.3".into(),
            title: "Story 2.3".into(),
            files: Vec::new(),
            done: false,
            current: true,
        }
    );
    assert_eq!(tasks.rows.iter().filter(|row| row.done).count(), 1);
    assert_eq!(tasks.rows.iter().filter(|row| row.current).count(), 1);
    assert_eq!(
        documents[1],
        DocRef {
            key: "story".into(),
            label: "story 2.3".into(),
            reported: Some(ReportedDoc {
                path: "stories/2.3.md".into(),
                rev: 1,
                at: 5,
            }),
            tasks: None,
        }
    );

    let note = |text: &str| OutlineLine::Note(text.into());
    assert_eq!(
        outline[4],
        OutlineBlock {
            label: "STORIES".into(),
            mark: BlockMark::None,
            lines: vec![
                note("epic 1 · 1 of 2 done"),
                note("epic 2 · 0 of 8 done"),
                note("epic 3 · 0 of 1 done"),
                note("epic 4 · 0 of 1 done"),
            ],
            doc: Some("stories".into()),
            current: true,
        }
    );
    assert_eq!(outline.iter().filter(|block| block.current).count(), 1);

    // Before the stories are planned there is no story list.
    let (_, outline, documents, _) = view(&fold(&events[..1]));
    assert_eq!(documents, Vec::new());
    assert_eq!((outline[4].lines.len(), &outline[4].doc), (0, &None));

    // A story started before any are planned leaves no story list either, only its file.
    let (_, outline, documents, _) = view(&fold(&[
        (1, start_run(RunPath::Project)),
        (2, planned(&[0])),
        (3, start_story("9.9", Some("stories/9.9.md"), None)),
    ]));
    let keys: Vec<&str> = documents.iter().map(|doc| doc.key.as_str()).collect();
    assert_eq!(keys, ["story"]);
    assert_eq!((outline[4].lines.len(), &outline[4].doc), (0, &None));
}

#[test]
fn an_unlisted_story_is_named_by_its_id() {
    for path in [RunPath::Epic, RunPath::Project] {
        let status = fold(&[
            (1, start_run(path)),
            (2, planned(&[2, 1])),
            (3, start_story("9.9", None, None)),
        ]);
        assert_eq!(
            the_loop(&status),
            story_loop("story 9.9", Some(0), StepState::Current),
            "{path:?}"
        );
        assert_eq!(step_cell(&status).label, "story 9.9 · build", "{path:?}");
        let RailRight::Unit { title, .. } = view(&status).0.right else {
            panic!("no gate waits");
        };
        assert_eq!(title.as_deref(), Some("story 9.9"));
        let rows = view(&status).2[0].tasks.clone().unwrap().rows;
        assert!(rows.iter().all(|row| !row.current));
    }
}

#[test]
fn run_persona_rows_and_notes() {
    let row = |events: &[(u64, WorkflowEvent)]| {
        let (_, last) = events.last().unwrap();
        transcript_row(&fold(events), last)
    };
    let persona = |persona: &str, text: &str, path: Option<&str>| {
        Some(WorkflowRow::Persona {
            persona: persona.into(),
            text: text.into(),
            path: path.map(PathBuf::from),
        })
    };
    let mut events = vec![(1, start_run(RunPath::Project))];
    let mut then = |event: Event| {
        events.push((u64::try_from(events.len()).unwrap() + 1, bm(event)));
        row(&events)
    };
    assert_eq!(
        then(Event::StartPlanning {
            doc: PlanningDoc::Prd,
            persona: Some("John · pm".into()),
        }),
        persona("John · pm", "prd", None)
    );
    assert_eq!(then(planned_event(&[2])), None);
    assert_eq!(
        then(Event::StartSprintPlanning {
            persona: Some("Bob · sm".into()),
        }),
        persona("Bob · sm", "sprint planning", None)
    );
    assert_eq!(
        then(Event::StartStory {
            id: "1.2".into(),
            path: Some("stories/1.2.md".into()),
            persona: Some("Amelia · dev".into()),
        }),
        persona(
            "Amelia · dev",
            "story 1.2 · Story 1.2",
            Some("stories/1.2.md")
        )
    );
    assert_eq!(
        then(Event::StartStoryReview {
            id: "1.2".into(),
            persona: Some("Quinn · qa".into()),
        }),
        persona("Quinn · qa", "review of story 1.2", None)
    );
    assert_eq!(
        then(Event::StartStory {
            id: "9.9".into(),
            path: None,
            persona: Some("Amelia · dev".into()),
        }),
        persona("Amelia · dev", "story 9.9", None)
    );
    // A review of a story that is not the current one changes nothing, so it names no one.
    assert_eq!(
        then(Event::StartStoryReview {
            id: "1.2".into(),
            persona: Some("Quinn · qa".into()),
        }),
        None
    );
    assert_eq!(
        then(Event::StartRetrospective {
            epic: "2".into(),
            persona: Some("Bob · sm".into()),
        }),
        persona("Bob · sm", "retro · epic 2", None)
    );
    for event in [
        Event::StartPlanning {
            doc: PlanningDoc::Architecture,
            persona: None,
        },
        Event::StartStory {
            id: "1.2".into(),
            path: None,
            persona: None,
        },
        Event::StartSprintPlanning { persona: None },
        Event::StartStoryReview {
            id: "1.2".into(),
            persona: None,
        },
        Event::StartRetrospective {
            epic: "2".into(),
            persona: None,
        },
        Event::StartEpic { id: "1".into() },
    ] {
        assert_eq!(then(event), None);
    }
}

#[test]
fn run_step_notes() {
    let note = |event: WorkflowEvent| step_note(&event, "14:02");
    let notes = [
        (start_run(RunPath::Project), "bmad · Invoices · project"),
        (start_run(RunPath::Epic), "bmad · Invoices · epic"),
        (start_run(RunPath::Single), "bmad · Invoices · single"),
        (planned(&[2, 8, 1, 1]), "4 epics · 12 stories planned"),
        (bm(Event::StartEpic { id: "2".into() }), "epic 2"),
        (start_story("1.2", None, None), "story 1.2 started"),
        (finish_story("1.2", StoryOutcome::Done), "story 1.2 done"),
        (
            finish_story("1.2", StoryOutcome::ChangesRequested),
            "story 1.2 back to build",
        ),
        (
            bm(Event::FinishRun {
                outcome: RunOutcome::Completed,
            }),
            "run completed",
        ),
        (
            bm(Event::FinishRun {
                outcome: RunOutcome::Abandoned,
            }),
            "run abandoned",
        ),
    ];
    for (event, expected) in notes {
        assert_eq!(note(event).as_deref(), Some(expected));
    }
    for event in [
        Event::StartPlanning {
            doc: PlanningDoc::Prd,
            persona: None,
        },
        Event::StartSprintPlanning { persona: None },
        Event::StartStoryReview {
            id: "1.2".into(),
            persona: None,
        },
        Event::StartRetrospective {
            epic: "2".into(),
            persona: None,
        },
    ] {
        assert_eq!(note(bm(event)), None);
    }
}
