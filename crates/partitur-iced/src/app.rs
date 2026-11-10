//! The application: its state, the messages that change it, and the update, view and
//! subscription logic iced drives.

use futures::channel::mpsc::UnboundedReceiver;
use iced::theme::Mode;
use iced::widget::column;
use iced::{Element, Event, Subscription, Task, event, keyboard, system, window};
use partitur_core::{App, AppError, BackendEvent};

use crate::archive_sheet::{self, ArchiveMessage, ArchiveSheet};
use crate::env_view::{self, EnvMessage};
use crate::jump::{self, JumpMessage, JumpState};
use crate::projects_view::{self, ProjectsMessage, ProjectsState};
use crate::session_view::{self, SessionMessage, SessionState};
use crate::{bridge, layout, theme, toolbar};

/// The views the toolbar switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The open projects and their sessions.
    Projects,
    /// The selected session's transcript and prompt.
    Session,
    /// What the selected session runs with.
    Env,
}

impl Tab {
    /// The tab after this one in the toolbar's order; after the last comes the first.
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Projects => Self::Session,
            Self::Session => Self::Env,
            Self::Env => Self::Projects,
        }
    }
}

/// A key pressed without a modifier that the application answers to. Who answers depends on
/// what the window shows, which only [`update`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareKey {
    /// Tab: the tab after the active one.
    Tab,
    /// The down arrow: the row below the highlighted one.
    ArrowDown,
    /// The up arrow: the row above the highlighted one.
    ArrowUp,
    /// Enter: the highlighted row is opened, or a proposed split is approved.
    Enter,
    /// Escape: the jump panel closes.
    Escape,
    /// N: a new session.
    N,
    /// A: the highlighted session is archived.
    A,
}

/// Everything that changes the [`State`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The backend reported something.
    Backend(BackendEvent),
    /// The desktop's preference for light or dark, at start and whenever it changes.
    SystemTheme(Mode),
    /// The user picked a tab.
    SelectTab(Tab),
    /// The user pressed one of the bare keys.
    Key(BareKey),
    /// An interaction on the Projects tab.
    Projects(ProjectsMessage),
    /// An interaction on the Session tab.
    Session(SessionMessage),
    /// An interaction on the Env tab.
    Env(EnvMessage),
    /// An interaction with the jump panel, or the request to open it.
    Jump(JumpMessage),
    /// An interaction with the archive sheet.
    Archive(ArchiveMessage),
    /// Something failed that the user should know about.
    Error(String),
    /// The user dismissed the error bar.
    DismissError,
}

/// The state of the interface: the core [`App`] and what only the interface remembers.
pub struct State {
    pub(crate) app: App,
    /// The desktop's preference for light or dark, which picks the theme.
    pub(crate) mode: Mode,
    pub(crate) tab: Tab,
    /// The error the bar under the toolbar shows, until it is dismissed.
    pub(crate) error: Option<String>,
    pub(crate) projects: ProjectsState,
    pub(crate) session: SessionState,
    /// The jump panel, while it is open over the window.
    pub(crate) jump: Option<JumpState>,
    /// The archive sheet, while it is open over the window.
    pub(crate) archive: Option<ArchiveSheet>,
}

/// The initial state on the Projects tab with the tree's cursor on the current session, and
/// the task that learns the desktop's light or
/// dark preference and forwards every event `events` yields as a [`Message::Backend`].
pub fn boot(app: App, events: UnboundedReceiver<BackendEvent>) -> (State, Task<Message>) {
    let state = State {
        projects: ProjectsState {
            cursor: app.current_session(),
            ..ProjectsState::default()
        },
        app,
        mode: Mode::default(),
        tab: Tab::Projects,
        error: None,
        session: SessionState::default(),
        jump: None,
        archive: None,
    };
    let mode = system::theme().map(Message::SystemTheme);
    (state, Task::batch([mode, bridge::events(events)]))
}

/// Applies `message` to `state` and returns the follow-up work for the runtime.
///
/// A backend event the [`App`] rejects is logged and shown in the error bar. Every backend
/// event brings the current session's parsed answers up to date.
pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Backend(event) => {
            if let Err(error) = state.app.apply_event(event) {
                tracing::warn!(%error, "a backend event could not be applied");
                state.error = Some(error.to_string());
            }
            session_view::sync_markdown(state);
            Task::none()
        }
        Message::SystemTheme(mode) => {
            state.mode = mode;
            Task::none()
        }
        // The sheet covers the window and keeps the user's attention: no shortcut reaches
        // the page under it.
        Message::SelectTab(_) | Message::Jump(JumpMessage::Open) if state.archive.is_some() => {
            Task::none()
        }
        Message::SelectTab(tab) => {
            state.tab = tab;
            Task::none()
        }
        Message::Key(key) => press(state, key),
        Message::Projects(message) => projects_view::update(state, message),
        Message::Session(message) => session_view::update(state, message),
        Message::Env(message) => env_view::update(message),
        Message::Jump(message) => jump::update(state, message),
        Message::Archive(message) => archive_sheet::update(state, message),
        Message::Error(error) => {
            state.error = Some(error);
            Task::none()
        }
        Message::DismissError => {
            state.error = None;
            Task::none()
        }
    }
}

/// Hands the bare `key` to whoever answers to it now: the archive sheet or the jump panel
/// while it is open, else
/// the tabs (Tab cycles them), the Session tab (Enter approves a proposed split) and the
/// Projects tab's tree (the arrow keys, Enter, N and A).
fn press(state: &mut State, key: BareKey) -> Task<Message> {
    if state.archive.is_some() {
        return archive_sheet::press(state, key);
    }
    if state.jump.is_some() {
        return jump::press(state, key);
    }
    let tree_key = match key {
        BareKey::Tab => {
            state.tab = state.tab.next();
            return Task::none();
        }
        BareKey::Escape => return Task::none(),
        BareKey::Enter if state.tab == Tab::Session => return session_view::press_enter(state),
        BareKey::A => ProjectsMessage::ArchiveAtCursor,
        BareKey::ArrowDown => ProjectsMessage::CursorDown,
        BareKey::ArrowUp => ProjectsMessage::CursorUp,
        BareKey::Enter => ProjectsMessage::OpenCursor,
        BareKey::N => ProjectsMessage::NewSessionAtCursor,
    };
    projects_view::update(state, tree_key)
}

/// The window: the toolbar, the error bar when there is an error, and the active tab; over
/// all of it the archive sheet or the jump panel while it is open.
pub fn view(state: &State) -> Element<'_, Message> {
    let no_session = || layout::empty("No session selected");
    let tree = state.app.tree();
    let body = match state.tab {
        Tab::Projects => projects_view::view(&tree, &state.projects),
        Tab::Session => state.app.session_view().map_or_else(no_session, |model| {
            session_view::view(&model, &state.session, theme::tokens_for(state.mode))
        }),
        Tab::Env => state
            .app
            .current_session()
            .zip(state.app.env_view())
            .map_or_else(no_session, |(session, model)| {
                env_view::view(&session, &model)
            }),
    };

    let mut page = column![toolbar::toolbar(state.tab)];
    if let Some(error) = &state.error {
        page = page.push(layout::error_bar(error));
    }
    let page = page.push(body).into();
    if let Some(sheet) = &state.archive {
        return archive_sheet::view(
            page,
            sheet,
            state.error.as_deref(),
            theme::tokens_for(state.mode),
        );
    }
    match &state.jump {
        Some(panel) => jump::view(page, &tree, panel),
        None => page,
    }
}

/// The keyboard shortcuts (Ctrl+K opens the jump panel; Ctrl+1, Ctrl+2 and Ctrl+3 select a
/// tab; the bare keys Tab, the arrows, Enter, Escape, N and A go to whoever answers to them)
/// and the desktop's changes between light and dark.
pub fn subscription(_state: &State) -> Subscription<Message> {
    Subscription::batch([
        event::listen_with(key_message),
        system::theme_changes().map(Message::SystemTheme),
    ])
}

/// What the keyboard `event` asks for, given whether a widget took it (`status`).
///
/// A shortcut counts only when no widget took its key, with one exception: a focused text
/// field takes Escape to give up the focus, and the jump panel's field has the focus while
/// the panel is open, yet Escape closes the panel all the same.
fn key_message(event: Event, status: event::Status, _window: window::Id) -> Option<Message> {
    let Event::Keyboard(event) = event else {
        return None;
    };
    let message = shortcut(event)?;
    (status == event::Status::Ignored || message == Message::Key(BareKey::Escape))
        .then_some(message)
}

/// The value of an `App` call, or `None` after logging its error and showing it in the
/// error bar.
///
/// Takes only the error bar, so the caller can borrow `state.app` for the call.
pub(crate) fn report<T>(error_bar: &mut Option<String>, result: Result<T, AppError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "a command could not be applied");
            *error_bar = Some(error.to_string());
            None
        }
    }
}

/// What a key press asks for, if it is one of the shortcuts.
fn shortcut(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        repeat,
        ..
    } = event
    else {
        return None;
    };
    // Only the bare keys count: with Alt or Ctrl held, Tab belongs to the desktop, and a
    // letter with a modifier is not the letter's shortcut.
    if modifiers.is_empty() {
        // A held Enter counts once: its first press can open an orchestrator's session, and
        // the presses that follow would approve the split proposed there.
        return bare_key(&key)
            .filter(|bare| !(repeat && *bare == BareKey::Enter))
            .map(Message::Key);
    }
    if jump_shortcut(&key, physical_key, modifiers) {
        return Some(Message::Jump(JumpMessage::Open));
    }
    tab_shortcut(&key, physical_key, modifiers).map(Message::SelectTab)
}

/// The bare key that `key`, pressed without a modifier, is, if the application answers to
/// it.
///
/// The subscription cannot see what the window shows, so the keys are reported as they are
/// and [`update`] decides who answers.
fn bare_key(key: &keyboard::Key) -> Option<BareKey> {
    use keyboard::key::Named;

    match key.as_ref() {
        keyboard::Key::Named(Named::Tab) => Some(BareKey::Tab),
        keyboard::Key::Named(Named::ArrowDown) => Some(BareKey::ArrowDown),
        keyboard::Key::Named(Named::ArrowUp) => Some(BareKey::ArrowUp),
        keyboard::Key::Named(Named::Enter) => Some(BareKey::Enter),
        keyboard::Key::Named(Named::Escape) => Some(BareKey::Escape),
        // Caps Lock types the capital without a modifier; it is still the bare key.
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("n") => {
            Some(BareKey::N)
        }
        keyboard::Key::Character(character) if character.eq_ignore_ascii_case("a") => {
            Some(BareKey::A)
        }
        _ => None,
    }
}

/// Whether the key press is Ctrl and K, which opens the jump panel.
///
/// The letter is the one the key types on a Latin layout, so the shortcut sits on the same
/// key whatever script the layout types.
pub(crate) fn jump_shortcut(
    key: &keyboard::Key,
    physical_key: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
) -> bool {
    modifiers.command()
        && key
            .to_latin(physical_key)
            .is_some_and(|letter| letter.eq_ignore_ascii_case(&'k'))
}

/// The tab that Ctrl and a digit select.
///
/// The digit is read from the key's character. Only when the key types no digit (the digit
/// row of AZERTY types symbols) does the physical key in the digit's position count.
pub(crate) fn tab_shortcut(
    key: &keyboard::Key,
    physical_key: keyboard::key::Physical,
    modifiers: keyboard::Modifiers,
) -> Option<Tab> {
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Character(character) if character.chars().all(|c| c.is_ascii_digit()) => {
            match character {
                "1" => Some(Tab::Projects),
                "2" => Some(Tab::Session),
                "3" => Some(Tab::Env),
                _ => None,
            }
        }
        _ => match physical_key {
            keyboard::key::Physical::Code(keyboard::key::Code::Digit1) => Some(Tab::Projects),
            keyboard::key::Physical::Code(keyboard::key::Code::Digit2) => Some(Tab::Session),
            keyboard::key::Physical::Code(keyboard::key::Code::Digit3) => Some(Tab::Env),
            _ => None,
        },
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

    use futures::channel::mpsc;
    use iced::keyboard::key;
    use iced::keyboard::{self, Key, Modifiers};
    use iced_test::simulator;
    use partitur_core::testing::{MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{App, BackendEvent, SessionId, Update};

    use super::*;

    fn state() -> State {
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(RecordingBackend::default()),
            Box::new(MemoryStore::default()),
            backends,
            default,
        )
        .unwrap();
        let (_events, receiver) = mpsc::unbounded();
        boot(app, receiver).0
    }

    /// Feeds every message through `update`, dropping the tasks: the simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = Message>) {
        for message in messages {
            let _task = update(state, message);
        }
    }

    #[test]
    fn the_toolbar_shows_three_tabs_and_projects_is_active_at_start() {
        let state = state();
        let mut ui = simulator(view(&state));

        for tab in ["Projects", "Session", "Env"] {
            ui.find(tab).unwrap();
        }
        ui.find("No projects open").unwrap();
        assert_eq!(state.tab, Tab::Projects);
    }

    #[test]
    fn clicking_session_switches_the_active_tab() {
        let mut state = state();
        let mut ui = simulator(view(&state));

        ui.click("Session").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::SelectTab(Tab::Session)]);
        apply(&mut state, messages);
        let mut ui = simulator(view(&state));
        ui.find("No session selected").unwrap();
        assert!(ui.find("No projects open").is_err());
    }

    #[test]
    fn the_toolbar_shows_the_wordmark() {
        let state = state();
        let mut ui = simulator(view(&state));

        ui.find("Partitur").unwrap();
    }

    #[test]
    fn the_tab_key_cycles_through_the_tabs() {
        let mut state = state();
        let tab = |modifiers| keyboard::Event::KeyPressed {
            key: Key::Named(key::Named::Tab),
            modified_key: Key::Named(key::Named::Tab),
            physical_key: key::Physical::Code(key::Code::Tab),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(tab(Modifiers::empty())),
            Some(Message::Key(BareKey::Tab))
        );
        assert_eq!(shortcut(tab(Modifiers::ALT)), None);
        assert_eq!(shortcut(tab(Modifiers::COMMAND)), None);

        let mut visited = Vec::new();
        for _ in 0..3 {
            apply(&mut state, [Message::Key(BareKey::Tab)]);
            visited.push(state.tab);
        }
        assert_eq!(visited, [Tab::Session, Tab::Env, Tab::Projects]);
    }

    #[test]
    fn the_bare_keys_are_reported_as_they_are() {
        let named = |named: key::Named, modifiers| keyboard::Event::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let letter = |character: &str, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let bare = |key| Some(Message::Key(key));

        assert_eq!(
            shortcut(named(key::Named::ArrowDown, Modifiers::empty())),
            bare(BareKey::ArrowDown)
        );
        assert_eq!(
            shortcut(named(key::Named::ArrowUp, Modifiers::empty())),
            bare(BareKey::ArrowUp)
        );
        assert_eq!(
            shortcut(named(key::Named::Enter, Modifiers::empty())),
            bare(BareKey::Enter)
        );
        assert_eq!(
            shortcut(named(key::Named::Escape, Modifiers::empty())),
            bare(BareKey::Escape)
        );
        assert_eq!(shortcut(letter("n", Modifiers::empty())), bare(BareKey::N));
        // Caps Lock types the capital without a modifier.
        assert_eq!(shortcut(letter("N", Modifiers::empty())), bare(BareKey::N));
        assert_eq!(shortcut(letter("a", Modifiers::empty())), bare(BareKey::A));
        assert_eq!(shortcut(letter("A", Modifiers::empty())), bare(BareKey::A));
        assert_eq!(shortcut(letter("a", Modifiers::COMMAND)), None);

        assert_eq!(
            shortcut(named(key::Named::ArrowDown, Modifiers::SHIFT)),
            None
        );
        assert_eq!(shortcut(named(key::Named::Enter, Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("N", Modifiers::SHIFT)), None);
        assert_eq!(shortcut(letter("n", Modifiers::COMMAND)), None);
        assert_eq!(shortcut(letter("m", Modifiers::empty())), None);
        assert_eq!(
            shortcut(named(key::Named::ArrowLeft, Modifiers::empty())),
            None
        );
        assert_eq!(shortcut(named(key::Named::Escape, Modifiers::SHIFT)), None);
    }

    #[test]
    fn a_held_enter_counts_once_and_a_held_arrow_keeps_moving() {
        let held = |named: key::Named| keyboard::Event::KeyPressed {
            key: Key::Named(named),
            modified_key: Key::Named(named),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: true,
        };

        assert_eq!(shortcut(held(key::Named::Enter)), None);
        assert_eq!(
            shortcut(held(key::Named::ArrowDown)),
            Some(Message::Key(BareKey::ArrowDown))
        );
        assert_eq!(
            shortcut(held(key::Named::Tab)),
            Some(Message::Key(BareKey::Tab))
        );
    }

    #[test]
    fn ctrl_and_k_ask_for_the_jump_panel() {
        let press = |character: &str, code, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };
        let open = Some(Message::Jump(JumpMessage::Open));

        assert_eq!(
            shortcut(press("k", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        assert_eq!(
            shortcut(press("K", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        // A layout of another script types its own letter on the key.
        assert_eq!(
            shortcut(press("л", key::Code::KeyK, Modifiers::COMMAND)),
            open
        );
        assert_eq!(
            shortcut(press("k", key::Code::KeyK, Modifiers::empty())),
            None
        );
        assert_eq!(shortcut(press("k", key::Code::KeyK, Modifiers::ALT)), None);
        assert_eq!(
            shortcut(press("j", key::Code::KeyJ, Modifiers::COMMAND)),
            None
        );
    }

    #[test]
    fn a_key_a_widget_took_is_no_shortcut_except_escape() {
        let named = |named: key::Named| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modified_key: Key::Named(named),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: keyboard::Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let window = window::Id::unique();
        let (ignored, captured) = (event::Status::Ignored, event::Status::Captured);

        assert_eq!(
            key_message(named(key::Named::Enter), ignored, window),
            Some(Message::Key(BareKey::Enter))
        );
        assert_eq!(
            key_message(named(key::Named::Enter), captured, window),
            None
        );
        // A focused text field takes Escape for itself, and the jump panel still closes.
        assert_eq!(
            key_message(named(key::Named::Escape), captured, window),
            Some(Message::Key(BareKey::Escape))
        );
        assert_eq!(
            key_message(named(key::Named::Escape), ignored, window),
            Some(Message::Key(BareKey::Escape))
        );
        assert_eq!(
            key_message(named(key::Named::ArrowLeft), ignored, window),
            None
        );
        let moved = Event::Mouse(iced::mouse::Event::CursorLeft);
        assert_eq!(key_message(moved, ignored, window), None);
    }

    #[test]
    fn escape_without_the_jump_panel_changes_nothing() {
        let mut state = state();

        apply(&mut state, [Message::Key(BareKey::Escape)]);

        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.jump, None);
        assert_eq!(state.error, None);
    }

    #[test]
    fn the_desktop_preference_picks_the_theme() {
        let mut state = state();
        assert_eq!(state.mode, Mode::None);

        apply(&mut state, [Message::SystemTheme(Mode::Light)]);

        assert_eq!(state.mode, Mode::Light);
    }

    #[test]
    fn a_failed_backend_event_shows_a_dismissible_error_bar() {
        let mut state = state();
        let unknown = BackendEvent::Update {
            session: SessionId::new(),
            update: Update::Notice("hello".into()),
        };

        apply(&mut state, [Message::Backend(unknown)]);
        let error = state.error.clone().unwrap();
        let mut ui = simulator(view(&state));
        ui.find(error.as_str()).unwrap();
        ui.click("dismiss").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::DismissError]);
        apply(&mut state, messages);
        let mut ui = simulator(view(&state));
        assert!(ui.find(error.as_str()).is_err());
    }

    #[test]
    fn an_error_message_is_shown_in_the_error_bar() {
        let mut state = state();

        apply(&mut state, [Message::Error("the folder vanished".into())]);

        let mut ui = simulator(view(&state));
        ui.find("the folder vanished").unwrap();
    }

    #[test]
    fn ctrl_and_a_digit_select_the_tabs() {
        let press = |character: &str, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(press("1", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Projects))
        );
        assert_eq!(
            shortcut(press("2", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Session))
        );
        assert_eq!(
            shortcut(press("3", Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Env))
        );
        assert_eq!(shortcut(press("2", Modifiers::empty())), None);
        assert_eq!(shortcut(press("4", Modifiers::COMMAND)), None);
    }

    #[test]
    fn the_physical_digit_keys_select_the_tabs_on_other_layouts() {
        let press = |code, modifiers| keyboard::Event::KeyPressed {
            key: Key::Character("&".into()),
            modified_key: Key::Character("&".into()),
            physical_key: key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert_eq!(
            shortcut(press(key::Code::Digit1, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Projects))
        );
        assert_eq!(
            shortcut(press(key::Code::Digit2, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Session))
        );
        assert_eq!(
            shortcut(press(key::Code::Digit3, Modifiers::COMMAND)),
            Some(Message::SelectTab(Tab::Env))
        );
        assert_eq!(shortcut(press(key::Code::Digit4, Modifiers::COMMAND)), None);
        assert_eq!(shortcut(press(key::Code::Digit1, Modifiers::empty())), None);
    }

    #[test]
    fn a_digit_typed_by_another_key_wins_over_the_physical_position() {
        let event = keyboard::Event::KeyPressed {
            key: Key::Character("7".into()),
            modified_key: Key::Character("7".into()),
            physical_key: key::Physical::Code(key::Code::Digit1),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: None,
            repeat: false,
        };

        assert_eq!(shortcut(event), None);
    }
}
