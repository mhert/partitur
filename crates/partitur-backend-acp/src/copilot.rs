//! GitHub Copilot CLI's profile: what Partitur does differently for Copilot, apart from the
//! protocol version it speaks.
//!
//! A session's [`Quirks`] say which of Copilot's ways the connection allows for. They are
//! chosen from the agent's `initialize` answer ([`Quirks::for_agent`]): an agent that names
//! itself `Copilot` gets [`Quirks::copilot`], any other agent [`Quirks::none`], whichever
//! protocol version it speaks. So Copilot keeps its handling over ACP v2 as over v1, and
//! another v1 agent inherits none of it. An agent whose answer names no agent is not Copilot.
//!
//! Copilot's ways are these:
//!
//! - The opt-in ([`opt_in`]): the `_meta` of the client capabilities in `initialize` that asks
//!   Copilot for its session events.
//! - Its session events, the vendor notification `github.com/copilot/sessionEvent`
//!   ([`SessionEventNotification`]), which [`translate_event`] turns into v2 states:
//!   `assistant.turn_start` is running, `assistant.idle` ends the turn (cancelled when the
//!   event says it was aborted) while the session's own work, one item named
//!   [`COPILOT_SESSION_WORK`], goes on in the background, and `session.idle` ends that work. A
//!   subagent's events, which carry its id, and every other event mean nothing.
//! - Its subagents' updates, tagged with the subagent's id in
//!   `_meta["github.com/copilot"].agentId` ([`from_subagent`]), are dropped: the subagent runs
//!   on its parent's session, and Partitur shows a subagent's work only through its parent's
//!   tool call, as with Claude's subagents.
//! - A call of an MCP tool, which Copilot reports over v1 with no name and titled
//!   `<server>-<tool>`, is named `mcp__<server>__<tool>` when the title names a tool of
//!   Partitur's MCP server ([`tool_name`]).
//! - An orchestrator's Copilot is started again without its editing and shell tools, the
//!   [`EXCLUDED_FOR_ORCHESTRATOR`], when its arguments do not exclude them all already
//!   ([`needs_restart`], [`with_exclusion`]).
//! - The folder of Partitur's instructions is named in [`INSTRUCTIONS_DIRS_VAR`].
//! - Its config option `allow_all`, which on runs every tool without asking, is switched off.
//! - A prompt is held while an earlier one is unanswered, as Copilot cancels the earlier one
//!   when a prompt arrives before it was answered.
//!
//! The opt-in and the instructions variable go out before the agent says who it is: the
//! process starts with the variable, and the `initialize` that asks who it is carries the
//! opt-in. So they follow [`Quirks::before_initialize`], every profile's needs at once, and
//! every agent gets them; one that is not Copilot ignores them. Where that and the agent's own
//! quirks disagree, what was decided before `initialize` stands for the process it was decided
//! for, and everything after the answer follows the agent's quirks. The restart's arguments are
//! the backend's with the exclusion added, known before the restarted process is initialized.

use agent_client_protocol::v2::schema as two;
use partitur_core::{TOOL_SERVER, ToolRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The name GitHub Copilot CLI gives itself in its `initialize` answer.
const COPILOT: &str = "Copilot";
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
/// Copilot's config option that, on, runs every tool without asking.
pub(crate) const ALLOW_ALL_OPTION: &str = "allow_all";
/// The value of [`ALLOW_ALL_OPTION`] that keeps the agent asking.
pub(crate) const ALLOW_ALL_OFF: &str = "off";
/// Copilot's option, in its `=` form, that starts it without the tools its comma-separated
/// list names.
const EXCLUDED_TOOLS: &str = "--excluded-tools=";
/// The tools an orchestrator's GitHub Copilot CLI is started without: its editing tools
/// (`edit`, `create`, or `apply_patch` on Codex models) and its shell tools (`bash`, or
/// `powershell` on Windows).
pub(crate) const EXCLUDED_FOR_ORCHESTRATOR: [&str; 5] =
    ["edit", "create", "apply_patch", "bash", "powershell"];

/// Which of Copilot's ways a session's connection allows for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent way of the agent's"
)]
pub(crate) struct Quirks {
    /// Whether `initialize` opts into Copilot's session events ([`opt_in`]).
    pub(crate) opt_in: bool,
    /// Whether Copilot's session events are read.
    pub(crate) session_events: bool,
    /// Whether updates tagged as a subagent's are dropped.
    pub(crate) drop_subagent_updates: bool,
    /// Whether an MCP tool's call named only by its `<server>-<tool>` title gets the name
    /// `mcp__<server>__<tool>` of Partitur's tool it calls ([`tool_name`]).
    pub(crate) tool_titles: bool,
    /// The tools an orchestrator is started again without, if it must be.
    pub(crate) orchestrator_restart: Option<&'static [&'static str]>,
    /// The variable naming the folder of Partitur's instructions, if the agent reads one.
    pub(crate) instructions_env: Option<&'static str>,
    /// Whether [`ALLOW_ALL_OPTION`] is switched to [`ALLOW_ALL_OFF`].
    pub(crate) allow_all: bool,
    /// Whether a prompt is held while an earlier one is unanswered.
    pub(crate) hold_prompt_while_unanswered: bool,
}

impl Quirks {
    /// No quirks: an agent that speaks the protocol as it is.
    pub(crate) const fn none() -> Self {
        Self {
            opt_in: false,
            session_events: false,
            drop_subagent_updates: false,
            tool_titles: false,
            orchestrator_restart: None,
            instructions_env: None,
            allow_all: false,
            hold_prompt_while_unanswered: false,
        }
    }

    /// GitHub Copilot CLI's quirks: every one.
    pub(crate) const fn copilot() -> Self {
        Self {
            opt_in: true,
            session_events: true,
            drop_subagent_updates: true,
            tool_titles: true,
            orchestrator_restart: Some(&EXCLUDED_FOR_ORCHESTRATOR),
            instructions_env: Some(INSTRUCTIONS_DIRS_VAR),
            allow_all: true,
            hold_prompt_while_unanswered: true,
        }
    }

    /// The quirks of the agent that named itself `name` in its `initialize` answer:
    /// [`Quirks::copilot`] for `Copilot`, else [`Quirks::none`].
    pub(crate) fn for_agent(name: Option<&str>) -> Self {
        if name == Some(COPILOT) {
            Self::copilot()
        } else {
            Self::none()
        }
    }

    /// What is decided before the agent says who it is, the process's environment and the
    /// `initialize` request: every profile's needs at once, which are Copilot's, as an agent
    /// without quirks needs nothing then.
    pub(crate) const fn before_initialize() -> Self {
        Self::copilot()
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

/// The opt-in an `initialize` carries: [`opt_in`] when [`Quirks::before_initialize`] says so.
pub(crate) fn initialize_meta() -> Option<two::Meta> {
    Quirks::before_initialize().opt_in.then(opt_in)
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

/// The v2 state the session event `event` means, when `quirks` read session events.
pub(crate) fn event_state(
    quirks: &Quirks,
    event: &SessionEventNotification,
) -> Option<two::UpdateSessionNotification> {
    if !quirks.session_events {
        return None;
    }
    CopilotEvent::parse(&event.0).and_then(|event| translate_event(&event))
}

/// Whether `update`, a session update of either protocol version, comes from a Copilot
/// subagent: its `_meta["github.com/copilot"].agentId` is set.
///
/// The `_meta` is read from the update's wire form, which costs one serialization per update
/// but covers every kind of update, those of later schema versions included, with no arm per
/// kind to keep in step.
pub(crate) fn from_subagent(update: &impl Serialize) -> bool {
    let wire = serde_json::to_value(update).unwrap_or_default();
    let agent = wire
        .get("_meta")
        .and_then(|meta| meta.get(COPILOT_META)?.get("agentId"));
    agent.is_some_and(|agent| !agent.is_null())
}

/// Whether `quirks` drop `update` as a subagent's ([`from_subagent`]).
pub(crate) fn dropped(quirks: &Quirks, update: &impl Serialize) -> bool {
    quirks.drop_subagent_updates && from_subagent(update)
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

/// The name `quirks` give a tool call of the kind `other` that carries none, titled `title`
/// ([`tool_name`]); `None` when they read no titles. Only such a call: the core lets
/// Partitur's own tools run without asking, so a built-in tool, whatever its title, or another
/// server's tool must never pass for one.
pub(crate) fn titled_name(quirks: &Quirks, title: &str) -> Option<String> {
    quirks.tool_titles.then(|| tool_name(title)).flatten()
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

/// The tools a session in `role` with `quirks`, started with `args`, must be started again
/// without ([`with_exclusion`]): for an orchestrator whose quirks restart it, their tools
/// when its `--excluded-tools=<list>` arguments do not name every one of them; else `None`.
pub(crate) fn needs_restart(
    role: ToolRole,
    quirks: &Quirks,
    args: &[String],
) -> Option<&'static [&'static str]> {
    let tools = quirks
        .orchestrator_restart
        .filter(|_| role == ToolRole::Orchestrator)?;
    let all = tools
        .iter()
        .all(|tool| excluded(args).any(|listed| listed == *tool));
    (!all).then_some(tools)
}

#[cfg(test)]
mod tests;
