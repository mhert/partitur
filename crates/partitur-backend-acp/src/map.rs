//! Pure translation between the ACP schema types and Partitur's core vocabulary.
//!
//! Nothing here performs I/O or allocates ids: permission ids and session routing belong to
//! the connection code, which feeds schema values in and forwards the core values out. The
//! only memory is a session's [`SessionState`], which ACP's upsert-style updates need to be
//! read at all.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema::{
    self, ContentBlock, DiffChangeOperation, ImageContent, ResourceLink, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionUpdate, StateUpdate, TerminalId, TextContent,
    ToolCallContent, ToolCallUpdate,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use partitur_core::{
    ConfigCategory, ConfigChoice, ConfigId, ConfigOption, ConfigValueId, PermissionKind,
    PermissionOption, PermissionOptionId, PromptContent, StopReason, ToolCallId, ToolContent,
    ToolKind, ToolStatus, Update,
};

/// Stands in for content blocks Partitur cannot render yet (images, audio, resources).
const NON_TEXT_PLACEHOLDER: &str = "[non-text content]";
/// What a failed turn reports when the agent gave no details.
const UNKNOWN_ERROR: &str = "the agent stopped with an error";

/// What one session's updates have established so far.
///
/// ACP reports tool calls and terminals as upserts and foreground work as snapshots, so
/// reading an update needs the ones before it: whether a tool call is new, which call shows a
/// terminal's output, and whether a turn is open. A session starts with none of these.
///
/// Every turn the core sees opened, by a prompt or by the agent's own work, is ended exactly
/// once: by the `idle` that ends the agent's foreground work, or by the refusal of the prompt
/// when no work of the agent's keeps the turn going.
#[derive(Debug, Default)]
pub struct SessionState {
    /// The tool calls already reported.
    calls: HashSet<String>,
    /// Whether the agent's foreground work is open.
    working: bool,
    /// Whether the core sees a turn open that has not been ended yet.
    turn: bool,
    /// How a prompt refused while the agent's work was open ends the turn, if that work goes
    /// idle without a stop reason of its own.
    refused: Option<Result<StopReason, String>>,
    /// The terminals seen so far, by id.
    terminals: HashMap<TerminalId, Terminal>,
}

/// A terminal's output and the tool call that shows it.
#[derive(Debug, Default)]
struct Terminal {
    /// The call whose content names the terminal, once one does.
    call: Option<ToolCallId>,
    /// The output bytes received so far.
    output: Vec<u8>,
}

impl Terminal {
    /// The output as text, invalid UTF-8 replaced.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

/// Translates one ACP session update into a core [`Update`], reading and updating `state`.
///
/// Returns `None` for updates Partitur ignores: plans, command lists, session info,
/// compaction progress, whole-message snapshots (the chunks carry the text, and the core
/// records user text itself), and tool call or terminal updates that change nothing
/// Partitur shows.
///
/// The first update of a tool call announces it; later ones change it. A tool call that
/// reports no content but a raw output shows that output as text: a JSON string as is, any
/// other value pretty-printed. Raw inputs are not shown. A terminal's output is shown as the
/// whole content of the tool call naming the terminal.
///
/// Foreground work reported while none is open starts a turn; an `idle` with a stop reason
/// while work is open ends it, as a failure for the `error` reason. An `idle` without a stop
/// reason ends nothing: it only says what is running in the background.
///
/// ```
/// use agent_client_protocol::v2::schema::{ContentBlock, ContentChunk, SessionUpdate, TextContent};
/// use partitur_backend_acp::map::{SessionState, map_update};
/// use partitur_core::Update;
///
/// let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("hi")), "message-1");
/// assert_eq!(
///     map_update(SessionUpdate::AgentMessageChunk(chunk), &mut SessionState::default()),
///     Some(Update::AgentText("hi".into()))
/// );
/// ```
pub fn map_update(update: SessionUpdate, state: &mut SessionState) -> Option<Update> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            Some(Update::AgentText(chunk_text(&chunk.content)))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            Some(Update::AgentThought(chunk_text(&chunk.content)))
        }
        SessionUpdate::ToolCallUpdate(update) => state.tool_call(update),
        SessionUpdate::TerminalUpdate(update) => {
            let output = match update.output {
                MaybeUndefined::Value(output) => Some(decode(&output.data)?),
                MaybeUndefined::Null => Some(Vec::new()),
                MaybeUndefined::Undefined => None,
            };
            state.terminal_output(update.terminal_id, output?, false)
        }
        SessionUpdate::TerminalOutputChunk(chunk) => {
            let bytes = decode(&chunk.data)?;
            state.terminal_output(chunk.terminal_id, bytes, true)
        }
        SessionUpdate::StateUpdate(update) => state.foreground(update),
        SessionUpdate::ConfigOptionUpdate(update) => Some(Update::ConfigOptions(
            map_config_options(&update.config_options),
        )),
        SessionUpdate::UsageUpdate(usage) => Some(Update::Usage {
            used: usage.used,
            size: usage.size,
        }),
        SessionUpdate::Notice(notice) => Some(Update::Notice(match notice.description {
            Some(description) => format!("{}: {description}", notice.title),
            None => notice.title,
        })),
        // Message snapshots, user chunks, plans, command lists, session info and compaction
        // updates carry nothing Partitur shows or records.
        _ => None,
    }
}

impl SessionState {
    /// Announces a tool call on its first update and changes it on later ones.
    fn tool_call(&mut self, update: ToolCallUpdate) -> Option<Update> {
        let id = ToolCallId::new(update.tool_call_id.to_string());
        let content = update.content.map_value(|content| {
            content
                .into_iter()
                .map(|content| self.call_content(&id, content))
                .collect::<Vec<_>>()
        });
        if self.calls.insert(id.as_str().to_owned()) {
            return Some(Update::ToolCall {
                id,
                title: update.title.take().unwrap_or_default(),
                kind: update.kind.value().map_or(ToolKind::Other, map_tool_kind),
                status: update
                    .status
                    .value()
                    .map_or(ToolStatus::Pending, map_tool_status),
                content: tool_output(content.take().unwrap_or_default(), update.raw_output.take()),
                locations: update
                    .locations
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|location| location.path.into_inner())
                    .collect(),
            });
        }
        let changed = Update::ToolCallUpdate {
            id,
            title: patch(update.title, Option::unwrap_or_default),
            kind: patch(update.kind, |kind| {
                kind.as_ref().map_or(ToolKind::Other, map_tool_kind)
            }),
            status: patch(update.status, |status| {
                status.as_ref().map_or(ToolStatus::Pending, map_tool_status)
            }),
            content: patch(content, Option::unwrap_or_default),
            raw_output: update.raw_output.take().map(raw_text),
        };
        let unchanged = matches!(
            changed,
            Update::ToolCallUpdate {
                title: None,
                kind: None,
                status: None,
                content: None,
                raw_output: None,
                ..
            }
        );
        (!unchanged).then_some(changed)
    }

    /// Translates one piece of the output of the tool call `id`.
    ///
    /// Text stays text, and a diff becomes the paths it changes and its patch text (empty
    /// when it has none). A terminal becomes the call's and shows its output so far. Anything
    /// else becomes a placeholder line.
    fn call_content(&mut self, id: &ToolCallId, content: ToolCallContent) -> ToolContent {
        match content {
            ToolCallContent::Diff(diff) => ToolContent::Patch {
                paths: diff.changes.into_iter().filter_map(changed_path).collect(),
                text: diff.patch.map(|patch| patch.text).unwrap_or_default(),
            },
            ToolCallContent::Terminal(terminal) => {
                let terminal = self.terminals.entry(terminal.terminal_id).or_default();
                terminal.call = Some(id.clone());
                ToolContent::Terminal(terminal.text())
            }
            ToolCallContent::Content(content) => ToolContent::Text(chunk_text(&content.content)),
            _ => ToolContent::Text(NON_TEXT_PLACEHOLDER.into()),
        }
    }

    /// Replaces the output of terminal `id` with `bytes`, or appends them if `append`, and
    /// shows the output as the whole content of the call naming the terminal, if one does.
    fn terminal_output(&mut self, id: TerminalId, bytes: Vec<u8>, append: bool) -> Option<Update> {
        let terminal = self.terminals.entry(id).or_default();
        if append {
            terminal.output.extend(bytes);
        } else {
            terminal.output = bytes;
        }
        Some(Update::ToolCallUpdate {
            id: terminal.call.clone()?,
            title: None,
            kind: None,
            status: None,
            content: Some(vec![ToolContent::Terminal(terminal.text())]),
            raw_output: None,
        })
    }

    /// Records that a prompt was sent: the core sees a turn open from now on.
    pub fn prompted(&mut self) {
        self.turn = true;
    }

    /// Settles a prompt the agent refused, `ending` being how its turn ends: cancelled, or
    /// failed with a message.
    ///
    /// With the agent's foreground work open, that work is the turn the core sees, whether
    /// it ran for the prompt or is a turn of the agent's own the prompt was held behind, so
    /// the refusal ends nothing yet: the `idle` ending the work ends the turn, and `ending`
    /// only stands in for the stop reason when that `idle` brings none. A turn that already
    /// ended, for example when the agent's work went idle before the refusal arrived, is not
    /// ended again.
    pub fn refused(&mut self, ending: Result<StopReason, String>) -> Option<Update> {
        if self.working {
            self.refused = Some(ending);
            return None;
        }
        std::mem::replace(&mut self.turn, false).then(|| turn_end(ending))
    }

    /// Follows the agent's foreground work: its start opens a turn, its end with a stop
    /// reason closes it. An `idle` without a stop reason closes it only after a refusal
    /// arrived while the work was open, with that refusal's ending; otherwise it, and a state
    /// this version does not know, change nothing.
    fn foreground(&mut self, update: StateUpdate) -> Option<Update> {
        match update {
            StateUpdate::Running(_) | StateUpdate::RequiresAction(_) => {
                self.turn = true;
                (!std::mem::replace(&mut self.working, true)).then_some(Update::TurnStarted)
            }
            StateUpdate::Idle(idle) if self.working => {
                let ending = match idle.stop_reason {
                    Some(reason) => map_stop_reason(reason),
                    None => self.refused.take()?,
                };
                self.working = false;
                self.turn = false;
                self.refused = None;
                Some(turn_end(ending))
            }
            _ => None,
        }
    }
}

/// The update ending a turn: with a stop reason, or as a failure with its message.
fn turn_end(ending: Result<StopReason, String>) -> Update {
    ending.map_or_else(Update::Failed, Update::TurnEnded)
}

/// A patch field as a change: `None` when absent, else `read` of the value, which is `None`
/// when the field was cleared, so `read` gives the value it is reset to.
fn patch<T, U>(field: MaybeUndefined<T>, read: impl FnOnce(Option<T>) -> U) -> Option<U> {
    match field {
        MaybeUndefined::Undefined => None,
        MaybeUndefined::Null => Some(read(None)),
        MaybeUndefined::Value(value) => Some(read(Some(value))),
    }
}

/// The bytes of a base64 payload, or `None` when it is not base64.
fn decode(data: &str) -> Option<Vec<u8>> {
    STANDARD.decode(data).ok()
}

/// The path a diff change leaves behind: the changed path, or the destination of a move or
/// copy. A change of a kind this version does not know names none.
fn changed_path(change: schema::DiffChange) -> Option<PathBuf> {
    let path = match change.operation {
        DiffChangeOperation::Add(change)
        | DiffChangeOperation::Delete(change)
        | DiffChangeOperation::Modify(change) => change.path,
        DiffChangeOperation::Move(change) | DiffChangeOperation::Copy(change) => change.path,
        _ => return None,
    };
    Some(path.into_inner())
}

/// Flattens ACP config options into core [`ConfigOption`]s.
///
/// Only select-style options are kept; boolean options are skipped. Grouped choices are
/// flattened in group order. The category maps `Model` to [`ConfigCategory::Model`],
/// `ThoughtLevel` to [`ConfigCategory::Effort`] and `Mode` to [`ConfigCategory::Mode`]; every
/// other category, and a missing one, becomes [`ConfigCategory::Other`] holding the category
/// name or, without a category, the option's name.
pub fn map_config_options(options: &[SessionConfigOption]) -> Vec<ConfigOption> {
    options.iter().filter_map(map_config_option).collect()
}

fn map_config_option(option: &SessionConfigOption) -> Option<ConfigOption> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let choices = map_select_options(&select.options);
    Some(ConfigOption {
        id: ConfigId::new(option.config_id.to_string()),
        name: option.name.clone(),
        category: map_config_category(option.category.as_ref(), &option.name),
        current: ConfigValueId::new(select.current_value.to_string()),
        choices,
    })
}

/// The choices of a select option, with grouped choices flattened in group order. An options
/// shape this version does not know yields no choices.
fn map_select_options(options: &SessionConfigSelectOptions) -> Vec<ConfigChoice> {
    if let SessionConfigSelectOptions::Grouped(groups) = options {
        return groups
            .iter()
            .flat_map(|group| group.options.iter().map(map_config_choice))
            .collect();
    }
    // Kept on one line so the defensive fallback for a shape this version cannot name does
    // not count as a separate, unreachable line.
    #[rustfmt::skip]
    let SessionConfigSelectOptions::Ungrouped(choices) = options else { return Vec::new() };
    choices.iter().map(map_config_choice).collect()
}

fn map_config_choice(choice: &SessionConfigSelectOption) -> ConfigChoice {
    ConfigChoice {
        id: ConfigValueId::new(choice.value.to_string()),
        name: choice.name.clone(),
    }
}

fn map_config_category(
    category: Option<&SessionConfigOptionCategory>,
    name: &str,
) -> ConfigCategory {
    match category {
        Some(SessionConfigOptionCategory::Model) => ConfigCategory::Model,
        Some(SessionConfigOptionCategory::ThoughtLevel) => ConfigCategory::Effort,
        Some(SessionConfigOptionCategory::Mode) => ConfigCategory::Mode,
        Some(SessionConfigOptionCategory::ModelConfig) => {
            ConfigCategory::Other("model_config".into())
        }
        Some(SessionConfigOptionCategory::Other(other)) => ConfigCategory::Other(other.clone()),
        Some(_) | None => ConfigCategory::Other(name.into()),
    }
}

/// Translates the options of an ACP permission request, keeping each option's id, label and
/// kind. An option kind this version does not know is treated as [`PermissionKind::RejectOnce`],
/// the safe reading.
pub fn map_permission_options(options: &[schema::PermissionOption]) -> Vec<PermissionOption> {
    options
        .iter()
        .map(|option| PermissionOption {
            id: PermissionOptionId::new(option.option_id.to_string()),
            name: option.name.clone(),
            kind: match option.kind {
                schema::PermissionOptionKind::AllowOnce => PermissionKind::AllowOnce,
                schema::PermissionOptionKind::AllowAlways => PermissionKind::AllowAlways,
                schema::PermissionOptionKind::RejectAlways => PermissionKind::RejectAlways,
                schema::PermissionOptionKind::RejectOnce | _ => PermissionKind::RejectOnce,
            },
        })
        .collect()
}

/// Translates why the agent's foreground work stopped.
///
/// The `error` reason is no stop reason but a failure: it yields the error's message, or a
/// generic one when the agent gave none. A reason this version does not know ends the turn
/// normally, as [`StopReason::EndTurn`].
///
/// # Errors
///
/// Returns the failure's message for the `error` reason.
pub fn map_stop_reason(reason: schema::StopReason) -> Result<StopReason, String> {
    Ok(match reason {
        schema::StopReason::MaxTokens => StopReason::MaxTokens,
        schema::StopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        schema::StopReason::Refusal => StopReason::Refusal,
        schema::StopReason::Cancelled => StopReason::Cancelled,
        schema::StopReason::Error(failure) => {
            return Err(failure
                .error
                .map_or_else(|| UNKNOWN_ERROR.to_owned(), |error| error.message));
        }
        schema::StopReason::EndTurn | _ => StopReason::EndTurn,
    })
}

/// Translates prompt parts into ACP content blocks.
///
/// Files are sent as `file://` resource links named after the file, so the agent reads them
/// itself instead of receiving their contents.
pub fn map_prompt(content: Vec<PromptContent>) -> Vec<ContentBlock> {
    content
        .into_iter()
        .map(|part| match part {
            PromptContent::Text(text) => ContentBlock::Text(TextContent::new(text)),
            PromptContent::Image { mime, base64 } => {
                ContentBlock::Image(ImageContent::new(base64, mime))
            }
            PromptContent::FileLink { path } => ContentBlock::ResourceLink(ResourceLink::new(
                file_name(&path),
                format!("file://{}", path.display()),
            )),
        })
        .collect()
}

/// A tool call's output: its content, or when it has none, its raw output as one text.
fn tool_output(content: Vec<ToolContent>, raw: Option<serde_json::Value>) -> Vec<ToolContent> {
    match raw {
        Some(raw) if content.is_empty() => vec![ToolContent::Text(raw_text(raw))],
        _ => content,
    }
}

/// A raw tool output as text: a string as is, anything else pretty-printed.
fn raw_text(raw: serde_json::Value) -> String {
    match raw {
        serde_json::Value::String(text) => text,
        other => serde_json::to_string_pretty(&other).unwrap_or_default(),
    }
}

/// Translates a tool kind. A kind this version does not know is [`ToolKind::Other`].
pub(crate) fn map_tool_kind(kind: &schema::ToolKind) -> ToolKind {
    match kind {
        schema::ToolKind::Read => ToolKind::Read,
        schema::ToolKind::Edit => ToolKind::Edit,
        schema::ToolKind::Delete => ToolKind::Delete,
        schema::ToolKind::Move => ToolKind::Move,
        schema::ToolKind::Search => ToolKind::Search,
        schema::ToolKind::Execute => ToolKind::Execute,
        schema::ToolKind::Think => ToolKind::Think,
        schema::ToolKind::Fetch => ToolKind::Fetch,
        schema::ToolKind::SwitchMode => ToolKind::SwitchMode,
        schema::ToolKind::Other | _ => ToolKind::Other,
    }
}

/// Translates a tool call status. A paused call is still in progress, a cancelled one has
/// failed, and a status this version does not know is [`ToolStatus::Pending`].
fn map_tool_status(status: &schema::ToolCallStatus) -> ToolStatus {
    match status {
        schema::ToolCallStatus::InProgress | schema::ToolCallStatus::Paused => {
            ToolStatus::InProgress
        }
        schema::ToolCallStatus::Completed => ToolStatus::Completed,
        schema::ToolCallStatus::Failed | schema::ToolCallStatus::Cancelled => ToolStatus::Failed,
        schema::ToolCallStatus::Pending | _ => ToolStatus::Pending,
    }
}

/// The text of a content block, or a placeholder for anything that is not text.
fn chunk_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        _ => NON_TEXT_PLACEHOLDER.into(),
    }
}

/// The last component of `path`, or the whole path when it has none.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use agent_client_protocol::schema::MaybeUndefined;
    use agent_client_protocol::v2::schema::{
        AgentMessage, AgentThought, ContentChunk, Diff, DiffChange, DiffChangeOperation,
        ErrorStopReason, IdleStateUpdate, ImageContent as SchemaImage, Notice, NoticeSeverity,
        OtherDiffChange, PermissionOptionKind, RequiresActionStateUpdate, RunningStateUpdate,
        SessionConfigSelectGroup, SessionInfoUpdate, StateUpdate, Terminal, TerminalExitStatus,
        TerminalOutput, TerminalOutputChunk, TerminalUpdate, ToolCallLocation, ToolCallUpdate,
        UsageUpdate, UserMessage,
    };

    use super::*;

    /// Maps `update` with a fresh session state.
    fn map(update: SessionUpdate) -> Option<Update> {
        map_update(update, &mut SessionState::default())
    }

    fn text_chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)), "m-1")
    }

    fn text_content(text: &str) -> ToolCallContent {
        ToolCallContent::from(ContentBlock::Text(TextContent::new(text)))
    }

    fn state(update: StateUpdate) -> SessionUpdate {
        SessionUpdate::StateUpdate(update)
    }

    fn running() -> SessionUpdate {
        state(StateUpdate::Running(RunningStateUpdate::new()))
    }

    fn idle(reason: Option<schema::StopReason>) -> SessionUpdate {
        state(StateUpdate::Idle(
            IdleStateUpdate::new().stop_reason(reason),
        ))
    }

    /// A tool call update for `id` that changes only `content`.
    fn content_update(id: &str, content: Vec<ToolContent>) -> Update {
        Update::ToolCallUpdate {
            id: ToolCallId::new(id),
            title: None,
            kind: None,
            status: None,
            content: Some(content),
            raw_output: None,
        }
    }

    #[test]
    fn agent_message_text_maps_to_agent_text() {
        assert_eq!(
            map(SessionUpdate::AgentMessageChunk(text_chunk("hello"))),
            Some(Update::AgentText("hello".into()))
        );
        let image = ContentChunk::new(
            ContentBlock::Image(SchemaImage::new("aGk=", "image/png")),
            "m-1",
        );
        assert_eq!(
            map(SessionUpdate::AgentMessageChunk(image)),
            Some(Update::AgentText("[non-text content]".into()))
        );
    }

    #[test]
    fn a_thought_chunk_maps_to_agent_thought() {
        assert_eq!(
            map(SessionUpdate::AgentThoughtChunk(text_chunk("hmm"))),
            Some(Update::AgentThought("hmm".into()))
        );
    }

    #[test]
    fn first_tool_call_update_is_a_tool_call() {
        let mut session = SessionState::default();
        let first = ToolCallUpdate::new("t-1")
            .title("Edit main.rs")
            .kind(schema::ToolKind::Edit)
            .status(schema::ToolCallStatus::InProgress)
            .content(vec![text_content("out")])
            .locations(vec![ToolCallLocation::new("/src/main.rs")]);
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(first), &mut session),
            Some(Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Edit main.rs".into(),
                kind: ToolKind::Edit,
                status: ToolStatus::InProgress,
                content: vec![ToolContent::Text("out".into())],
                locations: vec![PathBuf::from("/src/main.rs")],
            })
        );

        let cleared = ToolCallUpdate::new("t-1")
            .title(MaybeUndefined::Null)
            .kind(MaybeUndefined::Null)
            .status(MaybeUndefined::Null)
            .content(MaybeUndefined::Null);
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(cleared), &mut session),
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-1"),
                title: Some(String::new()),
                kind: Some(ToolKind::Other),
                status: Some(ToolStatus::Pending),
                content: Some(Vec::new()),
                raw_output: None,
            })
        );

        let sparse = ToolCallUpdate::new("t-1").status(schema::ToolCallStatus::Completed);
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(sparse), &mut session),
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-1"),
                title: None,
                kind: None,
                status: Some(ToolStatus::Completed),
                content: None,
                raw_output: None,
            })
        );
    }

    #[test]
    fn a_first_update_without_fields_starts_at_the_defaults() {
        for update in [
            ToolCallUpdate::new("t-1"),
            ToolCallUpdate::new("t-1")
                .title(MaybeUndefined::Null)
                .kind(MaybeUndefined::Null)
                .status(MaybeUndefined::Null)
                .content(MaybeUndefined::Null)
                .locations(MaybeUndefined::Null),
        ] {
            assert_eq!(
                map(SessionUpdate::ToolCallUpdate(update)),
                Some(Update::ToolCall {
                    id: ToolCallId::new("t-1"),
                    title: String::new(),
                    kind: ToolKind::Other,
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    locations: Vec::new(),
                })
            );
        }
    }

    #[test]
    fn tool_calls_are_told_apart_by_id() {
        let mut session = SessionState::default();
        let first = |id: &str| SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id).title("x"));
        assert!(matches!(
            map_update(first("t-1"), &mut session),
            Some(Update::ToolCall { .. })
        ));
        assert!(matches!(
            map_update(first("t-2"), &mut session),
            Some(Update::ToolCall { .. })
        ));
        assert!(matches!(
            map_update(first("t-1"), &mut session),
            Some(Update::ToolCallUpdate { .. })
        ));
    }

    #[test]
    fn every_tool_kind_and_status_maps() {
        let kinds = [
            (schema::ToolKind::Read, ToolKind::Read),
            (schema::ToolKind::Edit, ToolKind::Edit),
            (schema::ToolKind::Delete, ToolKind::Delete),
            (schema::ToolKind::Move, ToolKind::Move),
            (schema::ToolKind::Search, ToolKind::Search),
            (schema::ToolKind::Execute, ToolKind::Execute),
            (schema::ToolKind::Think, ToolKind::Think),
            (schema::ToolKind::Fetch, ToolKind::Fetch),
            (schema::ToolKind::SwitchMode, ToolKind::SwitchMode),
            (schema::ToolKind::Other, ToolKind::Other),
            (schema::ToolKind::Unknown("_x".into()), ToolKind::Other),
        ];
        for (schema_kind, kind) in kinds {
            assert_eq!(map_tool_kind(&schema_kind), kind);
        }
        let statuses = [
            (schema::ToolCallStatus::Pending, ToolStatus::Pending),
            (schema::ToolCallStatus::InProgress, ToolStatus::InProgress),
            (schema::ToolCallStatus::Paused, ToolStatus::InProgress),
            (schema::ToolCallStatus::Completed, ToolStatus::Completed),
            (schema::ToolCallStatus::Failed, ToolStatus::Failed),
            (schema::ToolCallStatus::Cancelled, ToolStatus::Failed),
            (
                schema::ToolCallStatus::Other("_x".into()),
                ToolStatus::Pending,
            ),
        ];
        for (schema_status, status) in statuses {
            assert_eq!(map_tool_status(&schema_status), status);
        }
    }

    #[test]
    fn a_tool_call_update_that_changes_nothing_shown_is_dropped() {
        let mut session = SessionState::default();
        map_update(
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
            &mut session,
        );
        for update in [
            ToolCallUpdate::new("t-1"),
            ToolCallUpdate::new("t-1")
                .locations(vec![ToolCallLocation::new("/a.rs")])
                .raw_input(serde_json::json!({"command": "ls"}))
                .raw_output(MaybeUndefined::Null),
        ] {
            assert_eq!(
                map_update(SessionUpdate::ToolCallUpdate(update), &mut session),
                None
            );
        }
    }

    #[test]
    fn a_tool_call_update_with_any_shown_field_is_forwarded() {
        let mut session = SessionState::default();
        map_update(
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
            &mut session,
        );
        let mut forwarded = |update: ToolCallUpdate| {
            map_update(SessionUpdate::ToolCallUpdate(update), &mut session)
        };
        let update = |title: Option<&str>, kind, content: Option<Vec<ToolContent>>| {
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-1"),
                title: title.map(str::to_owned),
                kind,
                status: None,
                content,
                raw_output: None,
            })
        };
        assert_eq!(
            forwarded(ToolCallUpdate::new("t-1").title("Run")),
            update(Some("Run"), None, None)
        );
        assert_eq!(
            forwarded(ToolCallUpdate::new("t-1").kind(schema::ToolKind::Read)),
            update(None, Some(ToolKind::Read), None)
        );
        assert_eq!(
            forwarded(ToolCallUpdate::new("t-1").content(Vec::new())),
            update(None, None, Some(Vec::new()))
        );
    }

    #[test]
    fn a_raw_output_is_mapped_next_to_the_content_never_into_it() {
        let mut session = SessionState::default();
        map_update(
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1")),
            &mut session,
        );
        let mut output = |update: ToolCallUpdate| {
            map_update(SessionUpdate::ToolCallUpdate(update), &mut session)
        };
        let update = |content: Option<Vec<ToolContent>>, raw_output: &str| {
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-1"),
                title: None,
                kind: None,
                status: None,
                content,
                raw_output: Some(raw_output.into()),
            })
        };
        assert_eq!(
            output(ToolCallUpdate::new("t-1").raw_output(serde_json::json!("total 0\n"))),
            update(None, "total 0\n")
        );
        assert_eq!(
            output(
                ToolCallUpdate::new("t-1")
                    .content(Vec::new())
                    .raw_output(serde_json::json!({"exit": 0}))
            ),
            update(Some(Vec::new()), "{\n  \"exit\": 0\n}")
        );
        assert_eq!(
            output(
                ToolCallUpdate::new("t-1")
                    .content(vec![text_content("shown")])
                    .raw_output(serde_json::json!("hidden"))
            ),
            update(Some(vec![ToolContent::Text("shown".into())]), "hidden")
        );

        let call = |id: &str, content: Vec<ToolContent>| {
            Some(Update::ToolCall {
                id: ToolCallId::new(id),
                title: "List".into(),
                kind: ToolKind::Other,
                status: ToolStatus::Pending,
                content,
                locations: Vec::new(),
            })
        };
        let listed = ToolCallUpdate::new("t-2")
            .title("List")
            .raw_output(serde_json::json!(["a", "b"]));
        assert_eq!(
            map(SessionUpdate::ToolCallUpdate(listed)),
            call(
                "t-2",
                vec![ToolContent::Text("[\n  \"a\",\n  \"b\"\n]".into())]
            )
        );
        let shown = ToolCallUpdate::new("t-4")
            .title("List")
            .content(vec![text_content("shown")])
            .raw_output(serde_json::json!("hidden"));
        assert_eq!(
            map(SessionUpdate::ToolCallUpdate(shown)),
            call("t-4", vec![ToolContent::Text("shown".into())])
        );
    }

    #[test]
    fn diff_maps_to_patch() {
        let patched = Diff::patch("@@ -1 +1 @@\n-a\n+b\n", vec![DiffChange::modify("/a.rs")]);
        assert_eq!(
            map_content(ToolCallContent::Diff(patched)),
            ToolContent::Patch {
                paths: vec![PathBuf::from("/a.rs")],
                text: "@@ -1 +1 @@\n-a\n+b\n".into(),
            }
        );
        let unpatched = Diff::new(vec![
            DiffChange::add("/new.rs"),
            DiffChange::delete("/old.rs"),
            DiffChange::move_file("/from.rs", "/to.rs"),
            DiffChange::copy("/src.rs", "/copy.rs"),
            DiffChange::new(DiffChangeOperation::Other(OtherDiffChange::new(
                "_rewrite",
                BTreeMap::default(),
            ))),
        ]);
        assert_eq!(
            map_content(ToolCallContent::Diff(unpatched)),
            ToolContent::Patch {
                paths: ["/new.rs", "/old.rs", "/to.rs", "/copy.rs"]
                    .map(PathBuf::from)
                    .to_vec(),
                text: String::new(),
            }
        );
    }

    /// Maps one piece of tool content with a fresh session state.
    fn map_content(content: ToolCallContent) -> ToolContent {
        SessionState::default().call_content(&ToolCallId::new("t-1"), content)
    }

    #[test]
    fn non_text_tool_content_becomes_a_placeholder() {
        let image =
            ToolCallContent::from(ContentBlock::Image(SchemaImage::new("aGk=", "image/png")));
        assert_eq!(
            map_content(image),
            ToolContent::Text("[non-text content]".into())
        );
        let other = ToolCallContent::Other(schema::OtherToolCallContent::new(
            "_widget",
            BTreeMap::default(),
        ));
        assert_eq!(
            map_content(other),
            ToolContent::Text("[non-text content]".into())
        );
    }

    fn terminal(id: &str) -> TerminalUpdate {
        TerminalUpdate::new(id)
    }

    fn output(id: &str, base64: &str) -> SessionUpdate {
        SessionUpdate::TerminalUpdate(terminal(id).output(TerminalOutput::new(base64)))
    }

    fn names_terminal(call: &str, terminal: &str) -> SessionUpdate {
        SessionUpdate::ToolCallUpdate(
            ToolCallUpdate::new(call)
                .content(vec![ToolCallContent::Terminal(Terminal::new(terminal))]),
        )
    }

    #[test]
    fn terminal_output_fills_its_tool_call() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        // The terminal is announced before any call names it.
        assert_eq!(map(SessionUpdate::TerminalUpdate(terminal("term-1"))), None);
        assert_eq!(
            map(names_terminal("t-1", "term-1")),
            Some(Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: String::new(),
                kind: ToolKind::Other,
                status: ToolStatus::Pending,
                content: vec![ToolContent::Terminal(String::new())],
                locations: Vec::new(),
            })
        );
        assert_eq!(
            map(SessionUpdate::TerminalUpdate(
                terminal("term-1").command("ls")
            )),
            None
        );
        assert_eq!(
            map(output("term-1", "bm90ZXMudHh0Cg==")),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("notes.txt\n".into())]
            ))
        );
        assert_eq!(
            map(SessionUpdate::TerminalUpdate(
                terminal("term-1").exit_status(TerminalExitStatus::new().exit_code(0))
            )),
            None
        );
    }

    #[test]
    fn terminal_output_snapshots_replace_and_chunks_append() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        map(names_terminal("t-1", "term-1"));
        assert_eq!(
            map(output("term-1", "YQ==")),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("a".into())]
            ))
        );
        let chunk = TerminalOutputChunk::new("term-1", "Yg==");
        assert_eq!(
            map(SessionUpdate::TerminalOutputChunk(chunk)),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("ab".into())]
            ))
        );
        assert_eq!(
            map(output("term-1", "Yw==")),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("c".into())]
            ))
        );
        assert_eq!(
            map(SessionUpdate::TerminalUpdate(
                terminal("term-1").output(MaybeUndefined::Null)
            )),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal(String::new())]
            ))
        );
        // A payload that is not base64 changes nothing.
        assert_eq!(map(output("term-1", "not base64!")), None);
        let broken = TerminalOutputChunk::new("term-1", "not base64!");
        assert_eq!(map(SessionUpdate::TerminalOutputChunk(broken)), None);
    }

    #[test]
    fn terminal_output_waits_for_a_call_naming_its_terminal() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        assert_eq!(map(output("term-1", "aGk=")), None);
        let chunk = TerminalOutputChunk::new("term-1", "IQ==");
        assert_eq!(map(SessionUpdate::TerminalOutputChunk(chunk)), None);
        assert!(map(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("t-1"))).is_some());
        assert_eq!(
            map(names_terminal("t-1", "term-1")),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("hi!".into())]
            ))
        );
    }

    #[test]
    fn multi_byte_output_split_across_chunks_decodes_whole() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        map(names_terminal("t-1", "term-1"));
        // "é" is 0xC3 0xA9; each chunk carries one of its bytes.
        map(SessionUpdate::TerminalOutputChunk(
            TerminalOutputChunk::new("term-1", "ww=="),
        ));
        assert_eq!(
            map(SessionUpdate::TerminalOutputChunk(
                TerminalOutputChunk::new("term-1", "qQ==",)
            )),
            Some(content_update(
                "t-1",
                vec![ToolContent::Terminal("é".into())]
            ))
        );
    }

    #[test]
    fn idle_with_stop_reason_after_running_ends_turn() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        assert_eq!(map(running()), Some(Update::TurnStarted));
        assert_eq!(map(running()), None);
        assert_eq!(
            map(state(StateUpdate::RequiresAction(
                RequiresActionStateUpdate::new()
            ))),
            None
        );
        assert_eq!(
            map(idle(Some(schema::StopReason::EndTurn))),
            Some(Update::TurnEnded(StopReason::EndTurn))
        );
        // A turn that starts blocked on the user is a turn too.
        assert_eq!(
            map(state(StateUpdate::RequiresAction(
                RequiresActionStateUpdate::new()
            ))),
            Some(Update::TurnStarted)
        );
    }

    #[test]
    fn idle_error_fails_turn() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        map(running());
        let error = ErrorStopReason::new().error(schema::Error::new(-32000, "sign in"));
        assert_eq!(
            map(idle(Some(schema::StopReason::Error(error)))),
            Some(Update::Failed("sign in".into()))
        );
        map(running());
        assert_eq!(
            map(idle(Some(
                schema::StopReason::Error(ErrorStopReason::new())
            ))),
            Some(Update::Failed("the agent stopped with an error".into()))
        );
    }

    #[test]
    fn idle_without_stop_reason_ends_nothing() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        map(running());
        assert_eq!(map(idle(None)), None);
        // The turn is still open: another `running` starts nothing new.
        assert_eq!(map(running()), None);
        assert_eq!(
            map(idle(Some(schema::StopReason::EndTurn))),
            Some(Update::TurnEnded(StopReason::EndTurn))
        );
        assert_eq!(map(idle(Some(schema::StopReason::EndTurn))), None);
        assert_eq!(map(idle(None)), None);
        assert_eq!(map(running()), Some(Update::TurnStarted));
    }

    #[test]
    fn a_refused_prompt_without_work_ends_its_turn_once() {
        let mut session = SessionState::default();
        assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
        session.prompted();
        // A snapshot `idle` before any work does not end the prompt's turn.
        assert_eq!(
            map_update(idle(Some(schema::StopReason::EndTurn)), &mut session),
            None
        );
        assert_eq!(
            session.refused(Ok(StopReason::Cancelled)),
            Some(Update::TurnEnded(StopReason::Cancelled))
        );
        assert_eq!(session.refused(Err("again".into())), None);
    }

    #[test]
    fn a_refused_prompt_during_work_leaves_the_end_to_its_idle() {
        let mut session = SessionState::default();
        session.prompted();
        assert_eq!(
            map_update(running(), &mut session),
            Some(Update::TurnStarted)
        );
        assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
        assert_eq!(
            map_update(idle(Some(schema::StopReason::EndTurn)), &mut session),
            Some(Update::TurnEnded(StopReason::EndTurn))
        );
        assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
        // The refusal is spent: a later idle without a stop reason ends nothing.
        assert_eq!(
            map_update(running(), &mut session),
            Some(Update::TurnStarted)
        );
        assert_eq!(map_update(idle(None), &mut session), None);
    }

    #[test]
    fn a_refusal_ends_work_that_goes_idle_without_a_stop_reason() {
        let mut session = SessionState::default();
        map_update(running(), &mut session);
        assert_eq!(session.refused(Err("sign in".into())), None);
        assert_eq!(
            map_update(idle(None), &mut session),
            Some(Update::Failed("sign in".into()))
        );
        assert_eq!(session.refused(Err("sign in".into())), None);
        assert_eq!(
            map_update(running(), &mut session),
            Some(Update::TurnStarted)
        );
    }

    #[test]
    fn a_refusal_after_the_work_went_idle_ends_nothing() {
        let mut session = SessionState::default();
        session.prompted();
        map_update(running(), &mut session);
        assert_eq!(
            map_update(idle(Some(schema::StopReason::Cancelled)), &mut session),
            Some(Update::TurnEnded(StopReason::Cancelled))
        );
        assert_eq!(session.refused(Ok(StopReason::Cancelled)), None);
    }

    #[test]
    fn an_unknown_state_changes_nothing() {
        let mut session = SessionState::default();
        let mut map = |update| map_update(update, &mut session);
        let other: StateUpdate =
            serde_json::from_value(serde_json::json!({"state": "_paused"})).unwrap();
        assert_eq!(map(state(other.clone())), None);
        map(running());
        assert_eq!(map(state(other)), None);
        assert_eq!(
            map(idle(Some(schema::StopReason::Cancelled))),
            Some(Update::TurnEnded(StopReason::Cancelled))
        );
    }

    #[test]
    fn every_stop_reason_maps() {
        let reasons = [
            (schema::StopReason::EndTurn, StopReason::EndTurn),
            (schema::StopReason::MaxTokens, StopReason::MaxTokens),
            (
                schema::StopReason::MaxTurnRequests,
                StopReason::MaxTurnRequests,
            ),
            (schema::StopReason::Refusal, StopReason::Refusal),
            (schema::StopReason::Cancelled, StopReason::Cancelled),
            (
                schema::StopReason::Other(schema::OtherStopReason::new(
                    "_paused",
                    BTreeMap::default(),
                )),
                StopReason::EndTurn,
            ),
        ];
        for (schema_reason, reason) in reasons {
            assert_eq!(map_stop_reason(schema_reason), Ok(reason));
        }
        let error = ErrorStopReason::new().error(schema::Error::new(-32000, "sign in"));
        assert_eq!(
            map_stop_reason(schema::StopReason::Error(error)),
            Err("sign in".to_owned())
        );
    }

    #[test]
    fn notice_keeps_title_and_description() {
        assert_eq!(
            map(SessionUpdate::Notice(Notice::new(
                NoticeSeverity::Info,
                "Heads up"
            ))),
            Some(Update::Notice("Heads up".into()))
        );
        assert_eq!(
            map(SessionUpdate::Notice(
                Notice::new(NoticeSeverity::Warning, "Heads up").description("Be careful")
            )),
            Some(Update::Notice("Heads up: Be careful".into()))
        );
    }

    #[test]
    fn usage_and_config_updates_map() {
        assert_eq!(
            map(SessionUpdate::UsageUpdate(UsageUpdate::new(10, 200))),
            Some(Update::Usage {
                used: 10,
                size: 200
            })
        );
        let option = SessionConfigOption::select(
            "model",
            "Model",
            "a",
            vec![SessionConfigSelectOption::new("a", "A")],
        );
        let mapped = map(SessionUpdate::ConfigOptionUpdate(
            schema::ConfigOptionUpdate::new(vec![option.clone()]),
        ));
        assert_eq!(
            mapped,
            Some(Update::ConfigOptions(map_config_options(&[option])))
        );
        assert!(matches!(mapped, Some(Update::ConfigOptions(options)) if options.len() == 1));
    }

    #[test]
    fn ignored_variants_map_to_none() {
        let content = vec![ContentBlock::from("whole")];
        for update in [
            SessionUpdate::UserMessageChunk(text_chunk("echo")),
            SessionUpdate::UserMessage(UserMessage::new("m-1").content(content.clone())),
            SessionUpdate::AgentMessage(AgentMessage::new("m-2").content(content.clone())),
            SessionUpdate::AgentThought(AgentThought::new("m-3").content(content)),
            SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new().title("t")),
        ] {
            assert_eq!(map(update), None);
        }
    }

    fn select(
        id: &str,
        category: Option<SessionConfigOptionCategory>,
        current: &str,
        values: &[&str],
    ) -> SessionConfigOption {
        SessionConfigOption::select(
            id.to_owned(),
            id.to_uppercase(),
            current.to_owned(),
            values
                .iter()
                .map(|value| {
                    SessionConfigSelectOption::new(value.to_string(), value.to_uppercase())
                })
                .collect::<Vec<_>>(),
        )
        .category(category)
    }

    fn category_of(option: &ConfigOption) -> ConfigCategory {
        option.category.clone()
    }

    #[test]
    fn config_categories_map_model_thought_level_and_mode() {
        let options = vec![
            select(
                "model",
                Some(SessionConfigOptionCategory::Model),
                "m1",
                &["m1", "m2"],
            ),
            select(
                "effort",
                Some(SessionConfigOptionCategory::ThoughtLevel),
                "low",
                &["low"],
            ),
            select(
                "mode",
                Some(SessionConfigOptionCategory::Mode),
                "default",
                &["default"],
            ),
            select("agent", None, "default", &["default", "web-fetch"]),
            select(
                "tuning",
                Some(SessionConfigOptionCategory::ModelConfig),
                "x",
                &["x"],
            ),
            select(
                "custom",
                Some(SessionConfigOptionCategory::Other("_vendor".into())),
                "x",
                &["x"],
            ),
            SessionConfigOption::boolean("flag", "Flag", true),
        ];
        let mapped = map_config_options(&options);
        assert_eq!(
            mapped.iter().map(category_of).collect::<Vec<_>>(),
            [
                ConfigCategory::Model,
                ConfigCategory::Effort,
                ConfigCategory::Mode,
                ConfigCategory::Other("AGENT".into()),
                ConfigCategory::Other("model_config".into()),
                ConfigCategory::Other("_vendor".into()),
            ]
        );
        let model = &mapped[0];
        assert_eq!(model.id, ConfigId::new("model"));
        assert_eq!(model.name, "MODEL");
        assert_eq!(model.current, ConfigValueId::new("m1"));
        assert_eq!(
            model.choices,
            [
                ConfigChoice {
                    id: ConfigValueId::new("m1"),
                    name: "M1".into()
                },
                ConfigChoice {
                    id: ConfigValueId::new("m2"),
                    name: "M2".into()
                },
            ]
        );
    }

    #[test]
    fn grouped_select_options_are_flattened() {
        let groups = vec![
            SessionConfigSelectGroup::new(
                "g1",
                "First",
                vec![SessionConfigSelectOption::new("a", "A")],
            ),
            SessionConfigSelectGroup::new(
                "g2",
                "Second",
                vec![
                    SessionConfigSelectOption::new("b", "B"),
                    SessionConfigSelectOption::new("c", "C"),
                ],
            ),
        ];
        let option = SessionConfigOption::select("model", "Model", "b", groups);
        let mapped = map_config_options(&[option]);
        let ids: Vec<_> = mapped[0]
            .choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn permission_options_keep_id_name_and_kind() {
        let kinds = [
            (PermissionOptionKind::AllowOnce, PermissionKind::AllowOnce),
            (
                PermissionOptionKind::AllowAlways,
                PermissionKind::AllowAlways,
            ),
            (PermissionOptionKind::RejectOnce, PermissionKind::RejectOnce),
            (
                PermissionOptionKind::RejectAlways,
                PermissionKind::RejectAlways,
            ),
            (
                PermissionOptionKind::Other("_maybe".into()),
                PermissionKind::RejectOnce,
            ),
        ];
        let options: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(index, (kind, _))| {
                schema::PermissionOption::new(
                    format!("opt-{index}"),
                    format!("Option {index}"),
                    kind.clone(),
                )
            })
            .collect();
        let mapped = map_permission_options(&options);
        assert_eq!(mapped.len(), kinds.len());
        for (index, (option, (_, expected))) in mapped.iter().zip(kinds).enumerate() {
            assert_eq!(option.id, PermissionOptionId::new(format!("opt-{index}")));
            assert_eq!(option.name, format!("Option {index}"));
            assert_eq!(option.kind, expected);
        }
    }

    #[test]
    fn prompt_content_maps_text_image_and_file_link() {
        let mapped = map_prompt(vec![
            PromptContent::Text("hi".into()),
            PromptContent::Image {
                mime: "image/png".into(),
                base64: "aGk=".into(),
            },
            PromptContent::FileLink {
                path: PathBuf::from("/work/src/main.rs"),
            },
            PromptContent::FileLink {
                path: PathBuf::from("/"),
            },
        ]);
        assert_eq!(
            mapped,
            vec![
                ContentBlock::Text(TextContent::new("hi")),
                ContentBlock::Image(SchemaImage::new("aGk=", "image/png")),
                ContentBlock::ResourceLink(ResourceLink::new(
                    "main.rs",
                    "file:///work/src/main.rs"
                )),
                ContentBlock::ResourceLink(ResourceLink::new("/", "file:///")),
            ]
        );
    }
}
