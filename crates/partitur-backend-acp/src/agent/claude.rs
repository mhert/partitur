//! The profile of Claude Code's ACP adapter: what Partitur does differently for Claude.
//!
//! [`Claude`] is the [`AgentProfile`] of an agent that names itself
//! [`CLAUDE_ADAPTER_NAME`] in its `initialize` answer, the name the adapter takes from its
//! package ([`crate::agent::for_agent`]). The hooks it overrides are these:
//!
//! - [`AgentProfile::process_env`]: the process starts with [`THINKING_DISPLAY_VAR`] set
//!   from the configured [`crate::runtime::ThinkingDisplay`], which tells the adapter how to
//!   report reasoning.
//! - [`AgentProfile::session_meta`]: every `session/new` and `session/resume` carries the
//!   `_meta` that appends Partitur's tool instructions to the adapter's own system prompt,
//!   `systemPrompt.append`. It must be an object with `append`: a plain string would replace
//!   the whole prompt. The adapter keeps nothing between processes, so every opening request
//!   sends it.
//! - [`AgentProfile::adapt_update`]: the adapter names a tool call's tool in
//!   `_meta.claudeCode.toolName` rather than in its `name`, so a tool call update without a
//!   name takes that one. A name the update carries wins, and every other update passes
//!   unchanged.
//! - [`AgentProfile::reports_server_status`]: the adapter marks a notice about an MCP server's
//!   status with `_meta.claudeCode.kind` set to `mcpServerStatus`.

use agent_client_protocol::v2::schema as two;

use super::{AgentProfile, ProcessStart};

/// The name Claude Code's ACP adapter gives itself in its `initialize` answer.
pub(crate) const CLAUDE_ADAPTER_NAME: &str = "@agentclientprotocol/claude-agent-acp";

/// The environment variable that selects how the adapter reports reasoning.
pub(crate) const THINKING_DISPLAY_VAR: &str = "CLAUDE_CODE_THINKING_DISPLAY";

/// The key of the adapter's own `_meta`.
const CLAUDE_META: &str = "claudeCode";

/// Claude Code's ACP adapter's profile.
pub(crate) struct Claude;

impl AgentProfile for Claude {
    fn label(&self) -> &'static str {
        "claude"
    }

    fn agent_name(&self) -> Option<&'static str> {
        Some(CLAUDE_ADAPTER_NAME)
    }

    fn process_env(&self, start: &ProcessStart<'_>) -> Vec<(String, String)> {
        let display = start.config.thinking_display.env_value();
        vec![(THINKING_DISPLAY_VAR.to_owned(), display.to_owned())]
    }

    fn session_meta(&self) -> Option<two::Meta> {
        Some(two::Meta::from_iter([(
            "systemPrompt".to_owned(),
            serde_json::json!({ "append": partitur_core::tool_instructions() }),
        )]))
    }

    fn adapt_update(&self, update: two::SessionUpdate) -> two::SessionUpdate {
        let two::SessionUpdate::ToolCallUpdate(call) = update else {
            return update;
        };
        if call.name.value().is_some() {
            return two::SessionUpdate::ToolCallUpdate(call);
        }
        let name = call.meta.value().and_then(|meta| {
            let name = meta.get(CLAUDE_META)?.get("toolName")?.as_str()?;
            Some(name.to_owned())
        });
        two::SessionUpdate::ToolCallUpdate(match name {
            Some(name) => call.name(name),
            None => call,
        })
    }

    fn reports_server_status(&self, update: &two::SessionUpdate) -> bool {
        let two::SessionUpdate::Notice(notice) = update else {
            return false;
        };
        let kind = notice.meta.as_ref().and_then(|meta| {
            let kind = meta.get(CLAUDE_META)?.get("kind")?;
            kind.as_str()
        });
        kind == Some("mcpServerStatus")
    }
}

#[cfg(test)]
mod tests;
