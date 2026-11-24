#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use super::*;
use crate::transcript::{Entry, Transcript};

/// One record of every kind, as JSON lines of a history.
const EVERY_KIND: &[&str] = &[
    r#"{"UserText":"hi"}"#,
    r#"{"UserAttachments":[]}"#,
    r#"{"AgentText":"hello"}"#,
    r#"{"AgentThought":"hm"}"#,
    r#"{"ToolCall":{"id":"t","title":"Read","kind":"Read","status":"Pending","content":[],"locations":[]}}"#,
    r#"{"ToolCallUpdate":{"id":"u"}}"#,
    r#"{"PermissionRequested":{"id":1,"tool_call_id":"t","title":"","kind":"Read","options":[]}}"#,
    r#"{"PermissionAnswered":{"id":1,"option":null}}"#,
    r#"{"Usage":{"used":1,"size":2}}"#,
    r#"{"Title":"t"}"#,
    r#"{"RunningBackground":1}"#,
    r#"{"ConfigOptions":[]}"#,
    r#"{"ConfigRejected":{"option":"model","value":"x","error":"no"}}"#,
    r#"{"Notice":"note"}"#,
    r#""TurnStarted""#,
    r#"{"TurnEnded":"EndTurn"}"#,
    r#"{"Failed":"bad"}"#,
    r#"{"ChildrenProposed":{"id":"0d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a","children":[]}}"#,
    r#"{"ProposalDecided":{"id":"0d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a","decision":{"Rejected":{"note":null}}}}"#,
    r#"{"QuestionAsked":{"id":"1d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a","question":{"text":"?","options":[]}}}"#,
    r#"{"QuestionClosed":{"id":"1d9e8f7a-6b5c-4d3e-8f2a-1b0c9d8e7f6a","outcome":"Cancelled"}}"#,
    r#"{"OrchestratorPrompt":"go"}"#,
    r#"{"PartiturPrompt":"go"}"#,
    r#"{"ChildResult":{"child":"6f1c1d8e-2b4a-4c3e-9f5d-1a2b3c4d5e6f","repository":"api","title":"t","origin":"User","outcome":{"Ended":"EndTurn"},"text":"done"}}"#,
    r#"{"ChildPrompted":{"child":"6f1c1d8e-2b4a-4c3e-9f5d-1a2b3c4d5e6f","prompt":"go"}}"#,
    r#"{"TitleProposed":{"child":"6f1c1d8e-2b4a-4c3e-9f5d-1a2b3c4d5e6f","repository":"api","title":"t","proposed":"u","by_user":false}}"#,
    r#"{"Workflow":{"at":1,"event":{"superpowers":{"start_session":{"topic":"login"}}}}}"#,
];

fn every_kind() -> Vec<TranscriptRecord> {
    EVERY_KIND
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect()
}

/// The marks as the replayed transcript of `records` answers them.
fn replayed(records: &[TranscriptRecord]) -> HistoryMarks {
    let transcript = Transcript::from_updates(records);
    HistoryMarks {
        has_entries: !transcript.entries().is_empty(),
        has_history: transcript
            .entries()
            .iter()
            .any(|entry| !matches!(entry, Entry::Notice(_))),
        open_permissions: transcript.unanswered_permissions().collect(),
    }
}

#[test]
fn every_kind_of_record_marks_what_its_replay_shows() {
    let records = every_kind();
    for record in &records {
        let one = std::slice::from_ref(record);
        assert_eq!(HistoryMarks::of(one), replayed(one), "{record:?}");
    }
    for start in 0..records.len() {
        let tail = &records[start..];
        assert_eq!(HistoryMarks::of(tail), replayed(tail), "from {start}");
    }
}

#[test]
fn a_history_of_notices_has_entries_but_no_history() {
    let notices = [
        TranscriptRecord::Notice("note".into()),
        serde_json::from_str(EVERY_KIND[12]).unwrap(),
    ];

    let marks = HistoryMarks::of(&notices);

    assert!(marks.has_entries);
    assert!(!marks.has_history);
    assert_eq!(marks, replayed(&notices));
}

#[test]
fn records_that_show_nothing_leave_the_marks_alone() {
    let mut marks = HistoryMarks::default();
    for line in [EVERY_KIND[8], EVERY_KIND[9], EVERY_KIND[11]] {
        assert!(!marks.apply(&serde_json::from_str(line).unwrap()), "{line}");
    }
    assert_eq!(marks, HistoryMarks::default());
}

fn requested(id: u64) -> TranscriptRecord {
    serde_json::from_str(&format!(
        r#"{{"PermissionRequested":{{"id":{id},"tool_call_id":"t","title":"","kind":"Read","options":[]}}}}"#
    ))
    .unwrap()
}

fn answered(id: u64) -> TranscriptRecord {
    TranscriptRecord::PermissionAnswered {
        id: PermissionId(id),
        option: None,
    }
}

#[test]
fn a_permission_stays_open_until_an_answer_with_its_id() {
    let mut marks = HistoryMarks::default();

    assert!(marks.apply(&requested(1)));
    assert!(marks.has_open_permission());
    assert!(!marks.apply(&answered(2)), "another id settles nothing");
    assert!(marks.has_open_permission());
    assert!(marks.apply(&answered(1)));
    assert!(!marks.has_open_permission());
    assert!(
        !marks.apply(&answered(1)),
        "a second answer settles nothing"
    );
    assert_eq!(marks.open_permissions, Vec::new());
}

#[test]
fn a_reused_permission_id_needs_an_answer_per_request() {
    let records = [requested(1), requested(2), requested(1), answered(1)];

    let marks = HistoryMarks::of(&records);

    assert_eq!(marks, replayed(&records));
    assert_eq!(marks.open_permissions, [PermissionId(1), PermissionId(2)]);
    assert!(marks.has_open_permission());
}

#[test]
fn marks_are_stored_without_an_empty_permission_list() {
    let marks = HistoryMarks {
        has_entries: true,
        has_history: false,
        open_permissions: Vec::new(),
    };

    let json = serde_json::to_string(&marks).unwrap();

    assert_eq!(json, r#"{"has_entries":true,"has_history":false}"#);
    assert_eq!(serde_json::from_str::<HistoryMarks>(&json).unwrap(), marks);
    let open = HistoryMarks::of(&[requested(4)]);
    let json = serde_json::to_string(&open).unwrap();
    assert_eq!(
        json,
        r#"{"has_entries":true,"has_history":true,"open_permissions":[4]}"#
    );
}
