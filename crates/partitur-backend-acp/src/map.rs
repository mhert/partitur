//! Pure translation between the ACP schema types and Partitur's core vocabulary.
//!
//! Nothing here performs I/O or allocates ids: permission ids and session routing belong to
//! the connection code, which feeds schema values in and forwards the core values out.

use std::path::Path;

use agent_client_protocol::v1::schema::{
    self, ContentBlock, ImageContent, ResourceLink, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionConfigSelectOptions,
    SessionUpdate, TextContent, ToolCallContent,
};
use partitur_core::{
    ConfigCategory, ConfigChoice, ConfigId, ConfigOption, ConfigValueId, PermissionKind,
    PermissionOption, PermissionOptionId, PromptContent, StopReason, ToolCallId, ToolContent,
    ToolKind, ToolStatus, Update,
};

/// Stands in for content blocks Partitur cannot render yet (images, audio, resources).
const NON_TEXT_PLACEHOLDER: &str = "[non-text content]";

/// Translates one ACP session update into a core [`Update`].
///
/// Returns `None` for updates Partitur ignores: plans, command lists, mode changes, session
/// info, compaction progress, echoed user messages (the core records user text itself), and
/// tool call updates that change nothing Partitur shows.
///
/// A tool call that reports no content but a raw output shows that output as text: a JSON
/// string as is, any other value pretty-printed. Raw inputs are not shown.
///
/// ```
/// use agent_client_protocol::v1::schema::{ContentBlock, ContentChunk, SessionUpdate, TextContent};
/// use partitur_backend_acp::map_update;
/// use partitur_core::Update;
///
/// let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new("hi")));
/// assert_eq!(
///     map_update(SessionUpdate::AgentMessageChunk(chunk)),
///     Some(Update::AgentText("hi".into()))
/// );
/// ```
pub fn map_update(update: SessionUpdate) -> Option<Update> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            Some(Update::AgentText(chunk_text(&chunk.content)))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            Some(Update::AgentThought(chunk_text(&chunk.content)))
        }
        SessionUpdate::ToolCall(call) => Some(Update::ToolCall {
            id: ToolCallId::new(call.tool_call_id.to_string()),
            title: call.title,
            kind: map_tool_kind(call.kind),
            status: map_tool_status(call.status),
            content: tool_output(call.content, call.raw_output),
            locations: call
                .locations
                .into_iter()
                .map(|location| location.path)
                .collect(),
        }),
        SessionUpdate::ToolCallUpdate(update) => {
            let fields = update.fields;
            let update = Update::ToolCallUpdate {
                id: ToolCallId::new(update.tool_call_id.to_string()),
                title: fields.title,
                kind: fields.kind.map(map_tool_kind),
                status: fields.status.map(map_tool_status),
                content: fields
                    .content
                    .map(|content| content.into_iter().map(map_tool_content).collect()),
                raw_output: fields.raw_output.map(raw_text),
            };
            let unchanged = matches!(
                update,
                Update::ToolCallUpdate {
                    title: None,
                    kind: None,
                    status: None,
                    content: None,
                    raw_output: None,
                    ..
                }
            );
            (!unchanged).then_some(update)
        }
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
        // Plan, AvailableCommandsUpdate, CurrentModeUpdate, SessionInfoUpdate, the compaction
        // updates and UserMessageChunk carry nothing Partitur shows or records.
        _ => None,
    }
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
        id: ConfigId::new(option.id.to_string()),
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

/// Translates why an agent turn ended. A reason this version does not know ends the turn
/// normally, as [`StopReason::EndTurn`].
pub fn map_stop_reason(reason: schema::StopReason) -> StopReason {
    match reason {
        schema::StopReason::MaxTokens => StopReason::MaxTokens,
        schema::StopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        schema::StopReason::Refusal => StopReason::Refusal,
        schema::StopReason::Cancelled => StopReason::Cancelled,
        schema::StopReason::EndTurn | _ => StopReason::EndTurn,
    }
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

/// Translates one piece of tool call output.
///
/// Text stays text, diffs keep their path and both sides, and a terminal becomes its id.
/// Non-text content blocks become a placeholder line.
pub fn map_tool_content(content: ToolCallContent) -> ToolContent {
    if let ToolCallContent::Diff(diff) = content {
        return ToolContent::Diff {
            path: diff.path,
            old: diff.old_text,
            new: diff.new_text,
        };
    }
    if let ToolCallContent::Terminal(terminal) = content {
        return ToolContent::Terminal(terminal.terminal_id.to_string());
    }
    // Anything that is neither text, a diff nor a terminal is shown as a placeholder too.
    // Kept on one line so the defensive fallback does not count as an unreachable line.
    #[rustfmt::skip]
    let ToolCallContent::Content(content) = content else { return ToolContent::Text(NON_TEXT_PLACEHOLDER.into()) };
    ToolContent::Text(chunk_text(&content.content))
}

/// A tool call's output: its content, or when it has none, its raw output as one text.
fn tool_output(content: Vec<ToolCallContent>, raw: Option<serde_json::Value>) -> Vec<ToolContent> {
    match raw {
        Some(raw) if content.is_empty() => vec![ToolContent::Text(raw_text(raw))],
        _ => content.into_iter().map(map_tool_content).collect(),
    }
}

/// A raw tool output as text: a string as is, anything else pretty-printed.
fn raw_text(raw: serde_json::Value) -> String {
    match raw {
        serde_json::Value::String(text) => text,
        other => serde_json::to_string_pretty(&other).unwrap_or_default(),
    }
}

pub(crate) fn map_tool_kind(kind: schema::ToolKind) -> ToolKind {
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

fn map_tool_status(status: schema::ToolCallStatus) -> ToolStatus {
    match status {
        schema::ToolCallStatus::InProgress => ToolStatus::InProgress,
        schema::ToolCallStatus::Completed => ToolStatus::Completed,
        schema::ToolCallStatus::Failed => ToolStatus::Failed,
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

    use std::path::PathBuf;

    use agent_client_protocol::v1::schema::{
        ContentChunk, Diff, ImageContent as SchemaImage, Notice, NoticeSeverity,
        PermissionOptionKind, Plan, SessionConfigSelectGroup, Terminal, ToolCall, ToolCallLocation,
        ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
    };

    use super::*;

    fn text_chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    #[test]
    fn agent_message_text_maps_to_agent_text() {
        assert_eq!(
            map_update(SessionUpdate::AgentMessageChunk(text_chunk("hello"))),
            Some(Update::AgentText("hello".into()))
        );
        let image = ContentChunk::new(ContentBlock::Image(SchemaImage::new("aGk=", "image/png")));
        assert_eq!(
            map_update(SessionUpdate::AgentMessageChunk(image)),
            Some(Update::AgentText("[non-text content]".into()))
        );
    }

    #[test]
    fn a_thought_chunk_maps_to_agent_thought() {
        assert_eq!(
            map_update(SessionUpdate::AgentThoughtChunk(text_chunk("hmm"))),
            Some(Update::AgentThought("hmm".into()))
        );
    }

    #[test]
    fn a_tool_call_maps_all_fields_and_a_diff_content() {
        let call = ToolCall::new("t-1", "Edit main.rs")
            .kind(schema::ToolKind::Edit)
            .status(schema::ToolCallStatus::InProgress)
            .content(vec![
                ToolCallContent::Diff(Diff::new("src/main.rs", "new").old_text("old")),
                ToolCallContent::Terminal(Terminal::new("term-1")),
                ToolCallContent::from(ContentBlock::Text(TextContent::new("out"))),
            ])
            .locations(vec![ToolCallLocation::new("src/main.rs").line(3)]);
        assert_eq!(
            map_update(SessionUpdate::ToolCall(call)),
            Some(Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Edit main.rs".into(),
                kind: ToolKind::Edit,
                status: ToolStatus::InProgress,
                content: vec![
                    ToolContent::Diff {
                        path: PathBuf::from("src/main.rs"),
                        old: Some("old".into()),
                        new: "new".into(),
                    },
                    ToolContent::Terminal("term-1".into()),
                    ToolContent::Text("out".into()),
                ],
                locations: vec![PathBuf::from("src/main.rs")],
            })
        );
    }

    #[test]
    fn every_tool_kind_and_status_maps_one_to_one() {
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
        ];
        for (schema_kind, kind) in kinds {
            assert_eq!(map_tool_kind(schema_kind), kind);
        }
        let statuses = [
            (schema::ToolCallStatus::Pending, ToolStatus::Pending),
            (schema::ToolCallStatus::InProgress, ToolStatus::InProgress),
            (schema::ToolCallStatus::Completed, ToolStatus::Completed),
            (schema::ToolCallStatus::Failed, ToolStatus::Failed),
        ];
        for (schema_status, status) in statuses {
            assert_eq!(map_tool_status(schema_status), status);
        }
    }

    #[test]
    fn a_tool_call_update_maps_only_present_fields() {
        let sparse = ToolCallUpdate::new(
            "t-1",
            ToolCallUpdateFields::new().status(schema::ToolCallStatus::Completed),
        );
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(sparse)),
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-1"),
                title: None,
                kind: None,
                status: Some(ToolStatus::Completed),
                content: None,
                raw_output: None,
            })
        );
        let full = ToolCallUpdate::new(
            "t-2",
            ToolCallUpdateFields::new()
                .title("Run tests")
                .kind(schema::ToolKind::Execute)
                .status(schema::ToolCallStatus::Failed)
                .content(vec![ToolCallContent::Diff(Diff::new("a.rs", "x"))])
                .locations(vec![ToolCallLocation::new("a.rs")]),
        );
        assert_eq!(
            map_update(SessionUpdate::ToolCallUpdate(full)),
            Some(Update::ToolCallUpdate {
                id: ToolCallId::new("t-2"),
                title: Some("Run tests".into()),
                kind: Some(ToolKind::Execute),
                status: Some(ToolStatus::Failed),
                content: Some(vec![ToolContent::Diff {
                    path: PathBuf::from("a.rs"),
                    old: None,
                    new: "x".into(),
                }]),
                raw_output: None,
            })
        );
    }

    #[test]
    fn a_tool_call_update_that_changes_nothing_shown_is_dropped() {
        for fields in [
            ToolCallUpdateFields::new(),
            ToolCallUpdateFields::new()
                .locations(vec![ToolCallLocation::new("a.rs")])
                .raw_input(serde_json::json!({"command": "ls"})),
        ] {
            let update = ToolCallUpdate::new("t-1", fields);
            assert_eq!(map_update(SessionUpdate::ToolCallUpdate(update)), None);
        }
    }

    #[test]
    fn a_tool_call_update_with_any_shown_field_is_forwarded() {
        let forwarded = |fields: ToolCallUpdateFields| {
            map_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "t-1", fields,
            )))
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
            forwarded(ToolCallUpdateFields::new().title("Run".to_owned())),
            update(Some("Run"), None, None)
        );
        assert_eq!(
            forwarded(ToolCallUpdateFields::new().kind(schema::ToolKind::Read)),
            update(None, Some(ToolKind::Read), None)
        );
        assert_eq!(
            forwarded(ToolCallUpdateFields::new().content(Vec::new())),
            update(None, None, Some(Vec::new()))
        );
    }

    #[test]
    fn a_raw_output_is_mapped_next_to_the_content_never_into_it() {
        let output = |fields: ToolCallUpdateFields| {
            map_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "t-1", fields,
            )))
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
            output(ToolCallUpdateFields::new().raw_output(serde_json::json!("total 0\n"))),
            update(None, "total 0\n")
        );
        assert_eq!(
            output(
                ToolCallUpdateFields::new()
                    .content(Vec::new())
                    .raw_output(serde_json::json!({"exit": 0}))
            ),
            update(Some(Vec::new()), "{\n  \"exit\": 0\n}")
        );
        assert_eq!(
            output(
                ToolCallUpdateFields::new()
                    .content(vec![ToolCallContent::from(ContentBlock::Text(
                        TextContent::new("shown")
                    ))])
                    .raw_output(serde_json::json!("hidden"))
            ),
            update(Some(vec![ToolContent::Text("shown".into())]), "hidden")
        );

        let call = |id: &str, title: &str, content: Vec<ToolContent>| {
            Some(Update::ToolCall {
                id: ToolCallId::new(id),
                title: title.into(),
                kind: ToolKind::Other,
                status: ToolStatus::Pending,
                content,
                locations: Vec::new(),
            })
        };
        let listed = ToolCall::new("t-2", "List").raw_output(serde_json::json!(["a", "b"]));
        assert_eq!(
            map_update(SessionUpdate::ToolCall(listed)),
            call(
                "t-2",
                "List",
                vec![ToolContent::Text("[\n  \"a\",\n  \"b\"\n]".into())]
            )
        );
        let shown = ToolCall::new("t-4", "Shown")
            .content(vec![ToolCallContent::from(ContentBlock::Text(
                TextContent::new("shown"),
            ))])
            .raw_output(serde_json::json!("hidden"));
        assert_eq!(
            map_update(SessionUpdate::ToolCall(shown)),
            call("t-4", "Shown", vec![ToolContent::Text("shown".into())])
        );
        let empty = ToolCall::new("t-3", "Nothing");
        assert_eq!(
            map_update(SessionUpdate::ToolCall(empty)),
            call("t-3", "Nothing", Vec::new())
        );
    }

    #[test]
    fn usage_notice_and_config_updates_map() {
        assert_eq!(
            map_update(SessionUpdate::UsageUpdate(UsageUpdate::new(10, 200))),
            Some(Update::Usage {
                used: 10,
                size: 200
            })
        );
        assert_eq!(
            map_update(SessionUpdate::Notice(Notice::new(
                NoticeSeverity::Info,
                "Heads up"
            ))),
            Some(Update::Notice("Heads up".into()))
        );
        assert_eq!(
            map_update(SessionUpdate::Notice(
                Notice::new(NoticeSeverity::Warning, "Heads up").description("Be careful")
            )),
            Some(Update::Notice("Heads up: Be careful".into()))
        );
        let option = SessionConfigOption::select(
            "model",
            "Model",
            "a",
            vec![SessionConfigSelectOption::new("a", "A")],
        );
        let mapped = map_update(SessionUpdate::ConfigOptionUpdate(
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
        assert_eq!(
            map_update(SessionUpdate::UserMessageChunk(text_chunk("echo"))),
            None
        );
        assert_eq!(map_update(SessionUpdate::Plan(Plan::new(Vec::new()))), None);
    }

    fn select(
        id: &str,
        category: Option<SessionConfigOptionCategory>,
        current: &str,
        values: &[&str],
    ) -> SessionConfigOption {
        let mut option = SessionConfigOption::select(
            id.to_owned(),
            id.to_uppercase(),
            current.to_owned(),
            values
                .iter()
                .map(|value| {
                    SessionConfigSelectOption::new(value.to_string(), value.to_uppercase())
                })
                .collect::<Vec<_>>(),
        );
        option.category = category;
        option
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
        ];
        let options: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(index, (kind, _))| {
                schema::PermissionOption::new(
                    format!("opt-{index}"),
                    format!("Option {index}"),
                    *kind,
                )
            })
            .collect();
        let mapped = map_permission_options(&options);
        assert_eq!(mapped.len(), 4);
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
        ];
        for (schema_reason, reason) in reasons {
            assert_eq!(map_stop_reason(schema_reason), reason);
        }
    }

    #[test]
    fn non_text_tool_content_becomes_a_placeholder() {
        let image =
            ToolCallContent::from(ContentBlock::Image(SchemaImage::new("aGk=", "image/png")));
        assert_eq!(
            map_tool_content(image),
            ToolContent::Text("[non-text content]".into())
        );
    }
}
