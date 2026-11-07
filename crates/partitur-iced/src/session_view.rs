//! The Session tab: the selected session's transcript and the prompt that continues it.
//!
//! One centered column: the breadcrumb with the session's status, the transcript (which
//! stays pinned to its end while the agent streams), the files attached to the next prompt,
//! the prompt box with its attach and send (or stop) buttons, and a row with the backend,
//! model and effort pickers, the auto-allow switch and the context usage.
//!
//! How each transcript entry is drawn lives in [`entries`].

mod entries;

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use iced::keyboard::{self, key};
use iced::widget::{
    Column, Id, Row, button, column, container, markdown, operation, pick_list, row, space, text,
    text_editor, toggler,
};
use iced::{Alignment, Element, Length, Padding, Task};
use partitur_core::{
    App, AppError, Attachment, BackendId, BackendInfo, ChoiceView, ConfigValueId, PermissionId,
    PermissionOptionId, SessionId, SessionViewModel,
};

use crate::app::{self, Message, State};
use crate::layout;
use crate::markdown_cache::MarkdownCache;
use crate::projects_view::{bold, dimmed, pill_style, row_style, status_badge};

/// The id of the prompt box.
const PROMPT: &str = "prompt";

/// The ids of the backend, model and effort pickers and of the auto-allow switch: none of
/// them exposes its text to selectors.
const BACKEND: &str = "backend-picker";
const MODEL: &str = "model";
const EFFORT: &str = "effort";
const AUTO_ALLOW: &str = "auto-allow";

/// The tallest the prompt box grows before it scrolls, in logical pixels.
const PROMPT_MAX_HEIGHT: f32 = 200.0;

/// What the Session tab remembers between frames.
///
/// The prompt and its attachments belong to the tab, not to a session: switching sessions
/// keeps the draft.
#[derive(Debug)]
pub struct SessionState {
    /// The prompt being written.
    pub(crate) prompt: text_editor::Content,
    /// The files that go with the next prompt.
    pub(crate) attachments: Vec<PathBuf>,
    /// The transcript entries the user expanded (thoughts and tool calls), by session and
    /// entry index.
    pub(crate) expanded: HashSet<(SessionId, usize)>,
    /// The agent's answers, parsed as Markdown.
    pub(crate) markdown: MarkdownCache,
    /// The transcript's scrollable.
    pub(crate) scroll: Id,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            prompt: text_editor::Content::new(),
            attachments: Vec::new(),
            expanded: HashSet::new(),
            markdown: MarkdownCache::default(),
            scroll: Id::new("transcript"),
        }
    }
}

/// An interaction on the Session tab.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMessage {
    /// The user edited the prompt.
    Prompt(text_editor::Action),
    /// The user sent the prompt.
    Submit,
    /// The user asked to stop the running turn.
    Stop,
    /// The user asked to attach files; the file dialog opens.
    Attach,
    /// The file dialog closed with these files, none when it was cancelled.
    Attached(Vec<PathBuf>),
    /// The user removed the attachment at this position.
    RemoveAttachment(usize),
    /// The user expanded or collapsed the current session's transcript entry at this index.
    ToggleEntry(usize),
    /// The user answered a permission request with an option, or cancelled it with `None`.
    AnswerPermission(PermissionId, Option<PermissionOptionId>),
    /// The user switched granting permission requests without asking on or off.
    SetAutoAllow(bool),
    /// The user picked another backend to run the session on.
    SetBackend(BackendId),
    /// The user picked a model.
    SetModel(ConfigValueId),
    /// The user picked a reasoning effort.
    SetEffort(ConfigValueId),
    /// The user clicked a link in an answer.
    LinkClicked(markdown::Uri),
}

/// The Session tab for the selected session `model`.
pub fn view<'a>(model: &SessionViewModel<'a>, state: &'a SessionState) -> Element<'a, Message> {
    let header = row![
        text(model.breadcrumb.join(" › "))
            .font(bold())
            .width(Length::Fill),
        status_badge(model.status),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let transcript = iced::widget::scrollable(entries::transcript(model, state))
        .anchor_bottom()
        .id(state.scroll.clone())
        .width(Length::Fill)
        .height(Length::Fill);

    let content = column![header, transcript, composer(model, state)]
        .spacing(12)
        .padding(Padding::ZERO.top(16).bottom(16));
    layout::centered(content.into())
}

/// Applies `message` to the Session tab and returns the follow-up work.
///
/// Prompts, permission answers and settings go to the current session through the core
/// [`App`]; when it refuses, the error bar shows why. A clicked link is copied to the
/// clipboard: opening links is not supported yet.
pub fn update(state: &mut State, message: SessionMessage) -> Task<Message> {
    let session = &mut state.session;
    match message {
        SessionMessage::Prompt(action) => session.prompt.perform(action),
        SessionMessage::Submit => return submit(state),
        SessionMessage::Stop => {
            app::report(&mut state.error, state.app.cancel());
        }
        SessionMessage::Attach => {
            return Task::perform(pick_files(), |files| {
                Message::Session(SessionMessage::Attached(files))
            });
        }
        SessionMessage::Attached(files) => session.attachments.extend(files),
        SessionMessage::RemoveAttachment(index) => {
            if index < session.attachments.len() {
                session.attachments.remove(index);
            }
        }
        SessionMessage::ToggleEntry(index) => {
            if let Some(current) = state.app.current_session() {
                let entry = (current, index);
                if !session.expanded.remove(&entry) {
                    session.expanded.insert(entry);
                }
            }
        }
        SessionMessage::AnswerPermission(permission, option) => {
            on_current_session(state, |app, session| {
                app.answer_permission(session, permission, option)
            });
        }
        SessionMessage::SetAutoAllow(enabled) => {
            on_current_session(state, |app, session| app.set_auto_allow(session, enabled));
        }
        SessionMessage::SetBackend(backend) => {
            on_current_session(state, |app, session| app.switch_backend(session, backend));
            // The switch can append a notice to the transcript.
            sync_markdown(state);
        }
        SessionMessage::SetModel(value) => {
            on_current_session(state, |app, session| app.set_model(session, value));
        }
        SessionMessage::SetEffort(value) => {
            on_current_session(state, |app, session| app.set_effort(session, value));
        }
        SessionMessage::LinkClicked(uri) => return iced::clipboard::write(uri),
    }
    Task::none()
}

/// Parses what the current session's answers gained since the last call.
///
/// Runs after every change that can add answer text or switch the session, so the view
/// only reads parsed Markdown.
pub(crate) fn sync_markdown(state: &mut State) {
    if let Some(model) = state.app.session_view() {
        state.session.markdown.sync(&model);
    }
}

/// Sends the prompt and the attachments, then empties the prompt box and scrolls to the end.
///
/// The prompt goes out trimmed; a blank prompt without attachments is not sent.
fn submit(state: &mut State) -> Task<Message> {
    let session = &mut state.session;
    let text = session.prompt.text();
    let prompt = text.trim();
    if !sendable(prompt, &session.attachments) {
        return Task::none();
    }
    let attachments = session
        .attachments
        .iter()
        .map(|path| Attachment { path: path.clone() })
        .collect();
    if app::report(
        &mut state.error,
        state.app.submit_prompt(prompt, attachments),
    )
    .is_none()
    {
        return Task::none();
    }
    state.session.prompt = text_editor::Content::new();
    state.session.attachments.clear();
    operation::snap_to_end(state.session.scroll.clone())
}

/// Whether there is something to send: trimmed prompt text or an attachment.
fn sendable(trimmed_prompt: &str, attachments: &[PathBuf]) -> bool {
    !trimmed_prompt.is_empty() || !attachments.is_empty()
}

/// Runs `command` on the current session, showing its error in the error bar.
fn on_current_session(
    state: &mut State,
    command: impl FnOnce(&mut App, SessionId) -> Result<(), AppError>,
) {
    let result = state
        .app
        .current_session()
        .ok_or(AppError::NoCurrentSession)
        .and_then(|session| command(&mut state.app, session));
    app::report(&mut state.error, result);
}

/// Lets the user pick files in the desktop's file dialog; none when it is cancelled.
async fn pick_files() -> Vec<PathBuf> {
    let files = rfd::AsyncFileDialog::new().pick_files().await;
    files
        .unwrap_or_default()
        .iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}

/// Everything under the transcript: the attachments, the prompt row and the settings row.
fn composer<'a>(model: &SessionViewModel<'_>, state: &'a SessionState) -> Element<'a, Message> {
    let mut composer = Column::new().spacing(8);
    if !state.attachments.is_empty() {
        composer = composer.push(attachment_chips(&state.attachments));
    }

    let scope = model.breadcrumb.last().map_or("", String::as_str);
    let running = model.running;
    let editor = text_editor(&state.prompt)
        .id(PROMPT)
        .placeholder(format!("Ask {scope}…"))
        .on_action(|action| Message::Session(SessionMessage::Prompt(action)))
        .key_binding(move |press| prompt_binding(press, running))
        .padding(8)
        .max_height(PROMPT_MAX_HEIGHT);
    let attach = button(text("+"))
        .padding([6, 12])
        .on_press(Message::Session(SessionMessage::Attach));
    let send_or_stop = if running {
        button(text("Stop"))
            .style(button::danger)
            .on_press(Message::Session(SessionMessage::Stop))
    } else {
        let can_send = sendable(state.prompt.text().trim(), &state.attachments);
        button(text("Send"))
            .on_press_maybe(can_send.then_some(Message::Session(SessionMessage::Submit)))
    };
    let prompt_row = row![attach, editor, send_or_stop.padding([6, 12])]
        .spacing(8)
        .align_y(Alignment::End);

    composer.push(prompt_row).push(settings(model)).into()
}

/// One chip per attached file: its name and a button that removes it.
fn attachment_chips(attachments: &[PathBuf]) -> Element<'_, Message> {
    let chips = attachments.iter().enumerate().map(|(index, path)| {
        let remove = button(text("×").size(12))
            .padding([0, 4])
            .style(row_style)
            .on_press(Message::Session(SessionMessage::RemoveAttachment(index)));
        let remove = container(remove).id(format!("remove-attachment-{index}"));
        container(
            row![text(file_name(path)).size(12), remove]
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .padding([1, 8])
        .style(pill_style)
        .into()
    });
    Row::with_children(chips).spacing(6).wrap().into()
}

/// The file's name, or the whole path when it has none.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The backend picker, the model and effort pickers (when the backend offers them), the
/// auto-allow switch and the context usage (once the agent reported it).
fn settings<'a>(model: &SessionViewModel<'_>) -> Element<'a, Message> {
    let mut settings = Row::new().spacing(12).align_y(Alignment::Center);
    settings = settings.push(container(backend_picker(model)).id(BACKEND));
    if let Some(choice) = &model.model {
        settings = settings.push(container(picker(choice, SessionMessage::SetModel)).id(MODEL));
    }
    if let Some(choice) = &model.effort {
        settings = settings.push(container(picker(choice, SessionMessage::SetEffort)).id(EFFORT));
    }
    let auto_allow = toggler(model.auto_allow)
        .label("auto-allow")
        .text_size(13)
        .on_toggle(|enabled| Message::Session(SessionMessage::SetAutoAllow(enabled)));
    settings = settings
        .push(container(auto_allow).id(AUTO_ALLOW))
        .push(space::horizontal());
    if let Some((used, size)) = model.usage {
        settings = settings.push(text(format!("{used} / {size}")).size(12).style(dimmed));
    }
    settings.into()
}

/// The configured backends to switch the session to, with the session's own selected.
///
/// While a turn runs the session cannot switch, and iced's pick list has no disabled state,
/// so a plain label with the backend's name stands in for it.
fn backend_picker<'a>(model: &SessionViewModel<'_>) -> Element<'a, Message> {
    let (options, current) = backend_choices(model);
    if model.running {
        let name = current.map_or_else(|| model.backend.to_string(), |option| option.name);
        return container(text(name).size(13).style(dimmed))
            .padding([4, 8])
            .style(pill_style)
            .into();
    }
    pick_list(options, current, |picked: BackendChoice| {
        Message::Session(SessionMessage::SetBackend(picked.id))
    })
    .placeholder(format!("{} (not configured)", model.backend))
    .text_size(13)
    .into()
}

/// The pick list's entries (the configured backends, in order) and the one the session runs
/// on, which is none when that backend is no longer configured.
fn backend_choices(model: &SessionViewModel<'_>) -> (Vec<BackendChoice>, Option<BackendChoice>) {
    let options: Vec<BackendChoice> = model.backends.iter().map(BackendChoice::from).collect();
    let current = options
        .iter()
        .find(|option| option.id == model.backend)
        .cloned();
    (options, current)
}

/// One configured backend in the backend picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct BackendChoice {
    id: BackendId,
    name: String,
}

impl From<&BackendInfo> for BackendChoice {
    fn from(info: &BackendInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
        }
    }
}

impl fmt::Display for BackendChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// A picker over `choice`'s values that sends `on_select` with the picked value.
fn picker<'a>(
    choice: &ChoiceView,
    on_select: fn(ConfigValueId) -> SessionMessage,
) -> Element<'a, Message> {
    let options: Vec<Choice> = choice
        .choices
        .iter()
        .map(|option| Choice {
            id: option.id.clone(),
            name: option.name.clone(),
        })
        .collect();
    let current = Choice {
        id: choice.current.clone(),
        name: choice.current_name.clone(),
    };
    pick_list(options, Some(current), move |picked: Choice| {
        Message::Session(on_select(picked.id))
    })
    .text_size(13)
    .into()
}

/// One value in a picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct Choice {
    id: ConfigValueId,
    name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// The prompt box's keys: Enter sends (and does nothing while a turn is `running`, keeping
/// the text), Shift+Enter breaks the line, and the tab shortcuts switch tabs instead of
/// typing their digit. Everything else edits as usual.
fn prompt_binding(
    press: text_editor::KeyPress,
    running: bool,
) -> Option<text_editor::Binding<Message>> {
    if !matches!(press.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    if let Some(tab) = app::tab_shortcut(&press.key, press.physical_key, press.modifiers) {
        return Some(text_editor::Binding::Custom(Message::SelectTab(tab)));
    }
    match press.key.as_ref() {
        keyboard::Key::Named(key::Named::Enter) if !press.modifiers.shift() => (!running)
            .then_some(text_editor::Binding::Custom(Message::Session(
                SessionMessage::Submit,
            ))),
        _ => text_editor::Binding::from_key_press(press),
    }
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

    use futures::channel::mpsc;
    use iced::keyboard::{self, Key, Modifiers, key};
    use iced::widget::{Id, text_editor};
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{
        App, BackendEvent, BackendId, BackendInfo, BackendSessionId, ConfigCategory, ConfigChoice,
        ConfigId, ConfigOption, ConfigValueId, Entry, PermissionId, PermissionKind,
        PermissionOption, PermissionOptionId, PromptContent, Scope, SessionId, StopReason,
        ToolCallId, ToolContent, ToolKind, ToolStatus, Update,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Message, State, Tab, boot, update, view};
    use crate::test_support::{backend_id, state_on_removed_backend, two_backends};

    /// A state whose current session was created by the backend with a model and an effort
    /// option, plus the backend that recorded the calls and the session's id.
    struct Fixture {
        state: State,
        backend: RecordingBackend,
        session: SessionId,
        _dir: TempDir,
    }

    fn choice(id: &str, name: &str) -> ConfigChoice {
        ConfigChoice {
            id: ConfigValueId::new(id),
            name: name.into(),
        }
    }

    fn config() -> Vec<ConfigOption> {
        vec![
            ConfigOption {
                id: ConfigId::new("model"),
                name: "Model".into(),
                category: ConfigCategory::Model,
                current: ConfigValueId::new("sonnet"),
                choices: vec![choice("sonnet", "Sonnet"), choice("opus", "Opus")],
            },
            ConfigOption {
                id: ConfigId::new("effort"),
                name: "Effort".into(),
                category: ConfigCategory::Effort,
                current: ConfigValueId::new("medium"),
                choices: vec![choice("low", "Low"), choice("medium", "Medium")],
            },
        ]
    }

    fn fixture(updates: impl IntoIterator<Item = Update>) -> Fixture {
        let (backends, default) = single_claude();
        fixture_on(backends, default, updates)
    }

    /// Like [`fixture`], with `backends` configured and `default` running the session.
    fn fixture_on(
        backends: Vec<BackendInfo>,
        default: BackendId,
        updates: impl IntoIterator<Item = Update>,
    ) -> Fixture {
        let dir = TempDir::new().unwrap();
        let backend = RecordingBackend::new();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let session = state.app.new_session(project, Scope::Project).unwrap();
        let (request, ..) = backend
            .calls()
            .first()
            .and_then(Call::as_new_session)
            .map(|(request, session, cwd)| (request, session.clone(), cwd.to_path_buf()))
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session: session.clone(),
            backend_session: BackendSessionId::new("b-1"),
            config: config(),
        };
        let events = std::iter::once(created).chain(updates.into_iter().map(|update| {
            BackendEvent::Update {
                session: session.clone(),
                update,
            }
        }));
        apply(&mut state, events.map(Message::Backend));
        state.tab = Tab::Session;
        assert_eq!(state.error, None);
        Fixture {
            state,
            backend,
            session,
            _dir: dir,
        }
    }

    /// A user prompt, an answer, a thought, a tool call and a permission request.
    fn conversation() -> Vec<Update> {
        vec![
            Update::UserText("Explain the build".into()),
            Update::AgentText("Hello **world**".into()),
            Update::AgentThought("the workspace has two crates".into()),
            Update::ToolCall {
                id: ToolCallId::new("t-1"),
                title: "Read Cargo.toml".into(),
                kind: ToolKind::Read,
                status: ToolStatus::Completed,
                content: vec![ToolContent::Text("[workspace]".into())],
                locations: vec![PathBuf::from("Cargo.toml")],
            },
            Update::PermissionRequested {
                id: PermissionId(1),
                tool_call_id: ToolCallId::new("t-2"),
                title: "Run cargo test".into(),
                kind: ToolKind::Execute,
                options: vec![
                    PermissionOption {
                        id: PermissionOptionId::new("allow"),
                        name: "Allow once".into(),
                        kind: PermissionKind::AllowOnce,
                    },
                    PermissionOption {
                        id: PermissionOptionId::new("reject"),
                        name: "Reject".into(),
                        kind: PermissionKind::RejectOnce,
                    },
                ],
            },
        ]
    }

    /// Feeds every message through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    fn names(choices: &[BackendChoice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.name.as_str()).collect()
    }

    fn session(message: SessionMessage) -> Message {
        Message::Session(message)
    }

    /// A key press with `modifiers` held, as the windowing system reports it.
    fn press(key: Key, text: Option<&str>, modifiers: Modifiers) -> iced::Event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
    }

    #[test]
    fn a_session_renders_user_agent_thought_tool_and_permission_entries() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.find("Explain the build").unwrap();
        ui.find("thinking ▸").unwrap();
        assert!(ui.find("the workspace has two crates").is_err());
        ui.find("Read Cargo.toml").unwrap();
        ui.find("read").unwrap();
        ui.find("Run cargo test").unwrap();
        ui.find("Allow once").unwrap();
        ui.find("Reject").unwrap();
        ui.find("asks you").unwrap();
        // The answer is rendered as Markdown from the cache, not as its raw text.
        assert!(ui.find("Hello **world**").is_err());
        let items = fixture
            .state
            .session
            .markdown
            .items(&fixture.session, 1)
            .unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn a_thought_and_a_tool_call_expand_when_clicked() {
        let mut fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click("thinking ▸").unwrap();
        ui.click("Read Cargo.toml").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(
            messages,
            [
                session(SessionMessage::ToggleEntry(2)),
                session(SessionMessage::ToggleEntry(3))
            ]
        );
        apply(&mut fixture.state, messages);
        let mut ui = simulator(view(&fixture.state));
        ui.find("thinking ▾").unwrap();
        ui.find("the workspace has two crates").unwrap();
        ui.find("[workspace]").unwrap();
        ui.find("Cargo.toml").unwrap();
    }

    #[test]
    fn an_expanded_entry_stays_with_its_session() {
        let mut fixture = fixture(conversation());
        apply(
            &mut fixture.state,
            [session(SessionMessage::ToggleEntry(2))],
        );
        let project = fixture.state.app.tree().projects[0].id.clone();
        let other = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        let request = fixture
            .backend
            .calls()
            .last()
            .and_then(Call::as_new_session)
            .map(|(request, ..)| request)
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session: other.clone(),
            backend_session: BackendSessionId::new("b-2"),
            config: Vec::new(),
        };
        let updates = [
            Update::UserText("Another question".into()),
            Update::AgentText("Another answer".into()),
            Update::AgentThought("other thoughts".into()),
        ];
        let events = std::iter::once(created).chain(updates.into_iter().map(|update| {
            BackendEvent::Update {
                session: other.clone(),
                update,
            }
        }));
        apply(&mut fixture.state, events.map(Message::Backend));

        let mut ui = simulator(view(&fixture.state));
        ui.find("thinking ▸").unwrap();
        assert!(ui.find("other thoughts").is_err());
        drop(ui);

        apply(
            &mut fixture.state,
            [Message::Projects(
                crate::projects_view::ProjectsMessage::Select(fixture.session.clone()),
            )],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("thinking ▾").unwrap();
    }

    #[test]
    fn enter_submits_and_shift_enter_inserts_a_newline() {
        let fixture = fixture(Vec::new());
        let prompt = Id::new("prompt");

        let mut ui = simulator(view(&fixture.state));
        let _ = ui.tap_key(key::Named::Enter);
        let unfocused: Vec<Message> = ui.into_messages().collect();
        assert!(!unfocused.contains(&session(SessionMessage::Submit)));

        let mut ui = simulator(view(&fixture.state));
        ui.click(prompt.clone()).unwrap();
        let _ = ui.typewrite("hi");
        let _ = ui.tap_key(key::Named::Enter);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.contains(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Insert('h')
            ))))
        );
        assert_eq!(messages.last(), Some(&session(SessionMessage::Submit)));

        let mut ui = simulator(view(&fixture.state));
        ui.click(prompt).unwrap();
        let _ = ui.simulate([press(Key::Named(key::Named::Enter), None, Modifiers::SHIFT)]);
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(!messages.contains(&session(SessionMessage::Submit)));
        assert_eq!(
            messages.last(),
            Some(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Enter
            ))))
        );
    }

    #[test]
    fn ctrl_and_a_digit_in_the_prompt_select_a_tab_instead_of_typing() {
        let fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click(Id::new("prompt")).unwrap();
        let _ = ui.simulate([press(
            Key::Character("1".into()),
            Some("1"),
            Modifiers::CTRL,
        )]);
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages.last(), Some(&Message::SelectTab(Tab::Projects)));
        assert!(
            !messages.contains(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                text_editor::Edit::Insert('1')
            ))))
        );
    }

    #[test]
    fn submitting_sends_the_prompt_and_clears_the_editor() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            "  Run it"
                .chars()
                .map(text_editor::Edit::Insert)
                .chain([text_editor::Edit::Enter, text_editor::Edit::Enter])
                .map(|edit| session(SessionMessage::Prompt(text_editor::Action::Edit(edit))))
                .chain([
                    session(SessionMessage::Attached(vec![PathBuf::from(
                        "/tmp/notes.txt",
                    )])),
                    session(SessionMessage::Submit),
                ]),
        );

        let prompt = fixture
            .backend
            .calls()
            .into_iter()
            .rev()
            .find_map(|call| match call {
                Call::Prompt { content, .. } => Some(content),
                _ => None,
            });
        let prompt = prompt.unwrap();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt.first(), Some(&PromptContent::Text("Run it".into())));
        let entries = fixture.state.app.session_view().unwrap().entries.to_vec();
        assert_eq!(entries.last(), Some(&Entry::User("Run it".into())));
        assert_eq!(fixture.state.session.prompt.text(), "");
        assert!(fixture.state.session.attachments.is_empty());
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn an_empty_prompt_without_attachments_is_not_sent() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [text_editor::Edit::Insert(' '), text_editor::Edit::Enter]
                .map(|edit| session(SessionMessage::Prompt(text_editor::Action::Edit(edit)))),
        );

        let mut ui = simulator(view(&fixture.state));
        ui.click("Send").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "Send is disabled");
        apply(&mut fixture.state, [session(SessionMessage::Submit)]);

        assert_eq!(fixture.state.session.prompt.text(), " \n");
        assert!(
            !fixture
                .backend
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Prompt { .. }))
        );
        assert!(!fixture.state.app.session_view().unwrap().running);
    }

    #[test]
    fn attachments_show_as_chips_that_can_be_removed() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [session(SessionMessage::Attached(vec![
                PathBuf::from("/tmp/a.png"),
                PathBuf::from("/tmp/b.txt"),
            ]))],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("a.png").unwrap();
        ui.find("b.txt").unwrap();

        ui.click(Id::new("remove-attachment-0")).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::RemoveAttachment(0))]);
        apply(&mut fixture.state, messages);

        assert_eq!(
            fixture.state.session.attachments,
            [PathBuf::from("/tmp/b.txt")]
        );
    }

    #[test]
    fn answering_a_permission_sends_the_option_to_the_app() {
        let mut fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click("Allow once").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [session(SessionMessage::AnswerPermission(
                PermissionId(1),
                Some(PermissionOptionId::new("allow"))
            ))]
        );
        apply(&mut fixture.state, messages);

        assert_eq!(
            fixture.backend.calls().last(),
            Some(&Call::RespondPermission {
                session: fixture.session.clone(),
                permission: PermissionId(1),
                option: Some(PermissionOptionId::new("allow")),
            })
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find("→ Allow once").unwrap();
        assert!(ui.find("Reject").is_err());
    }

    #[test]
    fn choosing_a_model_calls_set_model() {
        let mut fixture = fixture(Vec::new());
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(MODEL)).unwrap();
        ui.find(Id::new(EFFORT)).unwrap();
        // The picked value comes from the pick list's overlay menu, whose entries selectors
        // cannot find; the messages it would send are applied directly.
        drop(ui);

        apply(
            &mut fixture.state,
            [
                session(SessionMessage::SetModel(ConfigValueId::new("opus"))),
                session(SessionMessage::SetEffort(ConfigValueId::new("low"))),
            ],
        );

        let calls = fixture.backend.calls();
        let set: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                Call::SetConfig { option, value, .. } => Some((option.clone(), value.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            set,
            [
                (ConfigId::new("model"), ConfigValueId::new("opus")),
                (ConfigId::new("effort"), ConfigValueId::new("low")),
            ]
        );
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn the_backend_picker_lists_the_configured_backends_and_selects_the_current() {
        let (backends, default) = two_backends();
        let fixture = fixture_on(backends.clone(), default, Vec::new());
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backends, backends);

        // A pick list exposes neither its selection nor its menu entries to selectors, so
        // the entries it is built from are checked directly.
        let (options, current) = backend_choices(&model);
        assert_eq!(names(&options), ["claude-main", "claude-work"]);
        assert_eq!(current.map(|choice| choice.id), Some(model.backend.clone()));
        assert_eq!(model.backend, backend_id("claude-main"));
        drop(model);
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(BACKEND)).unwrap();
    }

    #[test]
    fn choosing_another_backend_switches_the_session() {
        let (backends, default) = two_backends();
        let said = [Update::AgentText("Hello".into())];
        let mut fixture = fixture_on(backends, default, said);
        let calls = fixture.backend.calls().len();

        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id(
                "claude-work",
            )))],
        );

        assert_eq!(fixture.state.error, None);
        // The new agent is asked for a session at once.
        let recorded = fixture.backend.calls();
        assert_eq!(recorded.len(), calls + 1);
        let created = recorded.last().unwrap();
        assert!(matches!(
            created,
            Call::NewSession { backend, .. } if *backend == backend_id("claude-work")
        ));
        let (request, ..) = created.as_new_session().unwrap();
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("claude-work"));
        let (_, current) = backend_choices(&model);
        assert_eq!(
            current.map(|choice| choice.id),
            Some(backend_id("claude-work"))
        );
        drop(model);
        let mut ui = simulator(view(&fixture.state));
        // The transcript had an entry, so the switch is noticed.
        ui.find("switched to claude-work; the new agent has no memory of earlier turns")
            .unwrap();
        ui.find(Id::new(BACKEND)).unwrap();
        assert!(ui.find(Id::new(MODEL)).is_err());
        assert!(ui.find(Id::new(EFFORT)).is_err());
        drop(ui);

        // The pickers come back as soon as the new agent reports its options.
        let session = fixture.session.clone();
        apply(
            &mut fixture.state,
            [Message::Backend(BackendEvent::SessionCreated {
                request,
                session,
                backend_session: BackendSessionId::new("work-1"),
                config: config(),
            })],
        );
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(MODEL)).unwrap();
        ui.find(Id::new(EFFORT)).unwrap();
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn the_backend_picker_is_disabled_while_running() {
        let (backends, default) = two_backends();
        let mut fixture = fixture_on(backends, default, Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();

        // Disabled: the picker takes no clicks, and a switch that gets through anyway is
        // refused with the reason in the error bar.
        let mut ui = simulator(view(&fixture.state));
        ui.find(Id::new(BACKEND)).unwrap();
        ui.find("claude-main").unwrap();
        ui.click(Id::new(BACKEND)).unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id(
                "claude-work",
            )))],
        );

        let error = fixture.state.error.clone().unwrap();
        assert!(error.contains("a turn is still running"), "{error}");
        let model = fixture.state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("claude-main"));
    }

    #[test]
    fn switching_to_an_unknown_backend_shows_the_error() {
        let mut fixture = fixture(Vec::new());

        apply(
            &mut fixture.state,
            [session(SessionMessage::SetBackend(backend_id("nope")))],
        );

        let error = fixture.state.error.clone().unwrap();
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn the_picker_offers_configured_backends_for_a_removed_one() {
        let dir = TempDir::new().unwrap();
        let mut state = state_on_removed_backend(&dir);
        state.tab = Tab::Session;
        let model = state.app.session_view().unwrap();
        assert_eq!(model.backend, backend_id("gone"));
        let (options, current) = backend_choices(&model);
        assert_eq!(names(&options), ["claude-main", "claude-work"]);
        assert_eq!(current, None);
        drop(model);

        let mut ui = simulator(view(&state));
        ui.find(Id::new(BACKEND)).unwrap();
    }

    #[test]
    fn without_config_options_the_pickers_are_hidden() {
        let mut fixture = fixture(Vec::new());
        apply(
            &mut fixture.state,
            [Message::Backend(BackendEvent::Update {
                session: fixture.session.clone(),
                update: Update::ConfigOptions(Vec::new()),
            })],
        );

        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find(Id::new(MODEL)).is_err());
        assert!(ui.find(Id::new(EFFORT)).is_err());
        ui.find(Id::new(AUTO_ALLOW)).unwrap();
    }

    #[test]
    fn the_auto_allow_toggle_answers_waiting_requests() {
        let mut fixture = fixture(conversation());
        let mut ui = simulator(view(&fixture.state));

        ui.click(Id::new(AUTO_ALLOW)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::SetAutoAllow(true))]);
        apply(&mut fixture.state, messages);

        assert!(fixture.state.app.session_view().unwrap().auto_allow);
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::RespondPermission { .. })
        ));
    }

    #[test]
    fn the_stop_button_replaces_submit_while_running() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();

        let mut ui = simulator(view(&fixture.state));
        ui.find("Stop").unwrap();
        assert!(ui.find("Send").is_err());
        ui.click("Stop").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::Stop)]);
        apply(&mut fixture.state, messages);
        assert!(matches!(
            fixture.backend.calls().last(),
            Some(Call::Cancel { .. })
        ));

        let ended = BackendEvent::Update {
            session: fixture.session.clone(),
            update: Update::TurnEnded(StopReason::Cancelled),
        };
        apply(&mut fixture.state, [Message::Backend(ended)]);
        let mut ui = simulator(view(&fixture.state));
        ui.find("Send").unwrap();
        assert!(ui.find("Stop").is_err());
        ui.find("cancelled").unwrap();
    }

    #[test]
    fn enter_does_not_submit_while_the_turn_runs() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();
        let waiting = conversation().pop().unwrap();

        for turn in ["running", "waiting"] {
            if turn == "waiting" {
                let asked = BackendEvent::Update {
                    session: fixture.session.clone(),
                    update: waiting.clone(),
                };
                apply(&mut fixture.state, [Message::Backend(asked)]);
            }
            fixture.state.session.prompt = text_editor::Content::new();
            let mut ui = simulator(view(&fixture.state));
            ui.click(Id::new("prompt")).unwrap();
            let _ = ui.typewrite("hi");
            let _ = ui.tap_key(key::Named::Enter);
            let _ = ui.simulate([press(Key::Named(key::Named::Enter), None, Modifiers::SHIFT)]);
            let messages: Vec<Message> = ui.into_messages().collect();
            assert!(
                !messages.contains(&session(SessionMessage::Submit)),
                "{turn}"
            );
            assert_eq!(
                messages.last(),
                Some(&session(SessionMessage::Prompt(text_editor::Action::Edit(
                    text_editor::Edit::Enter
                )))),
                "{turn}"
            );
            apply(&mut fixture.state, messages);
            assert_eq!(fixture.state.session.prompt.text(), "hi\n", "{turn}");
        }
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn stop_is_shown_while_a_permission_waits() {
        let mut fixture = fixture(Vec::new());
        fixture
            .state
            .app
            .submit_prompt("Build it", Vec::new())
            .unwrap();
        let asked = BackendEvent::Update {
            session: fixture.session.clone(),
            update: conversation().pop().unwrap(),
        };
        apply(&mut fixture.state, [Message::Backend(asked)]);

        let mut ui = simulator(view(&fixture.state));
        ui.find("asks you").unwrap();
        ui.find("Stop").unwrap();
        assert!(ui.find("Send").is_err());
        ui.click("Stop").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [session(SessionMessage::Stop)]);
        apply(&mut fixture.state, messages);

        let calls = fixture.backend.calls();
        assert!(matches!(
            calls.as_slice(),
            [
                ..,
                Call::RespondPermission { option: None, .. },
                Call::Cancel { .. }
            ]
        ));
    }

    #[test]
    fn the_header_shows_the_breadcrumb_status_and_usage() {
        let fixture = fixture(vec![
            Update::Usage {
                used: 1200,
                size: 200_000,
            },
            Update::Notice("compacted".into()),
            Update::Failed("the agent crashed".into()),
        ]);
        let project = fixture.state.app.tree().projects[0].name.clone();

        let mut ui = simulator(view(&fixture.state));
        ui.find(format!("{project} › New session")).unwrap();
        ui.find("failed").unwrap();
        ui.find("1200 / 200000").unwrap();
        ui.find("compacted").unwrap();
        ui.find("failed: the agent crashed").unwrap();
    }

    #[test]
    fn a_clicked_link_is_copied_not_opened() {
        let mut fixture = fixture(Vec::new());
        let task = update(
            &mut fixture.state,
            session(SessionMessage::LinkClicked("https://example.com".into())),
        );
        assert!(task.units() > 0);
        assert_eq!(fixture.state.error, None);
    }
}
