//! Pure translation between the ACP schema types and Partitur's core vocabulary.
//!
//! Nothing here performs I/O or allocates ids: permission ids and session routing belong to
//! the connection code, which feeds schema values in and forwards the core values out. The
//! only memory is a session's [`SessionState`], which ACP's upsert-style updates need to be
//! read at all, and which also knows the session's MCP App widgets: the tools that declare one,
//! the widgets read so far, each by its MCP server and URI, since two servers may name a widget
//! alike, and the reads the connection still has to send.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::MaybeUndefined;
use agent_client_protocol::v2::schema::{
    self, BackgroundWork, ContentBlock, DiffChangeOperation, ImageContent, ResourceLink,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionUpdate, StateUpdate, TerminalId, TextContent,
    ToolCallContent, ToolCallUpdate,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use partitur_core::{
    AgentUpdate, ConfigCategory, ConfigChoice, ConfigId, ConfigOption, ConfigValueId,
    PermissionKind, PermissionOption, PermissionOptionId, PromptContent, SessionFailure,
    StopReason, ToolCallId, ToolContent, ToolKind, ToolStatus, Widget, WidgetSlot,
};
use serde_json::Value;

/// Stands in for content blocks Partitur cannot render yet (images, audio, resources).
const NON_TEXT_PLACEHOLDER: &str = "[non-text content]";
/// What a failed turn reports when the agent gave no details.
const UNKNOWN_ERROR: &str = "the agent stopped with an error";
/// Why a widget read whose reply carries no HTML failed.
const NO_HTML: &str = "the widget resource has no HTML";

/// What one session's updates have established so far.
///
/// ACP reports tool calls and terminals as upserts and foreground work as snapshots, so
/// reading an update needs the ones before it: whether a tool call is new, which call shows a
/// terminal's output, and whether a turn is open. A session starts with none of these.
///
/// Every turn the core sees opened, by a prompt or by the agent's own work, is ended exactly
/// once: by the `idle` that ends the agent's foreground work, or by the refusal of the prompt
/// when no work of the agent's keeps the turn going.
///
/// It also follows the work the agent last listed as running in the background, from the
/// `background` list every state update of the session may carry: how many items it named,
/// and which of them are subagents.
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
    refused: Option<Result<StopReason, SessionFailure>>,
    /// The terminals seen so far, by id.
    terminals: HashMap<TerminalId, Terminal>,
    /// The sessions of the subagents the last reported background list named.
    subagents: Vec<schema::SessionId>,
    /// How many items the last reported background list named, of any kind.
    background: usize,
    /// The MCP server and `ui://` URI of the widget of each tool that declares one, by the
    /// name the agent reports for the tool, `mcp__<server>__<tool>`.
    widget_tools: HashMap<String, (String, String)>,
    /// The widgets read so far, by server and URI: two servers may name a widget alike.
    widgets: HashMap<(String, String), Widget>,
    /// The calls waiting for the widget being read, by server and URI.
    waiting: HashMap<(String, String), Vec<ToolCallId>>,
    /// The widget reads, server and URI, the connection has yet to send.
    reads: Vec<(String, String)>,
    /// The MCP tools without a known widget that made the session list the tools again, each
    /// at most once.
    missed: HashSet<String>,
    /// The calls of missed tools waiting for the tool list under way, with their tools.
    unlisted: Vec<(ToolCallId, String)>,
    /// Where the tool list stands.
    listing: Listing,
}

/// Where a session's list of the tools that declare a widget stands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Listing {
    /// No list is under way.
    #[default]
    Idle,
    /// A missed tool wants a list the connection has yet to ask for.
    Wanted,
    /// A list was asked for and not answered yet.
    UnderWay,
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

/// Translates one ACP session update into a core [`AgentUpdate`], reading and updating `state`.
///
/// Returns `None` for updates Partitur ignores: plans, command lists, session info,
/// compaction progress, whole-message snapshots (the chunks carry the text, and the core
/// records user text itself), and tool call or terminal updates that change nothing
/// Partitur shows or keeps.
///
/// The first update of a tool call announces it; later ones change it. A tool call that
/// reports no content but a raw output shows that output as text: a JSON string as is, any
/// other value pretty-printed. Raw inputs are kept as JSON text, but not shown, as is the MCP
/// `CallToolResult` the patched adapter adds in `_meta.partitur.callToolResult`. A terminal's
/// output is shown as the whole content of the tool call naming the terminal.
///
/// A tool call announced for a tool that declares a widget carries it: ready when it was read
/// before, else loading, with a read queued for [`SessionState::take_reads`] unless one for
/// the same widget is under way. The tool is the call's `name`, or the `toolName` the adapter
/// puts in `_meta.claudeCode`. An MCP tool, one named `mcp__<server>__<tool>`, that is not known
/// to declare a widget may be one of a server that connected late: the first call of each such
/// tool waits for a new tool list, asked for through [`SessionState::take_list_wanted`] unless
/// one is under way, and gets its widget once the list names the tool.
///
/// Foreground work reported while none is open starts a turn; an `idle` with a stop reason
/// while work is open ends it, as a failure for the `error` reason. An `idle` without a stop
/// reason ends nothing: it only says what is running in the background.
///
/// ```
/// use agent_client_protocol::v2::schema::{ContentBlock, ContentChunk, SessionUpdate, TextContent};
/// use partitur_backend_acp::map::{SessionState, map_update};
/// use partitur_core::AgentUpdate;
///
/// let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("hi")), "message-1");
/// assert_eq!(
///     map_update(SessionUpdate::AgentMessageChunk(chunk), &mut SessionState::default()),
///     Some(AgentUpdate::AgentText("hi".into()))
/// );
/// ```
pub fn map_update(update: SessionUpdate, state: &mut SessionState) -> Option<AgentUpdate> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            Some(AgentUpdate::AgentText(chunk_text(&chunk.content)))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            Some(AgentUpdate::AgentThought(chunk_text(&chunk.content)))
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
        SessionUpdate::ConfigOptionUpdate(update) => Some(AgentUpdate::ConfigOptions(
            map_config_options(&update.config_options),
        )),
        SessionUpdate::UsageUpdate(usage) => Some(AgentUpdate::Usage {
            used: usage.used,
            size: usage.size,
        }),
        SessionUpdate::Notice(notice) => Some(AgentUpdate::Notice(match notice.description {
            Some(description) => format!("{}: {description}", notice.title),
            None => notice.title,
        })),
        SessionUpdate::SessionInfoUpdate(info) => info
            .title
            .value()
            .map(|title| title.trim())
            .filter(|title| !title.is_empty())
            .map(|title| AgentUpdate::Title(title.to_owned())),
        // Message snapshots, user chunks, plans, command lists and compaction updates carry
        // nothing Partitur shows or records.
        _ => None,
    }
}

impl SessionState {
    /// Announces a tool call on its first update and changes it on later ones.
    fn tool_call(&mut self, update: ToolCallUpdate) -> Option<AgentUpdate> {
        let id = ToolCallId::new(update.tool_call_id.to_string());
        let call_result = call_result(&update);
        let tool = tool_name(&update);
        let raw_input = update.raw_input.take().map(|input| input.to_string());
        let content = update.content.map_value(|content| {
            content
                .into_iter()
                .map(|content| self.call_content(&id, content))
                .collect::<Vec<_>>()
        });
        if self.calls.insert(id.as_str().to_owned()) {
            let widget = tool.and_then(|tool| {
                let widget = self.widget(&id, &tool);
                if widget.is_none() {
                    self.missed(&id, tool);
                }
                widget
            });
            return Some(AgentUpdate::ToolCall {
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
                raw_input,
                call_result,
                widget,
            });
        }
        let changed = AgentUpdate::ToolCallUpdate {
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
            raw_input,
            call_result,
            widget: None,
        };
        let unchanged = matches!(
            changed,
            AgentUpdate::ToolCallUpdate {
                title: None,
                kind: None,
                status: None,
                content: None,
                raw_output: None,
                raw_input: None,
                call_result: None,
                ..
            }
        );
        (!unchanged).then_some(changed)
    }

    /// The widget of the tool call `id` of `tool`, if the tool declares one: ready when it was
    /// read before, else loading. A widget not read yet gets a read queued, unless one is
    /// under way, and the call waits for it.
    fn widget(&mut self, id: &ToolCallId, tool: &str) -> Option<WidgetSlot> {
        let key = self.widget_tools.get(tool)?.clone();
        if let Some(widget) = self.widgets.get(&key) {
            return Some(WidgetSlot::Ready(widget.clone()));
        }
        let waiting = self.waiting.entry(key.clone()).or_default();
        if waiting.is_empty() {
            self.reads.push(key.clone());
        }
        waiting.push(id.clone());
        let (server, uri) = key;
        Some(WidgetSlot::Loading { server, uri })
    }

    /// Makes the call `id` of `tool`, which is not known to declare a widget, wait for a new
    /// tool list if it is the first call of an MCP tool to miss, asking for the list unless
    /// one is under way.
    fn missed(&mut self, id: &ToolCallId, tool: String) {
        if !tool.starts_with("mcp__") || !self.missed.insert(tool.clone()) {
            return;
        }
        self.unlisted.push((id.clone(), tool));
        if self.listing == Listing::Idle {
            self.listing = Listing::Wanted;
        }
    }

    /// Whether the session wants the tool list again since the last call.
    pub(crate) fn take_list_wanted(&mut self) -> bool {
        let wanted = self.listing == Listing::Wanted;
        if wanted {
            self.listing = Listing::UnderWay;
        }
        wanted
    }

    /// Records that a tool list is under way, so a missed tool does not ask for another.
    pub(crate) fn listing(&mut self) {
        self.listing = Listing::UnderWay;
    }

    /// Replaces the tools known to declare a widget with `tools`, as [`widget_tools`] reads
    /// them, ending the tool list under way. Returns the change of every call that waited for
    /// the list and whose tool it names: its widget, ready or loading, with a read queued for
    /// [`SessionState::take_reads`] as for an announced call. A call whose tool the list does
    /// not name keeps no widget, and its tool never asks for a list again.
    pub(crate) fn set_widget_tools(
        &mut self,
        tools: HashMap<String, (String, String)>,
    ) -> Vec<AgentUpdate> {
        self.widget_tools = tools;
        self.listing = Listing::Idle;
        std::mem::take(&mut self.unlisted)
            .into_iter()
            .filter_map(|(id, tool)| {
                let widget = self.widget(&id, &tool)?;
                Some(widget_change(id, widget))
            })
            .collect()
    }

    /// The widget reads, server and URI, queued since the last call, oldest first.
    pub(crate) fn take_reads(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.reads)
    }

    /// Settles the read of the widget `uri` of `server` with the adapter's `reply`, or the
    /// error it failed with, and returns the change of every call that waited for it.
    ///
    /// A reply with HTML is the widget, kept for later calls, and its `meta` (an empty
    /// object when it has none) the widget's resource meta. A failed read, or a reply without
    /// HTML, fails the waiting calls' widgets and is tried again by the next call.
    pub(crate) fn widget_read(
        &mut self,
        server: &str,
        uri: &str,
        reply: Result<Value, String>,
    ) -> Vec<AgentUpdate> {
        let read = reply.and_then(|reply| {
            let html = reply.get("html").and_then(Value::as_str);
            let html = html.ok_or_else(|| NO_HTML.to_owned())?;
            let meta = reply
                .get("meta")
                .map_or_else(|| "{}".to_owned(), Value::to_string);
            Ok(Widget {
                server: server.to_owned(),
                uri: uri.to_owned(),
                html: html.to_owned(),
                meta,
            })
        });
        let key = (server.to_owned(), uri.to_owned());
        let slot = match read {
            Ok(widget) => {
                self.widgets.insert(key.clone(), widget.clone());
                WidgetSlot::Ready(widget)
            }
            Err(error) => WidgetSlot::Failed {
                server: server.to_owned(),
                uri: uri.to_owned(),
                error,
            },
        };
        self.waiting
            .remove(&key)
            .unwrap_or_default()
            .into_iter()
            .map(|id| widget_change(id, slot.clone()))
            .collect()
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
    fn terminal_output(
        &mut self,
        id: TerminalId,
        bytes: Vec<u8>,
        append: bool,
    ) -> Option<AgentUpdate> {
        let terminal = self.terminals.entry(id).or_default();
        if append {
            terminal.output.extend(bytes);
        } else {
            terminal.output = bytes;
        }
        Some(AgentUpdate::ToolCallUpdate {
            id: terminal.call.clone()?,
            title: None,
            kind: None,
            status: None,
            content: Some(vec![ToolContent::Terminal(terminal.text())]),
            raw_output: None,
            raw_input: None,
            call_result: None,
            widget: None,
        })
    }

    /// Follows the work the state `update` lists as running in the background. A list
    /// replaces the one known before, and reports its number of items when that changed; a
    /// state that reports no list changes nothing. Every item counts: subagents, background
    /// terminals, tool calls and unknown items alike. The subagents among them are kept apart,
    /// since a cancel stops them.
    pub fn background(&mut self, update: &StateUpdate) -> Option<AgentUpdate> {
        let work = update.background()?;
        let listed: Vec<schema::SessionId> = work
            .iter()
            .filter_map(|work| match work {
                BackgroundWork::Subagent(subagent) => Some(subagent.session_id.clone()),
                _ => None,
            })
            .collect();
        self.subagents = listed;
        let changed = work.len() != self.background;
        self.background = work.len();
        changed.then_some(AgentUpdate::RunningBackground(self.background))
    }

    /// The sessions of the subagents the agent last listed as running.
    pub(crate) fn subagents(&self) -> impl Iterator<Item = &schema::SessionId> {
        self.subagents.iter()
    }

    /// Records that a prompt was sent: the core sees a turn open from now on.
    pub fn prompted(&mut self) {
        self.turn = true;
    }

    /// Settles a prompt the agent refused, `ending` being how its turn ends: cancelled, or
    /// failed.
    ///
    /// With the agent's foreground work open, that work is the turn the core sees, whether
    /// it ran for the prompt or is a turn of the agent's own the prompt was held behind, so
    /// the refusal ends nothing yet: the `idle` ending the work ends the turn, and `ending`
    /// only stands in for the stop reason when that `idle` brings none. A turn that already
    /// ended, for example when the agent's work went idle before the refusal arrived, is not
    /// ended again.
    pub fn refused(&mut self, ending: Result<StopReason, SessionFailure>) -> Option<AgentUpdate> {
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
    fn foreground(&mut self, update: StateUpdate) -> Option<AgentUpdate> {
        match update {
            StateUpdate::Running(_) | StateUpdate::RequiresAction(_) => {
                self.turn = true;
                (!std::mem::replace(&mut self.working, true)).then_some(AgentUpdate::TurnStarted)
            }
            StateUpdate::Idle(idle) if self.working => {
                let ending = match idle.stop_reason {
                    Some(reason) => map_stop_reason(reason).map_err(SessionFailure::RequestFailed),
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

/// The update ending a turn: with a stop reason, or as a failure recorded with its text.
fn turn_end(ending: Result<StopReason, SessionFailure>) -> AgentUpdate {
    ending.map_or_else(
        |failure| AgentUpdate::Failed(failure.to_string()),
        AgentUpdate::TurnEnded,
    )
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
/// itself instead of receiving their contents. The link is a valid, percent-encoded file URI
/// on every platform (`file:///C:/work/a.rs` on Windows). A relative path has no file URI, so
/// it keeps the text `file://<path>` as written.
pub fn map_prompt(content: Vec<PromptContent>) -> Vec<ContentBlock> {
    content
        .into_iter()
        .map(|part| match part {
            PromptContent::Text(text) => ContentBlock::Text(TextContent::new(text)),
            PromptContent::Image { mime, base64 } => {
                ContentBlock::Image(ImageContent::new(base64, mime))
            }
            PromptContent::FileLink { path } => {
                ContentBlock::ResourceLink(ResourceLink::new(file_name(&path), file_uri(&path)))
            }
        })
        .collect()
}

/// The `file://` URI of `path`, or `file://<path>` as written when the path is relative.
fn file_uri(path: &std::path::Path) -> String {
    url::Url::from_file_path(path)
        .map_or_else(|()| format!("file://{}", path.display()), String::from)
}

/// The tools that declare a widget in the patched adapter's `_partitur/widget_tools` `reply`,
/// by the name the agent reports for each, with the server and `ui://` URI of its widget.
///
/// The name is `mcp__<server>__<tool>`, the server's name with every character but ASCII
/// letters, digits, `_` and `-` replaced by `_`, as Claude Code names MCP tools. A listed
/// tool without a server, tool name or URI is left out, and a reply without a list lists none.
pub(crate) fn widget_tools(reply: &Value) -> HashMap<String, (String, String)> {
    let tools = reply.get("tools").and_then(Value::as_array);
    tools
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let field = |name| tool.get(name).and_then(Value::as_str);
            let (server, name, uri) = (field("server")?, field("tool")?, field("resourceUri")?);
            let normalized: String = server
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let key = format!("mcp__{normalized}__{name}");
            Some((key, (server.to_owned(), uri.to_owned())))
        })
        .collect()
}

/// The change of the tool call `id` to `widget`, and nothing else.
fn widget_change(id: ToolCallId, widget: WidgetSlot) -> AgentUpdate {
    AgentUpdate::ToolCallUpdate {
        id,
        title: None,
        kind: None,
        status: None,
        content: None,
        raw_output: None,
        raw_input: None,
        call_result: None,
        widget: Some(widget),
    }
}

/// The tool a call update names: its `name`, else the `toolName` the Claude Code adapter puts
/// in `_meta.claudeCode`.
fn tool_name(update: &ToolCallUpdate) -> Option<String> {
    if let Some(name) = update.name.value() {
        return Some(name.clone());
    }
    let meta = update.meta.value()?;
    let name = meta.get("claudeCode")?.get("toolName")?.as_str()?;
    Some(name.to_owned())
}

/// The MCP `CallToolResult` the patched adapter puts in `_meta.partitur.callToolResult`, as
/// JSON text.
fn call_result(update: &ToolCallUpdate) -> Option<String> {
    let meta = update.meta.value()?;
    let result = meta.get("partitur")?.get("callToolResult")?;
    Some(result.to_string())
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
mod tests;
