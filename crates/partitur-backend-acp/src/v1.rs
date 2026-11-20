//! Pure translation between an ACP v1 agent's traffic and the v2 types the session code reads.
//!
//! An agent that answers Partitur's v2 `initialize` with protocol version 1 is driven by a v1
//! client, and everything it reports is translated here into the v2 values the v2 path already
//! handles, so the session logic exists once. Nothing here performs I/O.
//!
//! The updates of a v1 `session/update` become these v2 updates:
//!
//! - `user_message_chunk`, `agent_message_chunk` and `agent_thought_chunk` keep their kind and
//!   content; a chunk without a message id gets the id [`NO_MESSAGE_ID`].
//! - `tool_call` becomes a `tool_call_update` that sets every field the v1 call carries: v2
//!   reports tool calls as upserts. A call of kind `other` without a name whose title names a
//!   tool of Partitur's MCP server, as Copilot titles an MCP tool's call `<server>-<tool>`,
//!   gets the name `mcp__<server>__<tool>` the v2 path knows those tools by ([`tool_name`]).
//!   Only such a call: the core lets Partitur's own tools run without asking, so a built-in
//!   tool, whatever its title, or another server's tool must never pass for one. A
//!   `tool_call_update` sets the fields it carries and leaves the others undefined.
//! - Text, image and resource content of a tool call keeps its kind, a terminal and a session
//!   reference stay what they are, and a v1 diff, which holds the whole old and new text of
//!   one file, becomes a v2 diff of that file, added when it has no old text and modified
//!   otherwise, with a git patch that removes every old line and adds every new one.
//! - `plan` becomes a `plan_update` of the plan [`PLAN_ID`]: a v1 agent keeps one plan per
//!   session and sends it whole.
//! - `available_commands_update` lists the same commands, a command's unstructured input
//!   becoming a text input with the same hint.
//! - `config_option_update` carries the options translated by [`translate_config`].
//! - `session_info_update`, `usage_update` and `notice` carry the same fields.
//! - `current_mode_update` is dropped: the mode is a config option, whose change arrives as a
//!   `config_option_update` too. So is every kind Partitur shows nothing of (compaction,
//!   subagents' messages) and every kind this version does not know.
//! - An update a Copilot subagent sent, tagged with its id in `_meta["github.com/copilot"]
//!   .agentId`, is dropped: the subagent runs on its parent's session, and Partitur shows a
//!   subagent's work only through its parent's tool call, as with Claude's subagents. A chunk
//!   or notice whose content has no v2 form is dropped too, and logged at debug level.
//!
//! A `session/request_permission` becomes the v2 request about the same tool call. Copilot
//! titles the request with the bare tool name and leaves out what the call already said, so
//! the name, title and kind it leaves out are those of the call reported before under the same
//! id ([`KnownCall`]), and Partitur's own tools are known as such; a name is taken over only
//! while the call's kind is still `other`. A call is forgotten once it completed or failed.
//!
//! A v1 agent has no `state_update`: [`running`] is what a prompt sent to it means, and
//! [`idle_after_response`] what the answer to that prompt means, with its stop reason and no
//! background work.
//!
//! GitHub Copilot CLI reports its state in the vendor notification
//! `github.com/copilot/sessionEvent` ([`SessionEventNotification`]) when the `initialize` opts
//! into it with [`copilot_opt_in`]. Its events become v2 states by
//! [`translate_event`]: `assistant.turn_start` is running, `assistant.idle` ends the turn
//! (cancelled when the event says it was aborted) while the session's own work, one item named
//! [`COPILOT_SESSION_WORK`], goes on in the background, and `session.idle` ends that work. A
//! subagent's events, which carry its id, and every other event mean nothing. The answer to the
//! prompt still means what it does without the events: once the turn already ended it changes
//! nothing, and it ends a cancelled turn, for which Copilot sends no idle.

use std::collections::HashMap;
use std::path::Path;

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use partitur_core::{PermissionOptionId, TOOL_SERVER};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permissions::Answer;

/// The message id of a v1 chunk that names none; v2 requires one, and Partitur reads none.
pub(crate) const NO_MESSAGE_ID: &str = "v1";
/// The id of a v1 agent's plan: it keeps one per session and sends it whole.
pub(crate) const PLAN_ID: &str = "plan";
/// The key of Copilot's `_meta`, in the `initialize` opt-in and on a subagent's updates.
const COPILOT: &str = "github.com/copilot";
/// The session events Partitur opts into.
const COPILOT_EVENTS: [&str; 3] = ["assistant.turn_start", "assistant.idle", "session.idle"];
/// The type of the background work item that stands for a Copilot session's own work from its
/// `assistant.idle` to its `session.idle`.
pub(crate) const COPILOT_SESSION_WORK: &str = "_copilot/session";

/// The `_meta` of the client capabilities in every `initialize`, v2 and v1 alike, opting into
/// Copilot's session events. An agent that does not know it ignores it.
pub(crate) fn copilot_opt_in() -> two::Meta {
    two::Meta::from_iter([(
        COPILOT.to_owned(),
        serde_json::json!({ "events": COPILOT_EVENTS }),
    )])
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
    session: String,
    /// What happened, such as `assistant.idle`.
    kind: String,
    /// The subagent it comes from, if a subagent sent it.
    agent: Option<String>,
    /// Whether it says the work was aborted.
    aborted: bool,
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

/// Whether `update` comes from a Copilot subagent: its `_meta["github.com/copilot"].agentId`
/// is set.
///
/// The `_meta` is read from the update's wire form, which costs one serialization per update
/// but covers every kind of update, those of later schema versions included, with no arm per
/// kind to keep in step.
fn from_subagent(update: &one::SessionUpdate) -> bool {
    let wire = serde_json::to_value(update).unwrap_or_default();
    let agent = wire
        .get("_meta")
        .and_then(|meta| meta.get(COPILOT)?.get("agentId"));
    agent.is_some_and(|agent| !agent.is_null())
}

/// `value`, or `None` with a debug log naming the `kind` of the update dropped for it.
fn kept<T>(value: Option<T>, kind: &str) -> Option<T> {
    if value.is_none() {
        tracing::debug!(kind, "dropped a v1 update whose content has no v2 form");
    }
    value
}

impl Answer for v1::Responder<one::RequestPermissionResponse> {
    fn answer(
        self,
        option: Option<PermissionOptionId>,
    ) -> Result<(), agent_client_protocol::Error> {
        let outcome = match option {
            Some(option) => one::RequestPermissionOutcome::Selected(
                one::SelectedPermissionOutcome::new(option.as_str().to_owned()),
            ),
            None => one::RequestPermissionOutcome::Cancelled,
        };
        self.respond(one::RequestPermissionResponse::new(outcome))
    }
}

/// `value` as the type `U` whose wire form is the same: a value of a kind both versions
/// share. `None` when the wire form does not read as a `U`.
fn reencode<T: Serialize, U: DeserializeOwned>(value: &T) -> Option<U> {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| serde_json::from_value(json).ok())
}

/// The v2 update a v1 `session/update` notification means, or `None` for one Partitur reads
/// nothing of.
pub(crate) fn translate_update(
    notification: one::SessionNotification,
) -> Option<two::UpdateSessionNotification> {
    if from_subagent(&notification.update) {
        return None;
    }
    let update = match notification.update {
        one::SessionUpdate::UserMessageChunk(chunk) => {
            let chunk = kept(translate_chunk(chunk), "user_message_chunk")?;
            two::SessionUpdate::UserMessageChunk(chunk)
        }
        one::SessionUpdate::AgentMessageChunk(chunk) => two::SessionUpdate::AgentMessageChunk(
            kept(translate_chunk(chunk), "agent_message_chunk")?,
        ),
        one::SessionUpdate::AgentThoughtChunk(chunk) => two::SessionUpdate::AgentThoughtChunk(
            kept(translate_chunk(chunk), "agent_thought_chunk")?,
        ),
        one::SessionUpdate::ToolCall(call) => two::SessionUpdate::ToolCallUpdate(created(call)),
        one::SessionUpdate::ToolCallUpdate(update) => {
            two::SessionUpdate::ToolCallUpdate(translate_tool_call_update(update))
        }
        one::SessionUpdate::Plan(plan) => two::SessionUpdate::PlanUpdate(translate_plan(plan)),
        one::SessionUpdate::AvailableCommandsUpdate(update) => {
            two::SessionUpdate::AvailableCommandsUpdate(
                two::AvailableCommandsUpdate::new(
                    update
                        .available_commands
                        .into_iter()
                        .map(translate_command)
                        .collect(),
                )
                .meta(update.meta),
            )
        }
        one::SessionUpdate::ConfigOptionUpdate(update) => two::SessionUpdate::ConfigOptionUpdate(
            two::ConfigOptionUpdate::new(translate_config(update.config_options)).meta(update.meta),
        ),
        one::SessionUpdate::SessionInfoUpdate(info) => two::SessionUpdate::SessionInfoUpdate(
            two::SessionInfoUpdate::new()
                .title(info.title)
                .updated_at(info.updated_at)
                .meta(defined(info.meta)),
        ),
        one::SessionUpdate::UsageUpdate(usage) => two::SessionUpdate::UsageUpdate(
            two::UsageUpdate::new(usage.used, usage.size)
                .cost(defined(
                    usage.cost.as_ref().and_then(reencode::<_, two::Cost>),
                ))
                .meta(usage.meta),
        ),
        one::SessionUpdate::Notice(notice) => two::SessionUpdate::Notice(
            two::Notice::new(kept(reencode(&notice.severity), "notice")?, notice.title)
                .description(notice.description)
                .meta(notice.meta),
        ),
        // The mode also is a config option, and a change of it arrives as such.
        one::SessionUpdate::CurrentModeUpdate(_) | _ => return None,
    };
    Some(
        two::UpdateSessionNotification::new(notification.session_id.0.to_string(), update)
            .meta(notification.meta),
    )
}

/// A v1 chunk as a v2 one, with [`NO_MESSAGE_ID`] when it names no message.
fn translate_chunk(chunk: one::ContentChunk) -> Option<two::ContentChunk> {
    let message = chunk
        .message_id
        .map_or_else(|| NO_MESSAGE_ID.to_owned(), |id| id.0.to_string());
    Some(two::ContentChunk::new(reencode(&chunk.content)?, message).meta(chunk.meta))
}

/// A v1 field as a v2 patch field: one v1 leaves out is undefined, so it changes nothing; v1
/// cannot clear a field.
fn defined<T>(value: Option<T>) -> MaybeUndefined<T> {
    value.map_or(MaybeUndefined::Undefined, MaybeUndefined::Value)
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

/// The v2 upsert announcing the v1 tool call `call`, with every field it carries set, and,
/// for a call of kind `other` that carries no name, the name its title gives ([`tool_name`]).
fn created(call: one::ToolCall) -> two::ToolCallUpdate {
    let mcp = call.kind == one::ToolKind::Other;
    let name = call
        .name
        .or_else(|| mcp.then(|| tool_name(&call.title)).flatten());
    two::ToolCallUpdate::new(call.tool_call_id.0.to_string())
        .title(call.title)
        .name(defined(name))
        .kind(translate_kind(call.kind))
        .status(translate_status(call.status))
        .content(translate_contents(call.content))
        .locations(translate_locations(call.locations))
        .raw_input(defined(call.raw_input))
        .raw_output(defined(call.raw_output))
        .meta(defined(call.meta))
}

/// The v2 upsert changing the fields the v1 `update` carries.
pub(crate) fn translate_tool_call_update(update: one::ToolCallUpdate) -> two::ToolCallUpdate {
    let fields = update.fields;
    two::ToolCallUpdate::new(update.tool_call_id.0.to_string())
        .title(defined(fields.title))
        .name(defined(fields.name))
        .kind(defined(fields.kind.map(translate_kind)))
        .status(defined(fields.status.map(translate_status)))
        .content(defined(fields.content.map(translate_contents)))
        .locations(defined(fields.locations.map(translate_locations)))
        .raw_input(defined(fields.raw_input))
        .raw_output(defined(fields.raw_output))
        .meta(defined(update.meta))
}

/// Translates a tool kind; a kind this version does not know is `other`.
fn translate_kind(kind: one::ToolKind) -> two::ToolKind {
    match kind {
        one::ToolKind::Read => two::ToolKind::Read,
        one::ToolKind::Edit => two::ToolKind::Edit,
        one::ToolKind::Delete => two::ToolKind::Delete,
        one::ToolKind::Move => two::ToolKind::Move,
        one::ToolKind::Search => two::ToolKind::Search,
        one::ToolKind::Execute => two::ToolKind::Execute,
        one::ToolKind::Think => two::ToolKind::Think,
        one::ToolKind::Fetch => two::ToolKind::Fetch,
        one::ToolKind::SwitchMode => two::ToolKind::SwitchMode,
        one::ToolKind::Other | _ => two::ToolKind::Other,
    }
}

/// Translates a tool call status; a status this version does not know is `pending`.
fn translate_status(status: one::ToolCallStatus) -> two::ToolCallStatus {
    match status {
        one::ToolCallStatus::InProgress => two::ToolCallStatus::InProgress,
        one::ToolCallStatus::Completed => two::ToolCallStatus::Completed,
        one::ToolCallStatus::Failed => two::ToolCallStatus::Failed,
        one::ToolCallStatus::Pending | _ => two::ToolCallStatus::Pending,
    }
}

/// Translates a tool call's content, dropping what has no v2 counterpart.
fn translate_contents(content: Vec<one::ToolCallContent>) -> Vec<two::ToolCallContent> {
    content.into_iter().filter_map(translate_content).collect()
}

/// Translates one piece of a tool call's content: a diff as the module states, and content,
/// terminals and session references, whose wire form both versions share, as they are.
fn translate_content(content: one::ToolCallContent) -> Option<two::ToolCallContent> {
    match content {
        one::ToolCallContent::Diff(diff) => {
            let text = whole_file_patch(&diff.path, diff.old_text.as_deref(), &diff.new_text);
            let change = match diff.old_text {
                Some(_) => two::DiffChange::modify(diff.path),
                None => two::DiffChange::add(diff.path),
            };
            let diff = two::Diff::patch(text, vec![change]).meta(diff.meta);
            Some(two::ToolCallContent::Diff(diff))
        }
        other => reencode(&other),
    }
}

/// The patch turning `old`, the whole text of the file at `file` (`None` when the file is
/// new), into `new`: one hunk removing every old line and adding every new one, its headers
/// naming the file as it is.
fn whole_file_patch(file: &Path, old: Option<&str>, new: &str) -> String {
    let file = file.display();
    let (from, removed) = match old {
        Some(old) => (file.to_string(), old.lines().collect::<Vec<_>>()),
        None => ("/dev/null".to_owned(), Vec::new()),
    };
    let added: Vec<&str> = new.lines().collect();
    let start = |count: usize| usize::from(count > 0);
    let mut patch = format!(
        "--- {from}\n+++ {file}\n@@ -{},{} +{},{} @@\n",
        start(removed.len()),
        removed.len(),
        start(added.len()),
        added.len()
    );
    patch.extend(removed.iter().map(|line| format!("-{line}\n")));
    patch.extend(added.iter().map(|line| format!("+{line}\n")));
    patch
}

/// Translates where a tool call works.
fn translate_locations(locations: Vec<one::ToolCallLocation>) -> Vec<two::ToolCallLocation> {
    locations
        .into_iter()
        .map(|at| {
            two::ToolCallLocation::new(at.path)
                .line(at.line)
                .meta(at.meta)
        })
        .collect()
}

/// A v1 plan as the v2 update of the plan [`PLAN_ID`], entries kept.
fn translate_plan(plan: one::Plan) -> two::PlanUpdate {
    let entries = plan
        .entries
        .iter()
        .filter_map(reencode::<_, two::PlanEntry>)
        .collect();
    two::PlanUpdate::new(two::PlanUpdateContent::Items(two::PlanItems::new(
        PLAN_ID, entries,
    )))
    .meta(plan.meta)
}

/// A v1 command as a v2 one, its unstructured input a text input with the same hint.
fn translate_command(command: one::AvailableCommand) -> two::AvailableCommand {
    two::AvailableCommand::Command(
        two::Command::new(command.name, command.description)
            .input(command.input.as_ref().and_then(translate_input))
            .meta(command.meta),
    )
}

/// A v1 command's input as a v2 one: v1 sends the unstructured input untagged, as its hint,
/// and v2 tags the same fields as `text`.
fn translate_input(input: &one::AvailableCommandInput) -> Option<two::AvailableCommandInput> {
    let mut wire = serde_json::to_value(input).ok()?;
    wire.as_object_mut()?
        .insert("type".to_owned(), "text".into());
    serde_json::from_value(wire).ok()
}

/// Translates a v1 agent's config options: an option's `id` becomes its `configId` and a
/// choice group's `group` its `groupId`; everything else is kept.
pub(crate) fn translate_config(
    options: Vec<one::SessionConfigOption>,
) -> Vec<two::SessionConfigOption> {
    options.into_iter().filter_map(translate_option).collect()
}

/// Translates one config option.
fn translate_option(option: one::SessionConfigOption) -> Option<two::SessionConfigOption> {
    let kind = match option.kind {
        one::SessionConfigKind::Select(select) => {
            two::SessionConfigKind::Select(two::SessionConfigSelect::new(
                select.current_value.0.to_string(),
                translate_choices(select.options)?,
            ))
        }
        // A boolean's wire form is the same in both versions.
        other => reencode(&other)?,
    };
    Some(
        two::SessionConfigOption::new(option.id.0.to_string(), option.name, kind)
            .description(option.description)
            .category(option.category.as_ref().and_then(reencode))
            .meta(option.meta),
    )
}

/// Translates a select option's choices, grouped or not.
fn translate_choices(
    choices: one::SessionConfigSelectOptions,
) -> Option<two::SessionConfigSelectOptions> {
    Some(match choices {
        one::SessionConfigSelectOptions::Grouped(groups) => {
            let groups: Vec<two::SessionConfigSelectGroup> = groups
                .into_iter()
                .map(|group| {
                    two::SessionConfigSelectGroup::new(
                        group.group.0.to_string(),
                        group.name,
                        translate_select_options(&group.options),
                    )
                    .meta(group.meta)
                })
                .collect();
            two::SessionConfigSelectOptions::from(groups)
        }
        // Ungrouped choices have the same wire form in both versions.
        other => reencode(&other)?,
    })
}

/// Translates the choices of one select option or group.
fn translate_select_options(
    choices: &[one::SessionConfigSelectOption],
) -> Vec<two::SessionConfigSelectOption> {
    choices.iter().filter_map(reencode).collect()
}

/// The v2 state a prompt sent to a v1 agent means: its foreground work runs.
pub(crate) fn running(session: two::SessionId) -> two::UpdateSessionNotification {
    two::UpdateSessionNotification::new(
        session,
        two::SessionUpdate::StateUpdate(two::StateUpdate::Running(two::RunningStateUpdate::new())),
    )
}

/// The v2 state the answer to a v1 agent's prompt means: its foreground work ended for `stop`,
/// and no work runs in the background.
pub(crate) fn idle_after_response(
    session: two::SessionId,
    stop: one::StopReason,
) -> two::UpdateSessionNotification {
    let idle = two::IdleStateUpdate::new()
        .stop_reason(translate_stop_reason(stop))
        .background(Vec::new());
    two::UpdateSessionNotification::new(
        session,
        two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(idle)),
    )
}

/// The v2 state a refused prompt of a v1 agent means: its foreground work ended without a
/// stop reason of its own, so the refusal decides how the turn ends.
pub(crate) fn idle_after_refusal(session: two::SessionId) -> two::UpdateSessionNotification {
    two::UpdateSessionNotification::new(
        session,
        two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
            two::IdleStateUpdate::new().background(Vec::new()),
        )),
    )
}

/// Translates a stop reason; one this version does not know ends the turn normally.
fn translate_stop_reason(stop: one::StopReason) -> two::StopReason {
    match stop {
        one::StopReason::MaxTokens => two::StopReason::MaxTokens,
        one::StopReason::MaxTurnRequests => two::StopReason::MaxTurnRequests,
        one::StopReason::Refusal => two::StopReason::Refusal,
        one::StopReason::Cancelled => two::StopReason::Cancelled,
        one::StopReason::EndTurn | _ => two::StopReason::EndTurn,
    }
}

/// Translates a prompt, the output of `map_prompt`, for a v1 agent: text, images and resource
/// links keep their kind. A block v1 has no kind for is dropped.
pub(crate) fn translate_prompt(content: Vec<two::ContentBlock>) -> Vec<one::ContentBlock> {
    content
        .into_iter()
        .filter_map(|block| reencode(&block))
        .collect()
}

/// What a session's tool call said about itself so far: the fields a permission request about
/// it may leave out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KnownCall {
    /// The tool it calls, as the v2 path names it.
    name: Option<String>,
    /// Its title.
    title: Option<String>,
    /// Its kind.
    kind: Option<two::ToolKind>,
}

/// Records in `calls`, by id, what the translated `notification` says about a tool call: the
/// name, title and kind it sets. A call that completed or failed is forgotten, as no request
/// is about it any more. Any other update records nothing.
pub(crate) fn remember(
    calls: &mut HashMap<String, KnownCall>,
    notification: &two::UpdateSessionNotification,
) {
    if let two::SessionUpdate::ToolCallUpdate(call) = &notification.update {
        let id = call.tool_call_id.0.to_string();
        if matches!(
            call.status.value(),
            Some(two::ToolCallStatus::Completed | two::ToolCallStatus::Failed)
        ) {
            calls.remove(&id);
            return;
        }
        let known = calls.entry(id).or_default();
        learn(&mut known.name, &call.name);
        learn(&mut known.title, &call.title);
        learn(&mut known.kind, &call.kind);
    }
}

/// Makes `known` the value `field` sets, if it sets one.
fn learn<T: Clone>(known: &mut Option<T>, field: &MaybeUndefined<T>) {
    if let Some(field) = field.value() {
        *known = Some(field.clone());
    }
}

/// `field`, or the `known` value when `field` is undefined.
fn or_known<T>(field: MaybeUndefined<T>, known: Option<T>) -> MaybeUndefined<T> {
    match (field, known) {
        (MaybeUndefined::Undefined, Some(known)) => MaybeUndefined::Value(known),
        (field, _) => field,
    }
}

/// A v1 permission request as the v2 one the session code asks the user: titled with the tool
/// call's title, about that call, with the same options. The name, title and kind the call
/// leaves out are those `calls` knows for its id, so a request about a call of Partitur's own
/// tools names it; the known name only while the call's kind is `other`, the kind of every MCP
/// tool's call, so a request saying it edits or executes never passes for one of them.
pub(crate) fn translate_permission(
    request: one::RequestPermissionRequest,
    calls: &HashMap<String, KnownCall>,
) -> two::RequestPermissionRequest {
    let known = calls
        .get(&*request.tool_call.tool_call_id.0)
        .cloned()
        .unwrap_or_default();
    let options = request
        .options
        .iter()
        .filter_map(reencode::<_, two::PermissionOption>)
        .collect();
    let mut call = translate_tool_call_update(request.tool_call);
    call.title = or_known(call.title, known.title);
    call.kind = or_known(call.kind, known.kind);
    let mcp = matches!(
        call.kind,
        MaybeUndefined::Value(two::ToolKind::Other) | MaybeUndefined::Undefined
    );
    call.name = or_known(call.name, known.name.filter(|_| mcp));
    let title = call.title.value().cloned().unwrap_or_default();
    two::RequestPermissionRequest::new(request.session_id.0.to_string(), title, options)
        .subject(two::RequestPermissionSubject::from(call))
        .meta(request.meta)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    const SESSION: &str = "cp-1";

    fn meta(key: &str) -> one::Meta {
        one::Meta::from_iter([(key.to_owned(), json!(true))])
    }

    fn notification(update: one::SessionUpdate) -> one::SessionNotification {
        one::SessionNotification::new(SESSION, update)
    }

    /// The v2 update `update` translates into, failing when it is dropped.
    fn translated(update: one::SessionUpdate) -> two::SessionUpdate {
        let translated = translate_update(notification(update)).expect("translated");
        assert_eq!(translated.session_id, two::SessionId::new(SESSION));
        translated.update
    }

    fn text(text: &str) -> one::ContentBlock {
        one::ContentBlock::Text(one::TextContent::new(text))
    }

    #[test]
    fn a_tool_call_becomes_an_upsert() {
        let call = one::ToolCall::new("t1", "Read README.md")
            .name("view".to_owned())
            .kind(one::ToolKind::Read)
            .status(one::ToolCallStatus::InProgress)
            .content(vec![one::ToolCallContent::from(text("ok"))])
            .locations(vec![one::ToolCallLocation::new("/work/README.md").line(3)])
            .raw_input(json!({"path": "README.md"}))
            .raw_output(json!("done"))
            .meta(meta("call"));
        let expected = two::ToolCallUpdate::new("t1")
            .title("Read README.md".to_owned())
            .name("view".to_owned())
            .kind(two::ToolKind::Read)
            .status(two::ToolCallStatus::InProgress)
            .content(vec![two::ToolCallContent::from(two::ContentBlock::from(
                "ok",
            ))])
            .locations(vec![two::ToolCallLocation::new("/work/README.md").line(3)])
            .raw_input(json!({"path": "README.md"}))
            .raw_output(json!("done"))
            .meta(meta("call"));
        assert_eq!(
            translated(one::SessionUpdate::ToolCall(call)),
            two::SessionUpdate::ToolCallUpdate(expected)
        );
        // A bare call still sets its content and locations, which it has none of, and leaves
        // what it does not carry undefined.
        assert_eq!(
            translated(one::SessionUpdate::ToolCall(one::ToolCall::new("t2", "x"))),
            two::SessionUpdate::ToolCallUpdate(
                two::ToolCallUpdate::new("t2")
                    .title("x".to_owned())
                    .kind(two::ToolKind::Other)
                    .status(two::ToolCallStatus::Pending)
                    .content(Vec::new())
                    .locations(Vec::new())
            )
        );
    }

    #[test]
    fn a_tool_call_update_changes_only_the_fields_it_carries() {
        let fields = one::ToolCallUpdateFields::new()
            .status(one::ToolCallStatus::Completed)
            .content(vec![one::ToolCallContent::from(text("ok"))]);
        let update = one::ToolCallUpdate::new("t1", fields).meta(meta("update"));
        assert_eq!(
            translated(one::SessionUpdate::ToolCallUpdate(update)),
            two::SessionUpdate::ToolCallUpdate(
                two::ToolCallUpdate::new("t1")
                    .status(two::ToolCallStatus::Completed)
                    .content(vec![two::ToolCallContent::from(two::ContentBlock::from(
                        "ok"
                    ))])
                    .meta(meta("update"))
            )
        );
        let full = one::ToolCallUpdateFields::new()
            .kind(one::ToolKind::Edit)
            .title("Edit".to_owned())
            .name("edit".to_owned())
            .locations(vec![one::ToolCallLocation::new("/a")])
            .raw_input(json!(1))
            .raw_output(json!(2));
        assert_eq!(
            translate_tool_call_update(one::ToolCallUpdate::new("t2", full)),
            two::ToolCallUpdate::new("t2")
                .kind(two::ToolKind::Edit)
                .title("Edit".to_owned())
                .name("edit".to_owned())
                .locations(vec![two::ToolCallLocation::new("/a")])
                .raw_input(json!(1))
                .raw_output(json!(2))
        );
    }

    #[test]
    fn every_tool_kind_and_status_is_translated() {
        let kinds = [
            (one::ToolKind::Read, two::ToolKind::Read),
            (one::ToolKind::Edit, two::ToolKind::Edit),
            (one::ToolKind::Delete, two::ToolKind::Delete),
            (one::ToolKind::Move, two::ToolKind::Move),
            (one::ToolKind::Search, two::ToolKind::Search),
            (one::ToolKind::Execute, two::ToolKind::Execute),
            (one::ToolKind::Think, two::ToolKind::Think),
            (one::ToolKind::Fetch, two::ToolKind::Fetch),
            (one::ToolKind::SwitchMode, two::ToolKind::SwitchMode),
            (one::ToolKind::Other, two::ToolKind::Other),
        ];
        for (v1, v2) in kinds {
            assert_eq!(translate_kind(v1), v2);
        }
        let statuses = [
            (one::ToolCallStatus::Pending, two::ToolCallStatus::Pending),
            (
                one::ToolCallStatus::InProgress,
                two::ToolCallStatus::InProgress,
            ),
            (
                one::ToolCallStatus::Completed,
                two::ToolCallStatus::Completed,
            ),
            (one::ToolCallStatus::Failed, two::ToolCallStatus::Failed),
        ];
        for (v1, v2) in statuses {
            assert_eq!(translate_status(v1), v2);
        }
    }

    #[test]
    fn a_diff_becomes_a_whole_file_patch() {
        let edit = one::Diff::new("/work/a.rs", "one\nthree\n").old_text("one\ntwo\n".to_owned());
        assert_eq!(
            translate_content(one::ToolCallContent::Diff(edit.meta(meta("diff")))),
            Some(two::ToolCallContent::Diff(
                two::Diff::patch(
                    "--- /work/a.rs\n+++ /work/a.rs\n@@ -1,2 +1,2 @@\n-one\n-two\n+one\n+three\n",
                    vec![two::DiffChange::modify("/work/a.rs")],
                )
                .meta(meta("diff"))
            ))
        );
        let created = one::Diff::new("/work/b.rs", "new\n");
        assert_eq!(
            translate_content(one::ToolCallContent::Diff(created)),
            Some(two::ToolCallContent::Diff(two::Diff::patch(
                "--- /dev/null\n+++ /work/b.rs\n@@ -0,0 +1,1 @@\n+new\n",
                vec![two::DiffChange::add("/work/b.rs")],
            )))
        );
        assert_eq!(
            whole_file_patch(&PathBuf::from("/c"), Some("gone\n"), ""),
            "--- /c\n+++ /c\n@@ -1,1 +0,0 @@\n-gone\n"
        );
    }

    #[test]
    fn terminals_and_session_references_stay_what_they_are() {
        let terminal = one::ToolCallContent::Terminal(one::Terminal::new("term-1").meta(meta("t")));
        assert_eq!(
            translate_content(terminal),
            Some(two::ToolCallContent::Terminal(
                two::Terminal::new("term-1").meta(meta("t"))
            ))
        );
        let session =
            one::ToolCallContent::Session(one::SessionReference::new("sub-1").meta(meta("s")));
        assert_eq!(
            translate_content(session),
            Some(two::ToolCallContent::Session(
                two::SessionReference::new("sub-1").meta(meta("s"))
            ))
        );
        let content = one::ToolCallContent::Content(one::Content::new(text("hi")).meta(meta("c")));
        assert_eq!(
            translate_content(content),
            Some(two::ToolCallContent::Content(Box::new(
                two::Content::new(two::ContentBlock::from("hi")).meta(meta("c"))
            )))
        );
    }

    #[test]
    fn chunks_keep_their_kind_and_message() {
        let chunk = || one::ContentChunk::new(text("hi"));
        let expected = |id: &str| two::ContentChunk::new(two::ContentBlock::from("hi"), id);
        assert_eq!(
            translated(one::SessionUpdate::UserMessageChunk(chunk())),
            two::SessionUpdate::UserMessageChunk(expected(NO_MESSAGE_ID))
        );
        assert_eq!(
            translated(one::SessionUpdate::AgentMessageChunk(
                chunk()
                    .message_id(one::MessageId::new("m-1"))
                    .meta(meta("m"))
            )),
            two::SessionUpdate::AgentMessageChunk(expected("m-1").meta(meta("m")))
        );
        assert_eq!(
            translated(one::SessionUpdate::AgentThoughtChunk(chunk())),
            two::SessionUpdate::AgentThoughtChunk(expected(NO_MESSAGE_ID))
        );
    }

    #[test]
    fn a_plan_becomes_the_update_of_the_one_plan() {
        let plan = one::Plan::new(vec![one::PlanEntry::new(
            "Write the test",
            one::PlanEntryPriority::High,
            one::PlanEntryStatus::InProgress,
        )])
        .meta(meta("plan"));
        assert_eq!(
            translated(one::SessionUpdate::Plan(plan)),
            two::SessionUpdate::PlanUpdate(
                two::PlanUpdate::new(two::PlanUpdateContent::Items(two::PlanItems::new(
                    PLAN_ID,
                    vec![two::PlanEntry::new(
                        "Write the test",
                        two::PlanEntryPriority::High,
                        two::PlanEntryStatus::InProgress,
                    )],
                )))
                .meta(meta("plan"))
            )
        );
    }

    #[test]
    fn commands_keep_their_hint() {
        let commands = one::AvailableCommandsUpdate::new(vec![
            one::AvailableCommand::new("review", "Reviews the change")
                .input(one::AvailableCommandInput::Unstructured(
                    one::UnstructuredCommandInput::new("what to review").meta(meta("i")),
                ))
                .meta(meta("c")),
            one::AvailableCommand::new("plan", "Plans"),
        ])
        .meta(meta("u"));
        assert_eq!(
            translated(one::SessionUpdate::AvailableCommandsUpdate(commands)),
            two::SessionUpdate::AvailableCommandsUpdate(
                two::AvailableCommandsUpdate::new(vec![
                    two::AvailableCommand::Command(
                        two::Command::new("review", "Reviews the change")
                            .input(two::AvailableCommandInput::Text(
                                two::TextCommandInput::new("what to review").meta(meta("i"))
                            ))
                            .meta(meta("c"))
                    ),
                    two::AvailableCommand::Command(two::Command::new("plan", "Plans")),
                ])
                .meta(meta("u"))
            )
        );
    }

    /// Copilot's `mode` option, choices ungrouped, `#agent` current.
    fn v1_mode() -> one::SessionConfigOption {
        one::SessionConfigOption::select(
            "mode",
            "Mode",
            "#agent",
            vec![
                one::SessionConfigSelectOption::new("#agent", "Agent"),
                one::SessionConfigSelectOption::new("#plan", "Plan").description("Plans first"),
            ],
        )
        .category(one::SessionConfigOptionCategory::Mode)
        .description("How the agent works".to_owned())
        .meta(meta("mode"))
    }

    #[test]
    fn config_ids_are_renamed() {
        let grouped = one::SessionConfigOption::select(
            "model",
            "Model",
            "gpt-5",
            vec![
                one::SessionConfigSelectGroup::new(
                    "openai",
                    "OpenAI",
                    vec![one::SessionConfigSelectOption::new("gpt-5", "GPT-5")],
                )
                .meta(meta("group")),
            ],
        );
        let boolean = one::SessionConfigOption::boolean("fast", "Fast", true);
        let translated = translate_config(vec![v1_mode(), grouped, boolean]);
        assert_eq!(
            translated,
            [
                two::SessionConfigOption::select(
                    "mode",
                    "Mode",
                    "#agent",
                    vec![
                        two::SessionConfigSelectOption::new("#agent", "Agent"),
                        two::SessionConfigSelectOption::new("#plan", "Plan")
                            .description("Plans first"),
                    ],
                )
                .category(two::SessionConfigOptionCategory::Mode)
                .description("How the agent works".to_owned())
                .meta(meta("mode")),
                two::SessionConfigOption::select(
                    "model",
                    "Model",
                    "gpt-5",
                    vec![
                        two::SessionConfigSelectGroup::new(
                            "openai",
                            "OpenAI",
                            vec![two::SessionConfigSelectOption::new("gpt-5", "GPT-5")],
                        )
                        .meta(meta("group"))
                    ],
                ),
                two::SessionConfigOption::boolean("fast", "Fast", true),
            ]
        );
        // On the wire, the ids sit under their v2 names.
        let wire = serde_json::to_value(&translated).unwrap();
        assert_eq!(wire[0]["configId"], json!("mode"));
        assert_eq!(wire[1]["options"][0]["groupId"], json!("openai"));
        assert!(wire[0].get("id").is_none());
    }

    #[test]
    fn a_config_update_carries_the_translated_options() {
        let update = one::ConfigOptionUpdate::new(vec![v1_mode()]).meta(meta("u"));
        assert_eq!(
            translated(one::SessionUpdate::ConfigOptionUpdate(update)),
            two::SessionUpdate::ConfigOptionUpdate(
                two::ConfigOptionUpdate::new(translate_config(vec![v1_mode()])).meta(meta("u"))
            )
        );
    }

    #[test]
    fn session_info_usage_and_notices_keep_their_fields() {
        let info = one::SessionInfoUpdate::new()
            .title("Fix the login".to_owned())
            .updated_at("2026-11-20T10:00:00Z".to_owned())
            .meta(meta("i"));
        assert_eq!(
            translated(one::SessionUpdate::SessionInfoUpdate(info)),
            two::SessionUpdate::SessionInfoUpdate(
                two::SessionInfoUpdate::new()
                    .title("Fix the login".to_owned())
                    .updated_at("2026-11-20T10:00:00Z".to_owned())
                    .meta(meta("i"))
            )
        );
        let usage = one::UsageUpdate::new(10, 1000)
            .cost(one::Cost::new(0.5, "USD"))
            .meta(meta("u"));
        assert_eq!(
            translated(one::SessionUpdate::UsageUpdate(usage)),
            two::SessionUpdate::UsageUpdate(
                two::UsageUpdate::new(10, 1000)
                    .cost(two::Cost::new(0.5, "USD"))
                    .meta(meta("u"))
            )
        );
        let notice = one::Notice::new(one::NoticeSeverity::Warning, "Rate limited")
            .description("try later".to_owned())
            .meta(meta("n"));
        assert_eq!(
            translated(one::SessionUpdate::Notice(notice)),
            two::SessionUpdate::Notice(
                two::Notice::new(two::NoticeSeverity::Warning, "Rate limited")
                    .description("try later".to_owned())
                    .meta(meta("n"))
            )
        );
    }

    #[test]
    fn a_notification_keeps_its_meta() {
        let note = notification(one::SessionUpdate::AgentMessageChunk(
            one::ContentChunk::new(text("hi")),
        ))
        .meta(meta("n"));
        assert_eq!(translate_update(note).unwrap().meta, Some(meta("n")));
    }

    #[test]
    fn an_unknown_update_is_dropped() {
        let mode = one::SessionUpdate::CurrentModeUpdate(one::CurrentModeUpdate::new("#plan"));
        assert_eq!(translate_update(notification(mode)), None);
        let compaction: one::SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "compaction_update",
            "compactionId": "c-1",
            "status": "started",
        }))
        .unwrap();
        assert_eq!(translate_update(notification(compaction)), None);
    }

    #[test]
    fn a_prompt_runs_and_its_answer_ends_the_work() {
        let session = two::SessionId::new(SESSION);
        assert_eq!(
            running(session.clone()),
            two::UpdateSessionNotification::new(
                SESSION,
                two::SessionUpdate::StateUpdate(two::StateUpdate::Running(
                    two::RunningStateUpdate::new()
                ))
            )
        );
        let ended = |stop: two::StopReason| {
            two::UpdateSessionNotification::new(
                SESSION,
                two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
                    two::IdleStateUpdate::new()
                        .stop_reason(stop)
                        .background(Vec::new()),
                )),
            )
        };
        let reasons = [
            (one::StopReason::EndTurn, two::StopReason::EndTurn),
            (one::StopReason::MaxTokens, two::StopReason::MaxTokens),
            (
                one::StopReason::MaxTurnRequests,
                two::StopReason::MaxTurnRequests,
            ),
            (one::StopReason::Refusal, two::StopReason::Refusal),
            (one::StopReason::Cancelled, two::StopReason::Cancelled),
        ];
        for (v1, v2) in reasons {
            assert_eq!(idle_after_response(session.clone(), v1), ended(v2));
        }
        assert_eq!(
            idle_after_refusal(session),
            two::UpdateSessionNotification::new(
                SESSION,
                two::SessionUpdate::StateUpdate(two::StateUpdate::Idle(
                    two::IdleStateUpdate::new().background(Vec::new()),
                )),
            )
        );
    }

    /// Copilot's session event `kind` for [`SESSION`], with `extra` merged into its params.
    fn event(kind: &str, extra: &serde_json::Value) -> CopilotEvent {
        let mut params = json!({"sessionId": SESSION, "type": kind, "data": {}});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        CopilotEvent::parse(&params).expect("an event")
    }

    fn state(state: two::StateUpdate) -> two::UpdateSessionNotification {
        two::UpdateSessionNotification::new(SESSION, two::SessionUpdate::StateUpdate(state))
    }

    fn assistant_idle(stop: two::StopReason) -> two::UpdateSessionNotification {
        let work = two::BackgroundWork::Other(two::OtherBackgroundWork::new(
            COPILOT_SESSION_WORK,
            std::collections::BTreeMap::new(),
        ));
        state(two::StateUpdate::Idle(
            two::IdleStateUpdate::new()
                .stop_reason(stop)
                .background(vec![work]),
        ))
    }

    #[test]
    fn a_session_event_is_read_from_its_params() {
        let params = json!({
            "sessionId": SESSION,
            "type": "assistant.idle",
            "data": {"aborted": true},
            "agentId": "sub-1",
        });
        assert_eq!(
            CopilotEvent::parse(&params),
            Some(CopilotEvent {
                session: SESSION.to_owned(),
                kind: "assistant.idle".to_owned(),
                agent: Some("sub-1".to_owned()),
                aborted: true,
            })
        );
        let plain = event("session.idle", &json!({"agentId": null}));
        assert_eq!(
            plain,
            CopilotEvent {
                session: SESSION.to_owned(),
                kind: "session.idle".to_owned(),
                agent: None,
                aborted: false,
            }
        );
        // An agent id of another type still names a subagent.
        assert_eq!(
            event("x", &json!({"agentId": 7})).agent,
            Some("7".to_owned())
        );
        assert_eq!(CopilotEvent::parse(&json!({"type": "x"})), None);
        assert_eq!(CopilotEvent::parse(&json!({"sessionId": SESSION})), None);
    }

    #[test]
    fn a_turn_start_means_running() {
        assert_eq!(
            translate_event(&event("assistant.turn_start", &json!({}))),
            Some(state(two::StateUpdate::Running(
                two::RunningStateUpdate::new()
            )))
        );
    }

    #[test]
    fn an_assistant_idle_ends_the_turn_and_opens_the_session_work() {
        assert_eq!(
            translate_event(&event("assistant.idle", &json!({}))),
            Some(assistant_idle(two::StopReason::EndTurn))
        );
        assert_eq!(
            translate_event(&event(
                "assistant.idle",
                &json!({"data": {"aborted": true}})
            )),
            Some(assistant_idle(two::StopReason::Cancelled))
        );
        let wire =
            serde_json::to_value(translate_event(&event("assistant.idle", &json!({})))).unwrap();
        assert_eq!(
            wire["update"]["background"],
            json!([{"type": "_copilot/session"}])
        );
    }

    #[test]
    fn a_session_idle_ends_the_background_work_only() {
        assert_eq!(
            translate_event(&event("session.idle", &json!({}))),
            Some(state(two::StateUpdate::Idle(
                two::IdleStateUpdate::new().background(Vec::new())
            )))
        );
    }

    #[test]
    fn subagent_and_unknown_events_mean_nothing() {
        for kind in ["assistant.turn_start", "assistant.idle", "session.idle"] {
            assert_eq!(
                translate_event(&event(kind, &json!({"agentId": "sub-1"}))),
                None,
                "{kind}"
            );
        }
        assert_eq!(
            translate_event(&event("assistant.message", &json!({}))),
            None
        );
    }

    #[test]
    fn a_subagent_update_is_dropped() {
        let tagged = |tag: serde_json::Value| {
            let meta = json!({"github.com/copilot": tag});
            let chunk = one::ContentChunk::new(text("hi")).meta(meta.as_object().cloned());
            translate_update(notification(one::SessionUpdate::AgentMessageChunk(chunk)))
        };
        assert_eq!(tagged(json!({"agentId": "sub-1"})), None);
        assert!(tagged(json!({"agentId": null})).is_some());
        assert!(tagged(json!({"other": 1})).is_some());
        let call = one::ToolCall::new("t1", "x").meta(
            json!({"github.com/copilot": {"agentId": "sub-1"}})
                .as_object()
                .cloned(),
        );
        assert_eq!(
            translate_update(notification(one::SessionUpdate::ToolCall(call))),
            None
        );
    }

    #[test]
    fn kept_passes_values_and_drops_none() {
        // Every content block and severity of this version has a v2 form; one that has none
        // drops its update.
        assert_eq!(kept::<u8>(None, "notice"), None);
        assert_eq!(kept(Some(1), "notice"), Some(1));
    }

    #[test]
    fn the_opt_in_asks_for_copilots_session_events() {
        assert_eq!(
            serde_json::Value::Object(copilot_opt_in()),
            json!({"github.com/copilot": {
                "events": ["assistant.turn_start", "assistant.idle", "session.idle"]
            }})
        );
    }

    #[test]
    fn an_image_prompt_is_kept() {
        let prompt = crate::map::map_prompt(vec![
            partitur_core::PromptContent::Text("look".to_owned()),
            partitur_core::PromptContent::Image {
                mime: "image/png".to_owned(),
                base64: "aGk=".to_owned(),
            },
            partitur_core::PromptContent::FileLink {
                path: PathBuf::from("/work/a.rs"),
            },
        ]);
        assert_eq!(
            translate_prompt(prompt),
            [
                text("look"),
                one::ContentBlock::Image(one::ImageContent::new("aGk=", "image/png")),
                one::ContentBlock::ResourceLink(one::ResourceLink::new(
                    "a.rs",
                    "file:///work/a.rs"
                )),
            ]
        );
    }

    #[test]
    fn a_permission_request_is_asked_about_its_tool_call() {
        let call = one::ToolCallUpdate::new(
            "t1",
            one::ToolCallUpdateFields::new()
                .title("ask_user".to_owned())
                .kind(one::ToolKind::Other),
        );
        let request = one::RequestPermissionRequest::new(
            SESSION,
            call,
            vec![one::PermissionOption::new(
                "allow_once",
                "Allow",
                one::PermissionOptionKind::AllowOnce,
            )],
        )
        .meta(meta("r"));
        assert_eq!(
            translate_permission(request, &HashMap::new()),
            two::RequestPermissionRequest::new(
                SESSION,
                "ask_user",
                vec![two::PermissionOption::new(
                    "allow_once",
                    "Allow",
                    two::PermissionOptionKind::AllowOnce,
                )],
            )
            .subject(two::RequestPermissionSubject::from(
                two::ToolCallUpdate::new("t1")
                    .title("ask_user".to_owned())
                    .kind(two::ToolKind::Other)
            ))
            .meta(meta("r"))
        );
        let untitled = one::RequestPermissionRequest::new(
            SESSION,
            one::ToolCallUpdate::new("t2", one::ToolCallUpdateFields::new()),
            Vec::new(),
        );
        assert_eq!(translate_permission(untitled, &HashMap::new()).title, "");
    }

    #[test]
    fn a_copilot_title_names_partiturs_tools_only() {
        assert_eq!(
            tool_name("partitur-ask_user"),
            Some("mcp__partitur__ask_user".to_owned())
        );
        assert_eq!(tool_name("srv-ping"), None);
        assert_eq!(tool_name("Editing a.txt"), None);
        assert_eq!(tool_name("partitur"), None);
        assert_eq!(tool_name("partitur_ask_user"), None);
        assert_eq!(tool_name("partitur-"), None);
        assert_eq!(tool_name("partitur-notes for a.txt"), None);
        // A server of the user's named `partitur-dev`, with its tool `exec`.
        assert_eq!(tool_name("partitur-dev-exec"), None);
        assert_eq!(tool_name("partitur-Ask"), None);
        assert_eq!(
            tool_name("partitur-superpowers_v7_start_task"),
            Some("mcp__partitur__superpowers_v7_start_task".to_owned())
        );
        assert_eq!(
            tool_name("partitur-bmad_v6_12_start_workflow"),
            Some("mcp__partitur__bmad_v6_12_start_workflow".to_owned())
        );
    }

    #[test]
    fn a_built_in_call_titled_like_partiturs_tool_is_not_named_or_flagged() {
        for kind in [one::ToolKind::Execute, one::ToolKind::Edit] {
            let announced = translate_update(notification(one::SessionUpdate::ToolCall(
                one::ToolCall::new("b1", "partitur-ask_user").kind(kind),
            )))
            .unwrap();
            assert!(
                matches!(
                    &announced.update,
                    two::SessionUpdate::ToolCallUpdate(call) if call.name.is_undefined()
                ),
                "{kind:?}"
            );
            let mut calls = HashMap::new();
            remember(&mut calls, &announced);
            let request = one::RequestPermissionRequest::new(
                SESSION,
                one::ToolCallUpdate::new("b1", one::ToolCallUpdateFields::new()),
                Vec::new(),
            );
            let asked = translate_permission(request, &calls);
            assert!(
                matches!(
                    &asked.subject,
                    Some(two::RequestPermissionSubject::ToolCall(subject))
                        if subject.tool_call.name.is_undefined()
                ),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_named_call_that_turns_into_a_built_in_lends_no_name() {
        let mut calls = HashMap::new();
        remember(
            &mut calls,
            &translate_update(notification(one::SessionUpdate::ToolCall(
                one::ToolCall::new("p1", "partitur-ask_user").kind(one::ToolKind::Other),
            )))
            .unwrap(),
        );
        let ask = |fields: one::ToolCallUpdateFields| {
            let request = one::RequestPermissionRequest::new(
                SESSION,
                one::ToolCallUpdate::new("p1", fields),
                Vec::new(),
            );
            translate_permission(request, &calls).subject
        };
        let named = |kind: two::ToolKind, name: Option<&str>| {
            let call = two::ToolCallUpdate::new("p1")
                .title("partitur-ask_user".to_owned())
                .kind(kind);
            Some(two::RequestPermissionSubject::from(match name {
                Some(name) => call.name(name.to_owned()),
                None => call,
            }))
        };
        assert_eq!(
            ask(one::ToolCallUpdateFields::new().kind(one::ToolKind::Execute)),
            named(two::ToolKind::Execute, None)
        );
        assert_eq!(
            ask(one::ToolCallUpdateFields::new()),
            named(two::ToolKind::Other, Some("mcp__partitur__ask_user"))
        );
    }

    #[test]
    fn an_ended_call_is_forgotten() {
        for status in [one::ToolCallStatus::Completed, one::ToolCallStatus::Failed] {
            let mut calls = HashMap::new();
            for update in [
                one::SessionUpdate::ToolCall(one::ToolCall::new("p1", "partitur-ask_user")),
                one::SessionUpdate::ToolCallUpdate(one::ToolCallUpdate::new(
                    "p1",
                    one::ToolCallUpdateFields::new().status(status),
                )),
            ] {
                remember(&mut calls, &translate_update(notification(update)).unwrap());
            }
            assert_eq!(calls, HashMap::new(), "{status:?}");
        }
        // A call still running is kept.
        let mut calls = HashMap::new();
        remember(
            &mut calls,
            &translate_update(notification(one::SessionUpdate::ToolCallUpdate(
                one::ToolCallUpdate::new(
                    "p1",
                    one::ToolCallUpdateFields::new().status(one::ToolCallStatus::InProgress),
                ),
            )))
            .unwrap(),
        );
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn a_tool_call_titled_after_partiturs_tool_is_named_after_it() {
        let call = |title: &str| {
            one::SessionUpdate::ToolCall(one::ToolCall::new("t1", title).kind(one::ToolKind::Other))
        };
        let named = |name: Option<&str>| {
            let call = two::ToolCallUpdate::new("t1")
                .kind(two::ToolKind::Other)
                .status(two::ToolCallStatus::Pending)
                .content(Vec::new())
                .locations(Vec::new());
            match name {
                Some(name) => call.name(name.to_owned()),
                None => call,
            }
        };
        assert_eq!(
            translated(call("partitur-propose_children")),
            two::SessionUpdate::ToolCallUpdate(
                named(Some("mcp__partitur__propose_children"))
                    .title("partitur-propose_children".to_owned())
            )
        );
        assert_eq!(
            translated(call("srv-ping")),
            two::SessionUpdate::ToolCallUpdate(named(None).title("srv-ping".to_owned()))
        );
    }

    #[test]
    fn a_permission_request_takes_what_it_leaves_out_from_the_call() {
        let mut calls = HashMap::new();
        let announced = translate_update(notification(one::SessionUpdate::ToolCall(
            one::ToolCall::new("p1", "partitur-propose_children").kind(one::ToolKind::Other),
        )))
        .unwrap();
        remember(&mut calls, &announced);
        // A later update changes the title and keeps the rest.
        let changed = translate_update(notification(one::SessionUpdate::ToolCallUpdate(
            one::ToolCallUpdate::new(
                "p1",
                one::ToolCallUpdateFields::new().title("Proposing children".to_owned()),
            ),
        )))
        .unwrap();
        remember(&mut calls, &changed);
        // Anything else records nothing.
        remember(
            &mut calls,
            &translate_update(notification(one::SessionUpdate::AgentMessageChunk(
                one::ContentChunk::new(text("hi")),
            )))
            .unwrap(),
        );
        assert_eq!(
            calls,
            HashMap::from([(
                "p1".to_owned(),
                KnownCall {
                    name: Some("mcp__partitur__propose_children".to_owned()),
                    title: Some("Proposing children".to_owned()),
                    kind: Some(two::ToolKind::Other),
                }
            )])
        );
        let ask = |id: &'static str, fields: one::ToolCallUpdateFields| {
            let request = one::RequestPermissionRequest::new(
                SESSION,
                one::ToolCallUpdate::new(id, fields),
                Vec::new(),
            );
            translate_permission(request, &calls)
        };
        // Copilot's request about an MCP tool: titled with the bare tool name, nothing else.
        let titled = ask(
            "p1",
            one::ToolCallUpdateFields::new().title("propose_children".to_owned()),
        );
        assert_eq!(
            titled,
            two::RequestPermissionRequest::new(SESSION, "propose_children", Vec::new()).subject(
                two::RequestPermissionSubject::from(
                    two::ToolCallUpdate::new("p1")
                        .title("propose_children".to_owned())
                        .name("mcp__partitur__propose_children".to_owned())
                        .kind(two::ToolKind::Other)
                )
            )
        );
        // A request that says nothing about the call takes its title too.
        let bare = ask("p1", one::ToolCallUpdateFields::new());
        assert_eq!(bare.title, "Proposing children");
        // What the request says wins.
        let named = ask(
            "p1",
            one::ToolCallUpdateFields::new()
                .name("own".to_owned())
                .kind(one::ToolKind::Edit),
        );
        assert_eq!(
            named.subject,
            Some(two::RequestPermissionSubject::from(
                two::ToolCallUpdate::new("p1")
                    .title("Proposing children".to_owned())
                    .name("own".to_owned())
                    .kind(two::ToolKind::Edit)
            ))
        );
        // An unknown call keeps what the request says, nothing more.
        let unknown = ask("p2", one::ToolCallUpdateFields::new());
        assert_eq!(
            unknown.subject,
            Some(two::RequestPermissionSubject::from(
                two::ToolCallUpdate::new("p2")
            ))
        );
    }
}
