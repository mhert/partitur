//! The iced graphical user interface for navigating sessions and writing prompts.
//!
//! [`run`] opens the Partitur window: under the system's title bar a toolbar with the
//! Projects, Session and Env tabs centered in it and the button that opens the jump panel
//! at its right, and the active tab's content below.

pub mod app;
pub mod badge;
pub mod breadcrumb;
pub mod bridge;
pub mod ellipsis;
pub mod env_view;
pub mod home_path;
pub mod icon;
pub mod jump;
pub mod layout;
pub mod markdown_cache;
pub mod projects_view;
pub mod session_view;
pub mod style;
pub mod theme;
pub mod tokens_label;
pub mod toolbar;

#[cfg(test)]
mod snapshots;
#[cfg(test)]
mod test_support;

use std::borrow::Cow;
use std::cell::Cell;

use futures::channel::mpsc::UnboundedReceiver;
use iced::widget::space;
use iced::{Element, Pixels, Settings, Size, Subscription, Task, Theme, window};
use partitur_core::{App, BackendEvent};

use crate::app::Message;

/// The application id the desktop matches against Partitur's `.desktop` file.
#[cfg(target_os = "linux")]
const APPLICATION_ID: &str = "io.hertlein.partitur";

/// Opens the Partitur window for `app` and runs it until the window is closed.
///
/// Every event `events` yields is applied to `app` as it arrives; the backend keeps the
/// matching sender. The window keeps the system's decorations, and its colors follow the
/// desktop's light or dark preference.
///
/// # Errors
///
/// Returns an [`iced::Error`] when the window or its renderer cannot be created.
///
/// # Examples
///
/// ```no_run
/// use partitur_core::App;
/// # use partitur_core::testing::{MemoryStore, RecordingBackend, single_claude};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let (backend, store) = (RecordingBackend::new(), MemoryStore::new());
/// # let (backends, default) = single_claude();
/// let app = App::load(Box::new(backend), Box::new(store), backends, default)?;
/// let (_sender, events) = futures::channel::mpsc::unbounded();
/// partitur_iced::run(app, events)?;
/// # Ok(())
/// # }
/// ```
pub fn run(app: App, events: UnboundedReceiver<BackendEvent>) -> iced::Result {
    // iced takes the boot logic as an `Fn`, yet `app` and `events` can be handed over only
    // once, so the closure moves them out of a cell.
    let seed = Cell::new(Some((app, events)));
    let boot = move || match seed.take() {
        Some((app, events)) => {
            let (state, task) = app::boot(app, events);
            (Shell(Some(state)), task)
        }
        None => (Shell(None), Task::none()),
    };

    iced::application(boot, Shell::update, Shell::view)
        .settings(settings())
        .title("Partitur")
        .window(window::Settings {
            size: Size::new(1200.0, 800.0),
            min_size: Some(Size::new(720.0, 480.0)),
            platform_specific: platform_specific(),
            ..window::Settings::default()
        })
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

/// The window's platform settings: on Linux the application id the desktop matches against
/// Partitur's `.desktop` file, elsewhere the defaults.
fn platform_specific() -> window::settings::PlatformSpecific {
    #[cfg(target_os = "linux")]
    {
        window::settings::PlatformSpecific {
            application_id: APPLICATION_ID.into(),
            ..window::settings::PlatformSpecific::default()
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        window::settings::PlatformSpecific::default()
    }
}

/// The state iced holds: the interface's [`State`](app::State), absent only if iced booted a
/// second time after the app and the event receiver were handed over. iced boots once; the
/// absent case shows an empty window rather than panicking.
struct Shell(Option<app::State>);

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
        self.0
            .as_ref()
            .map_or_else(Subscription::none, app::subscription)
    }
}
