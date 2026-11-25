#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use partitur_core::ToolRole;

use super::copilot::{Copilot, EXCLUDED_FOR_ORCHESTRATOR};
use super::generic::Generic;
use super::*;

#[test]
fn copilot_is_chosen_by_its_name() {
    assert_eq!(for_agent(Some("Copilot")).label(), "copilot");
}

#[test]
fn another_name_is_generic() {
    assert_eq!(for_agent(Some("Other")).label(), "generic");
}

#[test]
fn an_agent_without_info_is_generic() {
    assert_eq!(for_agent(None).label(), "generic");
}

#[test]
fn generic_has_no_ways() {
    let p = for_agent(None);
    assert!(!p.holds_prompts());
    assert_eq!(p.orchestrator_guard(), OrchestratorGuard::DenyRequests);
    assert_eq!(p.asking(), Asking::ByConfigOptions);
    assert_eq!(p.titled_tool_name("partitur-ask_user"), None);
    assert!(p.subagent_marker().is_none());
}

#[test]
fn an_orchestrators_copilot_restarts_without_its_tools() {
    assert_eq!(
        restart_without(&Copilot, ToolRole::Orchestrator, &[]),
        Some(&EXCLUDED_FOR_ORCHESTRATOR[..])
    );
    assert_eq!(restart_without(&Copilot, ToolRole::Plain, &[]), None);
    assert_eq!(restart_without(&Generic, ToolRole::Orchestrator, &[]), None);
    let all = vec![format!(
        "--excluded-tools={}",
        EXCLUDED_FOR_ORCHESTRATOR.join(",")
    )];
    assert_eq!(
        restart_without(&Copilot, ToolRole::Orchestrator, &all),
        None
    );
}

#[test]
fn only_copilot_drops_a_subagents_update() {
    let update = serde_json::json!({"_meta": {"github.com/copilot": {"agentId": "sub-1"}}});
    assert!(dropped(&Copilot, &update));
    assert!(!dropped(&Generic, &update));
}
