#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::json;

use partitur_core::ToolRole;

use super::*;
use crate::agent::generic::Generic;
use crate::agent::{dropped, for_agent, restart_without};

const SESSION: &str = "cp-1";

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
fn the_opt_in_asks_for_copilots_session_events() {
    assert_eq!(
        serde_json::Value::Object(opt_in()),
        json!({"github.com/copilot": {
            "events": ["assistant.turn_start", "assistant.idle", "session.idle"]
        }})
    );
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
fn only_an_agent_named_copilot_gets_copilots_profile() {
    assert_eq!(for_agent(Some("Copilot")).label(), "copilot");
    assert_eq!(for_agent(Some("copilot")).label(), "generic");
    assert_eq!(for_agent(Some("claude-agent-acp")).label(), "generic");
    assert_eq!(for_agent(None).label(), "generic");
}

#[test]
fn copilots_initialize_carries_the_opt_in() {
    assert_eq!(Copilot.initialize_meta(), Some(opt_in()));
}

#[test]
fn instructions_dirs_append_to_a_configured_value() {
    let dir = Path::new("/p");
    assert_eq!(instructions_dirs(Some("/a,/b"), dir), "/a,/b,/p");
    assert_eq!(instructions_dirs(None, dir), "/p");
    assert_eq!(instructions_dirs(Some(""), dir), "/p");
}

#[test]
fn copilot_holds_its_prompts_and_restarts_its_orchestrator() {
    assert!(Copilot.holds_prompts());
    assert_eq!(
        Copilot.orchestrator_guard(),
        OrchestratorGuard::RestartWithout(&EXCLUDED_FOR_ORCHESTRATOR)
    );
}

#[test]
fn session_events_are_read_only_with_copilots_profile() {
    let idle = SessionEventNotification(json!({"sessionId": SESSION, "type": "session.idle"}));
    assert_eq!(
        Copilot.session_event(&idle.0),
        Some(two::UpdateSessionNotification::new(
            SESSION,
            two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
                two::IdleStateUpdate::new().background(Vec::new())
            )),
        ))
    );
    assert_eq!(Generic.session_event(&idle.0), None);
    let unreadable = SessionEventNotification(json!({"type": "session.idle"}));
    assert_eq!(Copilot.session_event(&unreadable.0), None);
}

#[test]
fn only_copilots_profile_drops_a_subagents_update() {
    let tagged = json!({"_meta": {"github.com/copilot": {"agentId": "sub-1"}}});
    assert!(dropped(&Copilot, &tagged));
    assert!(!dropped(&Generic, &tagged));
    assert!(!dropped(&Copilot, &json!({"_meta": {}})));
    assert!(!dropped(
        &Copilot,
        &json!({"_meta": {"github.com/copilot": {"agentId": null}}})
    ));
}

#[test]
fn only_copilots_profile_names_a_tool_by_its_title() {
    assert_eq!(
        Copilot.titled_tool_name("partitur-ask_user"),
        Some("mcp__partitur__ask_user".to_owned())
    );
    assert_eq!(Generic.titled_tool_name("partitur-ask_user"), None);
    assert_eq!(Copilot.titled_tool_name("srv-ping"), None);
}

fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|&arg| arg.to_owned()).collect()
}

#[test]
fn only_a_copilot_orchestrator_without_the_exclusion_is_restarted() {
    let plain = args(&["--acp"]);
    assert_eq!(
        restart_without(&Copilot, ToolRole::Orchestrator, &plain),
        Some(&EXCLUDED_FOR_ORCHESTRATOR[..])
    );
    let excluded = with_exclusion(&plain, &EXCLUDED_FOR_ORCHESTRATOR);
    assert_eq!(
        restart_without(&Copilot, ToolRole::Orchestrator, &excluded),
        None
    );
    assert_eq!(
        restart_without(&Generic, ToolRole::Orchestrator, &plain),
        None
    );
    assert_eq!(restart_without(&Copilot, ToolRole::Child, &plain), None);
    assert_eq!(restart_without(&Copilot, ToolRole::Plain, &plain), None);
}

#[test]
fn a_partial_exclusion_restarts_and_a_full_one_does_not() {
    let restarts =
        |list: &[&str]| restart_without(&Copilot, ToolRole::Orchestrator, &args(list)).is_some();
    assert!(restarts(&["--acp", "--excluded-tools=edit,create"]));
    // The space-separated form is not read.
    assert!(restarts(&[
        "--excluded-tools",
        "edit,create,apply_patch,bash,powershell"
    ]));
    // Every tool excluded, over two lists and among others, needs no restart.
    assert!(!restarts(&[
        "--excluded-tools=web_fetch,edit,create",
        "--excluded-tools=apply_patch,bash,powershell",
    ]));
}

#[test]
fn the_exclusion_names_copilots_editing_and_shell_tools() {
    assert_eq!(
        with_exclusion(&args(&["--acp"]), &EXCLUDED_FOR_ORCHESTRATOR),
        args(&[
            "--acp",
            "--excluded-tools=edit,create,apply_patch,bash,powershell"
        ])
    );
}

#[test]
fn a_configured_exclusion_list_gets_the_missing_tools() {
    let exclude = |list: &[&str]| with_exclusion(&args(list), &EXCLUDED_FOR_ORCHESTRATOR);
    assert_eq!(
        exclude(&["--acp", "--excluded-tools=web_fetch,bash"]),
        args(&[
            "--acp",
            "--excluded-tools=web_fetch,bash,edit,create,apply_patch,powershell"
        ])
    );
    // An empty list gets every tool, and every list gets them.
    assert_eq!(
        exclude(&["--excluded-tools=", "--excluded-tools=edit"]),
        args(&[
            "--excluded-tools=edit,create,apply_patch,bash,powershell",
            "--excluded-tools=edit,create,apply_patch,bash,powershell",
        ])
    );
    // The space-separated form is left alone, and the list added after it.
    assert_eq!(
        exclude(&["--excluded-tools", "web_fetch"]),
        args(&[
            "--excluded-tools",
            "web_fetch",
            "--excluded-tools=edit,create,apply_patch,bash,powershell"
        ])
    );
}
