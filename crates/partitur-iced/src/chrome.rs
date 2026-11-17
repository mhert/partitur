//! The frame of the window: whether the system draws its title bar or Partitur's toolbar does.
//!
//! A [`WindowFrame`] is what the user chooses. [`chrome`] turns it and the [`Platform`] into
//! the [`Chrome`], the facts the rest of the interface asks: whether the system's decorations
//! stay, whether the toolbar draws its own window buttons and whether dragging it moves the
//! window. [`window_settings`] builds the window's settings from that chrome.

use iced::{Size, window};

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
// The toolbar reads the last three fields; until then only the window settings are built.
#[allow(dead_code, clippy::struct_excessive_bools)]
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

#[cfg(test)]
mod tests {
    use iced::Size;

    use super::{Chrome, MAC_INSET, Platform, WindowFrame, chrome, window_settings};

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
