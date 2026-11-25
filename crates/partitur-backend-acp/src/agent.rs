//! What Partitur does differently for one agent, behind one trait with a module per agent.
//!
//! An [`AgentProfile`] is a set of synchronous hooks over plain values that the protocol code
//! calls instead of knowing any agent. Every hook's default is the behaviour of an agent that
//! speaks ACP as it is, so a profile overrides only what differs for its agent: [`Generic`]
//! overrides nothing, [`Claude`] overrides what Claude Code's ACP adapter needs, [`Copilot`]
//! what GitHub Copilot CLI needs, and [`OpenCode`] what opencode needs.
//!
//! A session's profile is chosen from the agent's `initialize` answer, whichever protocol
//! version it speaks ([`for_agent`]), by the name the agent gives itself there:
//!
//! | `agentInfo.name`                        | profile     |
//! |-----------------------------------------|-------------|
//! | `@agentclientprotocol/claude-agent-acp` | [`Claude`]  |
//! | `Copilot`                               | [`Copilot`] |
//! | `OpenCode`                              | [`OpenCode`] |
//! | anything else, or none                  | [`Generic`] |
//!
//! Until that answer arrives a connection uses [`Generic`]. Two helpers apply a profile's hooks
//! where more than the hook is involved: [`dropped`] tells whether an update is dropped as a
//! subagent's, and [`restart_args`] whether an orchestrator's agent is started again, and with
//! which arguments.
//!
//! Both protocol clients hand every notification they have no typed handler for, whatever its
//! method, to [`AgentProfile::vendor_notification`], so the protocol code names no agent's
//! vendor notification and no agent's command line.
//!
//! What goes out before that answer cannot follow the agent's profile, since the agent is not
//! known yet. So it is the union of every profile's, in the order of [`PROFILES`]: the process
//! starts with the configured environment and then every profile's variables
//! ([`process_env`]), which win over a configured one of the same name, and the `initialize`
//! carries every profile's `_meta` keys ([`initialize_meta`]). Each agent is expected to ignore
//! the others'. So every agent's process, and every command it runs, inherits opencode's
//! `OPENCODE_CONFIG_CONTENT`, with that session's tools URL in it.

pub(crate) mod claude;
pub(crate) mod copilot;
pub(crate) mod generic;
pub(crate) mod opencode;

use agent_client_protocol::v2::schema as two;
use partitur_core::{TOOL_SERVER, ToolRole};
use serde::Serialize;
use serde_json::Value;

use crate::runtime::AcpConfig;

use claude::Claude;
use copilot::Copilot;
use generic::Generic;
use opencode::OpenCode;

/// Every profile, in the order their contributions before `initialize` are made, the one
/// place a new profile is added: [`for_agent`] chooses from it by
/// [`AgentProfile::agent_name`], with [`Generic`] for a name none of them gives.
const PROFILES: [&dyn AgentProfile; 4] = [&Claude, &Copilot, &OpenCode, &Generic];

/// What a session's process is started for, before the agent is known.
pub(crate) struct ProcessStart<'a> {
    /// How the process is started.
    pub(crate) config: &'a AcpConfig,
    /// The part the session plays.
    pub(crate) role: ToolRole,
    /// The URL of the session's tools.
    pub(crate) tools_url: &'a str,
    /// The value a variable has in Partitur's own environment.
    pub(crate) inherited: &'a dyn Fn(&str) -> Option<String>,
}

impl ProcessStart<'_> {
    /// The value `var` already has for the process: the last one the configured environment
    /// gives it, else Partitur's own.
    pub(crate) fn configured(&self, var: &str) -> Option<String> {
        self.config
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == var)
            .map(|(_, value)| value.clone())
            .or_else(|| (self.inherited)(var))
    }
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

    /// Whether Partitur's tool server is named in the requests that open a session; `false`
    /// for an agent that learns of it from its process environment instead (opencode).
    fn registers_tools_in_session(&self) -> bool {
        true
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

    /// The v2 update a notification of `method` with `params` means, one that neither protocol
    /// version defines (Copilot's `github.com/copilot/sessionEvent`); `None` for one this agent
    /// reads nothing of.
    fn vendor_notification(
        &self,
        _method: &str,
        _params: &Value,
    ) -> Option<two::UpdateSessionNotification> {
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

    /// The arguments an orchestrator's agent, started with `args`, must be started again with
    /// so that it cannot edit files or run commands (Copilot's `--excluded-tools=`); `None` when
    /// it needs no restart, as its denied permission requests are enough or `args` already
    /// keep it from them.
    fn orchestrator_restart_args(&self, _args: &[String]) -> Option<Vec<String>> {
        None
    }

    /// How this agent is kept asking before it acts.
    fn asking(&self) -> Asking {
        Asking::ByConfigOptions
    }
}

/// How an agent is kept asking before it acts.
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

/// The arguments a session in `role` on `profile`, started with `args`, must be started again
/// with: for an orchestrator, its profile's [`AgentProfile::orchestrator_restart_args`]; else
/// `None`, as children and plain sessions start once, unchanged.
pub(crate) fn restart_args(
    profile: &dyn AgentProfile,
    role: ToolRole,
    args: &[String],
) -> Option<Vec<String>> {
    if role != ToolRole::Orchestrator {
        return None;
    }
    profile.orchestrator_restart_args(args)
}

/// The name the core knows a tool of Partitur's MCP server ([`TOOL_SERVER`]) by that an agent
/// names `named`, `<server><separator><tool>`: `mcp__partitur__<tool>`, or `None` for any other
/// name. Partitur's tools are named with lowercase ASCII letters, digits and `_` only, so a name
/// whose `<tool>` has any other character, such as `partitur-dev-exec` for the tool `exec` of a
/// server `partitur-dev`, names none. With `_` as the separator that is no longer so: the tool
/// `exec` of a foreign server `partitur_dev` is named `partitur_dev_exec` and maps to
/// `mcp__partitur__dev_exec` too. So a name mapped with `_` is a display and mapping name only,
/// and must never feed `is_partitur_tool` or the permission auto-grant.
pub(crate) fn partitur_tool_name(named: &str, separator: char) -> Option<String> {
    let tool = named.strip_prefix(TOOL_SERVER)?.strip_prefix(separator)?;
    let partiturs = !tool.is_empty()
        && tool
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    partiturs.then(|| format!("mcp__{TOOL_SERVER}__{tool}"))
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
