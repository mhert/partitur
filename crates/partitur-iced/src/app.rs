//! The application: its state, the messages that change it, and the update, view and
//! subscription logic iced drives.

use futures::channel::mpsc::UnboundedReceiver;
use iced::widget::column;
use iced::{Element, Subscription, Task, keyboard, window};
use partitur_core::{App, BackendEvent};

use crate::env_view::{self, EnvMessage};
use crate::projects_view::{self, ProjectsMessage, ProjectsState};
use crate::session_view::{self, SessionMessage, SessionState};
use crate::{bridge, layout, title_bar};

/// The views the title bar switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The open projects and their sessions.
    Projects,
    /// The selected session's transcript and prompt.
    Session,
    /// What the selected session runs with.
    Env,
}

/// Everything that changes the [`State`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The backend reported something.
    Backend(BackendEvent),
    /// The window opened with this id.
    WindowOpened(window::Id),
    /// The user picked a tab.
    SelectTab(Tab),
    /// The user pressed the title bar: the window follows the pointer.
    DragWindow,
    /// The user asked to minimize the window.
    Minimize,
    /// The user asked to maximize the window, or to restore it when it is maximized.
    ToggleMaximize,
    /// The user asked to close the window, which ends the application.
    CloseWindow,
    /// An interaction on the Projects tab.
    Projects(ProjectsMessage),
    /// An interaction on the Session tab.
    Session(SessionMessage),
    /// An interaction on the Env tab.
    Env(EnvMessage),
    /// Something failed that the user should know about.
    Error(String),
    /// The user dismissed the error bar.
    DismissError,
}

/// The state of the interface: the core [`App`] and what only the interface remembers.
pub struct State {
    pub(crate) app: App,
    /// The window, once it has opened; window commands do nothing before.
    pub(crate) window: Option<window::Id>,
    pub(crate) tab: Tab,
    /// The error the bar under the title bar shows, until it is dismissed.
    pub(crate) error: Option<String>,
    pub(crate) projects: ProjectsState,
    pub(crate) session: SessionState,
}

/// The initial state on the Projects tab, and the task that learns the window's id and
/// forwards every event `events` yields as a [`Message::Backend`].
pub fn boot(app: App, events: UnboundedReceiver<BackendEvent>) -> (State, Task<Message>) {
    let projects = ProjectsState::expanded_for(&app.tree());
    let state = State {
        app,
        window: None,
        tab: Tab::Projects,
        error: None,
        projects,
        session: SessionState::default(),
    };
    let window = window::oldest()
        .and_then(Task::done)
        .map(Message::WindowOpened);
    (state, Task::batch([window, bridge::events(events)]))
}

/// Applies `message` to `state` and returns the follow-up work for the runtime.
///
/// A backend event the [`App`] rejects is logged and shown in the error bar.
pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::Backend(event) => {
            if let Err(error) = state.app.apply_event(event) {
                tracing::warn!(%error, "a backend event could not be applied");
                state.error = Some(error.to_string());
            }
            Task::none()
        }
        Message::WindowOpened(id) => {
            state.window = Some(id);
            Task::none()
        }
        Message::SelectTab(tab) => {
            state.tab = tab;
            Task::none()
        }
        // The drag must start while the press that asked for it is being handled: the
        // windowing system refuses a move that is not tied to a pressed button.
        Message::DragWindow => state.window.map_or_else(Task::none, window::drag),
        Message::Minimize => state
            .window
            .map_or_else(Task::none, |id| window::minimize(id, true)),
        Message::ToggleMaximize => state
            .window
            .map_or_else(Task::none, window::toggle_maximize),
        Message::CloseWindow => state.window.map_or_else(Task::none, window::close),
        Message::Projects(message) => projects_view::update(state, message),
        Message::Session(message) => match message {},
        Message::Env(message) => match message {},
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

/// The window: the title bar, the error bar when there is an error, and the active tab.
pub fn view(state: &State) -> Element<'_, Message> {
    let no_session = || layout::empty("No session selected");
    let tree = state.app.tree();
    let body = match state.tab {
        Tab::Projects => projects_view::view(&tree, &state.projects),
        Tab::Session => state.app.session_view().map_or_else(no_session, |model| {
            session_view::view(&model, &state.session)
        }),
        Tab::Env => state
            .app
            .env_view()
            .map_or_else(no_session, |model| env_view::view(&model)),
    };

    let mut page = column![title_bar::title_bar(state.tab)];
    if let Some(error) = &state.error {
        page = page.push(layout::error_bar(error));
    }
    page.push(body).into()
}

/// The keyboard shortcuts: Ctrl+1, Ctrl+2 and Ctrl+3 select the tabs.
pub fn subscription(_state: &State) -> Subscription<Message> {
    keyboard::listen().filter_map(shortcut)
}

/// The tab a key press selects, if it is one of the tab shortcuts.
///
/// The digit is read from the key's character; on keyboard layouts where the digit row
/// types something else (AZERTY), the physical key in that position selects the tab.
fn shortcut(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if !modifiers.command() {
        return None;
    }
    let tab = match key.as_ref() {
        keyboard::Key::Character("1") => Tab::Projects,
        keyboard::Key::Character("2") => Tab::Session,
        keyboard::Key::Character("3") => Tab::Env,
        _ => match physical_key {
            keyboard::key::Physical::Code(keyboard::key::Code::Digit1) => Tab::Projects,
            keyboard::key::Physical::Code(keyboard::key::Code::Digit2) => Tab::Session,
            keyboard::key::Physical::Code(keyboard::key::Code::Digit3) => Tab::Env,
            _ => return None,
        },
    };
    Some(Message::SelectTab(tab))
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
    use iced::keyboard::{self, Key, Modifiers};
    use iced::{Point, keyboard::key};
    use iced_test::simulator;
    use partitur_core::testing::{MemoryStore, RecordingBackend};
    use partitur_core::{App, BackendEvent, SessionId, Update};

    use super::*;

    fn state() -> State {
        let app = App::load(
            Box::new(RecordingBackend::default()),
            Box::new(MemoryStore::default()),
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
    fn the_title_bar_shows_three_tabs_and_projects_is_active_at_start() {
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
    fn the_window_controls_emit_their_messages() {
        let state = state();
        let mut ui = simulator(view(&state));

        ui.click("—").unwrap();
        ui.click("▢").unwrap();
        ui.click("✕").unwrap();

        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [
                Message::Minimize,
                Message::ToggleMaximize,
                Message::CloseWindow
            ]
        );
    }

    #[test]
    fn the_empty_title_bar_drags_and_double_click_maximizes() {
        let state = state();
        let mut ui = simulator(view(&state));

        ui.point_at(Point::new(8.0, 8.0));
        let _ = ui.simulate(iced_test::simulator::click());
        let _ = ui.simulate(iced_test::simulator::click());

        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages.first(), Some(&Message::DragWindow));
        assert!(messages.contains(&Message::ToggleMaximize));
    }

    #[test]
    fn window_messages_before_the_window_opened_do_nothing() {
        let mut state = state();

        apply(
            &mut state,
            [
                Message::DragWindow,
                Message::Minimize,
                Message::ToggleMaximize,
                Message::CloseWindow,
            ],
        );

        assert_eq!(state.window, None);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.error, None);
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
}
