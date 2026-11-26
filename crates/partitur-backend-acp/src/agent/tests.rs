#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use partitur_core::ToolRole;

use super::copilot::{Copilot, EXCLUDED_FOR_ORCHESTRATOR, INSTRUCTIONS_DIRS_VAR};
use super::generic::Generic;
use super::opencode::{CONFIG_CONTENT_VAR, config_content};
use super::*;
use crate::runtime::{AcpConfig, ThinkingDisplay};

#[test]
fn every_named_profile_is_chosen_by_its_name() {
    let named: Vec<_> = PROFILES
        .iter()
        .filter_map(|profile| Some((profile.agent_name()?, profile.label())))
        .collect();
    assert_eq!(
        named,
        [
            ("@agentclientprotocol/claude-agent-acp", "claude"),
            ("Copilot", "copilot"),
            ("OpenCode", "opencode")
        ]
    );
    for (name, label) in named {
        assert_eq!(for_agent(Some(name)).label(), label);
    }
}

#[test]
fn another_name_is_generic() {
    assert_eq!(for_agent(Some("Other")).label(), "generic");
    // A name is matched exactly, case included.
    assert_eq!(for_agent(Some("copilot")).label(), "generic");
    assert_eq!(for_agent(Some("claude-agent-acp")).label(), "generic");
}

#[test]
fn an_agent_without_info_is_generic() {
    assert_eq!(for_agent(None).label(), "generic");
}

#[test]
fn generic_has_no_ways() {
    let p = for_agent(None);
    assert_eq!(p.agent_name(), None);
    assert!(!p.holds_prompts());
    assert_eq!(p.asking(), Asking::ByConfigOptions);
    assert_eq!(p.titled_tool_name("partitur-ask_user"), None);
    assert!(p.subagent_marker().is_none());
    assert!(p.initialize_meta().is_none());
    let config = instructed();
    let start = ProcessStart {
        config: &config,
        role: ToolRole::Orchestrator,
        tools_url: "http://tools",
        inherited: &|_| None,
    };
    assert!(p.registers_tools_in_session());
    assert!(p.process_env(&start).is_empty());
    let all = vec![format!(
        "--excluded-tools={}",
        EXCLUDED_FOR_ORCHESTRATOR.join(",")
    )];
    assert_eq!(p.orchestrator_restart_args(&all), None);
    assert_eq!(p.orchestrator_restart_args(&[]), None);
    let idle = serde_json::json!({"sessionId": "s", "type": "session.idle"});
    assert_eq!(
        p.vendor_notification("github.com/copilot/sessionEvent", &idle),
        None
    );
}

#[test]
fn only_an_orchestrator_restarts_without_its_profiles_tools() {
    let all = vec![format!(
        "--excluded-tools={}",
        EXCLUDED_FOR_ORCHESTRATOR.join(",")
    )];
    assert_eq!(
        restart_args(&Copilot, ToolRole::Orchestrator, &[]),
        Some(all.clone())
    );
    assert_eq!(restart_args(&Copilot, ToolRole::Plain, &[]), None);
    assert_eq!(restart_args(&Copilot, ToolRole::Child, &[]), None);
    assert_eq!(restart_args(&Generic, ToolRole::Orchestrator, &[]), None);
    assert_eq!(restart_args(&Copilot, ToolRole::Orchestrator, &all), None);
}

#[test]
fn only_a_profile_with_a_subagent_marker_drops_an_update() {
    let update = serde_json::json!({"_meta": {"github.com/copilot": {"agentId": "sub-1"}}});
    assert!(dropped(&Copilot, &update));
    assert!(!dropped(&Generic, &update));
}

/// What a process for `config` is started with, with `inherited` as Partitur's own value of
/// every variable.
fn started_with(config: &AcpConfig, inherited: Option<&str>) -> Vec<(String, String)> {
    let inherited = |_: &str| inherited.map(str::to_owned);
    process_env(&ProcessStart {
        config,
        role: ToolRole::Plain,
        tools_url: "http://tools",
        inherited: &inherited,
    })
}

/// The last value `env` gives `name`, as the process would see it.
fn value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn instructed() -> AcpConfig {
    AcpConfig {
        env: vec![("AGENT_LOG".into(), "1".into())],
        thinking_display: ThinkingDisplay::Summarized,
        instructions_dir: Some(PathBuf::from("/data/agent-instructions")),
        ..AcpConfig::default()
    }
}

#[test]
fn the_process_gets_every_profiles_variables() {
    let env = started_with(&instructed(), None);
    assert_eq!(
        env,
        [
            ("AGENT_LOG".to_owned(), "1".to_owned()),
            (
                "CLAUDE_CODE_THINKING_DISPLAY".to_owned(),
                "summarized".to_owned()
            ),
            (
                INSTRUCTIONS_DIRS_VAR.to_owned(),
                "/data/agent-instructions".to_owned()
            ),
            (
                CONFIG_CONTENT_VAR.to_owned(),
                config_content(
                    None,
                    ToolRole::Plain,
                    Some(&Path::new("/data/agent-instructions").join("partitur.instructions.md")),
                    Some("http://tools")
                )
            ),
        ]
    );
}

#[test]
fn a_profiles_variable_wins_over_a_configured_one() {
    let mut config = instructed();
    config.thinking_display = ThinkingDisplay::Omitted;
    config
        .env
        .push(("CLAUDE_CODE_THINKING_DISPLAY".into(), "summarized".into()));
    let env = started_with(&config, None);
    assert_eq!(value(&env, "CLAUDE_CODE_THINKING_DISPLAY"), Some("omitted"));
}

#[test]
fn a_configured_instructions_folder_is_kept_first() {
    let mut config = instructed();
    config
        .env
        .push((INSTRUCTIONS_DIRS_VAR.into(), "/old".into()));
    config
        .env
        .push((INSTRUCTIONS_DIRS_VAR.into(), "/mine".into()));
    let env = started_with(&config, Some("/inherited"));
    assert_eq!(
        value(&env, INSTRUCTIONS_DIRS_VAR),
        Some("/mine,/data/agent-instructions")
    );
}

#[test]
fn an_inherited_instructions_folder_is_kept_first() {
    let env = started_with(&instructed(), Some("/inherited"));
    assert_eq!(
        value(&env, INSTRUCTIONS_DIRS_VAR),
        Some("/inherited,/data/agent-instructions")
    );
}

#[test]
fn without_a_folder_no_instructions_variable_is_set() {
    let config = AcpConfig {
        instructions_dir: None,
        ..instructed()
    };
    let env = started_with(&config, Some("/inherited"));
    assert_eq!(value(&env, INSTRUCTIONS_DIRS_VAR), None);
}

#[test]
fn initialize_carries_copilots_opt_in() {
    assert_eq!(initialize_meta(), Some(crate::agent::copilot::opt_in()));
}

#[test]
fn without_any_profiles_meta_initialize_carries_none() {
    assert_eq!(union_of_meta(&[&Generic, &Claude]), None);
}

#[test]
fn a_tool_of_partiturs_server_gets_its_mcp_name() {
    assert_eq!(
        partitur_tool_name("partitur-ask_user"),
        Some("mcp__partitur__ask_user".to_owned())
    );
    assert_eq!(
        partitur_tool_name("partitur-v7_x9"),
        Some("mcp__partitur__v7_x9".to_owned())
    );
    for other in [
        "partitur_ask_user",
        "partitur-",
        "other-ask_user",
        "partitur-Ask",
        "partitur-a-b",
    ] {
        assert_eq!(partitur_tool_name(other), None, "{other}");
    }
}

#[test]
fn a_variable_is_configured_by_the_last_entry_else_inherited() {
    let config = AcpConfig {
        env: vec![("V".into(), "a".into()), ("V".into(), "b".into())],
        ..AcpConfig::default()
    };
    let inherited = |name: &str| (name == "V" || name == "W").then(|| "i".to_owned());
    let start = ProcessStart {
        config: &config,
        role: ToolRole::Plain,
        tools_url: "http://tools",
        inherited: &inherited,
    };
    assert_eq!(start.configured("V").as_deref(), Some("b"));
    assert_eq!(start.configured("W").as_deref(), Some("i"));
    assert_eq!(start.configured("X"), None);
}
