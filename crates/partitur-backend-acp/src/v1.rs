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
//!   gets the name `mcp__<server>__<tool>` the v2 path knows those tools by, when the agent's
//!   [`Quirks`] read titles ([`crate::copilot::titled_name`]). A `tool_call_update` sets the
//!   fields it carries and leaves the others undefined.
//! - Text, image and resource content of a tool call keeps its kind, a terminal and a session
//!   reference stay what they are, and a v1 diff, which holds the whole old and new text of
//!   one file, becomes a v2 diff of that file, added when it has no old text and modified
//!   otherwise, with a git patch that removes every old line and adds every new one.
//! - `plan` becomes a `plan_update` of the plan [`PLAN_ID`]: a v1 agent keeps one plan per
//!   session and sends it whole.
//! - `available_commands_update` lists the same commands, a command's unstructured input
//!   becoming a text input with the same hint.
//! - `config_option_update` carries the options translated by [`translate_config`]. The legacy
//!   `modes` of a session's answer are not folded into config options, so a v1 agent offering
//!   only the legacy `modes` block is reported with the notice that it does not keep asking.
//! - `session_info_update`, `usage_update` and `notice` carry the same fields.
//! - `current_mode_update` is dropped: the mode is a config option, whose change arrives as a
//!   `config_option_update` too. So is every kind Partitur shows nothing of (compaction,
//!   subagents' messages) and every kind this version does not know.
//! - An update a Copilot subagent sent is dropped by [`observe`] when the agent's [`Quirks`]
//!   drop them ([`crate::copilot::dropped`]). What a subagent's tool call says about itself is
//!   still recorded, as for any call, since Copilot asks permission for it naming only its id.
//!   A chunk or notice whose content has no v2 form is dropped too, and logged at debug level.
//!
//! A `session/request_permission` becomes the v2 request about the same tool call. Copilot
//! titles the request with the bare tool name and leaves out what the call already said, so
//! the name, title and kind it leaves out are those of the call reported before under the same
//! id ([`KnownCall`]), and Partitur's own tools are known as such; a name is taken over only
//! while the call's kind is still `other`. A call is forgotten once it completed or failed.
//!
//! A v1 agent has no `state_update`: [`running`] is what a prompt sent to it means, and
//! [`idle_after_response`] what the answer to that prompt means, with its stop reason and no
//! background work. GitHub Copilot CLI's session events, which report its state instead, are
//! Copilot's own and translated in [`crate::copilot`]; the answer to the prompt still means
//! what it does without them: once the turn already ended it changes nothing, and it ends a
//! cancelled turn, for which Copilot sends no idle.

use std::collections::HashMap;
use std::path::Path;

use agent_client_protocol::schema::{MaybeUndefined, ProtocolVersion};
use agent_client_protocol::v1;
use agent_client_protocol::v1::schema as one;
use agent_client_protocol::v2::schema as two;
use partitur_core::PermissionOptionId;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::copilot::{Quirks, dropped, titled_name};
use crate::permissions::Answer;

/// The message id of a v1 chunk that names none; v2 requires one, and Partitur reads none.
pub(crate) const NO_MESSAGE_ID: &str = "v1";
/// The id of a v1 agent's plan: it keeps one per session and sends it whole.
pub(crate) const PLAN_ID: &str = "plan";
/// `value`, or `None` with a warning naming the `kind` of the update dropped for it.
fn kept<T>(value: Option<T>, kind: &str) -> Option<T> {
    if value.is_none() {
        tracing::warn!(kind, "dropped a v1 update whose content has no v2 form");
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

/// The v2 update a v1 `session/update` notification means for an agent with `quirks`,
/// recorded in `calls` by [`remember`], or `None` for one Partitur shows nothing of: one
/// [`translate_update`] drops, and one the quirks drop as a subagent's. A subagent's tool call
/// is recorded all the same, since Copilot asks permission for it naming only its id.
pub(crate) fn observe(
    calls: &mut HashMap<String, KnownCall>,
    quirks: &Quirks,
    notification: one::SessionNotification,
) -> Option<two::UpdateSessionNotification> {
    let subagent = dropped(quirks, &notification.update);
    let translated = translate_update(notification, quirks)?;
    remember(calls, &translated);
    (!subagent).then_some(translated)
}

/// The v2 update a v1 `session/update` notification means for an agent with `quirks`, or
/// `None` for one Partitur reads nothing of. A subagent's update is translated too; [`observe`]
/// drops it.
pub(crate) fn translate_update(
    notification: one::SessionNotification,
    quirks: &Quirks,
) -> Option<two::UpdateSessionNotification> {
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
        one::SessionUpdate::ToolCall(call) => {
            two::SessionUpdate::ToolCallUpdate(created(call, quirks))
        }
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

/// The v2 upsert announcing the v1 tool call `call`, with every field it carries set, and,
/// for a call of kind `other` that carries no name, the name `quirks` take from its title
/// ([`titled_name`]).
fn created(call: one::ToolCall, quirks: &Quirks) -> two::ToolCallUpdate {
    let mcp = call.kind == one::ToolKind::Other;
    let name = call
        .name
        .or_else(|| mcp.then(|| titled_name(quirks, &call.title)).flatten());
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

/// Translates the MCP servers a session is opened with. An HTTP server keeps its name, URL,
/// headers and `_meta`; v1 spells out its empty header list, which v2 leaves out. A server of
/// any other kind is dropped: Partitur opens sessions with its HTTP tool server only.
pub(crate) fn translate_servers(servers: &[two::McpServer]) -> Vec<one::McpServer> {
    servers
        .iter()
        .filter_map(|server| match server {
            two::McpServer::Http(http) => Some(one::McpServer::Http(
                one::McpServerHttp::new(http.name.clone(), http.url.clone())
                    .headers(http.headers.iter().filter_map(reencode).collect())
                    .meta(http.meta.clone()),
            )),
            other => {
                tracing::warn!(?other, "a v1 session is opened without this MCP server");
                None
            }
        })
        .collect()
}

/// The v1 `initialize` carrying what the v2 `request` does, as the connector derives it to
/// reuse the connection the v2 one opened: the client's name and version and the capabilities'
/// `_meta`, with the boolean config options v1 asks a client to declare.
pub(crate) fn translate_initialize(request: &two::InitializeRequest) -> one::InitializeRequest {
    let session = one::ClientSessionCapabilities::new().config_options(
        one::SessionConfigOptionsCapabilities::new()
            .boolean(one::BooleanConfigOptionCapabilities::new()),
    );
    let info: Option<one::Implementation> = reencode(&request.info);
    one::InitializeRequest::new(ProtocolVersion::V1)
        .client_info(info)
        .client_capabilities(
            one::ClientCapabilities::new()
                .session(session)
                .meta(request.capabilities.meta.clone()),
        )
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
mod tests;
