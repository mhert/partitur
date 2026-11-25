//! GitHub Copilot CLI's profile: what Partitur does differently for Copilot, apart from the
//! protocol version it speaks.
//!
//! [`Copilot`] is the [`AgentProfile`] of an agent that names itself `Copilot` in its
//! `initialize` answer, whichever protocol version it speaks ([`crate::agent::for_agent`]). So
//! Copilot keeps its handling over ACP v2 as over v1, and another v1 agent inherits none of it.
//! What keeps every agent asking, Copilot's `allow_all` switched off among it, is no hook of
//! Copilot's: it applies to every agent that offers it
//! ([`crate::connection::asking_settings`]).
//!
//! The hooks Copilot overrides are these:
//!
//! - [`AgentProfile::session_event`]: its session events, the vendor notification
//!   `github.com/copilot/sessionEvent` ([`SessionEventNotification`]), which
//!   [`translate_event`] turns into v2 states: `assistant.turn_start` is running,
//!   `assistant.idle` ends the turn (cancelled when the event says it was aborted) while the
//!   session's own work, one item named [`COPILOT_SESSION_WORK`], goes on in the background,
//!   and `session.idle` ends that work. A subagent's events, which carry its id, and every
//!   other event mean nothing.
//! - [`AgentProfile::subagent_marker`]: its subagents' updates, tagged with the subagent's id
//!   in `_meta["github.com/copilot"].agentId` ([`from_subagent_wire`]), are dropped: the
//!   subagent runs on its parent's session, and Partitur shows a subagent's work only through
//!   its parent's tool call, as with Claude's subagents.
//! - [`AgentProfile::titled_tool_name`]: a call of an MCP tool, which Copilot reports over v1
//!   with no name and titled `<server>-<tool>`, is named `mcp__<server>__<tool>` when the
//!   title names a tool of Partitur's MCP server ([`tool_name`]).
//! - [`AgentProfile::orchestrator_guard`]: an orchestrator's Copilot is started again without
//!   its editing and shell tools, the [`EXCLUDED_FOR_ORCHESTRATOR`], when its arguments do not
//!   exclude them all already ([`crate::agent::restart_without`], [`with_exclusion`]).
//! - [`AgentProfile::holds_prompts`]: a prompt is held while an earlier one is unanswered, as
//!   Copilot cancels the earlier one when a prompt arrives before it was answered.
//! - [`AgentProfile::process_env`]: when the config has an instructions folder, the process
//!   starts with it named in [`INSTRUCTIONS_DIRS_VAR`], after the folders the configured
//!   environment, else Partitur's own, already names there ([`instructions_dirs`]).
//! - [`AgentProfile::initialize_meta`]: the `initialize` carries the opt-in ([`opt_in`]), the
//!   `_meta` of the client capabilities that asks Copilot for its session events.
//!
//! Those two go out before the agent says who it is, so every agent gets them
//! ([`crate::agent::process_env`], [`crate::agent::initialize_meta`]), and one that is not
//! Copilot ignores them. Where that and the agent's own profile disagree,
//! what was decided before `initialize` stands for the process it was decided for, and
//! everything after the answer follows the profile. The restart's arguments are the backend's
//! with the exclusion added, known before the restarted process is initialized.

use std::path::Path;

use agent_client_protocol::v2::schema as two;
use partitur_core::TOOL_SERVER;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentProfile, OrchestratorGuard, ProcessStart};

/// The key of Copilot's `_meta`, in the `initialize` opt-in and on a subagent's updates.
const COPILOT_META: &str = "github.com/copilot";
/// The session events Partitur opts into.
const COPILOT_EVENTS: [&str; 3] = ["assistant.turn_start", "assistant.idle", "session.idle"];
/// The type of the background work item that stands for a Copilot session's own work from its
/// `assistant.idle` to its `session.idle`.
pub(crate) const COPILOT_SESSION_WORK: &str = "_copilot/session";
/// The environment variable naming the folders, comma-separated, whose `*.instructions.md`
/// files GitHub Copilot CLI inlines into its system message; Claude's adapter ignores it.
#[doc(hidden)]
pub const INSTRUCTIONS_DIRS_VAR: &str = "COPILOT_CUSTOM_INSTRUCTIONS_DIRS";
/// Copilot's option, in its `=` form, that starts it without the tools its comma-separated
/// list names.
const EXCLUDED_TOOLS: &str = "--excluded-tools=";
/// The tools an orchestrator's GitHub Copilot CLI is started without: its editing tools
/// (`edit`, `create`, or `apply_patch` on Codex models) and its shell tools (`bash`, or
/// `powershell` on Windows).
pub(crate) const EXCLUDED_FOR_ORCHESTRATOR: [&str; 5] =
    ["edit", "create", "apply_patch", "bash", "powershell"];

/// GitHub Copilot CLI's profile.
pub(crate) struct Copilot;

impl AgentProfile for Copilot {
    fn label(&self) -> &'static str {
        "copilot"
    }

    fn process_env(&self, start: &ProcessStart<'_>) -> Vec<(String, String)> {
        let Some(dir) = &start.config.instructions_dir else {
            return Vec::new();
        };
        let var = INSTRUCTIONS_DIRS_VAR;
        let configured = start
            .config
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == var)
            .map(|(_, value)| value.clone())
            .or_else(|| (start.inherited)(var));
        vec![(
            var.to_owned(),
            instructions_dirs(configured.as_deref(), dir),
        )]
    }

    fn initialize_meta(&self) -> Option<two::Meta> {
        Some(opt_in())
    }

    fn session_event(&self, params: &Value) -> Option<two::UpdateSessionNotification> {
        CopilotEvent::parse(params).and_then(|event| translate_event(&event))
    }

    fn subagent_marker(&self) -> Option<fn(&Value) -> bool> {
        Some(from_subagent_wire)
    }

    fn titled_tool_name(&self, title: &str) -> Option<String> {
        tool_name(title)
    }

    fn holds_prompts(&self) -> bool {
        true
    }

    fn orchestrator_guard(&self) -> OrchestratorGuard {
        OrchestratorGuard::RestartWithout(&EXCLUDED_FOR_ORCHESTRATOR)
    }

    fn excludes_already(&self, args: &[String], tools: &[&str]) -> bool {
        tools
            .iter()
            .all(|tool| excluded(args).any(|listed| listed == *tool))
    }
}

/// The `_meta` of the client capabilities in an `initialize`, opting into Copilot's session
/// events. An agent that does not know it ignores it.
pub(crate) fn opt_in() -> two::Meta {
    two::Meta::from_iter([(
        COPILOT_META.to_owned(),
        serde_json::json!({ "events": COPILOT_EVENTS }),
    )])
}

/// The value of the instructions variable naming `dir`: `dir` alone, or appended with a comma
/// to the `configured` folders, if there are any.
fn instructions_dirs(configured: Option<&str>, dir: &Path) -> String {
    let dir = dir.to_string_lossy();
    match configured.filter(|configured| !configured.is_empty()) {
        Some(configured) => format!("{configured},{dir}"),
        None => dir.into_owned(),
    }
}

/// Copilot's session event notification, its params kept as they are for [`CopilotEvent`].
#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcNotification)]
#[notification(method = "github.com/copilot/sessionEvent")]
#[serde(transparent)]
pub(crate) struct SessionEventNotification(pub(crate) Value);

/// One of Copilot's session events, as far as Partitur reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopilotEvent {
    /// The session it is about.
    pub(crate) session: String,
    /// What happened, such as `assistant.idle`.
    pub(crate) kind: String,
    /// The subagent it comes from, if a subagent sent it.
    pub(crate) agent: Option<String>,
    /// Whether it says the work was aborted.
    pub(crate) aborted: bool,
}

impl CopilotEvent {
    /// Reads the event from the params of a [`SessionEventNotification`]: `sessionId`,
    /// `type`, the optional `data.aborted` and the optional top-level `agentId`, which names a
    /// subagent whatever its type. `None` without a session or type.
    pub(crate) fn parse(params: &Value) -> Option<Self> {
        let text = |key: &str| params.get(key)?.as_str().map(str::to_owned);
        let agent = params.get("agentId").filter(|agent| !agent.is_null());
        let aborted = params.get("data").and_then(|data| data.get("aborted"));
        Some(Self {
            session: text("sessionId")?,
            kind: text("type")?,
            agent: agent.map(|agent| {
                agent
                    .as_str()
                    .map_or_else(|| agent.to_string(), str::to_owned)
            }),
            aborted: aborted.and_then(Value::as_bool).unwrap_or_default(),
        })
    }
}

/// The v2 state a Copilot session event means, or `None` for a subagent's event and one
/// Partitur reads nothing of.
pub(crate) fn translate_event(event: &CopilotEvent) -> Option<two::UpdateSessionNotification> {
    if event.agent.is_some() {
        return None;
    }
    let state = match event.kind.as_str() {
        "assistant.turn_start" => two::StateUpdate::Running(two::RunningStateUpdate::new()),
        "assistant.idle" => {
            let stop = if event.aborted {
                two::StopReason::Cancelled
            } else {
                two::StopReason::EndTurn
            };
            let work = two::BackgroundWork::Other(two::OtherBackgroundWork::new(
                COPILOT_SESSION_WORK,
                std::collections::BTreeMap::new(),
            ));
            two::StateUpdate::Idle(
                two::IdleStateUpdate::new()
                    .stop_reason(stop)
                    .background(vec![work]),
            )
        }
        "session.idle" => {
            two::StateUpdate::Idle(two::IdleStateUpdate::new().background(Vec::new()))
        }
        _ => return None,
    };
    Some(two::UpdateSessionNotification::new(
        event.session.clone(),
        two::SessionUpdate::StateUpdate(state),
    ))
}

/// Whether `wire`, the wire form of a session update of either protocol version, comes from a
/// Copilot subagent: its `_meta["github.com/copilot"].agentId` is set.
pub(crate) fn from_subagent_wire(wire: &Value) -> bool {
    let agent = wire
        .get("_meta")
        .and_then(|meta| meta.get(COPILOT_META)?.get("agentId"));
    agent.is_some_and(|agent| !agent.is_null())
}

/// The name the v2 path knows the tool by that a Copilot tool call titled `title` calls:
/// `mcp__partitur__<tool>` for the title `partitur-<tool>` of a tool of Partitur's MCP server
/// ([`TOOL_SERVER`]), and `None` for any other title. Partitur's tools are named with lowercase
/// ASCII letters, digits and `_` only, so a title whose `<tool>` has any other character, such
/// as `partitur-dev-exec` for the tool `exec` of a server `partitur-dev`, names none.
pub(crate) fn tool_name(title: &str) -> Option<String> {
    let tool = title.strip_prefix(TOOL_SERVER)?.strip_prefix('-')?;
    let named = !tool.is_empty()
        && tool
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    named.then(|| format!("mcp__{TOOL_SERVER}__{tool}"))
}

/// The tools the `--excluded-tools=<list>` arguments among `args` name.
fn excluded(args: &[String]) -> impl Iterator<Item = &str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(EXCLUDED_TOOLS))
        .flat_map(|list| list.split(','))
}

/// `args` excluding `tools`: each `--excluded-tools=<list>` already among them gets the tools
/// its list lacks appended, and without one `--excluded-tools=<those tools>` is appended. The
/// space-separated form, whose list cannot be told from the arguments after it, is left alone.
pub(crate) fn with_exclusion(args: &[String], tools: &[&str]) -> Vec<String> {
    let mut configured = false;
    let mut args: Vec<String> = args
        .iter()
        .map(|arg| match arg.strip_prefix(EXCLUDED_TOOLS) {
            Some(list) => {
                configured = true;
                let listed: Vec<&str> = list.split(',').filter(|tool| !tool.is_empty()).collect();
                let missing = tools.iter().filter(|tool| !listed.contains(tool));
                let tools: Vec<&str> = listed.iter().copied().chain(missing.copied()).collect();
                format!("{EXCLUDED_TOOLS}{}", tools.join(","))
            }
            None => arg.clone(),
        })
        .collect();
    if !configured {
        args.push(format!("{EXCLUDED_TOOLS}{}", tools.join(",")));
    }
    args
}

#[cfg(test)]
mod tests;
