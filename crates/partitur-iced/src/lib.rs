//! The iced graphical user interface for navigating sessions and writing prompts.
//!
//! [`run`] opens the Partitur window: a toolbar, which is the window's title bar unless the
//! [`WindowFrame`] leaves that to the system, with the wordmark at its left, the button that
//! opens the jump panel at its right and the Projects, Session, Spec and Env tabs centered
//! between them, and the active tab's content below. A link opener started from the window
//! runs in the process environment [`run`] is given. The MCP App widgets of tool calls are
//! drawn from the frames of the widget engine [`run`] is given.

pub(crate) mod app;
mod background;
pub(crate) mod badge;
mod branch_controls;
pub(crate) mod breadcrumb;
pub(crate) mod bridge;
mod chrome;
mod clock;
pub(crate) mod confirm_sheet;
pub(crate) mod controls;
pub(crate) mod ellipsis;
pub(crate) mod env_view;
pub(crate) mod flow_rail;
pub(crate) mod home_path;
pub(crate) mod icon;
pub(crate) mod jump;
pub(crate) mod layout;
pub(crate) mod link;
pub(crate) mod markdown_cache;
pub(crate) mod new_session_sheet;
pub(crate) mod paste;
mod prepare;
mod previews;
pub(crate) mod projects_view;
mod prose;
pub(crate) mod selectable;
pub(crate) mod session_view;
mod sheet;
pub(crate) mod spec_view;
pub(crate) mod style;
pub(crate) mod theme;
pub(crate) mod tokens_label;
pub(crate) mod toolbar;
pub(crate) mod viewer;
pub(crate) mod widgets;

#[cfg(test)]
mod snapshots;
#[cfg(test)]
mod test_support;

use std::borrow::Cow;
use std::cell::Cell;

use futures::channel::mpsc::UnboundedReceiver;
use iced::widget::space;
use iced::{Element, Pixels, Settings, Subscription, Task, Theme, window};
use partitur_core::widget_host::{WidgetHost, WidgetHostEvent};
use partitur_core::{App, BackendEvent, GitEvent, ProcessEnv, ToolCall};

use crate::app::Message;
use crate::chrome::{Platform, chrome, window_settings};

pub use crate::chrome::WindowFrame;

/// The items the `transcript_bench` example measures; not part of the supported interface.
#[doc(hidden)]
pub mod bench {
    pub use crate::markdown_cache::MarkdownCache;
    pub use crate::widgets::Widgets;

    /// The Session tab's view and state.
    pub mod session_view {
        pub use crate::session_view::{SessionState, view};
    }

    /// The markdown style.
    pub mod style {
        pub use crate::style::markdown;
    }

    /// The fonts and sizes the window starts with.
    pub mod theme {
        pub use crate::theme::{DARK, FONTS, TEXT, UI};
    }
}

/// How long [`run`] keeps the window open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// Until the user closes the window.
    UntilClosed,
    /// Until the window has drawn its first frame, then the application exits on its own.
    FirstFrame,
}

/// Opens the Partitur window for `app` and runs it until `run` says it ends: until the window
/// is closed, or until it has drawn its first frame.
///
/// Every event `events` and `git_events` yield is applied to `app` as it arrives, and every
/// call `tool_calls` yields is handled by it; the backend, the git port and the tool server
/// keep the matching senders. Widgets are shown through `widget_host`, which reports what
/// its views do through `widget_events`. A configured link opener runs in `env`, added to the inherited
/// environment; its thread waits while `env` is still pending, the window never does.
/// `frame` says who draws the window's title bar, and the window's colors follow the
/// desktop's light or dark preference.
///
/// # Errors
///
/// Returns an [`iced::Error`] when the window or its renderer cannot be created.
///
/// # Examples
///
/// ```no_run
/// use partitur_core::{App, GlobalSettings};
/// # use partitur_core::testing::{MemoryGit, MemoryStore, RecordingBackend, single_claude};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let (backend, store, git) = (RecordingBackend::new(), MemoryStore::new(), MemoryGit::new());
/// # let (backends, default) = single_claude();
/// let global = GlobalSettings::default();
/// let app = App::load(Box::new(backend), Box::new(store), Box::new(git), backends, default, global)?;
/// let (_sender, events) = futures::channel::mpsc::unbounded();
/// let (_git_sender, git_events) = futures::channel::mpsc::unbounded();
/// let (_caller, tool_calls) = futures::channel::mpsc::unbounded();
/// let widget_host = Box::new(partitur_core::widget_host::NoWidgetHost);
/// let (_engine, widget_events) = futures::channel::mpsc::unbounded();
/// let env = partitur_core::ProcessEnv::ready(Vec::new());
/// partitur_iced::run(
///     app,
///     events,
///     git_events,
///     tool_calls,
///     widget_host,
///     widget_events,
///     partitur_iced::Run::UntilClosed,
///     env,
///     partitur_iced::WindowFrame::default(),
/// )?;
/// # Ok(())
/// # }
/// ```
#[expect(
    clippy::too_many_arguments,
    reason = "each port of the application is handed over on its own"
)]
pub fn run(
    app: App,
    events: UnboundedReceiver<BackendEvent>,
    git_events: UnboundedReceiver<GitEvent>,
    tool_calls: UnboundedReceiver<ToolCall>,
    widget_host: Box<dyn WidgetHost>,
    widget_events: UnboundedReceiver<WidgetHostEvent>,
    run: Run,
    env: ProcessEnv,
    frame: WindowFrame,
) -> iced::Result {
    let chrome = chrome(frame, Platform::current());
    // iced takes the boot logic as an `Fn`, yet `app` and the receivers can be handed over
    // only once, so the closure moves them out of a cell.
    let widgets = (widget_host, widget_events);
    let seed = Cell::new(Some((app, events, git_events, tool_calls, widgets, env)));
    let boot = move || match seed.take() {
        Some((app, events, git_events, tool_calls, (widget_host, widget_events), env)) => {
            let (mut state, task) = app::boot(app, events, tool_calls);
            state.process_env = env;
            state.chrome = chrome;
            state.widget_host = widget_host;
            let git = bridge::git_events(git_events);
            let widgets = bridge::widget_events(widget_events);
            (Shell(Some(state), run), Task::batch([task, git, widgets]))
        }
        None => (Shell(None, run), Task::none()),
    };

    iced::application(boot, Shell::update, Shell::view)
        .settings(settings())
        .title("Partitur")
        .window(window_settings(chrome))
        .subscription(Shell::subscription)
        .theme(Shell::theme)
        .run()
}

/// The settings every renderer of the interface starts with: the bundled fonts, Inter as
/// the default font and the interface's text size.
pub(crate) fn settings() -> Settings {
    Settings {
        fonts: theme::FONTS.into_iter().map(Cow::Borrowed).collect(),
        default_font: theme::UI,
        default_text_size: Pixels(theme::TEXT),
        ..Settings::default()
    }
}

/// The subscription that reports the window's frames for a run that ends with the first one,
/// and none for a run that lasts until the window is closed.
fn frame_listener(run: Run) -> Option<Subscription<Message>> {
    match run {
        Run::UntilClosed => None,
        Run::FirstFrame => Some(window::frames().map(|_| Message::FrameDrawn)),
    }
}

/// The state iced holds: the interface's [`State`](app::State), absent only if iced booted a
/// second time after the app and the receivers were handed over. iced boots once; the
/// absent case shows an empty window rather than panicking.
struct Shell(Option<app::State>, Run);

impl Shell {
    fn update(&mut self, message: Message) -> Task<Message> {
        self.0
            .as_mut()
            .map_or_else(Task::none, |state| app::update(state, message))
    }

    fn view(&self) -> Element<'_, Message> {
        self.0.as_ref().map_or_else(|| space().into(), app::view)
    }

    /// The theme for the desktop's preference; the dark one before the interface booted.
    fn theme(&self) -> Theme {
        theme::theme(self.0.as_ref().map(|state| state.mode).unwrap_or_default())
    }

    fn subscription(&self) -> Subscription<Message> {
        let app = self
            .0
            .as_ref()
            .map_or_else(Subscription::none, app::subscription);
        Subscription::batch(std::iter::once(app).chain(frame_listener(self.1)))
    }
}

#[cfg(test)]
mod tests {
    use iced::Size;

    use super::{Run, frame_listener};
    use crate::chrome::window_icon;

    #[test]
    fn the_embedded_window_icon_decodes_at_256_pixels() {
        let size = window_icon().map(|icon| icon.into_raw().1);
        assert_eq!(size, Some(Size::new(256, 256)));
    }

    #[test]
    fn only_a_run_to_the_first_frame_listens_for_frames() {
        assert!(frame_listener(Run::FirstFrame).is_some());
        assert!(frame_listener(Run::UntilClosed).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_desktop_entry_names_the_windows_application_id() {
        let entry = include_str!("../../../packaging/linux/mhert.partitur.desktop");
        let id = crate::chrome::APPLICATION_ID;
        assert!(entry.lines().any(|line| line == format!("Icon={id}")));
        assert!(
            entry
                .lines()
                .any(|line| line == format!("StartupWMClass={id}"))
        );
        assert!(entry.lines().any(|line| line == "Exec=partitur"));
    }
}
