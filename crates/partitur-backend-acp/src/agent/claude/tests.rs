#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::json;

use super::*;
use crate::agent::for_agent;
use crate::agent::generic::Generic;

/// A tool call update `w1`, named `name` if given, with `meta` as its `_meta`.
fn call(name: Option<&str>, meta: &serde_json::Value) -> two::SessionUpdate {
    let call = two::ToolCallUpdate::new("w1").meta(meta.as_object().cloned().unwrap());
    let call = match name {
        Some(name) => call.name(name),
        None => call,
    };
    two::SessionUpdate::ToolCallUpdate(call)
}

/// The name of the tool call `update` is.
fn name_of(update: &two::SessionUpdate) -> Option<String> {
    let two::SessionUpdate::ToolCallUpdate(call) = update else {
        panic!("not a tool call: {update:?}");
    };
    call.name.value().cloned()
}

/// A notice carrying `meta` as its `_meta`.
fn notice(meta: Option<serde_json::Value>) -> two::SessionUpdate {
    let notice = two::Notice::new(two::NoticeSeverity::Info, "MCP server x connected");
    two::SessionUpdate::Notice(notice.meta(meta.and_then(|meta| meta.as_object().cloned())))
}

#[test]
fn claude_is_chosen_by_its_adapters_name() {
    assert_eq!(for_agent(Some(CLAUDE_ADAPTER_NAME)).label(), "claude");
}

#[test]
fn claude_appends_the_tool_instructions() {
    let meta = Claude.session_meta().unwrap();
    assert_eq!(
        meta["systemPrompt"],
        json!({"append": partitur_core::tool_instructions()})
    );
    assert_eq!(meta.len(), 1);
}

#[test]
fn generic_sends_no_session_meta() {
    assert!(Generic.session_meta().is_none());
}

#[test]
fn the_meta_tool_name_becomes_the_name() {
    let update = call(None, &json!({"claudeCode": {"toolName": "Bash"}}));
    assert_eq!(name_of(&Claude.adapt_update(update)), Some("Bash".into()));
}

#[test]
fn a_name_wins_over_the_meta_tool_name() {
    let update = call(Some("Read"), &json!({"claudeCode": {"toolName": "Bash"}}));
    assert_eq!(name_of(&Claude.adapt_update(update)), Some("Read".into()));
}

#[test]
fn a_meta_without_a_tool_name_names_nothing() {
    for meta in [
        json!({}),
        json!({"claudeCode": {}}),
        json!({"claudeCode": {"toolName": 1}}),
        json!({"other": {"toolName": "Bash"}}),
    ] {
        assert_eq!(name_of(&Claude.adapt_update(call(None, &meta))), None);
    }
    let bare = two::SessionUpdate::ToolCallUpdate(two::ToolCallUpdate::new("w1"));
    assert_eq!(name_of(&Claude.adapt_update(bare)), None);
}

#[test]
fn other_updates_pass_unchanged() {
    let update = notice(Some(json!({"claudeCode": {"toolName": "Bash"}})));
    assert_eq!(Claude.adapt_update(update.clone()), update);
}

#[test]
fn generic_reads_no_meta_tool_name() {
    let update = call(None, &json!({"claudeCode": {"toolName": "Bash"}}));
    assert_eq!(name_of(&Generic.adapt_update(update)), None);
}

#[test]
fn claude_reports_a_server_status_notice() {
    let status = || notice(Some(json!({"claudeCode": {"kind": "mcpServerStatus"}})));
    assert!(Claude.reports_server_status(&status()));
    assert!(!Generic.reports_server_status(&status()));
}

#[test]
fn only_a_server_status_notice_reports_a_server_status() {
    let other = json!({"claudeCode": {"kind": "other"}});
    assert!(!Claude.reports_server_status(&notice(Some(other))));
    assert!(!Claude.reports_server_status(&notice(Some(json!({})))));
    assert!(!Claude.reports_server_status(&notice(None)));
    let status = json!({"claudeCode": {"kind": "mcpServerStatus"}});
    assert!(!Claude.reports_server_status(&call(None, &status)));
}
