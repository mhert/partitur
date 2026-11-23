#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use serde_json::json;

use super::*;

const SESSION: &str = "cp-1";

fn meta(key: &str) -> one::Meta {
    one::Meta::from_iter([(key.to_owned(), json!(true))])
}

fn notification(update: one::SessionUpdate) -> one::SessionNotification {
    one::SessionNotification::new(SESSION, update)
}

/// The v2 update `update` translates into, failing when it is dropped.
fn translated(update: one::SessionUpdate) -> two::SessionUpdate {
    let translated = translate_update(notification(update)).expect("translated");
    assert_eq!(translated.session_id, two::SessionId::new(SESSION));
    translated.update
}

fn text(text: &str) -> one::ContentBlock {
    one::ContentBlock::Text(one::TextContent::new(text))
}

#[test]
fn a_tool_call_becomes_an_upsert() {
    let call = one::ToolCall::new("t1", "Read README.md")
        .name("view".to_owned())
        .kind(one::ToolKind::Read)
        .status(one::ToolCallStatus::InProgress)
        .content(vec![one::ToolCallContent::from(text("ok"))])
        .locations(vec![one::ToolCallLocation::new("/work/README.md").line(3)])
        .raw_input(json!({"path": "README.md"}))
        .raw_output(json!("done"))
        .meta(meta("call"));
    let expected = two::ToolCallUpdate::new("t1")
        .title("Read README.md".to_owned())
        .name("view".to_owned())
        .kind(two::ToolKind::Read)
        .status(two::ToolCallStatus::InProgress)
        .content(vec![two::ToolCallContent::from(two::ContentBlock::from(
            "ok",
        ))])
        .locations(vec![two::ToolCallLocation::new("/work/README.md").line(3)])
        .raw_input(json!({"path": "README.md"}))
        .raw_output(json!("done"))
        .meta(meta("call"));
    assert_eq!(
        translated(one::SessionUpdate::ToolCall(call)),
        two::SessionUpdate::ToolCallUpdate(expected)
    );
    // A bare call still sets its content and locations, which it has none of, and leaves
    // what it does not carry undefined.
    assert_eq!(
        translated(one::SessionUpdate::ToolCall(one::ToolCall::new("t2", "x"))),
        two::SessionUpdate::ToolCallUpdate(
            two::ToolCallUpdate::new("t2")
                .title("x".to_owned())
                .kind(two::ToolKind::Other)
                .status(two::ToolCallStatus::Pending)
                .content(Vec::new())
                .locations(Vec::new())
        )
    );
}

#[test]
fn a_tool_call_update_changes_only_the_fields_it_carries() {
    let fields = one::ToolCallUpdateFields::new()
        .status(one::ToolCallStatus::Completed)
        .content(vec![one::ToolCallContent::from(text("ok"))]);
    let update = one::ToolCallUpdate::new("t1", fields).meta(meta("update"));
    assert_eq!(
        translated(one::SessionUpdate::ToolCallUpdate(update)),
        two::SessionUpdate::ToolCallUpdate(
            two::ToolCallUpdate::new("t1")
                .status(two::ToolCallStatus::Completed)
                .content(vec![two::ToolCallContent::from(two::ContentBlock::from(
                    "ok"
                ))])
                .meta(meta("update"))
        )
    );
    let full = one::ToolCallUpdateFields::new()
        .kind(one::ToolKind::Edit)
        .title("Edit".to_owned())
        .name("edit".to_owned())
        .locations(vec![one::ToolCallLocation::new("/a")])
        .raw_input(json!(1))
        .raw_output(json!(2));
    assert_eq!(
        translate_tool_call_update(one::ToolCallUpdate::new("t2", full)),
        two::ToolCallUpdate::new("t2")
            .kind(two::ToolKind::Edit)
            .title("Edit".to_owned())
            .name("edit".to_owned())
            .locations(vec![two::ToolCallLocation::new("/a")])
            .raw_input(json!(1))
            .raw_output(json!(2))
    );
}

#[test]
fn every_tool_kind_and_status_is_translated() {
    let kinds = [
        (one::ToolKind::Read, two::ToolKind::Read),
        (one::ToolKind::Edit, two::ToolKind::Edit),
        (one::ToolKind::Delete, two::ToolKind::Delete),
        (one::ToolKind::Move, two::ToolKind::Move),
        (one::ToolKind::Search, two::ToolKind::Search),
        (one::ToolKind::Execute, two::ToolKind::Execute),
        (one::ToolKind::Think, two::ToolKind::Think),
        (one::ToolKind::Fetch, two::ToolKind::Fetch),
        (one::ToolKind::SwitchMode, two::ToolKind::SwitchMode),
        (one::ToolKind::Other, two::ToolKind::Other),
    ];
    for (v1, v2) in kinds {
        assert_eq!(translate_kind(v1), v2);
    }
    let statuses = [
        (one::ToolCallStatus::Pending, two::ToolCallStatus::Pending),
        (
            one::ToolCallStatus::InProgress,
            two::ToolCallStatus::InProgress,
        ),
        (
            one::ToolCallStatus::Completed,
            two::ToolCallStatus::Completed,
        ),
        (one::ToolCallStatus::Failed, two::ToolCallStatus::Failed),
    ];
    for (v1, v2) in statuses {
        assert_eq!(translate_status(v1), v2);
    }
}

#[test]
fn a_diff_becomes_a_whole_file_patch() {
    let edit = one::Diff::new("/work/a.rs", "one\nthree\n").old_text("one\ntwo\n".to_owned());
    assert_eq!(
        translate_content(one::ToolCallContent::Diff(edit.meta(meta("diff")))),
        Some(two::ToolCallContent::Diff(
            two::Diff::patch(
                "--- /work/a.rs\n+++ /work/a.rs\n@@ -1,2 +1,2 @@\n-one\n-two\n+one\n+three\n",
                vec![two::DiffChange::modify("/work/a.rs")],
            )
            .meta(meta("diff"))
        ))
    );
    let created = one::Diff::new("/work/b.rs", "new\n");
    assert_eq!(
        translate_content(one::ToolCallContent::Diff(created)),
        Some(two::ToolCallContent::Diff(two::Diff::patch(
            "--- /dev/null\n+++ /work/b.rs\n@@ -0,0 +1,1 @@\n+new\n",
            vec![two::DiffChange::add("/work/b.rs")],
        )))
    );
    assert_eq!(
        whole_file_patch(&PathBuf::from("/c"), Some("gone\n"), ""),
        "--- /c\n+++ /c\n@@ -1,1 +0,0 @@\n-gone\n"
    );
}

#[test]
fn terminals_and_session_references_stay_what_they_are() {
    let terminal = one::ToolCallContent::Terminal(one::Terminal::new("term-1").meta(meta("t")));
    assert_eq!(
        translate_content(terminal),
        Some(two::ToolCallContent::Terminal(
            two::Terminal::new("term-1").meta(meta("t"))
        ))
    );
    let session =
        one::ToolCallContent::Session(one::SessionReference::new("sub-1").meta(meta("s")));
    assert_eq!(
        translate_content(session),
        Some(two::ToolCallContent::Session(
            two::SessionReference::new("sub-1").meta(meta("s"))
        ))
    );
    let content = one::ToolCallContent::Content(one::Content::new(text("hi")).meta(meta("c")));
    assert_eq!(
        translate_content(content),
        Some(two::ToolCallContent::Content(Box::new(
            two::Content::new(two::ContentBlock::from("hi")).meta(meta("c"))
        )))
    );
}

#[test]
fn chunks_keep_their_kind_and_message() {
    let chunk = || one::ContentChunk::new(text("hi"));
    let expected = |id: &str| two::ContentChunk::new(two::ContentBlock::from("hi"), id);
    assert_eq!(
        translated(one::SessionUpdate::UserMessageChunk(chunk())),
        two::SessionUpdate::UserMessageChunk(expected(NO_MESSAGE_ID))
    );
    assert_eq!(
        translated(one::SessionUpdate::AgentMessageChunk(
            chunk()
                .message_id(one::MessageId::new("m-1"))
                .meta(meta("m"))
        )),
        two::SessionUpdate::AgentMessageChunk(expected("m-1").meta(meta("m")))
    );
    assert_eq!(
        translated(one::SessionUpdate::AgentThoughtChunk(chunk())),
        two::SessionUpdate::AgentThoughtChunk(expected(NO_MESSAGE_ID))
    );
}

#[test]
fn a_plan_becomes_the_update_of_the_one_plan() {
    let plan = one::Plan::new(vec![one::PlanEntry::new(
        "Write the test",
        one::PlanEntryPriority::High,
        one::PlanEntryStatus::InProgress,
    )])
    .meta(meta("plan"));
    assert_eq!(
        translated(one::SessionUpdate::Plan(plan)),
        two::SessionUpdate::PlanUpdate(
            two::PlanUpdate::new(two::PlanUpdateContent::Items(two::PlanItems::new(
                PLAN_ID,
                vec![two::PlanEntry::new(
                    "Write the test",
                    two::PlanEntryPriority::High,
                    two::PlanEntryStatus::InProgress,
                )],
            )))
            .meta(meta("plan"))
        )
    );
}

#[test]
fn commands_keep_their_hint() {
    let commands = one::AvailableCommandsUpdate::new(vec![
        one::AvailableCommand::new("review", "Reviews the change")
            .input(one::AvailableCommandInput::Unstructured(
                one::UnstructuredCommandInput::new("what to review").meta(meta("i")),
            ))
            .meta(meta("c")),
        one::AvailableCommand::new("plan", "Plans"),
    ])
    .meta(meta("u"));
    assert_eq!(
        translated(one::SessionUpdate::AvailableCommandsUpdate(commands)),
        two::SessionUpdate::AvailableCommandsUpdate(
            two::AvailableCommandsUpdate::new(vec![
                two::AvailableCommand::Command(
                    two::Command::new("review", "Reviews the change")
                        .input(two::AvailableCommandInput::Text(
                            two::TextCommandInput::new("what to review").meta(meta("i"))
                        ))
                        .meta(meta("c"))
                ),
                two::AvailableCommand::Command(two::Command::new("plan", "Plans")),
            ])
            .meta(meta("u"))
        )
    );
}

/// Copilot's `mode` option, choices ungrouped, `#agent` current.
fn v1_mode() -> one::SessionConfigOption {
    one::SessionConfigOption::select(
        "mode",
        "Mode",
        "#agent",
        vec![
            one::SessionConfigSelectOption::new("#agent", "Agent"),
            one::SessionConfigSelectOption::new("#plan", "Plan").description("Plans first"),
        ],
    )
    .category(one::SessionConfigOptionCategory::Mode)
    .description("How the agent works".to_owned())
    .meta(meta("mode"))
}

#[test]
fn config_ids_are_renamed() {
    let grouped = one::SessionConfigOption::select(
        "model",
        "Model",
        "gpt-5",
        vec![
            one::SessionConfigSelectGroup::new(
                "openai",
                "OpenAI",
                vec![one::SessionConfigSelectOption::new("gpt-5", "GPT-5")],
            )
            .meta(meta("group")),
        ],
    );
    let boolean = one::SessionConfigOption::boolean("fast", "Fast", true);
    let translated = translate_config(vec![v1_mode(), grouped, boolean]);
    assert_eq!(
        translated,
        [
            two::SessionConfigOption::select(
                "mode",
                "Mode",
                "#agent",
                vec![
                    two::SessionConfigSelectOption::new("#agent", "Agent"),
                    two::SessionConfigSelectOption::new("#plan", "Plan").description("Plans first"),
                ],
            )
            .category(two::SessionConfigOptionCategory::Mode)
            .description("How the agent works".to_owned())
            .meta(meta("mode")),
            two::SessionConfigOption::select(
                "model",
                "Model",
                "gpt-5",
                vec![
                    two::SessionConfigSelectGroup::new(
                        "openai",
                        "OpenAI",
                        vec![two::SessionConfigSelectOption::new("gpt-5", "GPT-5")],
                    )
                    .meta(meta("group"))
                ],
            ),
            two::SessionConfigOption::boolean("fast", "Fast", true),
        ]
    );
    // On the wire, the ids sit under their v2 names.
    let wire = serde_json::to_value(&translated).unwrap();
    assert_eq!(wire[0]["configId"], json!("mode"));
    assert_eq!(wire[1]["options"][0]["groupId"], json!("openai"));
    assert!(wire[0].get("id").is_none());
}

#[test]
fn a_config_update_carries_the_translated_options() {
    let update = one::ConfigOptionUpdate::new(vec![v1_mode()]).meta(meta("u"));
    assert_eq!(
        translated(one::SessionUpdate::ConfigOptionUpdate(update)),
        two::SessionUpdate::ConfigOptionUpdate(
            two::ConfigOptionUpdate::new(translate_config(vec![v1_mode()])).meta(meta("u"))
        )
    );
}

#[test]
fn session_info_usage_and_notices_keep_their_fields() {
    let info = one::SessionInfoUpdate::new()
        .title("Fix the login".to_owned())
        .updated_at("2026-11-20T10:00:00Z".to_owned())
        .meta(meta("i"));
    assert_eq!(
        translated(one::SessionUpdate::SessionInfoUpdate(info)),
        two::SessionUpdate::SessionInfoUpdate(
            two::SessionInfoUpdate::new()
                .title("Fix the login".to_owned())
                .updated_at("2026-11-20T10:00:00Z".to_owned())
                .meta(meta("i"))
        )
    );
    let usage = one::UsageUpdate::new(10, 1000)
        .cost(one::Cost::new(0.5, "USD"))
        .meta(meta("u"));
    assert_eq!(
        translated(one::SessionUpdate::UsageUpdate(usage)),
        two::SessionUpdate::UsageUpdate(
            two::UsageUpdate::new(10, 1000)
                .cost(two::Cost::new(0.5, "USD"))
                .meta(meta("u"))
        )
    );
    let notice = one::Notice::new(one::NoticeSeverity::Warning, "Rate limited")
        .description("try later".to_owned())
        .meta(meta("n"));
    assert_eq!(
        translated(one::SessionUpdate::Notice(notice)),
        two::SessionUpdate::Notice(
            two::Notice::new(two::NoticeSeverity::Warning, "Rate limited")
                .description("try later".to_owned())
                .meta(meta("n"))
        )
    );
}

#[test]
fn a_notification_keeps_its_meta() {
    let note = notification(one::SessionUpdate::AgentMessageChunk(
        one::ContentChunk::new(text("hi")),
    ))
    .meta(meta("n"));
    assert_eq!(translate_update(note).unwrap().meta, Some(meta("n")));
}

#[test]
fn an_unknown_update_is_dropped() {
    let mode = one::SessionUpdate::CurrentModeUpdate(one::CurrentModeUpdate::new("#plan"));
    assert_eq!(translate_update(notification(mode)), None);
    let compaction: one::SessionUpdate = serde_json::from_value(json!({
        "sessionUpdate": "compaction_update",
        "compactionId": "c-1",
        "status": "started",
    }))
    .unwrap();
    assert_eq!(translate_update(notification(compaction)), None);
}

#[test]
fn a_prompt_runs_and_its_answer_ends_the_work() {
    let session = two::SessionId::new(SESSION);
    assert_eq!(
        running(session.clone()),
        two::UpdateSessionNotification::new(
            SESSION,
            two::SessionUpdate::StateUpdate(two::StateUpdate::Running(
                two::RunningStateUpdate::new()
            ))
        )
    );
    let ended = |stop: two::StopReason| {
        two::UpdateSessionNotification::new(
            SESSION,
            two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
                two::IdleStateUpdate::new()
                    .stop_reason(stop)
                    .background(Vec::new()),
            )),
        )
    };
    let reasons = [
        (one::StopReason::EndTurn, two::StopReason::EndTurn),
        (one::StopReason::MaxTokens, two::StopReason::MaxTokens),
        (
            one::StopReason::MaxTurnRequests,
            two::StopReason::MaxTurnRequests,
        ),
        (one::StopReason::Refusal, two::StopReason::Refusal),
        (one::StopReason::Cancelled, two::StopReason::Cancelled),
    ];
    for (v1, v2) in reasons {
        assert_eq!(idle_after_response(session.clone(), v1), ended(v2));
    }
    assert_eq!(
        idle_after_refusal(session),
        two::UpdateSessionNotification::new(
            SESSION,
            two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
                two::IdleStateUpdate::new().background(Vec::new()),
            )),
        )
    );
}

/// Copilot's session event `kind` for [`SESSION`], with `extra` merged into its params.
fn event(kind: &str, extra: &serde_json::Value) -> CopilotEvent {
    let mut params = json!({"sessionId": SESSION, "type": kind, "data": {}});
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    CopilotEvent::parse(&params).expect("an event")
}

fn state(state: two::StateUpdate) -> two::UpdateSessionNotification {
    two::UpdateSessionNotification::new(SESSION, two::SessionUpdate::StateUpdate(state))
}

fn assistant_idle(stop: two::StopReason) -> two::UpdateSessionNotification {
    let work = two::BackgroundWork::Other(two::OtherBackgroundWork::new(
        COPILOT_SESSION_WORK,
        std::collections::BTreeMap::new(),
    ));
    state(two::StateUpdate::Idle(
        two::IdleStateUpdate::new()
            .stop_reason(stop)
            .background(vec![work]),
    ))
}

#[test]
fn a_session_event_is_read_from_its_params() {
    let params = json!({
        "sessionId": SESSION,
        "type": "assistant.idle",
        "data": {"aborted": true},
        "agentId": "sub-1",
    });
    assert_eq!(
        CopilotEvent::parse(&params),
        Some(CopilotEvent {
            session: SESSION.to_owned(),
            kind: "assistant.idle".to_owned(),
            agent: Some("sub-1".to_owned()),
            aborted: true,
        })
    );
    let plain = event("session.idle", &json!({"agentId": null}));
    assert_eq!(
        plain,
        CopilotEvent {
            session: SESSION.to_owned(),
            kind: "session.idle".to_owned(),
            agent: None,
            aborted: false,
        }
    );
    // An agent id of another type still names a subagent.
    assert_eq!(
        event("x", &json!({"agentId": 7})).agent,
        Some("7".to_owned())
    );
    assert_eq!(CopilotEvent::parse(&json!({"type": "x"})), None);
    assert_eq!(CopilotEvent::parse(&json!({"sessionId": SESSION})), None);
}

#[test]
fn a_turn_start_means_running() {
    assert_eq!(
        translate_event(&event("assistant.turn_start", &json!({}))),
        Some(state(two::StateUpdate::Running(
            two::RunningStateUpdate::new()
        )))
    );
}

#[test]
fn an_assistant_idle_ends_the_turn_and_opens_the_session_work() {
    assert_eq!(
        translate_event(&event("assistant.idle", &json!({}))),
        Some(assistant_idle(two::StopReason::EndTurn))
    );
    assert_eq!(
        translate_event(&event(
            "assistant.idle",
            &json!({"data": {"aborted": true}})
        )),
        Some(assistant_idle(two::StopReason::Cancelled))
    );
    let wire = serde_json::to_value(translate_event(&event("assistant.idle", &json!({})))).unwrap();
    assert_eq!(
        wire["update"]["background"],
        json!([{"type": "_copilot/session"}])
    );
}

#[test]
fn a_session_idle_ends_the_background_work_only() {
    assert_eq!(
        translate_event(&event("session.idle", &json!({}))),
        Some(state(two::StateUpdate::Idle(
            two::IdleStateUpdate::new().background(Vec::new())
        )))
    );
}

#[test]
fn subagent_and_unknown_events_mean_nothing() {
    for kind in ["assistant.turn_start", "assistant.idle", "session.idle"] {
        assert_eq!(
            translate_event(&event(kind, &json!({"agentId": "sub-1"}))),
            None,
            "{kind}"
        );
    }
    assert_eq!(
        translate_event(&event("assistant.message", &json!({}))),
        None
    );
}

#[test]
fn a_subagent_update_is_dropped() {
    let mut calls = HashMap::new();
    let mut tagged = |tag: serde_json::Value| {
        let meta = json!({"github.com/copilot": tag});
        let chunk = one::ContentChunk::new(text("hi")).meta(meta.as_object().cloned());
        observe(
            &mut calls,
            notification(one::SessionUpdate::AgentMessageChunk(chunk)),
        )
    };
    assert_eq!(tagged(json!({"agentId": "sub-1"})), None);
    assert!(tagged(json!({"agentId": null})).is_some());
    assert!(tagged(json!({"other": 1})).is_some());
    // An update Partitur reads nothing of is dropped, from the parent too.
    let mode = one::SessionUpdate::CurrentModeUpdate(one::CurrentModeUpdate::new("m"));
    assert_eq!(observe(&mut calls, notification(mode)), None);
    assert_eq!(calls, HashMap::new());
}

#[test]
fn a_subagents_tool_call_is_dropped_but_recorded() {
    let mut calls = HashMap::new();
    let call = one::ToolCall::new("s1", "Edit src/lib.rs")
        .kind(one::ToolKind::Edit)
        .meta(
            json!({"github.com/copilot": {"agentId": "sub-1"}})
                .as_object()
                .cloned(),
        );
    assert_eq!(
        observe(&mut calls, notification(one::SessionUpdate::ToolCall(call))),
        None
    );
    let known = KnownCall {
        name: None,
        title: Some("Edit src/lib.rs".to_owned()),
        kind: Some(two::ToolKind::Edit),
    };
    assert_eq!(calls, HashMap::from([("s1".to_owned(), known.clone())]));
    // The parent's own call is recorded and shown.
    let own = one::ToolCall::new("t1", "Edit src/lib.rs").kind(one::ToolKind::Edit);
    let shown = observe(&mut calls, notification(one::SessionUpdate::ToolCall(own)));
    assert!(shown.is_some());
    assert_eq!(calls.get("t1"), Some(&known));
}

#[test]
fn kept_passes_values_and_drops_none() {
    // Every content block and severity of this version has a v2 form; one that has none
    // drops its update.
    assert_eq!(kept::<u8>(None, "notice"), None);
    assert_eq!(kept(Some(1), "notice"), Some(1));
}

#[test]
fn the_opt_in_asks_for_copilots_session_events() {
    assert_eq!(
        serde_json::Value::Object(copilot_opt_in()),
        json!({"github.com/copilot": {
            "events": ["assistant.turn_start", "assistant.idle", "session.idle"]
        }})
    );
}

#[test]
fn an_image_prompt_is_kept() {
    let prompt = crate::map::map_prompt(vec![
        partitur_core::PromptContent::Text("look".to_owned()),
        partitur_core::PromptContent::Image {
            mime: "image/png".to_owned(),
            base64: "aGk=".to_owned(),
        },
        partitur_core::PromptContent::FileLink {
            path: PathBuf::from("/work/a.rs"),
        },
    ]);
    assert_eq!(
        translate_prompt(prompt),
        [
            text("look"),
            one::ContentBlock::Image(one::ImageContent::new("aGk=", "image/png")),
            one::ContentBlock::ResourceLink(one::ResourceLink::new("a.rs", "file:///work/a.rs")),
        ]
    );
}

#[test]
fn a_permission_request_is_asked_about_its_tool_call() {
    let call = one::ToolCallUpdate::new(
        "t1",
        one::ToolCallUpdateFields::new()
            .title("ask_user".to_owned())
            .kind(one::ToolKind::Other),
    );
    let request = one::RequestPermissionRequest::new(
        SESSION,
        call,
        vec![one::PermissionOption::new(
            "allow_once",
            "Allow",
            one::PermissionOptionKind::AllowOnce,
        )],
    )
    .meta(meta("r"));
    assert_eq!(
        translate_permission(request, &HashMap::new()),
        two::RequestPermissionRequest::new(
            SESSION,
            "ask_user",
            vec![two::PermissionOption::new(
                "allow_once",
                "Allow",
                two::PermissionOptionKind::AllowOnce,
            )],
        )
        .subject(two::RequestPermissionSubject::from(
            two::ToolCallUpdate::new("t1")
                .title("ask_user".to_owned())
                .kind(two::ToolKind::Other)
        ))
        .meta(meta("r"))
    );
    let untitled = one::RequestPermissionRequest::new(
        SESSION,
        one::ToolCallUpdate::new("t2", one::ToolCallUpdateFields::new()),
        Vec::new(),
    );
    assert_eq!(translate_permission(untitled, &HashMap::new()).title, "");
}

#[test]
fn a_copilot_title_names_partiturs_tools_only() {
    assert_eq!(
        tool_name("partitur-ask_user"),
        Some("mcp__partitur__ask_user".to_owned())
    );
    assert_eq!(tool_name("srv-ping"), None);
    assert_eq!(tool_name("Editing a.txt"), None);
    assert_eq!(tool_name("partitur"), None);
    assert_eq!(tool_name("partitur_ask_user"), None);
    assert_eq!(tool_name("partitur-"), None);
    assert_eq!(tool_name("partitur-notes for a.txt"), None);
    // A server of the user's named `partitur-dev`, with its tool `exec`.
    assert_eq!(tool_name("partitur-dev-exec"), None);
    assert_eq!(tool_name("partitur-Ask"), None);
    assert_eq!(
        tool_name("partitur-superpowers_v7_start_task"),
        Some("mcp__partitur__superpowers_v7_start_task".to_owned())
    );
    assert_eq!(
        tool_name("partitur-bmad_v6_12_start_workflow"),
        Some("mcp__partitur__bmad_v6_12_start_workflow".to_owned())
    );
}

#[test]
fn a_built_in_call_titled_like_partiturs_tool_is_not_named_or_flagged() {
    for kind in [one::ToolKind::Execute, one::ToolKind::Edit] {
        let announced = translate_update(notification(one::SessionUpdate::ToolCall(
            one::ToolCall::new("b1", "partitur-ask_user").kind(kind),
        )))
        .unwrap();
        assert!(
            matches!(
                &announced.update,
                two::SessionUpdate::ToolCallUpdate(call) if call.name.is_undefined()
            ),
            "{kind:?}"
        );
        let mut calls = HashMap::new();
        remember(&mut calls, &announced);
        let request = one::RequestPermissionRequest::new(
            SESSION,
            one::ToolCallUpdate::new("b1", one::ToolCallUpdateFields::new()),
            Vec::new(),
        );
        let asked = translate_permission(request, &calls);
        assert!(
            matches!(
                &asked.subject,
                Some(two::RequestPermissionSubject::ToolCall(subject))
                    if subject.tool_call.name.is_undefined()
            ),
            "{kind:?}"
        );
    }
}

#[test]
fn a_named_call_that_turns_into_a_built_in_lends_no_name() {
    let mut calls = HashMap::new();
    remember(
        &mut calls,
        &translate_update(notification(one::SessionUpdate::ToolCall(
            one::ToolCall::new("p1", "partitur-ask_user").kind(one::ToolKind::Other),
        )))
        .unwrap(),
    );
    let ask = |fields: one::ToolCallUpdateFields| {
        let request = one::RequestPermissionRequest::new(
            SESSION,
            one::ToolCallUpdate::new("p1", fields),
            Vec::new(),
        );
        translate_permission(request, &calls).subject
    };
    let named = |kind: two::ToolKind, name: Option<&str>| {
        let call = two::ToolCallUpdate::new("p1")
            .title("partitur-ask_user".to_owned())
            .kind(kind);
        Some(two::RequestPermissionSubject::from(match name {
            Some(name) => call.name(name.to_owned()),
            None => call,
        }))
    };
    assert_eq!(
        ask(one::ToolCallUpdateFields::new().kind(one::ToolKind::Execute)),
        named(two::ToolKind::Execute, None)
    );
    assert_eq!(
        ask(one::ToolCallUpdateFields::new()),
        named(two::ToolKind::Other, Some("mcp__partitur__ask_user"))
    );
}

#[test]
fn an_ended_call_is_forgotten() {
    for status in [one::ToolCallStatus::Completed, one::ToolCallStatus::Failed] {
        let mut calls = HashMap::new();
        for update in [
            one::SessionUpdate::ToolCall(one::ToolCall::new("p1", "partitur-ask_user")),
            one::SessionUpdate::ToolCallUpdate(one::ToolCallUpdate::new(
                "p1",
                one::ToolCallUpdateFields::new().status(status),
            )),
        ] {
            remember(&mut calls, &translate_update(notification(update)).unwrap());
        }
        assert_eq!(calls, HashMap::new(), "{status:?}");
    }
    // A call still running is kept.
    let mut calls = HashMap::new();
    remember(
        &mut calls,
        &translate_update(notification(one::SessionUpdate::ToolCallUpdate(
            one::ToolCallUpdate::new(
                "p1",
                one::ToolCallUpdateFields::new().status(one::ToolCallStatus::InProgress),
            ),
        )))
        .unwrap(),
    );
    assert_eq!(calls.len(), 1);
}

#[test]
fn a_tool_call_titled_after_partiturs_tool_is_named_after_it() {
    let call = |title: &str| {
        one::SessionUpdate::ToolCall(one::ToolCall::new("t1", title).kind(one::ToolKind::Other))
    };
    let named = |name: Option<&str>| {
        let call = two::ToolCallUpdate::new("t1")
            .kind(two::ToolKind::Other)
            .status(two::ToolCallStatus::Pending)
            .content(Vec::new())
            .locations(Vec::new());
        match name {
            Some(name) => call.name(name.to_owned()),
            None => call,
        }
    };
    assert_eq!(
        translated(call("partitur-propose_children")),
        two::SessionUpdate::ToolCallUpdate(
            named(Some("mcp__partitur__propose_children"))
                .title("partitur-propose_children".to_owned())
        )
    );
    assert_eq!(
        translated(call("srv-ping")),
        two::SessionUpdate::ToolCallUpdate(named(None).title("srv-ping".to_owned()))
    );
}

#[test]
fn a_permission_request_takes_what_it_leaves_out_from_the_call() {
    let mut calls = HashMap::new();
    let announced = translate_update(notification(one::SessionUpdate::ToolCall(
        one::ToolCall::new("p1", "partitur-propose_children").kind(one::ToolKind::Other),
    )))
    .unwrap();
    remember(&mut calls, &announced);
    // A later update changes the title and keeps the rest.
    let changed = translate_update(notification(one::SessionUpdate::ToolCallUpdate(
        one::ToolCallUpdate::new(
            "p1",
            one::ToolCallUpdateFields::new().title("Proposing children".to_owned()),
        ),
    )))
    .unwrap();
    remember(&mut calls, &changed);
    // Anything else records nothing.
    remember(
        &mut calls,
        &translate_update(notification(one::SessionUpdate::AgentMessageChunk(
            one::ContentChunk::new(text("hi")),
        )))
        .unwrap(),
    );
    assert_eq!(
        calls,
        HashMap::from([(
            "p1".to_owned(),
            KnownCall {
                name: Some("mcp__partitur__propose_children".to_owned()),
                title: Some("Proposing children".to_owned()),
                kind: Some(two::ToolKind::Other),
            }
        )])
    );
    let ask = |id: &'static str, fields: one::ToolCallUpdateFields| {
        let request = one::RequestPermissionRequest::new(
            SESSION,
            one::ToolCallUpdate::new(id, fields),
            Vec::new(),
        );
        translate_permission(request, &calls)
    };
    // Copilot's request about an MCP tool: titled with the bare tool name, nothing else.
    let titled = ask(
        "p1",
        one::ToolCallUpdateFields::new().title("propose_children".to_owned()),
    );
    assert_eq!(
        titled,
        two::RequestPermissionRequest::new(SESSION, "propose_children", Vec::new()).subject(
            two::RequestPermissionSubject::from(
                two::ToolCallUpdate::new("p1")
                    .title("propose_children".to_owned())
                    .name("mcp__partitur__propose_children".to_owned())
                    .kind(two::ToolKind::Other)
            )
        )
    );
    // A request that says nothing about the call takes its title too.
    let bare = ask("p1", one::ToolCallUpdateFields::new());
    assert_eq!(bare.title, "Proposing children");
    // What the request says wins.
    let named = ask(
        "p1",
        one::ToolCallUpdateFields::new()
            .name("own".to_owned())
            .kind(one::ToolKind::Edit),
    );
    assert_eq!(
        named.subject,
        Some(two::RequestPermissionSubject::from(
            two::ToolCallUpdate::new("p1")
                .title("Proposing children".to_owned())
                .name("own".to_owned())
                .kind(two::ToolKind::Edit)
        ))
    );
    // An unknown call keeps what the request says, nothing more.
    let unknown = ask("p2", one::ToolCallUpdateFields::new());
    assert_eq!(
        unknown.subject,
        Some(two::RequestPermissionSubject::from(
            two::ToolCallUpdate::new("p2")
        ))
    );
}

/// `wire` read as the v1 type `T`, then re-encoded as the v2 type `U`.
fn reencoded<T: DeserializeOwned + Serialize, U: DeserializeOwned>(wire: Value) -> Option<U> {
    let v1: T = serde_json::from_value(wire).expect("a v1 wire value");
    reencode(&v1)
}

#[test]
fn cost_reencodes_from_v1_wire_json() {
    let cost: two::Cost =
        reencoded::<one::Cost, _>(json!({"amount": 0.5, "currency": "USD"})).unwrap();
    assert_eq!(cost, two::Cost::new(0.5, "USD"));
}

#[test]
fn notice_severity_reencodes_from_v1_wire_json() {
    let severity: two::NoticeSeverity =
        reencoded::<one::NoticeSeverity, _>(json!("warning")).unwrap();
    assert_eq!(severity, two::NoticeSeverity::Warning);
}

#[test]
fn content_block_reencodes_from_v1_wire_json() {
    let block: two::ContentBlock =
        reencoded::<one::ContentBlock, _>(json!({"type": "text", "text": "hi"})).unwrap();
    assert_eq!(block, two::ContentBlock::from("hi"));
    let image: two::ContentBlock = reencoded::<one::ContentBlock, _>(
        json!({"type": "image", "data": "AAAA", "mimeType": "image/png"}),
    )
    .unwrap();
    let two::ContentBlock::Image(image) = image else {
        panic!("an image block");
    };
    assert_eq!(image.data, "AAAA");
    assert_eq!(image.mime_type, "image/png".into());
}

#[test]
fn prompt_content_block_reencodes_to_v1_wire_json() {
    let v1: one::ContentBlock =
        reencoded::<two::ContentBlock, _>(json!({"type": "text", "text": "hi"})).unwrap();
    assert_eq!(v1, one::ContentBlock::from("hi"));
}

#[test]
fn tool_call_content_reencodes_from_v1_wire_json() {
    let content: two::ToolCallContent = reencoded::<one::ToolCallContent, _>(
        json!({"type": "content", "content": {"type": "text", "text": "out"}}),
    )
    .unwrap();
    let two::ToolCallContent::Content(content) = content else {
        panic!("content");
    };
    assert_eq!(content.content, two::ContentBlock::from("out"));
    let terminal: two::ToolCallContent =
        reencoded::<one::ToolCallContent, _>(json!({"type": "terminal", "terminalId": "t1"}))
            .unwrap();
    let two::ToolCallContent::Terminal(terminal) = terminal else {
        panic!("a terminal");
    };
    assert_eq!(terminal.terminal_id.0.as_ref(), "t1");
}

#[test]
fn plan_entry_reencodes_from_v1_wire_json() {
    let entry: two::PlanEntry = reencoded::<one::PlanEntry, _>(
        json!({"content": "Write", "priority": "high", "status": "in_progress"}),
    )
    .unwrap();
    assert_eq!(entry.content, "Write");
    assert_eq!(entry.status, two::PlanEntryStatus::InProgress);
    assert_eq!(entry.priority, two::PlanEntryPriority::High);
}

#[test]
fn session_config_kind_reencodes_from_v1_wire_json() {
    let kind: two::SessionConfigKind =
        reencoded::<one::SessionConfigKind, _>(json!({"type": "boolean", "currentValue": true}))
            .unwrap();
    let two::SessionConfigKind::Boolean(boolean) = kind else {
        panic!("a boolean");
    };
    assert!(boolean.current_value);
}

#[test]
fn session_config_select_options_reencodes_from_v1_wire_json() {
    let options: two::SessionConfigSelectOptions = reencoded::<one::SessionConfigSelectOptions, _>(
        json!([{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(options).unwrap(),
        json!([{"value": "a", "name": "A"}, {"value": "b", "name": "B"}])
    );
}

#[test]
fn session_config_select_option_reencodes_from_v1_wire_json() {
    let option: two::SessionConfigSelectOption = reencoded::<one::SessionConfigSelectOption, _>(
        json!({"value": "a", "name": "A", "description": "first"}),
    )
    .unwrap();
    assert_eq!(option.name, "A");
    assert_eq!(option.description.as_deref(), Some("first"));
}

#[test]
fn session_config_option_category_reencodes_from_v1_wire_json() {
    let category: two::SessionConfigOptionCategory =
        reencoded::<one::SessionConfigOptionCategory, _>(json!("model")).unwrap();
    assert_eq!(category, two::SessionConfigOptionCategory::Model);
}

#[test]
fn permission_option_reencodes_from_v1_wire_json() {
    let option: two::PermissionOption = reencoded::<one::PermissionOption, _>(
        json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"}),
    )
    .unwrap();
    assert_eq!(option.name, "Allow");
    assert_eq!(option.kind, two::PermissionOptionKind::AllowOnce);
}
