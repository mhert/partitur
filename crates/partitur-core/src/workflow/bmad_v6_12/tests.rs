#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::json;

use super::*;
use crate::workflow::{
    GateKind, ModuleEvent, ModuleState, WorkflowEvent, WorkflowStatus, apply, gate_texts, rebuild,
    step_cell, step_note, view,
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

#[test]
fn the_placeholder_display_names_the_kind_and_the_documents() {
    let empty = fold(&[(1, closed(DOCUMENT, GateCloser::Prompt))]);
    assert_eq!(step_cell(&empty).label, "bmad 6.12");

    let status = fold(&[
        (1, start(WorkflowKind::CodeReview)),
        (
            2,
            bm(Event::RequestReviewVerdict {
                story: "1.2".into(),
                verdict: ReviewVerdict::Approved,
                path: "review.md".into(),
            }),
        ),
    ]);
    let (rail, outline, documents, gate) = view(&status);
    assert_eq!(rail.chip, "bmad 6.12");
    assert!(rail.steps.is_empty());
    assert_eq!(
        rail.right,
        RailRight::Gate {
            approve_label: "Accept verdict".into()
        }
    );
    assert!(outline.is_empty());
    assert_eq!(
        documents,
        [DocRef {
            key: REVIEW_VERDICT.into(),
            label: "review verdict".into(),
            reported: Some(ReportedDoc {
                path: "review.md".into(),
                rev: 1,
                at: 2,
            }),
            tasks: None,
        }]
    );
    assert_eq!(
        gate,
        Some(GateView {
            approve_label: "Accept verdict".into(),
            change_prefix: "Changes to the review: ".into(),
            hint: gate_texts(ModuleId::BmadV6_12, &GateKind::new(REVIEW_VERDICT))
                .hint
                .into(),
            doc: Some(REVIEW_VERDICT.into()),
        })
    );
    assert_eq!(
        step_cell(&status),
        StepCell {
            label: "code review".into(),
            gate_waiting: true,
        }
    );

    let quiet = apply(
        Some(status),
        &closed(REVIEW_VERDICT, GateCloser::Approve),
        3,
    );
    assert_eq!(
        view(&quiet).0.right,
        RailRight::Unit {
            persona: None,
            title: None,
            pill: None,
            link: None,
        }
    );
    assert!(!step_cell(&quiet).gate_waiting);
    assert_eq!(
        crate::workflow::transcript_row(&quiet, &start(WorkflowKind::Build)),
        None
    );

    // A gate without a document of its own names none.
    let bare = fold(&[(1, start(WorkflowKind::Prd)), (2, prd_request("prd.md"))]);
    let gate = OpenGate {
        kind: GateKind::new(STORY_ORDER),
        since: 2,
    };
    assert_eq!(BmadV6_12::gate_view(bmad_state(&bare), &gate).doc, None);
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
    let names: Vec<_> = BmadV6_12::tools().iter().map(|tool| tool.name).collect();
    assert_eq!(
        names,
        calls.iter().map(|(name, _, _)| *name).collect::<Vec<_>>()
    );
    for (name, arguments, event) in calls {
        assert_eq!(event.tool_name(), name);
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
        BmadV6_12::parse("bmad_v6_12_start_run", json!({})),
        Err("unknown tool bmad_v6_12_start_run".to_owned())
    );
    assert_eq!(
        crate::workflow::parse("bmad_v6_12_start_run", json!({}), true),
        None
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
}
