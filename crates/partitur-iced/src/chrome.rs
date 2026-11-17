//! The frame of the window: whether the system draws its title bar or Partitur's toolbar does.
//!
//! A [`WindowFrame`] is what the user chooses. [`chrome`] turns it and the [`Platform`] into
//! the [`Chrome`], the facts the rest of the interface asks: whether the system's decorations
//! stay, whether the toolbar draws its own window buttons and whether dragging it moves the
//! window. [`window_settings`] builds the window's settings from that chrome.
//!
//! Without the system's decorations the window is moved, resized, minimized, maximized and
//! closed through [`WindowAction`]s. [`resize_edges`] lays a frame of thin strips over the
//! window's edges, wider at its corners, whose press starts resizing the window in that
//! direction; inside the frame every event passes through to the content.

use iced::widget::{Id, Space, column, container, mouse_area, row, stack};
use iced::{Element, Length, Size, mouse, window};

use crate::app::Message;

/// The application id the desktop matches against Partitur's `.desktop` file.
#[cfg(target_os = "linux")]
pub(crate) const APPLICATION_ID: &str = "mhert.partitur";

/// The width on macOS the toolbar keeps free at its left for the system's traffic lights.
pub(crate) const MAC_INSET: f32 = 78.0;

/// Who draws the window's title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowFrame {
    /// Partitur's toolbar is the title bar.
    #[default]
    Partitur,
    /// The system draws the title bar, with the toolbar below it.
    System,
}

/// The platform the window runs on, as far as its frame differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    /// macOS, whose traffic lights stay when the title bar is unified with the toolbar.
    MacOs,
    /// Every other platform.
    Other,
}

impl Platform {
    /// The platform this build runs on.
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// What the window's frame means for the interface.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Chrome {
    /// Whether the system draws the window's decorations.
    pub decorations: bool,
    /// Whether the title bar is merged with the content (macOS), leaving the traffic lights.
    pub unified_title: bool,
    /// Whether the toolbar draws the window's minimize, maximize and close buttons.
    pub controls: bool,
    /// Whether dragging the toolbar moves the window.
    pub drags: bool,
    /// The width the toolbar keeps free at its left.
    pub leading_inset: f32,
}

/// The chrome of `frame` on `platform`.
pub(crate) fn chrome(frame: WindowFrame, platform: Platform) -> Chrome {
    match (frame, platform) {
        (WindowFrame::System, _) => Chrome {
            decorations: true,
            unified_title: false,
            controls: false,
            drags: false,
            leading_inset: 0.0,
        },
        (WindowFrame::Partitur, Platform::MacOs) => Chrome {
            decorations: true,
            unified_title: true,
            controls: false,
            drags: true,
            leading_inset: MAC_INSET,
        },
        (WindowFrame::Partitur, Platform::Other) => Chrome {
            decorations: false,
            unified_title: false,
            controls: true,
            drags: true,
            leading_inset: 0.0,
        },
    }
}

/// The settings of the window: its size, minimum size, icon, on Linux the application id the
/// desktop matches against Partitur's `.desktop` file, and the decorations `chrome` asks for.
pub(crate) fn window_settings(chrome: Chrome) -> window::Settings {
    window::Settings {
        size: Size::new(1200.0, 800.0),
        min_size: Some(Size::new(720.0, 480.0)),
        decorations: chrome.decorations,
        platform_specific: platform_specific(chrome),
        icon: window_icon(),
        ..window::Settings::default()
    }
}

/// The window's icon, the tree of notes embedded in the binary, or `None` when it fails to
/// decode, which leaves the window without one. Wayland ignores a window's own icon and takes
/// it from the desktop entry instead.
pub(crate) fn window_icon() -> Option<window::Icon> {
    window::icon::from_file_data(include_bytes!("../assets/app-icon.png"), None).ok()
}

/// The window's platform settings: on Linux the application id, on macOS a hidden, transparent
/// title bar over the content when the title is unified, elsewhere the defaults.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn platform_specific(chrome: Chrome) -> window::settings::PlatformSpecific {
    #[cfg(target_os = "linux")]
    {
        window::settings::PlatformSpecific {
            application_id: APPLICATION_ID.into(),
            ..window::settings::PlatformSpecific::default()
        }
    }
    #[cfg(target_os = "macos")]
    {
        window::settings::PlatformSpecific {
            title_hidden: chrome.unified_title,
            titlebar_transparent: chrome.unified_title,
            fullsize_content_view: chrome.unified_title,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        window::settings::PlatformSpecific::default()
    }
}

/// The thickness of the strips along the window's edges that resize it.
const EDGE: f32 = 4.0;

/// The size of the squares at the window's corners that resize it.
const CORNER: f32 = 8.0;

/// What the user asks of the window when the system draws no decorations for it.
///
/// Two actions are equal when they ask the same, for a resize towards the same direction;
/// iced's [`window::Direction`] cannot be compared itself.
#[derive(Debug, Clone, Copy)]
pub enum WindowAction {
    /// Move the window while the mouse button is held.
    Drag,
    /// Maximize the window, or restore it when it is maximized.
    ToggleMaximize,
    /// Minimize the window.
    Minimize,
    /// Close the window, which ends the application.
    Close,
    /// Show the system's window menu.
    Menu,
    /// Resize the window at the edge or corner `direction` points to while the mouse button
    /// is held.
    Resize(window::Direction),
}

impl WindowAction {
    /// A number that tells the actions apart, every direction of a resize its own.
    fn ordinal(self) -> u8 {
        match self {
            Self::Drag => 0,
            Self::ToggleMaximize => 1,
            Self::Minimize => 2,
            Self::Close => 3,
            Self::Menu => 4,
            Self::Resize(direction) => 5 + direction as u8,
        }
    }
}

impl PartialEq for WindowAction {
    fn eq(&self, other: &Self) -> bool {
        self.ordinal() == other.ordinal()
    }
}

impl Eq for WindowAction {}

/// `content` with the strips over its edges and corners that resize the window; the middle
/// of the frame is empty space, so content under it takes every event as before.
pub(crate) fn resize_edges(content: Element<'_, Message>) -> Element<'_, Message> {
    use window::Direction;

    let top = row![
        handle("resize-north-west", Direction::NorthWest, CORNER, CORNER),
        column![
            handle("resize-north", Direction::North, Length::Fill, EDGE),
            Space::new().height(CORNER - EDGE),
        ]
        .width(Length::Fill),
        handle("resize-north-east", Direction::NorthEast, CORNER, CORNER),
    ];
    let middle = row![
        handle("resize-west", Direction::West, EDGE, Length::Fill),
        Space::new().width(Length::Fill).height(Length::Fill),
        handle("resize-east", Direction::East, EDGE, Length::Fill),
    ]
    .height(Length::Fill);
    let bottom = row![
        handle("resize-south-west", Direction::SouthWest, CORNER, CORNER),
        column![
            Space::new().height(CORNER - EDGE),
            handle("resize-south", Direction::South, Length::Fill, EDGE),
        ]
        .width(Length::Fill),
        handle("resize-south-east", Direction::SouthEast, CORNER, CORNER),
    ];
    stack![content, column![top, middle, bottom]].into()
}

/// The strip named `id` of `width` and `height` that resizes the window towards `direction`
/// when pressed, showing the matching cursor while the mouse is over it.
fn handle(
    id: &'static str,
    direction: window::Direction,
    width: impl Into<Length>,
    height: impl Into<Length>,
) -> Element<'static, Message> {
    container(
        mouse_area(Space::new().width(width).height(height))
            .interaction(resize_cursor(direction))
            .on_press(Message::Window(WindowAction::Resize(direction))),
    )
    .id(Id::new(id))
    .into()
}

/// The cursor that shows the mouse resizes the window towards `direction`.
fn resize_cursor(direction: window::Direction) -> mouse::Interaction {
    use window::Direction;

    match direction {
        Direction::North | Direction::South => mouse::Interaction::ResizingVertically,
        Direction::East | Direction::West => mouse::Interaction::ResizingHorizontally,
        Direction::NorthEast | Direction::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
        Direction::NorthWest | Direction::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
    }
}

#[cfg(test)]
mod tests {
    use iced::Size;

    use iced::window::Direction;

    use super::{Chrome, MAC_INSET, Platform, WindowAction, WindowFrame, chrome, window_settings};

    #[test]
    fn window_actions_are_equal_only_when_they_ask_the_same() {
        let directions = [
            Direction::North,
            Direction::South,
            Direction::East,
            Direction::West,
            Direction::NorthEast,
            Direction::NorthWest,
            Direction::SouthEast,
            Direction::SouthWest,
        ];
        let actions: Vec<WindowAction> = [
            WindowAction::Drag,
            WindowAction::ToggleMaximize,
            WindowAction::Minimize,
            WindowAction::Close,
            WindowAction::Menu,
        ]
        .into_iter()
        .chain(directions.map(WindowAction::Resize))
        .collect();
        for (i, a) in actions.iter().enumerate() {
            for (j, b) in actions.iter().enumerate() {
                assert_eq!(a == b, i == j, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn the_system_frame_keeps_todays_window_on_every_platform() {
        for platform in [Platform::MacOs, Platform::Other] {
            assert_eq!(
                chrome(WindowFrame::System, platform),
                Chrome {
                    decorations: true,
                    unified_title: false,
                    controls: false,
                    drags: false,
                    leading_inset: 0.0,
                }
            );
        }
    }

    #[test]
    fn partiturs_frame_draws_its_own_buttons_off_macos() {
        assert_eq!(
            chrome(WindowFrame::Partitur, Platform::Other),
            Chrome {
                decorations: false,
                unified_title: false,
                controls: true,
                drags: true,
                leading_inset: 0.0,
            }
        );
    }

    #[test]
    fn partiturs_frame_keeps_the_traffic_lights_on_macos() {
        assert_eq!(
            chrome(WindowFrame::Partitur, Platform::MacOs),
            Chrome {
                decorations: true,
                unified_title: true,
                controls: false,
                drags: true,
                leading_inset: MAC_INSET,
            }
        );
    }

    #[test]
    fn the_window_settings_follow_the_chrome() {
        let other = chrome(WindowFrame::Partitur, Platform::Other);
        assert!(!window_settings(other).decorations);
        assert!(window_settings(chrome(WindowFrame::System, Platform::Other)).decorations);
        assert_eq!(
            window_settings(other).min_size,
            Some(Size::new(720.0, 480.0))
        );
    }
}
