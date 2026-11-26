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

/// The rules opencode makes of one configuration's `permission` object, in its keys' order,
/// as opencode 2.3.2 does: a key with one effect is one rule, a key with patterns one per
/// pattern, and `bash` and `task` are named `shell` and `subagent`.
fn rules_of_object(permission: &Value) -> Vec<(String, String, String)> {
    let mut rules = Vec::new();
    for (key, value) in permission.as_object().into_iter().flatten() {
        let action = match key.as_str() {
            "bash" => "shell",
            "task" => "subagent",
            key => key,
        }
        .to_owned();
        match value {
            Value::Object(patterns) => {
                for (pattern, effect) in patterns {
                    rules.push((
                        action.clone(),
                        pattern.clone(),
                        effect.as_str().unwrap().into(),
                    ));
                }
            }
            effect => rules.push((action, "*".to_owned(), effect.as_str().unwrap().into())),
        }
    }
    rules
}

/// The rules of a `permissions` list.
fn rules_of_list(permissions: &Value) -> Vec<(String, String, String)> {
    permissions
        .as_array()
        .into_iter()
        .flatten()
        .map(|rule| {
            let field = |name: &str| rule[name].as_str().unwrap().to_owned();
            (field("action"), field("resource"), field("effect"))
        })
        .collect()
}

/// Whether `text` matches opencode's wildcard `pattern`, in which `*` stands for any text.
fn matches(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            text.starts_with(head)
                && (0..=text.len() - head.len())
                    .any(|start| matches(tail, &text[head.len() + start..]))
        }
    }
}

/// What opencode decides for a call of `action` on `resource` by `agent` under the
/// configuration `content`: the effect of the last rule that matches, of the configuration's
/// `permission` and `permissions` and then of the agent's own (`agents.<agent>`, which
/// replaces `agent.<agent>`), and `allow`, opencode's default, when none does. An agent's
/// built-in rules, which come first, are left out.
fn decided(content: &str, agent: &str, action: &str, resource: &str) -> String {
    let v = parsed(content);
    let mut rules = rules_of_object(&v["permission"]);
    rules.extend(rules_of_list(&v["permissions"]));
    match v.get("agents").and_then(|agents| agents.get(agent)) {
        Some(own) => rules.extend(rules_of_list(&own["permissions"])),
        None => rules.extend(rules_of_object(&v["agent"][agent]["permission"])),
    }
    rules
        .iter()
        .rev()
        .find(|(a, r, _)| matches(a, action) && matches(r, resource))
        .map_or_else(|| "allow".to_owned(), |(_, _, effect)| effect.clone())
}

/// What opencode decides for a call of `action` on any resource under the configuration
/// Partitur makes of `configured` for a session in `role`, by the agent `agent`.
fn decided_for(configured: Option<&str>, role: ToolRole, agent: &str, action: &str) -> String {
    decided(
        &config_content(configured, role, None, None),
        agent,
        action,
        "x",
    )
}

/// An agent of the user's own, which gets only the top-level rules.
const MINE: &str = "mine";

#[test]
fn the_wildcard_matcher_follows_opencodes() {
    assert!(matches("*", "anything"));
    assert!(matches("partitur_*", "partitur_ask_user"));
    assert!(!matches("partitur_*", "mine_weather"));
    assert!(matches("*.env", "a/.env"));
    assert!(!matches("*.env", "a/.env.local"));
    assert!(matches("*.env.*", "a/.env.local"));
    assert!(matches("edit", "edit"));
    assert!(!matches("edit", "edits"));
}

#[test]
fn every_acting_permission_asks() {
    for agent in ["build", "plan", "general", MINE] {
        for action in ["shell", "webfetch", "websearch", "external_directory"] {
            assert_eq!(
                decided_for(None, ToolRole::Plain, agent, action),
                "ask",
                "{agent} {action}"
            );
        }
    }
    for agent in ["build", "general", MINE] {
        assert_eq!(
            decided_for(None, ToolRole::Plain, agent, "edit"),
            "ask",
            "{agent}"
        );
    }
    for agent in ["build", "plan", MINE] {
        assert_eq!(
            decided_for(None, ToolRole::Plain, agent, "subagent"),
            "ask",
            "{agent}"
        );
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
    for agent in ["build", "plan", "general", "explore", MINE] {
        for action in ["edit", "shell", "subagent"] {
            assert_eq!(
                decided_for(None, ToolRole::Orchestrator, agent, action),
                "deny",
                "{agent} {action}"
            );
        }
        for action in ["webfetch", "websearch", "external_directory"] {
            assert_eq!(
                decided_for(None, ToolRole::Orchestrator, agent, action),
                "ask",
                "{agent} {action}"
            );
        }
    }
}

#[test]
fn a_child_asks_as_a_plain_session_does() {
    // Only the tools allowed by name differ: a child has `propose_title`, not `rename_session`.
    let without_tools = |role| {
        let mut config = parsed(&config_content(None, role, None, None));
        let permission = config["permission"].as_object_mut().unwrap();
        permission.retain(|key, _| !key.starts_with("partitur_"));
        config
    };
    assert_eq!(
        without_tools(ToolRole::Child),
        without_tools(ToolRole::Plain)
    );
    let child = parsed(&config_content(None, ToolRole::Child, None, None));
    assert_eq!(child["permission"]["partitur_propose_title"], "allow");
    assert!(child["permission"].get("partitur_rename_session").is_none());
}

#[test]
fn opencodes_own_question_tool_is_denied() {
    for role in [ToolRole::Plain, ToolRole::Orchestrator] {
        for agent in ["build", "plan", MINE] {
            assert_eq!(decided_for(None, role, agent, "question"), "deny");
        }
    }
}

#[test]
fn every_other_tool_asks_but_reading_and_partiturs_own() {
    for agent in ["build", "plan", "general", MINE] {
        for action in ["mine_weather", "execute", "doom_loop", "unknown"] {
            assert_eq!(
                decided_for(None, ToolRole::Plain, agent, action),
                "ask",
                "{agent} {action}"
            );
        }
        for action in READ_ONLY.into_iter().chain(["read", "partitur_ask_user"]) {
            assert_eq!(
                decided_for(None, ToolRole::Plain, agent, action),
                "allow",
                "{agent} {action}"
            );
        }
    }
    assert_eq!(
        READ_ONLY,
        ["glob", "grep", "list", "lsp", "skill", "todowrite"]
    );
    let content = config_content(None, ToolRole::Plain, None, None);
    assert_eq!(decided(&content, "build", "read", "src/.env"), "ask");
    assert_eq!(decided(&content, "build", "read", "src/.env.local"), "ask");
    assert_eq!(
        decided(&content, "build", "read", "src/.env.example"),
        "allow"
    );
}

#[test]
fn a_configured_permission_decides_over_the_catch_all_but_not_over_partiturs() {
    let mine = r#"{"permission":{"read":{"secret/*":"deny"},"mine_*":"allow","bash":"allow"}}"#;
    let content = config_content(Some(mine), ToolRole::Plain, None, None);
    assert_eq!(decided(&content, MINE, "read", "secret/key"), "deny");
    assert_eq!(decided(&content, MINE, "read", "src/main.rs"), "allow");
    assert_eq!(decided(&content, MINE, "mine_weather", "x"), "allow");
    assert_eq!(decided(&content, MINE, "shell", "x"), "ask");
    // A key of Partitur's with one effect gets the configured patterns after it; a configured
    // effect replaces Partitur's.
    let mine = r#"{"permission":{"glob":{"secret/*":"deny"},"grep":"deny"}}"#;
    let content = config_content(Some(mine), ToolRole::Plain, None, None);
    assert_eq!(
        parsed(&content)["permission"]["glob"],
        json!({"*": "allow", "secret/*": "deny"})
    );
    assert_eq!(decided(&content, MINE, "glob", "secret/a"), "deny");
    assert_eq!(decided(&content, MINE, "glob", "src/a"), "allow");
    assert_eq!(decided(&content, MINE, "grep", "src/a"), "deny");
    assert_eq!(decided(&content, MINE, "mine_weather", "x"), "ask");
    let content = config_content(
        Some(r#"{"permission":{"*":"allow"}}"#),
        ToolRole::Plain,
        None,
        None,
    );
    assert_eq!(decided(&content, MINE, "mine_weather", "x"), "allow");
    assert_eq!(decided(&content, MINE, "shell", "x"), "ask");
}

#[test]
fn partiturs_permissions_come_after_a_configured_catch_all() {
    let mine = r#"{"permission":{"bash":"allow","*":"allow"},"agent":{"build":{"permission":{"bash":"allow","*":"allow","edit":"allow"}}}}"#;
    for agent in ["build", MINE] {
        assert_eq!(
            decided_for(Some(mine), ToolRole::Plain, agent, "shell"),
            "ask",
            "{agent}"
        );
        assert_eq!(
            decided_for(Some(mine), ToolRole::Plain, agent, "edit"),
            "ask",
            "{agent}"
        );
    }
    let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
    let keys: Vec<&String> = v["agent"]["build"]["permission"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        keys,
        [
            "*",
            "edit",
            "bash",
            "webfetch",
            "websearch",
            "external_directory",
            "task",
            "question"
        ]
    );
}

#[test]
fn partiturs_rules_come_after_a_configured_rule_list() {
    let mine = r#"{"permissions":[{"action":"shell","resource":"*","effect":"allow"},{"action":"*","resource":"*","effect":"allow"}]}"#;
    let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
    assert_eq!(v["permissions"][0]["effect"], "allow");
    assert_eq!(v["permissions"][1]["action"], "*");
    for action in ["shell", "edit", "subagent"] {
        assert_eq!(
            decided_for(Some(mine), ToolRole::Plain, MINE, action),
            "ask",
            "{action}"
        );
    }
    // A list that is no list is replaced.
    let v = parsed(&config_content(
        Some(r#"{"permissions":1}"#),
        ToolRole::Plain,
        None,
        None,
    ));
    assert_eq!(
        rules_of_list(&v["permissions"]),
        rules_of_list(&parsed(&config_content(None, ToolRole::Plain, None, None))["permissions"])
    );
}

#[test]
fn a_configured_agent_under_agents_gets_partiturs_rules_last() {
    let mine = r#"{"agents":{"build":{"model":"m","permissions":[{"action":"shell","resource":"*","effect":"allow"}]},"plan":{"permissions":1},"other":{}}}"#;
    let v = parsed(&config_content(Some(mine), ToolRole::Plain, None, None));
    assert_eq!(v["agents"]["build"]["model"], "m");
    assert_eq!(v["agents"]["build"]["permissions"][0]["effect"], "allow");
    assert_eq!(v["agents"]["other"], json!({}));
    assert_eq!(
        decided_for(Some(mine), ToolRole::Plain, "build", "shell"),
        "ask"
    );
    assert_eq!(
        decided_for(Some(mine), ToolRole::Plain, "plan", "shell"),
        "ask"
    );
    assert_eq!(
        decided_for(Some(mine), ToolRole::Plain, "plan", "edit"),
        "deny"
    );
    // An `agents` that is no object is left to opencode.
    let v = parsed(&config_content(
        Some(r#"{"agents":1}"#),
        ToolRole::Plain,
        None,
        None,
    ));
    assert_eq!(v["agents"], 1);
}

#[test]
fn plan_mode_stays_read_only_but_for_its_plan_folder() {
    let content = config_content(None, ToolRole::Plain, None, None);
    assert_eq!(
        decided(&content, "plan", "edit", "/work/src/main.rs"),
        "deny"
    );
    assert_eq!(
        decided(&content, "plan", "edit", "~/.opencode/plan/a.md"),
        "ask"
    );
    assert_eq!(
        decided(&content, "build", "edit", "~/.opencode/plan/a.md"),
        "ask"
    );
    let content = config_content(None, ToolRole::Orchestrator, None, None);
    assert_eq!(
        decided(&content, "plan", "edit", "~/.opencode/plan/a.md"),
        "deny"
    );
}

#[test]
fn the_subagents_keep_what_opencode_denies_them() {
    for agent in ["general", "explore"] {
        assert_eq!(
            decided_for(None, ToolRole::Plain, agent, "subagent"),
            "deny",
            "{agent}"
        );
    }
    for (action, effect) in [
        ("edit", "deny"),
        ("mine_weather", "deny"),
        ("partitur_ask_user", "deny"),
        ("list", "deny"),
        ("read", "allow"),
        ("grep", "allow"),
        ("glob", "allow"),
        ("shell", "ask"),
        ("webfetch", "ask"),
        ("websearch", "ask"),
        ("external_directory", "ask"),
        ("question", "deny"),
    ] {
        assert_eq!(
            decided_for(None, ToolRole::Plain, "explore", action),
            effect,
            "{action}"
        );
    }
    let content = config_content(None, ToolRole::Plain, None, None);
    assert_eq!(decided(&content, "explore", "read", "a/.env"), "ask");
}

#[test]
fn the_agents_that_use_no_tool_get_none() {
    for agent in ["title", "summary"] {
        for action in ["read", "shell", "partitur_ask_user"] {
            assert_eq!(
                decided_for(None, ToolRole::Plain, agent, action),
                "deny",
                "{agent} {action}"
            );
        }
    }
    assert_eq!(
        BUILT_IN_AGENTS,
        ["build", "plan", "general", "explore", "title", "summary"]
    );
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
    let mine = r#"{"model":"local/model","permission":{"bash":"allow","read":"allow"},"instructions":["/mine.md"]}"#;
    let content = config_content(Some(mine), ToolRole::Plain, Some(Path::new("/p.md")), None);
    let v = parsed(&content);
    assert_eq!(v["model"], "local/model");
    assert_eq!(decided(&content, MINE, "shell", "x"), "ask");
    assert_eq!(decided(&content, MINE, "read", "a/.env"), "allow");
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
    assert_eq!(v["permission"]["*"], "ask");
    assert_eq!(v["instructions"], json!(["/p.md"]));
    assert_eq!(v["mcp"]["partitur"]["url"], "http://t");
}

#[test]
fn a_configured_value_that_is_no_object_is_replaced() {
    for mine in ["[1]", "not json"] {
        let content = config_content(Some(mine), ToolRole::Plain, None, None);
        assert_eq!(content, config_content(None, ToolRole::Plain, None, None));
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
    let file = &Path::new("/data/agent-instructions").join("partitur.instructions.md");
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
fn only_partiturs_own_tools_are_allowed() {
    let plain = parsed(&config_content(None, ToolRole::Plain, None, None));
    assert_eq!(plain["permission"]["partitur_ask_user"], "allow");
    assert_eq!(plain["permission"]["partitur_show_html"], "allow");
    assert_eq!(
        plain["permission"]["partitur_superpowers_v7_start_session"],
        "allow"
    );
    assert!(plain["permission"].get("partitur_*").is_none());
    assert!(
        plain["permission"]
            .get("partitur_propose_children")
            .is_none()
    );
    let orchestrator = parsed(&config_content(None, ToolRole::Orchestrator, None, None));
    assert_eq!(
        orchestrator["permission"]["partitur_propose_children"],
        "allow"
    );
}

#[test]
fn a_foreign_partitur_prefixed_tool_asks_and_keeps_its_name() {
    let plain = parsed(&config_content(None, ToolRole::Plain, None, None));
    assert!(plain["permission"].get("partitur_dev_exec").is_none());
    assert_eq!(
        decided_for(None, ToolRole::Plain, "build", "partitur_dev_exec"),
        "ask"
    );
    let renamed = OpenCode.adapt_update(call("partitur_dev_exec"));
    assert_eq!(name_of(&renamed).as_deref(), Some("partitur_dev_exec"));
    assert_eq!(title_of(&renamed).as_deref(), Some("partitur_dev_exec"));
}

#[test]
fn the_tool_names_of_a_role_are_its_catalog_and_workflow_tools() {
    let plain = partiturs_tool_names(Some(ToolRole::Plain));
    let orchestrator = partiturs_tool_names(Some(ToolRole::Orchestrator));
    let every = partiturs_tool_names(None);
    assert!(plain.contains(&"propose_session"));
    assert!(!plain.contains(&"propose_children"));
    assert!(orchestrator.contains(&"propose_children"));
    assert!(!orchestrator.contains(&"propose_session"));
    for name in plain.iter().chain(&orchestrator) {
        assert!(every.contains(name), "{name}");
    }
    let run_level = workflow::tools(true).len() > workflow::tools(false).len();
    assert!(run_level);
    for tool in workflow::tools(true) {
        assert!(every.contains(&tool.name), "{}", tool.name);
        assert!(orchestrator.contains(&tool.name), "{}", tool.name);
    }
    for tool in workflow::tools(false) {
        assert!(plain.contains(&tool.name), "{}", tool.name);
    }
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
        "partitur_dev_exec",
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
        for agent in ["build", "plan", "general", "explore", "title", "summary"] {
            assert!(
                v["agent"][agent]["permission"].is_object(),
                "{agent} as {role:?}"
            );
        }
        assert_eq!(
            v["agent"]["build"]["permission"],
            json!(acting_permissions(role))
        );
    }
}

#[test]
fn a_configured_agents_own_settings_are_kept_beneath_partiturs() {
    let mine = r#"{"agent":{"build":{"model":"m","permission":{"bash":"allow","read":"deny"}},"review":{"model":"r"}}}"#;
    let content = config_content(Some(mine), ToolRole::Plain, None, None);
    let v = parsed(&content);
    assert_eq!(v["agent"]["build"]["model"], "m");
    assert_eq!(decided(&content, "build", "shell", "x"), "ask");
    assert_eq!(decided(&content, "build", "read", "x"), "deny");
    assert_eq!(v["agent"]["review"], json!({"model": "r"}));
}

#[test]
fn configured_agents_that_are_no_objects_are_replaced() {
    for mine in [
        r#"{"agent":1}"#,
        r#"{"agent":{"build":1}}"#,
        r#"{"agent":{"build":{"permission":"allow"}}}"#,
    ] {
        let content = config_content(Some(mine), ToolRole::Plain, None, None);
        assert_eq!(decided(&content, "build", "shell", "x"), "ask", "{mine}");
    }
}
