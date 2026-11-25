//! What Partitur does differently for one agent, behind one trait with a module per agent.
//!
//! An [`AgentProfile`] is a set of synchronous hooks over plain values that the protocol code
//! calls instead of knowing any agent. Every hook's default is the behaviour of an agent that
//! speaks ACP as it is, so a profile overrides only what differs for its agent: [`Generic`]
//! overrides nothing, [`Claude`] overrides what Claude Code's ACP adapter needs, and
//! [`Copilot`] what GitHub Copilot CLI needs.
//!
//! A session's profile is chosen from the agent's `initialize` answer, whichever protocol
//! version it speaks ([`for_agent`]), by the name the agent gives itself there:
//!
//! | `agentInfo.name`                        | profile     |
//! |-----------------------------------------|-------------|
//! | `@agentclientprotocol/claude-agent-acp` | [`Claude`]  |
//! | `Copilot`                               | [`Copilot`] |
//! | anything else, or none                  | [`Generic`] |
//!
//! Until that answer arrives a connection uses [`Generic`]. Two helpers apply a profile's hooks
//! where more than the hook is involved: [`dropped`] tells whether an update is dropped as a
//! subagent's, and [`restart_without`] whether an orchestrator's agent is started again
//! without some of its tools.

pub(crate) mod claude;
pub(crate) mod copilot;
pub(crate) mod generic;

use agent_client_protocol::v2::schema as two;
use partitur_core::ToolRole;
use serde::Serialize;
use serde_json::Value;

use claude::{CLAUDE_ADAPTER_NAME, Claude};
use copilot::Copilot;
use generic::Generic;

/// The name GitHub Copilot CLI gives itself in its `initialize` answer.
const COPILOT: &str = "Copilot";

/// What Partitur does differently for one agent. Every hook has the behaviour of an agent that
/// speaks ACP as it is; a profile overrides only what differs.
pub(crate) trait AgentProfile: Send + Sync + 'static {
    /// The profile's name in logs: "claude", "copilot", "opencode", "generic".
    fn label(&self) -> &'static str;

    /// The `_meta` of `session/new` and `session/resume` (Claude's `systemPrompt.append`).
    fn session_meta(&self) -> Option<two::Meta> {
        None
    }

    /// The update as the generic mapping should read it (Claude: the name from
    /// `_meta.claudeCode.toolName`).
    fn adapt_update(&self, update: two::SessionUpdate) -> two::SessionUpdate {
        update
    }

    /// Whether the update is an MCP server's status notice.
    fn reports_server_status(&self, _update: &two::SessionUpdate) -> bool {
        false
    }

    /// The v2 state a vendor session event's params mean (Copilot's sessionEvent).
    fn session_event(&self, _params: &Value) -> Option<two::UpdateSessionNotification> {
        None
    }

    /// The test that tells a subagent's update from its wire form, if this agent's are dropped.
    fn subagent_marker(&self) -> Option<fn(&Value) -> bool> {
        None
    }

    /// The name of a v1 tool call of kind `other` that has no name, from its title.
    fn titled_tool_name(&self, _title: &str) -> Option<String> {
        None
    }

    /// Whether a prompt waits while an earlier one is unanswered.
    fn holds_prompts(&self) -> bool {
        false
    }

    /// How an orchestrator is kept from editing and running commands.
    fn orchestrator_guard(&self) -> OrchestratorGuard {
        OrchestratorGuard::DenyRequests
    }

    /// How this agent is kept asking before it acts.
    #[allow(
        dead_code,
        reason = "every profile is kept asking by its config options yet"
    )]
    fn asking(&self) -> Asking {
        Asking::ByConfigOptions
    }
}

/// How an orchestrator is kept from editing and running commands, beyond the denied
/// permission requests every agent gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrchestratorGuard {
    /// Denied permission requests are enough.
    DenyRequests,
    /// The agent is started again with these tools excluded (Copilot's `--excluded-tools`).
    RestartWithout(&'static [&'static str]),
    /// The process environment set before `initialize` already removes the tools.
    #[expect(dead_code, reason = "no profile ensures it at start yet")]
    EnsuredAtStart,
}

/// How an agent is kept asking before it acts.
#[allow(
    dead_code,
    reason = "every profile is kept asking by its config options yet"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asking {
    /// Config options are switched as `asking_settings` says, with a notice if none fit.
    ByConfigOptions,
    /// The process environment already ensures it; no option is switched and no notice shown.
    EnsuredAtStart,
}

/// The profile of the agent that named itself `name` in its `initialize` answer: [`Claude`]
/// for [`CLAUDE_ADAPTER_NAME`], [`Copilot`] for `Copilot`, else [`Generic`].
pub(crate) fn for_agent(name: Option<&str>) -> &'static dyn AgentProfile {
    match name {
        Some(CLAUDE_ADAPTER_NAME) => &Claude,
        Some(COPILOT) => &Copilot,
        _ => &Generic,
    }
}

/// Whether `profile` drops `update`, a session update of either protocol version, as a
/// subagent's: its [`AgentProfile::subagent_marker`] holds for the update's wire form. Without
/// a marker nothing is dropped, and the update is not serialized.
///
/// The wire form costs one serialization per update but covers every kind of update, those of
/// later schema versions included, with no arm per kind to keep in step.
pub(crate) fn dropped(profile: &dyn AgentProfile, update: &impl Serialize) -> bool {
    profile
        .subagent_marker()
        .is_some_and(|marker| marker(&serde_json::to_value(update).unwrap_or_default()))
}

/// The tools a session in `role` on `profile`, started with `args`, must be started again
/// without ([`copilot::with_exclusion`]): for an orchestrator whose guard is
/// [`OrchestratorGuard::RestartWithout`], its tools when the `--excluded-tools=<list>`
/// arguments among `args` do not name every one of them; else `None`.
pub(crate) fn restart_without(
    profile: &dyn AgentProfile,
    role: ToolRole,
    args: &[String],
) -> Option<&'static [&'static str]> {
    let OrchestratorGuard::RestartWithout(tools) = profile.orchestrator_guard() else {
        return None;
    };
    if role != ToolRole::Orchestrator {
        return None;
    }
    let all = tools
        .iter()
        .all(|tool| copilot::excluded(args).any(|listed| listed == *tool));
    (!all).then_some(tools)
}

#[cfg(test)]
mod tests;
