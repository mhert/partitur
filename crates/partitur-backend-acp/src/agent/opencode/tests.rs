#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::agent::generic::Generic;
use crate::agent::{dropped, for_agent, restart_args};
use crate::runtime::AcpConfig;

fn parsed(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// A session info update with `title`, if given, and `meta` as its `_meta`.
fn info(title: Option<&str>, meta: &Value) -> two::SessionUpdate {
    let info = two::SessionInfoUpdate::new().meta(meta.as_object().cloned().unwrap());
    let info = match title {
        Some(title) => info.title(title),
        None => info,
    };
    two::SessionUpdate::SessionInfoUpdate(info)
}

/// A tool call update `t1` named `name`.
fn call(name: &str) -> two::SessionUpdate {
    two::SessionUpdate::ToolCallUpdate(two::ToolCallUpdate::new("t1").name(name).title(name))
}

/// The name of the tool call `update` is.
fn name_of(update: &two::SessionUpdate) -> Option<String> {
    let two::SessionUpdate::ToolCallUpdate(call) = update else {
        panic!("not a tool call: {update:?}");
    };
    call.name.value().cloned()
}

#[test]
fn opencode_is_chosen_by_its_name() {
    assert_eq!(OPENCODE_NAME, "OpenCode");
    assert_eq!(for_agent(Some("OpenCode")).label(), "opencode");
}

#[test]
fn every_acting_permission_asks() {
    let v = parsed(&config_content(None, ToolRole::Plain, None, None));
    for key in PERMISSION_KEYS {
        assert_eq!(v["permission"][key], "ask");
    }
    assert_eq!(
        PERMISSION_KEYS,
        [
            "edit",
            "bash",
            "webfetch",
            "websearch",
            "external_directory",
            "task"
        ]
    );
}

#[test]
fn an_orchestrator_may_not_edit_or_run_commands() {
    let v = parsed(&config_content(None, ToolRole::Orchestrator, None, None));
    assert_eq!(v["permission"]["edit"], "deny");
    assert_eq!(v["permission"]["bash"], "deny");
    assert_eq!(v["permission"]["task"], "deny");
    assert_eq!(v["permission"]["webfetch"], "ask");
    assert_eq!(v["permission"]["websearch"], "ask");
    assert_eq!(v["permission"]["external_directory"], "ask");
}

#[test]
fn a_child_asks_as_a_plain_session_does() {
    let v = parsed(&config_content(None, ToolRole::Child, None, None));
    for key in PERMISSION_KEYS {
        assert_eq!(v["permission"][key], "ask");
    }
}

#[test]
fn a_plain_session_asks_before_a_subagent_starts() {
    assert_eq!(
        parsed(&config_content(None, ToolRole::Plain, None, None))["permission"]["task"],
        "ask"
    );
}

#[test]
fn opencodes_own_question_tool_is_denied() {
    for role in [ToolRole::Plain, ToolRole::Orchestrator] {
        assert_eq!(
            parsed(&config_content(None, role, None, None))["permission"]["question"],
            "deny"
        );
    }
}

#[test]
fn the_instructions_file_is_named() {
    let f = Path::new("/data/agent-instructions/partitur.instructions.md");
    let v = parsed(&config_content(None, ToolRole::Plain, Some(f), None));
    assert_eq!(v["instructions"], json!([f]));
}

#[test]
fn partiturs_tool_server_is_registered_without_code_mode() {
    let v = parsed(&config_content(
        None,
        ToolRole::Plain,
        None,
        Some("http://127.0.0.1:4000/plain"),
    ));
    assert_eq!(
        v["mcp"]["partitur"],
        json!({"type": "remote", "url": "http://127.0.0.1:4000/plain", "codemode": false})
    );
}

#[test]
fn without_a_tools_url_no_server_is_registered() {
    let v = parsed(&config_content(None, ToolRole::Plain, None, None));
    assert!(v.get("mcp").is_none());
}

#[test]
fn a_configured_server_is_kept_beside_partiturs() {
    let mine = r#"{"mcp":{"other":{"type":"local"}}}"#;
    let v = parsed(&config_content(
        Some(mine),
        ToolRole::Plain,
        None,
        Some("http://t"),
    ));
    assert_eq!(v["mcp"]["other"], json!({"type": "local"}));
    assert_eq!(v["mcp"]["partitur"]["url"], "http://t");
}

#[test]
fn opencode_gets_no_tool_server_in_its_session_requests() {
    assert!(!OpenCode.registers_tools_in_session());
    assert!(Generic.registers_tools_in_session());
}

#[test]
fn without_an_instructions_folder_no_instructions_are_named() {
    assert!(
        parsed(&config_content(None, ToolRole::Plain, None, None))
            .get("instructions")
            .is_none()
    );
}

#[test]
fn partiturs_permissions_win_over_a_configured_value() {
    let mine = r#"{"model":"strata/strata","permission":{"bash":"allow","read":"allow"},"instructions":["/mine.md"]}"#;
    let v = parsed(&config_content(
        Some(mine),
        ToolRole::Plain,
        Some(Path::new("/p.md")),
        None,
    ));
    assert_eq!(v["model"], "strata/strata");
    assert_eq!(v["permission"]["bash"], "ask");
    assert_eq!(v["permission"]["read"], "allow");
    assert_eq!(v["instructions"], json!(["/mine.md", "/p.md"]));
}

#[test]
fn configured_parts_that_are_no_objects_or_lists_are_replaced() {
    let mine = r#"{"permission":"allow","instructions":"/mine.md","mcp":1}"#;
    let v = parsed(&config_content(
        Some(mine),
        ToolRole::Plain,
        Some(Path::new("/p.md")),
        Some("http://t"),
    ));
    assert_eq!(v["permission"]["bash"], "ask");
    assert_eq!(v["instructions"], json!(["/p.md"]));
    assert_eq!(v["mcp"]["partitur"]["url"], "http://t");
}

#[test]
fn a_configured_value_that_is_no_object_is_replaced() {
    for mine in ["[1]", "not json"] {
        let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
        assert_eq!(v["permission"]["bash"], "ask");
        assert!(v.get("0").is_none());
    }
}

#[test]
fn opencode_is_kept_asking_at_start() {
    assert_eq!(OpenCode.asking(), Asking::EnsuredAtStart);
    assert_eq!(restart_args(&OpenCode, ToolRole::Orchestrator, &[]), None);
}

/// What the process of a session at `tools_url` in `role` on `config` gets from opencode's
/// profile, with `inherited` as Partitur's own value of every variable.
fn process_env_of(
    config: &AcpConfig,
    role: ToolRole,
    inherited: Option<&str>,
) -> Vec<(String, String)> {
    let inherited = |_: &str| inherited.map(str::to_owned);
    OpenCode.process_env(&ProcessStart {
        config,
        role,
        tools_url: "http://tools/s",
        inherited: &inherited,
    })
}

#[test]
fn the_process_gets_the_config_content() {
    let config = AcpConfig {
        instructions_dir: Some(PathBuf::from("/data/agent-instructions")),
        ..AcpConfig::default()
    };
    let file = Path::new("/data/agent-instructions/partitur.instructions.md");
    assert_eq!(
        process_env_of(&config, ToolRole::Orchestrator, None),
        [(
            CONFIG_CONTENT_VAR.to_owned(),
            config_content(
                None,
                ToolRole::Orchestrator,
                Some(file),
                Some("http://tools/s")
            )
        )]
    );
    assert_eq!(CONFIG_CONTENT_VAR, "OPENCODE_CONFIG_CONTENT");
}

#[test]
fn the_configured_content_wins_over_the_inherited_one() {
    let config = AcpConfig {
        env: vec![
            (
                CONFIG_CONTENT_VAR.to_owned(),
                r#"{"model":"old"}"#.to_owned(),
            ),
            (
                CONFIG_CONTENT_VAR.to_owned(),
                r#"{"model":"mine"}"#.to_owned(),
            ),
        ],
        ..AcpConfig::default()
    };
    let env = process_env_of(&config, ToolRole::Plain, Some(r#"{"model":"inherited"}"#));
    assert_eq!(parsed(&env[0].1)["model"], "mine");
    assert!(parsed(&env[0].1).get("instructions").is_none());
    let env = process_env_of(
        &AcpConfig::default(),
        ToolRole::Plain,
        Some(r#"{"model":"i"}"#),
    );
    assert_eq!(parsed(&env[0].1)["model"], "i");
}

#[test]
fn a_retry_becomes_a_notice() {
    let meta = json!({"opencode/retry": {
        "attempt": 3,
        "nextRetryAt": "2026-11-25T10:00:00Z",
        "error": {"type": "provider.transport", "message": "ECONNREFUSED"},
    }});
    assert_eq!(
        OpenCode.adapt_update(info(None, &meta)),
        two::SessionUpdate::Notice(
            two::Notice::new(
                two::NoticeSeverity::Warning,
                "The model is unreachable, retrying (attempt 3)"
            )
            .description("ECONNREFUSED".to_owned())
        )
    );
}

#[test]
fn a_retry_without_its_details_is_still_a_notice() {
    let meta = json!({"opencode/retry": {}});
    assert_eq!(
        OpenCode.adapt_update(info(None, &meta)),
        two::SessionUpdate::Notice(two::Notice::new(
            two::NoticeSeverity::Warning,
            "The model is unreachable, retrying"
        ))
    );
}

#[test]
fn a_cleared_retry_shows_nothing_and_keeps_the_title() {
    let update = info(Some("Fix the build"), &json!({"opencode/retry": null}));
    assert_eq!(OpenCode.adapt_update(update.clone()), update);
    let update = info(Some("Fix the build"), &json!({}));
    assert_eq!(OpenCode.adapt_update(update.clone()), update);
    let update = two::SessionUpdate::SessionInfoUpdate(two::SessionInfoUpdate::new());
    assert_eq!(OpenCode.adapt_update(update.clone()), update);
}

#[test]
fn partiturs_tools_get_their_mcp_name() {
    assert_eq!(
        name_of(&OpenCode.adapt_update(call("partitur_ask_user"))),
        Some("mcp__partitur__ask_user".to_owned())
    );
    assert_eq!(
        name_of(&OpenCode.adapt_update(call("partitur_superpowers_v7_start_task"))),
        Some("mcp__partitur__superpowers_v7_start_task".to_owned())
    );
    for other in [
        "other_ask_user",
        "partitur_dev-exec",
        "partitur_",
        "partitur",
        "shell",
    ] {
        assert_eq!(
            name_of(&OpenCode.adapt_update(call(other))),
            Some(other.to_owned())
        );
    }
    // A call without a name, and an update that is no tool call, pass unchanged.
    let unnamed = two::SessionUpdate::ToolCallUpdate(two::ToolCallUpdate::new("t1"));
    assert_eq!(OpenCode.adapt_update(unnamed.clone()), unnamed);
    let usage = two::SessionUpdate::UsageUpdate(two::UsageUpdate::new(1, 2));
    assert_eq!(OpenCode.adapt_update(usage.clone()), usage);
}

#[test]
fn a_child_sessions_update_is_dropped() {
    let child = json!({"_meta": {"opencode/child-session": {
        "id": "ses_c", "parentID": "ses_p", "depth": 1, "title": "t"
    }}});
    assert!(dropped(&OpenCode, &child));
    assert!(!dropped(&OpenCode, &json!({"_meta": {}})));
    assert!(!dropped(
        &OpenCode,
        &json!({"sessionUpdate": "agent_message_chunk"})
    ));
    assert!(!dropped(
        &OpenCode,
        &json!({"_meta": {"opencode/child-session": null}})
    ));
}

/// The title of the tool call `update` is.
fn title_of(update: &two::SessionUpdate) -> Option<String> {
    let two::SessionUpdate::ToolCallUpdate(call) = update else {
        panic!("not a tool call: {update:?}");
    };
    call.title.value().cloned()
}

#[test]
fn partiturs_tools_get_their_mcp_title() {
    let renamed = OpenCode.adapt_update(call("partitur_show_svg"));
    assert_eq!(
        title_of(&renamed).as_deref(),
        Some("mcp__partitur__show_svg")
    );
    // A later update carrying only the title, of kind `other`, is renamed too.
    let titled = two::ToolCallUpdate::new("t1")
        .title("partitur_show_html")
        .kind(two::ToolKind::Other);
    let renamed = OpenCode.adapt_update(two::SessionUpdate::ToolCallUpdate(titled));
    assert_eq!(
        title_of(&renamed).as_deref(),
        Some("mcp__partitur__show_html")
    );
    assert_eq!(name_of(&renamed), None);
    // A call of another kind keeps its title, and so does another title.
    let shell = two::ToolCallUpdate::new("t1")
        .title("partitur_show_svg")
        .kind(two::ToolKind::Execute);
    let kept = OpenCode.adapt_update(two::SessionUpdate::ToolCallUpdate(shell));
    assert_eq!(title_of(&kept).as_deref(), Some("partitur_show_svg"));
    let other = OpenCode.adapt_update(call("Echo hi: shell"));
    assert_eq!(title_of(&other).as_deref(), Some("Echo hi: shell"));
}

#[test]
fn a_blank_configured_value_counts_as_none() {
    let none = config_content(None, ToolRole::Plain, None, None);
    for blank in ["", "  \n"] {
        assert_eq!(
            config_content(Some(blank), ToolRole::Plain, None, None),
            none
        );
    }
}

#[test]
fn every_built_in_agent_gets_partiturs_permissions() {
    for role in [ToolRole::Plain, ToolRole::Orchestrator] {
        let v = parsed(&config_content(None, role, None, None));
        for agent in ["build", "plan", "general"] {
            assert_eq!(
                v["agent"][agent]["permission"], v["permission"],
                "{agent} as {role:?}"
            );
        }
    }
    let v = parsed(&config_content(None, ToolRole::Orchestrator, None, None));
    assert_eq!(v["agent"]["general"]["permission"]["bash"], "deny");
}

#[test]
fn a_configured_agents_own_settings_are_kept_beneath_partiturs() {
    let mine = r#"{"agent":{"build":{"model":"m","permission":{"bash":"allow","read":"allow"}},"review":{"model":"r"}}}"#;
    let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
    assert_eq!(v["agent"]["build"]["model"], "m");
    assert_eq!(v["agent"]["build"]["permission"]["bash"], "ask");
    assert_eq!(v["agent"]["build"]["permission"]["read"], "allow");
    assert_eq!(v["agent"]["review"], json!({"model": "r"}));
    assert_eq!(v["agent"]["plan"]["permission"]["edit"], "ask");
}

#[test]
fn configured_agents_that_are_no_objects_are_replaced() {
    for mine in [
        r#"{"agent":1}"#,
        r#"{"agent":{"build":1}}"#,
        r#"{"agent":{"build":{"permission":"allow"}}}"#,
    ] {
        let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
        assert_eq!(v["agent"]["build"]["permission"]["bash"], "ask", "{mine}");
    }
}
