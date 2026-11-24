#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema::{
    AgentMessage, AgentThought, BackgroundSubagent, BackgroundTerminal, BackgroundToolCall,
    ContentChunk, Diff, DiffChange, DiffChangeOperation, ErrorStopReason, IdleStateUpdate,
    ImageContent as SchemaImage, Notice, NoticeSeverity, OtherDiffChange, PermissionOptionKind,
    RequiresActionStateUpdate, RunningStateUpdate, SessionConfigSelectGroup, SessionInfoUpdate,
    StateUpdate, Terminal, TerminalExitStatus, TerminalOutput, TerminalOutputChunk, TerminalUpdate,
    ToolCallLocation, ToolCallUpdate, UsageUpdate, UserMessage,
};

use partitur_core::SessionFailure;

use super::*;

/// Maps `update` with a fresh session state.
fn map(update: SessionUpdate) -> Option<AgentUpdate> {
    map_update(update, &mut SessionState::default())
}

fn text_chunk(text: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)), "m-1")
}

fn text_content(text: &str) -> ToolCallContent {
    ToolCallContent::from(ContentBlock::Text(TextContent::new(text)))
}

fn state(update: StateUpdate) -> SessionUpdate {
    SessionUpdate::StateUpdate(update)
}

fn running() -> SessionUpdate {
    state(StateUpdate::Running(RunningStateUpdate::new()))
}

fn idle(reason: Option<schema::StopReason>) -> SessionUpdate {
    state(StateUpdate::Idle(
        IdleStateUpdate::new().stop_reason(reason),
    ))
}

/// A subagent item of a background list.
fn subagent(id: &str) -> BackgroundWork {
    BackgroundWork::Subagent(BackgroundSubagent::new(id))
}

/// A backgrounded command's terminal item of a background list.
fn command(id: &str) -> BackgroundWork {
    BackgroundWork::Terminal(BackgroundTerminal::new(id))
}

/// An idle state without a stop reason that lists `background`.
fn idle_listing(background: Option<Vec<BackgroundWork>>) -> StateUpdate {
    StateUpdate::Idle(IdleStateUpdate::new().background(background))
}

fn listed(state: &SessionState) -> Vec<String> {
    state.subagents().map(ToString::to_string).collect()
}

#[test]
fn a_listed_subagent_is_reported_once() {
    let mut session = SessionState::default();
    let listing = idle_listing(Some(vec![subagent("sub-1")]));
    assert_eq!(
        session.background(&listing),
        Some(AgentUpdate::RunningBackground(1))
    );
    assert_eq!(session.background(&listing), None);
    assert_eq!(listed(&session), ["sub-1"]);
}

#[test]
fn every_listed_item_counts() {
    let mut session = SessionState::default();
    let listing = StateUpdate::Running(RunningStateUpdate::new().background(vec![
        command("t-1"),
        subagent("sub-1"),
        BackgroundWork::ToolCall(BackgroundToolCall::new("call-1")),
        subagent("sub-2"),
    ]));
    assert_eq!(
        session.background(&listing),
        Some(AgentUpdate::RunningBackground(4))
    );
    assert_eq!(listed(&session), ["sub-1", "sub-2"]);
}

#[test]
fn commands_alone_count() {
    let mut session = SessionState::default();
    let listing = idle_listing(Some(vec![command("t-1")]));
    assert_eq!(
        session.background(&listing),
        Some(AgentUpdate::RunningBackground(1))
    );
    assert!(listed(&session).is_empty());
}

#[test]
fn an_empty_list_reports_zero_after_subagents() {
    let mut session = SessionState::default();
    session.background(&idle_listing(Some(vec![subagent("sub-1")])));
    assert_eq!(
        session.background(&idle_listing(Some(Vec::new()))),
        Some(AgentUpdate::RunningBackground(0))
    );
    assert!(listed(&session).is_empty());
}

#[test]
fn a_missing_list_changes_nothing() {
    let mut session = SessionState::default();
    session.background(&idle_listing(Some(vec![subagent("sub-1")])));
    assert_eq!(session.background(&idle_listing(None)), None);
    assert_eq!(listed(&session), ["sub-1"]);
}

#[test]
fn nothing_listed_at_first_reports_nothing() {
    let mut session = SessionState::default();
    assert_eq!(session.background(&idle_listing(Some(Vec::new()))), None);
}

#[test]
fn a_changed_list_of_the_same_length_reports_nothing_but_is_kept() {
    let mut session = SessionState::default();
    session.background(&idle_listing(Some(vec![subagent("sub-1")])));
    assert_eq!(
        session.background(&idle_listing(Some(vec![command("t-1")]))),
        None
    );
    assert!(listed(&session).is_empty());
}

/// A tool call update for `id` that changes only `content`.
fn content_update(id: &str, content: Vec<ToolContent>) -> AgentUpdate {
    AgentUpdate::ToolCallUpdate {
        id: ToolCallId::new(id),
        title: None,
        kind: None,
        status: None,
        content: Some(content),
        raw_output: None,
        raw_input: None,
        call_result: None,
        widget: None,
    }
}

#[test]
fn agent_message_text_maps_to_agent_text() {
    assert_eq!(
        map(SessionUpdate::AgentMessageChunk(text_chunk("hello"))),
        Some(AgentUpdate::AgentText("hello".into()))
    );
    let image = ContentChunk::new(
        ContentBlock::Image(SchemaImage::new("aGk=", "image/png")),
        "m-1",
    );
    assert_eq!(
        map(SessionUpdate::AgentMessageChunk(image)),
        Some(AgentUpdate::AgentText("[non-text content]".into()))
    );
}

#[test]
fn a_thought_chunk_maps_to_agent_thought() {
    assert_eq!(
        map(SessionUpdate::AgentThoughtChunk(text_chunk("hmm"))),
        Some(AgentUpdate::AgentThought("hmm".into()))
    );
}

#[test]
fn first_tool_call_update_is_a_tool_call() {
    let mut session = SessionState::default();
    let first = ToolCallUpdate::new("t-1")
        .title("Edit main.rs")
        .kind(schema::ToolKind::Edit)
        .status(schema::ToolCallStatus::InProgress)
        .content(vec![text_content("out")])
        .locations(vec![ToolCallLocation::new("/src/main.rs")]);
    assert_eq!(
        map_update(SessionUpdate::ToolCallUpdate(first), &mut session),
        Some(AgentUpdate::ToolCall {
            id: ToolCallId::new("t-1"),
            title: "Edit main.rs".into(),
            kind: ToolKind::Edit,
            status: ToolStatus::InProgress,
            content: vec![ToolContent::Text("out".into())],
            locations: vec![PathBuf::from("/src/main.rs")],
            raw_input: None,
            call_result: None,
            widget: None,
        })
    );

    let cleared = ToolCallUpdate::new("t-1")
        .title(MaybeUndefined::Null)
        .kind(MaybeUndefined::Null)
        .status(MaybeUndefined::Null)
        .content(MaybeUndefined::Null);
    assert_eq!(
        map_update(SessionUpdate::ToolCallUpdate(cleared), &mut session),
        Some(AgentUpdate::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: Some(String::new()),
            kind: Some(ToolKind::Other),
            status: Some(ToolStatus::Pending),
            content: Some(Vec::new()),
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        })
    );

    let sparse = ToolCallUpdate::new("t-1").status(schema::ToolCallStatus::Completed);
    assert_eq!(
        map_update(SessionUpdate::ToolCallUpdate(sparse), &mut session),
        Some(AgentUpdate::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: None,
            kind: None,
            status: Some(ToolStatus::Completed),
            content: None,
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        })
    );
}

#[test]
fn a_first_update_without_fields_starts_at_the_defaults() {
    for update in [
        ToolCallUpdate::new("t-1"),
        ToolCallUpdate::new("t-1")
            .title(MaybeUndefined::Null)
            .kind(MaybeUndefined::Null)
            .status(MaybeUndefined::Null)
            .content(MaybeUndefined::Null)
            .locations(MaybeUndefined::Null),
    ] {
        assert_eq!(
            map(SessionUpdate::ToolCallUpdate(update)),
            Some(AgentUpdate::ToolCall {
                id: ToolCallId::new("t-1"),
                title: String::new(),
                kind: ToolKind::Other,
                status: ToolStatus::Pending,
                content: Vec::new(),
                locations: Vec::new(),
                raw_input: None,
                call_result: None,
                widget: None,
            })
        );
    }
}

/// The tool list of the patched adapter with the Jira issue widget's tool, beside a tool of
/// a server whose name Claude Code changes in tool names.
fn listed_tools() -> serde_json::Value {
    serde_json::json!({"tools": [
        {
            "server": "atlassian",
            "tool": "getJiraIssue",
            "resourceUri": "ui://jira/issue",
            "visibility": ["model", "app"],
        },
        {"server": "claude.ai Todoist", "tool": "find-tasks", "resourceUri": "ui://todo"},
    ]})
}

/// A session that knows the tools of [`listed_tools`].
fn widget_session() -> SessionState {
    let mut session = SessionState::default();
    session.set_widget_tools(widget_tools(&listed_tools()));
    session
}

/// The first update of the tool call `id`, named `tool` in the adapter's `_meta`.
fn named_in_meta(id: &str, tool: &str) -> SessionUpdate {
    let meta = serde_json::json!({"claudeCode": {"toolName": tool}});
    SessionUpdate::ToolCallUpdate(
        ToolCallUpdate::new(id)
            .title("getJiraIssue")
            .meta(meta.as_object().cloned().unwrap()),
    )
}

/// Whether `update` announces a tool call carrying `expected` as its widget.
fn announces(update: Option<AgentUpdate>, expected: Option<&WidgetSlot>) -> bool {
    matches!(update, Some(AgentUpdate::ToolCall { widget, .. }) if widget.as_ref() == expected)
}

/// The Jira issue widget: read when `read`, else being read.
fn jira(read: bool) -> WidgetSlot {
    if read {
        WidgetSlot::Ready(Widget {
            server: "atlassian".into(),
            uri: "ui://jira/issue".into(),
            html: "<p>issue</p>".into(),
            meta: r#"{"ui":{"prefersBorder":true}}"#.into(),
        })
    } else {
        WidgetSlot::Loading {
            server: "atlassian".into(),
            uri: "ui://jira/issue".into(),
        }
    }
}

/// The change of the tool call `id` to `widget` alone.
fn widget_changed(id: &str, widget: WidgetSlot) -> AgentUpdate {
    AgentUpdate::ToolCallUpdate {
        id: ToolCallId::new(id),
        title: None,
        kind: None,
        status: None,
        content: None,
        raw_output: None,
        raw_input: None,
        call_result: None,
        widget: Some(widget),
    }
}

#[test]
fn tool_name_from_meta_matches_widget_tool() {
    let mut session = widget_session();
    let call = named_in_meta("w1", "mcp__atlassian__getJiraIssue");
    assert!(announces(
        map_update(call, &mut session),
        Some(&jira(false))
    ));
    assert_eq!(
        session.take_reads(),
        [("atlassian".to_owned(), "ui://jira/issue".to_owned())]
    );
    assert!(session.take_reads().is_empty());

    // A second call while the widget is read waits for the same read.
    let second = named_in_meta("w2", "mcp__atlassian__getJiraIssue");
    assert!(announces(
        map_update(second, &mut session),
        Some(&jira(false))
    ));
    assert!(session.take_reads().is_empty());

    let reply = serde_json::json!({
        "mimeType": "text/html;profile=mcp-app",
        "html": "<p>issue</p>",
        "meta": {"ui": {"prefersBorder": true}},
    });
    assert_eq!(
        session.widget_read("atlassian", "ui://jira/issue", Ok(reply)),
        [
            widget_changed("w1", jira(true)),
            widget_changed("w2", jira(true))
        ]
    );
    // The read widget is kept: a later call shows it at once.
    let third = named_in_meta("w3", "mcp__atlassian__getJiraIssue");
    assert!(announces(
        map_update(third, &mut session),
        Some(&jira(true))
    ));
    assert!(session.take_reads().is_empty());
    // The read was answered: it ends no call twice.
    assert!(
        session
            .widget_read("atlassian", "ui://jira/issue", Err("late".into()))
            .is_empty()
    );
}

#[test]
fn the_tool_name_is_also_read_from_the_calls_name() {
    let mut session = widget_session();
    let named = ToolCallUpdate::new("w1").name("mcp__claude_ai_Todoist__find-tasks");
    assert!(announces(
        map_update(SessionUpdate::ToolCallUpdate(named), &mut session),
        Some(&WidgetSlot::Loading {
            server: "claude.ai Todoist".into(),
            uri: "ui://todo".into(),
        })
    ));
    // A tool without a widget, or one of a server with another name, has none.
    for tool in [
        "mcp__atlassian__search",
        "mcp__claude.ai Todoist__find-tasks",
    ] {
        let call = named_in_meta(tool, tool);
        assert!(announces(map_update(call, &mut session), None));
    }
    let unnamed = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-9"));
    assert!(announces(map_update(unnamed, &mut session), None));
    assert_eq!(
        session.take_reads(),
        [("claude.ai Todoist".to_owned(), "ui://todo".to_owned())]
    );
}

#[test]
fn a_failed_read_fails_every_waiting_call_and_is_tried_again() {
    let mut session = widget_session();
    for id in ["w1", "w2"] {
        map_update(
            named_in_meta(id, "mcp__atlassian__getJiraIssue"),
            &mut session,
        );
    }
    session.take_reads();
    let failed = WidgetSlot::Failed {
        server: "atlassian".into(),
        uri: "ui://jira/issue".into(),
        error: "Server not found: atlassian".into(),
    };
    assert_eq!(
        session.widget_read(
            "atlassian",
            "ui://jira/issue",
            Err("Server not found: atlassian".into())
        ),
        [
            widget_changed("w1", failed.clone()),
            widget_changed("w2", failed)
        ]
    );
    map_update(
        named_in_meta("w3", "mcp__atlassian__getJiraIssue"),
        &mut session,
    );
    assert_eq!(
        session.take_reads(),
        [("atlassian".to_owned(), "ui://jira/issue".to_owned())]
    );
    // A reply without HTML is no widget.
    let no_html = serde_json::json!({"mimeType": "text/html"});
    assert_eq!(
        session.widget_read("atlassian", "ui://jira/issue", Ok(no_html)),
        [widget_changed(
            "w3",
            WidgetSlot::Failed {
                server: "atlassian".into(),
                uri: "ui://jira/issue".into(),
                error: "the widget resource has no HTML".into(),
            }
        )]
    );
}

// Two servers may both name a widget `ui://shared/main.html`: each server's calls get
// that server's widget, never the other's.
#[test]
fn two_servers_with_one_uri_keep_their_own_widgets() {
    let mut session = SessionState::default();
    session.set_widget_tools(widget_tools(&serde_json::json!({"tools": [
        {"server": "one", "tool": "show", "resourceUri": "ui://shared/main.html"},
        {"server": "two", "tool": "show", "resourceUri": "ui://shared/main.html"},
    ]})));
    let of = |server: &str, html: Option<&str>| match html {
        Some(html) => WidgetSlot::Ready(Widget {
            server: server.into(),
            uri: "ui://shared/main.html".into(),
            html: html.into(),
            meta: "{}".into(),
        }),
        None => WidgetSlot::Loading {
            server: server.into(),
            uri: "ui://shared/main.html".into(),
        },
    };
    assert!(announces(
        map_update(named_in_meta("a1", "mcp__one__show"), &mut session),
        Some(&of("one", None))
    ));
    assert!(announces(
        map_update(named_in_meta("b1", "mcp__two__show"), &mut session),
        Some(&of("two", None))
    ));
    // Each server's widget is read on its own.
    assert_eq!(
        session.take_reads(),
        [
            ("one".to_owned(), "ui://shared/main.html".to_owned()),
            ("two".to_owned(), "ui://shared/main.html".to_owned())
        ]
    );
    let reply = |html: &str| Ok(serde_json::json!({"html": html}));
    assert_eq!(
        session.widget_read("one", "ui://shared/main.html", reply("<p>one</p>")),
        [widget_changed("a1", of("one", Some("<p>one</p>")))]
    );
    // The first server's widget is not the second's.
    assert!(announces(
        map_update(named_in_meta("b2", "mcp__two__show"), &mut session),
        Some(&of("two", None))
    ));
    assert!(session.take_reads().is_empty(), "the read is under way");
    assert_eq!(
        session.widget_read("two", "ui://shared/main.html", reply("<p>two</p>")),
        [
            widget_changed("b1", of("two", Some("<p>two</p>"))),
            widget_changed("b2", of("two", Some("<p>two</p>")))
        ]
    );
    assert!(announces(
        map_update(named_in_meta("a2", "mcp__one__show"), &mut session),
        Some(&of("one", Some("<p>one</p>")))
    ));
    assert!(announces(
        map_update(named_in_meta("b3", "mcp__two__show"), &mut session),
        Some(&of("two", Some("<p>two</p>")))
    ));
}

#[test]
fn a_widget_without_meta_gets_an_empty_one() {
    let mut session = widget_session();
    map_update(
        named_in_meta("w1", "mcp__atlassian__getJiraIssue"),
        &mut session,
    );
    let reply = serde_json::json!({"html": "<p>issue</p>"});
    let changed = session.widget_read("atlassian", "ui://jira/issue", Ok(reply));
    assert!(matches!(
        &changed[..],
        [AgentUpdate::ToolCallUpdate {
            widget: Some(WidgetSlot::Ready(Widget { meta, .. })),
            ..
        }] if meta == "{}"
    ));
}

#[test]
fn the_tool_list_keeps_only_complete_tools() {
    let reply = serde_json::json!({"tools": [
        {"server": "a", "tool": "t", "resourceUri": "ui://a"},
        {"server": "my-mcp_1.x", "tool": "t", "resourceUri": "ui://m"},
        {"server": "a", "tool": "no-uri"},
        {"server": "a", "resourceUri": "ui://no-tool"},
        {"tool": "no-server", "resourceUri": "ui://b"},
    ]});
    assert_eq!(
        widget_tools(&reply),
        HashMap::from([
            (
                "mcp__a__t".to_owned(),
                ("a".to_owned(), "ui://a".to_owned())
            ),
            (
                "mcp__my-mcp_1_x__t".to_owned(),
                ("my-mcp_1.x".to_owned(), "ui://m".to_owned())
            ),
        ])
    );
    assert!(widget_tools(&serde_json::json!({})).is_empty());
    // A new list replaces the one known before.
    let mut session = widget_session();
    session.set_widget_tools(widget_tools(&reply));
    let call = named_in_meta("w1", "mcp__atlassian__getJiraIssue");
    assert!(announces(map_update(call, &mut session), None));
}

#[test]
fn a_missed_mcp_tool_asks_for_one_list_and_gets_its_widget_from_it() {
    let mut session = SessionState::default();
    let call = |id: &str, tool: &str, session: &mut SessionState| {
        let call = ToolCallUpdate::new(id).name(tool);
        map_update(SessionUpdate::ToolCallUpdate(call), session)
    };
    assert!(announces(
        call("w1", "mcp__atlassian__getJiraIssue", &mut session),
        None
    ));
    assert!(session.take_list_wanted());
    assert!(!session.take_list_wanted());
    // While that list is under way, another missed tool waits for it, and the same tool
    // does not wait twice.
    call("x1", "mcp__other__lookup", &mut session);
    call("w2", "mcp__atlassian__getJiraIssue", &mut session);
    call("b1", "Bash", &mut session);
    assert!(!session.take_list_wanted());
    assert_eq!(
        session.set_widget_tools(widget_tools(&listed_tools())),
        [widget_changed("w1", jira(false))]
    );
    assert_eq!(
        session.take_reads(),
        [("atlassian".to_owned(), "ui://jira/issue".to_owned())]
    );
    // A tool still unknown after the list asks for none again; a new one does.
    call("x2", "mcp__other__lookup", &mut session);
    assert!(!session.take_list_wanted());
    call("n1", "mcp__new__tool", &mut session);
    assert!(session.take_list_wanted());
}

#[test]
fn a_list_under_way_keeps_a_missed_tool_from_asking_for_another() {
    let mut session = SessionState::default();
    session.listing();
    let call = ToolCallUpdate::new("w1").name("mcp__atlassian__getJiraIssue");
    map_update(SessionUpdate::ToolCallUpdate(call), &mut session);
    assert!(!session.take_list_wanted());
    assert_eq!(
        session.set_widget_tools(widget_tools(&listed_tools())),
        [widget_changed("w1", jira(false))]
    );
    // The list ended: a later miss asks again.
    let call = ToolCallUpdate::new("n1").name("mcp__new__tool");
    map_update(SessionUpdate::ToolCallUpdate(call), &mut session);
    assert!(session.take_list_wanted());
}

#[test]
fn call_tool_result_from_meta_is_kept() {
    let mut session = SessionState::default();
    let meta = |value: serde_json::Value| value.as_object().cloned().unwrap();
    let first = ToolCallUpdate::new("w1")
        .raw_input(serde_json::json!({"issueKey": "PROJ-1"}))
        .meta(meta(
            serde_json::json!({"partitur": {"callToolResult": {"isError": true}}}),
        ));
    assert!(matches!(
        map_update(SessionUpdate::ToolCallUpdate(first), &mut session),
        Some(AgentUpdate::ToolCall { raw_input: Some(input), call_result: Some(result), .. })
            if input == r#"{"issueKey":"PROJ-1"}"# && result == r#"{"isError":true}"#
    ));
    let later = ToolCallUpdate::new("w1")
        .raw_input(serde_json::json!({}))
        .meta(meta(
            serde_json::json!({"partitur": {"callToolResult": {"content": []}}}),
        ));
    assert_eq!(
        map_update(SessionUpdate::ToolCallUpdate(later), &mut session),
        Some(AgentUpdate::ToolCallUpdate {
            id: ToolCallId::new("w1"),
            title: None,
            kind: None,
            status: None,
            content: None,
            raw_output: None,
            raw_input: Some("{}".into()),
            call_result: Some(r#"{"content":[]}"#.into()),
            widget: None,
        })
    );
    let input_only = ToolCallUpdate::new("w1").raw_input(serde_json::json!(1));
    assert!(matches!(
        map_update(SessionUpdate::ToolCallUpdate(input_only), &mut session),
        Some(AgentUpdate::ToolCallUpdate { raw_input: Some(input), call_result: None, .. })
            if input == "1"
    ));
    let result_only = ToolCallUpdate::new("w1")
        .meta(meta(serde_json::json!({"partitur": {"callToolResult": 2}})));
    assert!(matches!(
        map_update(SessionUpdate::ToolCallUpdate(result_only), &mut session),
        Some(AgentUpdate::ToolCallUpdate { raw_input: None, call_result: Some(result), .. })
            if result == "2"
    ));
    // Other meta changes nothing Partitur shows.
    let other = ToolCallUpdate::new("w1").meta(meta(serde_json::json!({"claudeCode": {}})));
    assert_eq!(
        map_update(SessionUpdate::ToolCallUpdate(other), &mut session),
        None
    );
}

#[test]
fn tool_calls_are_told_apart_by_id() {
    let mut session = SessionState::default();
    let first = |id: &str| SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id).title("x"));
    assert!(matches!(
        map_update(first("t-1"), &mut session),
        Some(AgentUpdate::ToolCall { .. })
    ));
    assert!(matches!(
        map_update(first("t-2"), &mut session),
        Some(AgentUpdate::ToolCall { .. })
    ));
    assert!(matches!(
        map_update(first("t-1"), &mut session),
        Some(AgentUpdate::ToolCallUpdate { .. })
    ));
}

#[test]
fn every_tool_kind_and_status_maps() {
    let kinds = [
        (schema::ToolKind::Read, ToolKind::Read),
        (schema::ToolKind::Edit, ToolKind::Edit),
        (schema::ToolKind::Delete, ToolKind::Delete),
        (schema::ToolKind::Move, ToolKind::Move),
        (schema::ToolKind::Search, ToolKind::Search),
        (schema::ToolKind::Execute, ToolKind::Execute),
        (schema::ToolKind::Think, ToolKind::Think),
        (schema::ToolKind::Fetch, ToolKind::Fetch),
        (schema::ToolKind::SwitchMode, ToolKind::SwitchMode),
        (schema::ToolKind::Other, ToolKind::Other),
        (schema::ToolKind::Unknown("_x".into()), ToolKind::Other),
    ];
    for (schema_kind, kind) in kinds {
        assert_eq!(map_tool_kind(&schema_kind), kind);
    }
    let statuses = [
        (schema::ToolCallStatus::Pending, ToolStatus::Pending),
        (schema::ToolCallStatus::InProgress, ToolStatus::InProgress),
        (schema::ToolCallStatus::Paused, ToolStatus::InProgress),
        (schema::ToolCallStatus::Completed, ToolStatus::Completed),
        (schema::ToolCallStatus::Failed, ToolStatus::Failed),
        (schema::ToolCallStatus::Cancelled, ToolStatus::Failed),
        (
            schema::ToolCallStatus::Other("_x".into()),
            ToolStatus::Pending,
        ),
    ];
    for (schema_status, status) in statuses {
        assert_eq!(map_tool_status(&schema_status), status);
    }
}

#[test]
fn a_tool_call_update_that_changes_nothing_shown_is_dropped() {
    let mut session = SessionState::default();
    map_update(
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
        &mut session,
    );
    for update in [
        ToolCallUpdate::new("t-1"),
        ToolCallUpdate::new("t-1")
            .locations(vec![ToolCallLocation::new("/a.rs")])
            .raw_input(MaybeUndefined::Null)
            .raw_output(MaybeUndefined::Null),
    ] {
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(update), &mut session),
            None
        );
    }
}

#[test]
fn a_tool_call_update_with_any_shown_field_is_forwarded() {
    let mut session = SessionState::default();
    map_update(
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
        &mut session,
    );
    let mut forwarded =
        |update: ToolCallUpdate| map_update(SessionUpdate::ToolCallUpdate(update), &mut session);
    let update = |title: Option<&str>, kind, content: Option<Vec<ToolContent>>| {
        Some(AgentUpdate::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: title.map(str::to_owned),
            kind,
            status: None,
            content,
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        })
    };
    assert_eq!(
        forwarded(ToolCallUpdate::new("t-1").title("Run")),
        update(Some("Run"), None, None)
    );
    assert_eq!(
        forwarded(ToolCallUpdate::new("t-1").kind(schema::ToolKind::Read)),
        update(None, Some(ToolKind::Read), None)
    );
    assert_eq!(
        forwarded(ToolCallUpdate::new("t-1").content(Vec::new())),
        update(None, None, Some(Vec::new()))
    );
}

#[test]
fn a_raw_output_is_mapped_next_to_the_content_never_into_it() {
    let mut session = SessionState::default();
    map_update(
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
        &mut session,
    );
    let mut output =
        |update: ToolCallUpdate| map_update(SessionUpdate::ToolCallUpdate(update), &mut session);
    let update = |content: Option<Vec<ToolContent>>, raw_output: &str| {
        Some(AgentUpdate::ToolCallUpdate {
            id: ToolCallId::new("t-1"),
            title: None,
            kind: None,
            status: None,
            content,
            raw_output: Some(raw_output.into()),
            raw_input: None,
            call_result: None,
            widget: None,
        })
    };
    assert_eq!(
        output(ToolCallUpdate::new("t-1").raw_output(serde_json::json!("total 0\n"))),
        update(None, "total 0\n")
    );
    assert_eq!(
        output(
            ToolCallUpdate::new("t-1")
                .content(Vec::new())
                .raw_output(serde_json::json!({"exit": 0}))
        ),
        update(Some(Vec::new()), "{\n  \"exit\": 0\n}")
    );
    assert_eq!(
        output(
            ToolCallUpdate::new("t-1")
                .content(vec![text_content("shown")])
                .raw_output(serde_json::json!("hidden"))
        ),
        update(Some(vec![ToolContent::Text("shown".into())]), "hidden")
    );

    let call = |id: &str, content: Vec<ToolContent>| {
        Some(AgentUpdate::ToolCall {
            id: ToolCallId::new(id),
            title: "List".into(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            content,
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        })
    };
    let listed = ToolCallUpdate::new("t-2")
        .title("List")
        .raw_output(serde_json::json!(["a", "b"]));
    assert_eq!(
        map(SessionUpdate::ToolCallUpdate(listed)),
        call(
            "t-2",
            vec![ToolContent::Text("[\n  \"a\",\n  \"b\"\n]".into())]
        )
    );
    let shown = ToolCallUpdate::new("t-4")
        .title("List")
        .content(vec![text_content("shown")])
        .raw_output(serde_json::json!("hidden"));
    assert_eq!(
        map(SessionUpdate::ToolCallUpdate(shown)),
        call("t-4", vec![ToolContent::Text("shown".into())])
    );
}

#[test]
fn diff_maps_to_patch() {
    let patched = Diff::patch("@@ -1 +1 @@\n-a\n+b\n", vec![DiffChange::modify("/a.rs")]);
    assert_eq!(
        map_content(ToolCallContent::Diff(patched)),
        ToolContent::Patch {
            paths: vec![PathBuf::from("/a.rs")],
            text: "@@ -1 +1 @@\n-a\n+b\n".into(),
        }
    );
    let unpatched = Diff::new(vec![
        DiffChange::add("/new.rs"),
        DiffChange::delete("/old.rs"),
        DiffChange::move_file("/from.rs", "/to.rs"),
        DiffChange::copy("/src.rs", "/copy.rs"),
        DiffChange::new(DiffChangeOperation::Other(OtherDiffChange::new(
            "_rewrite",
            BTreeMap::default(),
        ))),
    ]);
    assert_eq!(
        map_content(ToolCallContent::Diff(unpatched)),
        ToolContent::Patch {
            paths: ["/new.rs", "/old.rs", "/to.rs", "/copy.rs"]
                .map(PathBuf::from)
                .to_vec(),
            text: String::new(),
        }
    );
}

/// Maps one piece of tool content with a fresh session state.
fn map_content(content: ToolCallContent) -> ToolContent {
    SessionState::default().call_content(&ToolCallId::new("t-1"), content)
}

#[test]
fn non_text_tool_content_becomes_a_placeholder() {
    let image = ToolCallContent::from(ContentBlock::Image(SchemaImage::new("aGk=", "image/png")));
    assert_eq!(
        map_content(image),
        ToolContent::Text("[non-text content]".into())
    );
    let other = ToolCallContent::Other(schema::OtherToolCallContent::new(
        "_widget",
        BTreeMap::default(),
    ));
    assert_eq!(
        map_content(other),
        ToolContent::Text("[non-text content]".into())
    );
}

fn terminal(id: &str) -> TerminalUpdate {
    TerminalUpdate::new(id)
}

fn output(id: &str, base64: &str) -> SessionUpdate {
    SessionUpdate::TerminalUpdate(terminal(id).output(TerminalOutput::new(base64)))
}

fn names_terminal(call: &str, terminal: &str) -> SessionUpdate {
    SessionUpdate::ToolCallUpdate(
        ToolCallUpdate::new(call).content(vec![ToolCallContent::Terminal(Terminal::new(terminal))]),
    )
}

#[test]
fn terminal_output_fills_its_tool_call() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    // The terminal is announced before any call names it.
    assert_eq!(map(SessionUpdate::TerminalUpdate(terminal("term-1"))), None);
    assert_eq!(
        map(names_terminal("t-1", "term-1")),
        Some(AgentUpdate::ToolCall {
            id: ToolCallId::new("t-1"),
            title: String::new(),
            kind: ToolKind::Other,
            status: ToolStatus::Pending,
            content: vec![ToolContent::Terminal(String::new())],
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        })
    );
    assert_eq!(
        map(SessionUpdate::TerminalUpdate(
            terminal("term-1").command("ls")
        )),
        None
    );
    assert_eq!(
        map(output("term-1", "bm90ZXMudHh0Cg==")),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("notes.txt\n".into())]
        ))
    );
    assert_eq!(
        map(SessionUpdate::TerminalUpdate(
            terminal("term-1").exit_status(TerminalExitStatus::new().exit_code(0))
        )),
        None
    );
}

#[test]
fn terminal_output_snapshots_replace_and_chunks_append() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    map(names_terminal("t-1", "term-1"));
    assert_eq!(
        map(output("term-1", "YQ==")),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("a".into())]
        ))
    );
    let chunk = TerminalOutputChunk::new("term-1", "Yg==");
    assert_eq!(
        map(SessionUpdate::TerminalOutputChunk(chunk)),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("ab".into())]
        ))
    );
    assert_eq!(
        map(output("term-1", "Yw==")),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("c".into())]
        ))
    );
    assert_eq!(
        map(SessionUpdate::TerminalUpdate(
            terminal("term-1").output(MaybeUndefined::Null)
        )),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal(String::new())]
        ))
    );
    // A payload that is not base64 changes nothing.
    assert_eq!(map(output("term-1", "not base64!")), None);
    let broken = TerminalOutputChunk::new("term-1", "not base64!");
    assert_eq!(map(SessionUpdate::TerminalOutputChunk(broken)), None);
}

#[test]
fn terminal_output_waits_for_a_call_naming_its_terminal() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    assert_eq!(map(output("term-1", "aGk=")), None);
    let chunk = TerminalOutputChunk::new("term-1", "IQ==");
    assert_eq!(map(SessionUpdate::TerminalOutputChunk(chunk)), None);
    assert!(map(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1"))).is_some());
    assert_eq!(
        map(names_terminal("t-1", "term-1")),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("hi!".into())]
        ))
    );
}

#[test]
fn multi_byte_output_split_across_chunks_decodes_whole() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    map(names_terminal("t-1", "term-1"));
    // "é" is 0xC3 0xA9; each chunk carries one of its bytes.
    map(SessionUpdate::TerminalOutputChunk(
        TerminalOutputChunk::new("term-1", "ww=="),
    ));
    assert_eq!(
        map(SessionUpdate::TerminalOutputChunk(
            TerminalOutputChunk::new("term-1", "qQ==",)
        )),
        Some(content_update(
            "t-1",
            vec![ToolContent::Terminal("é".into())]
        ))
    );
}

#[test]
fn idle_with_stop_reason_after_running_ends_turn() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    assert_eq!(map(running()), Some(AgentUpdate::TurnStarted));
    assert_eq!(map(running()), None);
    assert_eq!(
        map(state(StateUpdate::RequiresAction(
            RequiresActionStateUpdate::new()
        ))),
        None
    );
    assert_eq!(
        map(idle(Some(schema::StopReason::EndTurn))),
        Some(AgentUpdate::TurnEnded(StopReason::EndTurn))
    );
    // A turn that starts blocked on the user is a turn too.
    assert_eq!(
        map(state(StateUpdate::RequiresAction(
            RequiresActionStateUpdate::new()
        ))),
        Some(AgentUpdate::TurnStarted)
    );
}

#[test]
fn idle_error_fails_turn() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    map(running());
    let error = ErrorStopReason::new().error(schema::Error::new(-32000, "sign in"));
    assert_eq!(
        map(idle(Some(schema::StopReason::Error(error)))),
        Some(AgentUpdate::Failed("sign in".into()))
    );
    map(running());
    assert_eq!(
        map(idle(Some(
            schema::StopReason::Error(ErrorStopReason::new())
        ))),
        Some(AgentUpdate::Failed(
            "the agent stopped with an error".into()
        ))
    );
}

#[test]
fn idle_without_stop_reason_ends_nothing() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    map(running());
    assert_eq!(map(idle(None)), None);
    // The turn is still open: another `running` starts nothing new.
    assert_eq!(map(running()), None);
    assert_eq!(
        map(idle(Some(schema::StopReason::EndTurn))),
        Some(AgentUpdate::TurnEnded(StopReason::EndTurn))
    );
    assert_eq!(map(idle(Some(schema::StopReason::EndTurn))), None);
    assert_eq!(map(idle(None)), None);
    assert_eq!(map(running()), Some(AgentUpdate::TurnStarted));
}

#[test]
fn a_refused_prompt_without_work_ends_its_turn_once() {
    let mut session = SessionState::default();
    assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
    session.prompted();
    // A snapshot `idle` before any work does not end the prompt's turn.
    assert_eq!(
        map_update(idle(Some(schema::StopReason::EndTurn)), &mut session),
        None
    );
    assert_eq!(
        session.refused(Ok(StopReason::Cancelled)),
        Some(AgentUpdate::TurnEnded(StopReason::Cancelled))
    );
    assert_eq!(
        session.refused(Err(SessionFailure::RequestFailed("again".into()))),
        None
    );
}

#[test]
fn a_refused_prompt_during_work_leaves_the_end_to_its_idle() {
    let mut session = SessionState::default();
    session.prompted();
    assert_eq!(
        map_update(running(), &mut session),
        Some(AgentUpdate::TurnStarted)
    );
    assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
    assert_eq!(
        map_update(idle(Some(schema::StopReason::EndTurn)), &mut session),
        Some(AgentUpdate::TurnEnded(StopReason::EndTurn))
    );
    assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
    // The refusal is spent: a later idle without a stop reason ends nothing.
    assert_eq!(
        map_update(running(), &mut session),
        Some(AgentUpdate::TurnStarted)
    );
    assert_eq!(map_update(idle(None), &mut session), None);
}

#[test]
fn a_refusal_ends_work_that_goes_idle_without_a_stop_reason() {
    let mut session = SessionState::default();
    map_update(running(), &mut session);
    assert_eq!(
        session.refused(Err(SessionFailure::AuthRequired("sign in".into()))),
        None
    );
    assert_eq!(
        map_update(idle(None), &mut session),
        Some(AgentUpdate::Failed("sign in".into()))
    );
    assert_eq!(
        session.refused(Err(SessionFailure::AuthRequired("sign in".into()))),
        None
    );
    assert_eq!(
        map_update(running(), &mut session),
        Some(AgentUpdate::TurnStarted)
    );
}

#[test]
fn a_refusal_after_the_work_went_idle_ends_nothing() {
    let mut session = SessionState::default();
    session.prompted();
    map_update(running(), &mut session);
    assert_eq!(
        map_update(idle(Some(schema::StopReason::Cancelled)), &mut session),
        Some(AgentUpdate::TurnEnded(StopReason::Cancelled))
    );
    assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
}

#[test]
fn an_unknown_state_changes_nothing() {
    let mut session = SessionState::default();
    let mut map = |update| map_update(update, &mut session);
    let other: StateUpdate =
        serde_json::from_value(serde_json::json!({"state": "_paused"})).unwrap();
    assert_eq!(map(state(other.clone())), None);
    map(running());
    assert_eq!(map(state(other)), None);
    assert_eq!(
        map(idle(Some(schema::StopReason::Cancelled))),
        Some(AgentUpdate::TurnEnded(StopReason::Cancelled))
    );
}

#[test]
fn every_stop_reason_maps() {
    let reasons = [
        (schema::StopReason::EndTurn, StopReason::EndTurn),
        (schema::StopReason::MaxTokens, StopReason::MaxTokens),
        (
            schema::StopReason::MaxTurnRequests,
            StopReason::MaxTurnRequests,
        ),
        (schema::StopReason::Refusal, StopReason::Refusal),
        (schema::StopReason::Cancelled, StopReason::Cancelled),
        (
            schema::StopReason::Other(schema::OtherStopReason::new("_paused", BTreeMap::default())),
            StopReason::EndTurn,
        ),
    ];
    for (schema_reason, reason) in reasons {
        assert_eq!(map_stop_reason(schema_reason), Ok(reason));
    }
    let error = ErrorStopReason::new().error(schema::Error::new(-32000, "sign in"));
    assert_eq!(
        map_stop_reason(schema::StopReason::Error(error)),
        Err("sign in".to_owned())
    );
}

#[test]
fn notice_keeps_title_and_description() {
    assert_eq!(
        map(SessionUpdate::Notice(Notice::new(
            NoticeSeverity::Info,
            "Heads up"
        ))),
        Some(AgentUpdate::Notice("Heads up".into()))
    );
    assert_eq!(
        map(SessionUpdate::Notice(
            Notice::new(NoticeSeverity::Warning, "Heads up").description("Be careful")
        )),
        Some(AgentUpdate::Notice("Heads up: Be careful".into()))
    );
}

#[test]
fn usage_and_config_updates_map() {
    assert_eq!(
        map(SessionUpdate::UsageUpdate(UsageUpdate::new(10, 200))),
        Some(AgentUpdate::Usage {
            used: 10,
            size: 200
        })
    );
    let option = SessionConfigOption::select(
        "model",
        "Model",
        "a",
        vec![SessionConfigSelectOption::new("a", "A")],
    );
    let mapped = map(SessionUpdate::ConfigOptionUpdate(
        schema::ConfigOptionUpdate::new(vec![option.clone()]),
    ));
    assert_eq!(
        mapped,
        Some(AgentUpdate::ConfigOptions(map_config_options(&[option])))
    );
    assert!(matches!(mapped, Some(AgentUpdate::ConfigOptions(options)) if options.len() == 1));
}

#[test]
fn a_session_title_maps_to_a_title_update() {
    let info = SessionInfoUpdate::new().title("  Worktree support ");

    assert_eq!(
        map(SessionUpdate::SessionInfoUpdate(info)),
        Some(AgentUpdate::Title("Worktree support".into()))
    );
}

#[test]
fn session_info_without_a_title_maps_to_none() {
    for info in [
        SessionInfoUpdate::new(),
        SessionInfoUpdate::new().title("   "),
    ] {
        assert_eq!(map(SessionUpdate::SessionInfoUpdate(info)), None);
    }
}

#[test]
fn ignored_variants_map_to_none() {
    let content = vec![ContentBlock::from("whole")];
    for update in [
        SessionUpdate::UserMessageChunk(text_chunk("echo")),
        SessionUpdate::UserMessage(UserMessage::new("m-1").content(content.clone())),
        SessionUpdate::AgentMessage(AgentMessage::new("m-2").content(content.clone())),
        SessionUpdate::AgentThought(AgentThought::new("m-3").content(content)),
    ] {
        assert_eq!(map(update), None);
    }
}

fn select(
    id: &str,
    category: Option<SessionConfigOptionCategory>,
    current: &str,
    values: &[&str],
) -> SessionConfigOption {
    SessionConfigOption::select(
        id.to_owned(),
        id.to_uppercase(),
        current.to_owned(),
        values
            .iter()
            .map(|value| SessionConfigSelectOption::new(value.to_string(), value.to_uppercase()))
            .collect::<Vec<_>>(),
    )
    .category(category)
}

fn category_of(option: &ConfigOption) -> ConfigCategory {
    option.category.clone()
}

#[test]
fn config_categories_map_model_thought_level_and_mode() {
    let options = vec![
        select(
            "model",
            Some(SessionConfigOptionCategory::Model),
            "m1",
            &["m1", "m2"],
        ),
        select(
            "effort",
            Some(SessionConfigOptionCategory::ThoughtLevel),
            "low",
            &["low"],
        ),
        select(
            "mode",
            Some(SessionConfigOptionCategory::Mode),
            "default",
            &["default"],
        ),
        select("agent", None, "default", &["default", "web-fetch"]),
        select(
            "tuning",
            Some(SessionConfigOptionCategory::ModelConfig),
            "x",
            &["x"],
        ),
        select(
            "custom",
            Some(SessionConfigOptionCategory::Other("_vendor".into())),
            "x",
            &["x"],
        ),
        SessionConfigOption::boolean("flag", "Flag", true),
    ];
    let mapped = map_config_options(&options);
    assert_eq!(
        mapped.iter().map(category_of).collect::<Vec<_>>(),
        [
            ConfigCategory::Model,
            ConfigCategory::Effort,
            ConfigCategory::Mode,
            ConfigCategory::Other("AGENT".into()),
            ConfigCategory::Other("model_config".into()),
            ConfigCategory::Other("_vendor".into()),
        ]
    );
    let model = &mapped[0];
    assert_eq!(model.id, ConfigId::new("model"));
    assert_eq!(model.name, "MODEL");
    assert_eq!(model.current, ConfigValueId::new("m1"));
    assert_eq!(
        model.choices,
        [
            ConfigChoice {
                id: ConfigValueId::new("m1"),
                name: "M1".into()
            },
            ConfigChoice {
                id: ConfigValueId::new("m2"),
                name: "M2".into()
            },
        ]
    );
}

#[test]
fn grouped_select_options_are_flattened() {
    let groups = vec![
        SessionConfigSelectGroup::new(
            "g1",
            "First",
            vec![SessionConfigSelectOption::new("a", "A")],
        ),
        SessionConfigSelectGroup::new(
            "g2",
            "Second",
            vec![
                SessionConfigSelectOption::new("b", "B"),
                SessionConfigSelectOption::new("c", "C"),
            ],
        ),
    ];
    let option = SessionConfigOption::select("model", "Model", "b", groups);
    let mapped = map_config_options(&[option]);
    let ids: Vec<_> = mapped[0]
        .choices
        .iter()
        .map(|choice| choice.id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[test]
fn permission_options_keep_id_name_and_kind() {
    let kinds = [
        (PermissionOptionKind::AllowOnce, PermissionKind::AllowOnce),
        (
            PermissionOptionKind::AllowAlways,
            PermissionKind::AllowAlways,
        ),
        (PermissionOptionKind::RejectOnce, PermissionKind::RejectOnce),
        (
            PermissionOptionKind::RejectAlways,
            PermissionKind::RejectAlways,
        ),
        (
            PermissionOptionKind::Other("_maybe".into()),
            PermissionKind::RejectOnce,
        ),
    ];
    let options: Vec<_> = kinds
        .iter()
        .enumerate()
        .map(|(index, (kind, _))| {
            schema::PermissionOption::new(
                format!("opt-{index}"),
                format!("Option {index}"),
                kind.clone(),
            )
        })
        .collect();
    let mapped = map_permission_options(&options);
    assert_eq!(mapped.len(), kinds.len());
    for (index, (option, (_, expected))) in mapped.iter().zip(kinds).enumerate() {
        assert_eq!(option.id, PermissionOptionId::new(format!("opt-{index}")));
        assert_eq!(option.name, format!("Option {index}"));
        assert_eq!(option.kind, expected);
    }
}

#[test]
fn prompt_content_maps_text_and_image() {
    let mapped = map_prompt(vec![
        PromptContent::Text("hi".into()),
        PromptContent::Image {
            mime: "image/png".into(),
            base64: "aGk=".into(),
        },
    ]);
    assert_eq!(
        mapped,
        vec![
            ContentBlock::Text(TextContent::new("hi")),
            ContentBlock::Image(SchemaImage::new("aGk=", "image/png")),
        ]
    );
}

fn file_link(path: &str) -> ContentBlock {
    map_prompt(vec![PromptContent::FileLink {
        path: PathBuf::from(path),
    }])
    .remove(0)
}

#[cfg(unix)] // The paths are Unix absolute paths; `prompt_content_maps_windows_file_links` is the Windows twin.
#[test]
fn prompt_content_maps_file_links() {
    assert_eq!(
        file_link("/work/src/main.rs"),
        ContentBlock::ResourceLink(ResourceLink::new("main.rs", "file:///work/src/main.rs"))
    );
    assert_eq!(
        file_link("/"),
        ContentBlock::ResourceLink(ResourceLink::new("/", "file:///"))
    );
}

#[cfg(windows)]
#[test]
fn prompt_content_maps_windows_file_links() {
    assert_eq!(
        file_link(r"C:\work\src\main.rs"),
        ContentBlock::ResourceLink(ResourceLink::new("main.rs", "file:///C:/work/src/main.rs"))
    );
}

#[cfg(unix)] // A Unix path with a space; Windows paths are covered by the next test.
#[test]
fn a_file_link_with_a_space_is_percent_encoded() {
    assert_eq!(
        file_link("/work/my file.rs"),
        ContentBlock::ResourceLink(ResourceLink::new("my file.rs", "file:///work/my%20file.rs"))
    );
}

#[cfg(windows)]
#[test]
fn a_windows_file_link_is_a_three_slash_uri() {
    assert_eq!(
        file_link(r"C:\work\a.rs"),
        ContentBlock::ResourceLink(ResourceLink::new("a.rs", "file:///C:/work/a.rs"))
    );
}

#[test]
fn a_relative_file_link_keeps_the_path_as_written() {
    assert_eq!(
        file_link("src/a.rs"),
        ContentBlock::ResourceLink(ResourceLink::new("a.rs", "file://src/a.rs"))
    );
}
