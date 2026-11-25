//! opencode's profile: what Partitur does differently for opencode (`opencode acp`), which
//! speaks ACP v1.
//!
//! [`OpenCode`] is the [`AgentProfile`] of an agent that names itself [`OPENCODE_NAME`] in its
//! `initialize` answer ([`crate::agent::for_agent`]). opencode offers no config option that keeps
//! it asking, and by default every one of its tools acts without asking. So what keeps it asking
//! is its configuration, which Partitur hands it in [`CONFIG_CONTENT_VAR`] when the process
//! starts. The hooks it overrides are these:
//!
//! - [`AgentProfile::process_env`]: the process starts with [`CONFIG_CONTENT_VAR`] set to
//!   [`config_content`]. Its `permission` makes every tool that changes files, runs commands,
//!   reaches the network or starts a subagent ask ([`PERMISSION_KEYS`]); for an orchestrator it
//!   denies editing, commands and subagents instead ([`DENIED_FOR_ORCHESTRATOR`]), which holds in
//!   subagents too. opencode's own `question` tool is denied in every session
//!   ([`ALWAYS_DENIED`]), so the model asks through Partitur's `ask_user`. Its `instructions`
//!   name Partitur's instructions file, and its `mcp` registers Partitur's tool server with
//!   opencode's Code Mode off, so that Partitur's tools are called directly. A value the backend's
//!   environment, else Partitur's own, already gives the variable is kept beneath Partitur's.
//! - [`AgentProfile::asking`]: that environment keeps it asking, so no config option is switched
//!   and no notice says it is not kept asking.
//! - [`AgentProfile::registers_tools_in_session`]: Partitur's tool server is not named in
//!   `session/new` or `session/load`, since opencode puts a server registered there behind its
//!   Code Mode `execute` tool, where no call names the tool it calls.
//! - [`AgentProfile::subagent_marker`]: a subagent's updates, which arrive on its parent's
//!   session tagged with `_meta["opencode/child-session"]` ([`from_child_session_wire`]), are
//!   dropped, as for every agent whose subagents Partitur shows only through the parent's call.
//! - [`AgentProfile::adapt_update`]: a session info update carrying `_meta["opencode/retry"]`,
//!   which opencode sends while its model is unreachable, becomes a warning naming the attempt
//!   and the error ([`retry_notice`]); the update that clears it shows nothing. A call of a tool
//!   of Partitur's server, which opencode names and titles `partitur_<tool>`, is named and
//!   titled `mcp__partitur__<tool>`, as Claude's are, so that the core recognises the drawings of
//!   `show_svg` and `show_html` by their title. Only a title of a call whose kind is `other`, or
//!   not given, is renamed: every MCP tool's call has that kind. The name is no more than a
//!   display name ([`crate::agent::partitur_tool_name`]): it is put on the update after the
//!   permission bookkeeping read it, so it never lets a call run unasked.
//!
//! Its turn ends with the prompt's answer, as for every v1 agent. Once a background command or
//! a subagent (opencode runs every subagent in the background) finishes, opencode streams a
//! further turn of its own after that answer, with nothing marking its end; it shows as it comes.
//! A rejected permission ends opencode's turn as cancelled, which is opencode's own behaviour.
//!
//! The environment goes out before the agent says who it is, so every agent's process gets
//! [`CONFIG_CONTENT_VAR`], with the session's tools URL in it ([`crate::agent::process_env`]),
//! and one that is not opencode ignores it. An empty or blank configured value counts as none.

use std::path::Path;

use agent_client_protocol::v2::schema as two;
use partitur_core::{TOOL_SERVER, ToolRole};
use serde_json::{Map, Value};

use super::{AgentProfile, Asking, ProcessStart, partitur_tool_name};
use crate::runtime::INSTRUCTIONS_FILE;

/// The name opencode gives itself in its `initialize` answer.
pub(crate) const OPENCODE_NAME: &str = "OpenCode";
/// The environment variable whose JSON opencode merges over its own configuration.
pub(crate) const CONFIG_CONTENT_VAR: &str = "OPENCODE_CONFIG_CONTENT";
/// opencode's permissions that change files, run commands, reach the network or start a
/// subagent; by default all of them act without asking. `task` is among them since a
/// subagent's own tools never ask, so starting one is the last point the user is asked.
pub(crate) const PERMISSION_KEYS: [&str; 6] = [
    "edit",
    "bash",
    "webfetch",
    "websearch",
    "external_directory",
    "task",
];
/// The permissions an orchestrator is denied instead: plain sessions ask before a subagent
/// starts, an orchestrator may start none; a denial reaches subagents.
pub(crate) const DENIED_FOR_ORCHESTRATOR: [&str; 3] = ["edit", "bash", "task"];
/// The permissions denied in every session: opencode's own `question` tool asks through ACP
/// elicitation, which Partitur does not offer, so the model uses Partitur's `ask_user`.
pub(crate) const ALWAYS_DENIED: [&str; 1] = ["question"];
/// The key of the `_meta` that marks a subagent's update.
const CHILD_SESSION_META: &str = "opencode/child-session";
/// The key of the `_meta` of a session info update while the model is retried.
const RETRY_META: &str = "opencode/retry";

/// opencode's profile.
pub(crate) struct OpenCode;

impl AgentProfile for OpenCode {
    fn label(&self) -> &'static str {
        "opencode"
    }

    fn agent_name(&self) -> Option<&'static str> {
        Some(OPENCODE_NAME)
    }

    fn process_env(&self, start: &ProcessStart<'_>) -> Vec<(String, String)> {
        let configured = start.configured(CONFIG_CONTENT_VAR);
        let file = start
            .config
            .instructions_dir
            .as_ref()
            .map(|dir| dir.join(INSTRUCTIONS_FILE));
        let content = config_content(
            configured.as_deref(),
            start.role,
            file.as_deref(),
            Some(start.tools_url),
        );
        vec![(CONFIG_CONTENT_VAR.to_owned(), content)]
    }

    fn asking(&self) -> Asking {
        Asking::EnsuredAtStart
    }

    fn registers_tools_in_session(&self) -> bool {
        false
    }

    fn subagent_marker(&self) -> Option<fn(&Value) -> bool> {
        Some(from_child_session_wire)
    }

    fn adapt_update(&self, update: two::SessionUpdate) -> two::SessionUpdate {
        match update {
            two::SessionUpdate::SessionInfoUpdate(info) => match retry_notice(&info) {
                Some(notice) => two::SessionUpdate::Notice(notice),
                None => two::SessionUpdate::SessionInfoUpdate(info),
            },
            two::SessionUpdate::ToolCallUpdate(call) => {
                two::SessionUpdate::ToolCallUpdate(partiturs_call_renamed(call))
            }
            update => update,
        }
    }
}

/// The JSON of [`CONFIG_CONTENT_VAR`] for a session in `role`: `configured`, the value the
/// variable already has, if it is a JSON object (an empty or blank one counts as none, anything
/// else is replaced, with a warning), with
/// `permission.<key>` set to `ask` for every one of the [`PERMISSION_KEYS`] (`deny` for the
/// [`DENIED_FOR_ORCHESTRATOR`] when `role` is an orchestrator) and to `deny` for the
/// [`ALWAYS_DENIED`], `instructions_file` appended to `instructions` when there is one, and,
/// when `tools_url` is given, `mcp.<TOOL_SERVER>` set to the remote server at that URL with Code
/// Mode off. A configured `permission` or `mcp` that is no object, or `instructions` that is no
/// list, is replaced.
pub(crate) fn config_content(
    configured: Option<&str>,
    role: ToolRole,
    instructions_file: Option<&Path>,
    tools_url: Option<&str>,
) -> String {
    let configured = configured.filter(|configured| !configured.trim().is_empty());
    let mut config = match configured.map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(config))) => config,
        Some(_) => {
            tracing::warn!("{CONFIG_CONTENT_VAR} is no JSON object; Partitur's replaces it");
            Map::new()
        }
        None => Map::new(),
    };
    let mut permission = taken_object(&mut config, "permission");
    for key in PERMISSION_KEYS {
        let denied = role == ToolRole::Orchestrator && DENIED_FOR_ORCHESTRATOR.contains(&key);
        let value = if denied { "deny" } else { "ask" };
        permission.insert(key.to_owned(), Value::from(value));
    }
    for key in ALWAYS_DENIED {
        permission.insert(key.to_owned(), Value::from("deny"));
    }
    config.insert("permission".to_owned(), Value::Object(permission));
    if let Some(file) = instructions_file {
        let mut instructions = match config.remove("instructions") {
            Some(Value::Array(instructions)) => instructions,
            _ => Vec::new(),
        };
        instructions.push(Value::from(file.to_string_lossy()));
        config.insert("instructions".to_owned(), Value::Array(instructions));
    }
    if let Some(url) = tools_url {
        let mut mcp = taken_object(&mut config, "mcp");
        let server = serde_json::json!({"type": "remote", "url": url, "codemode": false});
        mcp.insert(TOOL_SERVER.to_owned(), server);
        config.insert("mcp".to_owned(), Value::Object(mcp));
    }
    Value::Object(config).to_string()
}

/// The object at `key` in `config`, taken out of it; an empty one when it is missing or no
/// object.
fn taken_object(config: &mut Map<String, Value>, key: &str) -> Map<String, Value> {
    match config.remove(key) {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    }
}

/// Whether `wire`, the wire form of a session update of either protocol version, comes from one
/// of opencode's child sessions: its `_meta["opencode/child-session"]` is an object.
pub(crate) fn from_child_session_wire(wire: &Value) -> bool {
    wire.get("_meta")
        .and_then(|meta| meta.get(CHILD_SESSION_META))
        .is_some_and(Value::is_object)
}

/// The warning a session info update means whose `_meta["opencode/retry"]` is an object: the
/// model is unreachable and retried, at the attempt it names, for the error message it carries.
/// `None` for any other info update, the one clearing the retry with `null` among them.
fn retry_notice(info: &two::SessionInfoUpdate) -> Option<two::Notice> {
    let retry = info.meta.value()?.get(RETRY_META)?.as_object()?;
    let title = match retry.get("attempt").and_then(Value::as_u64) {
        Some(attempt) => format!("The model is unreachable, retrying (attempt {attempt})"),
        None => "The model is unreachable, retrying".to_owned(),
    };
    let message = retry
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    Some(
        two::Notice::new(two::NoticeSeverity::Warning, title)
            .description(message.map(str::to_owned)),
    )
}

/// `call` with a name and, for a call of kind `other` or of no kind given, a title of the form
/// `partitur_<tool>` renamed to `mcp__partitur__<tool>` ([`mcp_name`]).
fn partiturs_call_renamed(call: two::ToolCallUpdate) -> two::ToolCallUpdate {
    let name = call.name.value().and_then(|name| mcp_name(name));
    let mcp = matches!(call.kind.value(), None | Some(two::ToolKind::Other));
    let title = call
        .title
        .value()
        .filter(|_| mcp)
        .and_then(|title| mcp_name(title));
    let call = match name {
        Some(name) => call.name(name),
        None => call,
    };
    match title {
        Some(title) => call.title(title),
        None => call,
    }
}

/// The name the core knows a tool of Partitur's server by that opencode names `name`:
/// `mcp__partitur__<tool>` for `partitur_<tool>` ([`partitur_tool_name`]), else `None`.
fn mcp_name(name: &str) -> Option<String> {
    partitur_tool_name(name, '_')
}

#[cfg(test)]
mod tests;
