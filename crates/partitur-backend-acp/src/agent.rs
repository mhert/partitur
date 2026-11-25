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
//!
//! What goes out before that answer cannot follow the agent's profile, since the agent is not
//! known yet. So it is the union of every profile's, in the order of [`PROFILES`]: the process
//! starts with the configured environment and then every profile's variables
//! ([`process_env`]), which win over a configured one of the same name, and the `initialize`
//! carries every profile's `_meta` keys ([`initialize_meta`]). Each agent ignores the others'.

pub(crate) mod claude;
pub(crate) mod copilot;
pub(crate) mod generic;

use agent_client_protocol::v2::schema as two;
use partitur_core::ToolRole;
use serde::Serialize;
use serde_json::Value;

use crate::runtime::AcpConfig;

use claude::Claude;
use copilot::Copilot;
use generic::Generic;

// The vendor session event both protocol clients route to [`AgentProfile::session_event`], and
// the argument exclusion an orchestrator's restart applies, for the protocol code to name
// without naming an agent.
pub(crate) use copilot::{SessionEventNotification, with_exclusion};

/// Every profile, in the order their contributions before `initialize` are made, the one
/// place a new profile is added: [`for_agent`] chooses from it by
/// [`AgentProfile::agent_name`], with [`Generic`] for a name none of them gives.
const PROFILES: [&dyn AgentProfile; 3] = [&Claude, &Copilot, &Generic];

/// What a session's process is started for, before the agent is known.
pub(crate) struct ProcessStart<'a> {
    /// How the process is started.
    pub(crate) config: &'a AcpConfig,
    /// The part the session plays.
    #[allow(dead_code, reason = "no profile starts its process by the role yet")]
    pub(crate) role: ToolRole,
    /// The value a variable has in Partitur's own environment.
    pub(crate) inherited: &'a dyn Fn(&str) -> Option<String>,
}

/// What Partitur does differently for one agent. Every hook has the behaviour of an agent that
/// speaks ACP as it is; a profile overrides only what differs.
pub(crate) trait AgentProfile: Send + Sync + 'static {
    /// The profile's name in logs: "claude", "copilot", "opencode", "generic".
    fn label(&self) -> &'static str;

    /// The `agentInfo.name` of the agent this profile is for, in its `initialize` answer;
    /// `None` for a profile no agent is chosen for by name.
    fn agent_name(&self) -> Option<&'static str> {
        None
    }

    /// The environment variables this agent needs its process started with. They go to every
    /// agent's process, since it starts before the agent is known.
    fn process_env(&self, _start: &ProcessStart<'_>) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The `_meta` keys this agent needs in the `initialize` client capabilities. They go to
    /// every agent, since `initialize` asks who the agent is.
    fn initialize_meta(&self) -> Option<two::Meta> {
        None
    }

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

    /// Whether `args`, the arguments the agent is started with, already exclude every one of
    /// `tools`, so that [`OrchestratorGuard::RestartWithout`] needs no restart.
    fn excludes_already(&self, _args: &[String], _tools: &[&str]) -> bool {
        false
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

/// The profile of the agent that named itself `name` in its `initialize` answer: the one in
/// [`PROFILES`] whose [`AgentProfile::agent_name`] it is, else [`Generic`].
pub(crate) fn for_agent(name: Option<&str>) -> &'static dyn AgentProfile {
    PROFILES
        .into_iter()
        .find(|profile| name.is_some() && profile.agent_name() == name)
        .unwrap_or(&Generic)
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
/// [`OrchestratorGuard::RestartWithout`], its tools unless `args` already exclude them all
/// ([`AgentProfile::excludes_already`]); else `None`.
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
    (!profile.excludes_already(args, tools)).then_some(tools)
}

/// The environment of a process started for `start`: the configured variables, then every
/// profile's [`AgentProfile::process_env`] in the order of [`PROFILES`], so that a profile's
/// variable wins over a configured one of the same name.
pub(crate) fn process_env(start: &ProcessStart<'_>) -> Vec<(String, String)> {
    let configured = start.config.env.iter().cloned();
    configured
        .chain(
            PROFILES
                .iter()
                .flat_map(|profile| profile.process_env(start)),
        )
        .collect()
}

/// The `_meta` of the `initialize` client capabilities: the union of every profile's
/// [`AgentProfile::initialize_meta`] keys, `None` when no profile has any.
pub(crate) fn initialize_meta() -> Option<two::Meta> {
    union_of_meta(&PROFILES)
}

/// The union of the `initialize` `_meta` keys of `profiles`, a later profile's key winning;
/// `None` when they have none.
fn union_of_meta(profiles: &[&dyn AgentProfile]) -> Option<two::Meta> {
    let meta: two::Meta = profiles
        .iter()
        .filter_map(|profile| profile.initialize_meta())
        .flatten()
        .collect();
    (!meta.is_empty()).then_some(meta)
}

#[cfg(test)]
mod tests;
