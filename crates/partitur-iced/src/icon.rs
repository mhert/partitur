//! The bundled icons: 12 pixel line drawings with a 1.5 pixel stroke and round caps, tinted
//! from the theme's tokens.

use iced::widget::svg::{self, Svg};
use iced::{Color, ContentFit, Length, Theme};

use crate::theme::{self, Tokens};

/// The side of an icon, in logical pixels.
pub const SIZE: f32 = 12.0;

/// One of the bundled icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// Marks a row whose rows below are shown.
    ChevronDown,
    /// Marks a row whose rows below are hidden.
    ChevronRight,
    /// Adds something: a session, a project, an attachment.
    Plus,
    /// Marks a subagent as spawned by the row above.
    Subagent,
    /// Closes or removes something.
    Close,
    /// Marks what is archived.
    Archive,
    /// Minimizes the window.
    Minimize,
    /// Maximizes the window.
    Maximize,
    /// Restores a maximized window to its former size.
    Restore,
}

impl Icon {
    /// The icon's SVG source.
    fn source(self) -> &'static [u8] {
        match self {
            Self::ChevronDown => include_bytes!("../assets/icons/chevron-down.svg"),
            Self::ChevronRight => include_bytes!("../assets/icons/chevron-right.svg"),
            Self::Plus => include_bytes!("../assets/icons/plus.svg"),
            Self::Subagent => include_bytes!("../assets/icons/subagent.svg"),
            Self::Close => include_bytes!("../assets/icons/close.svg"),
            Self::Archive => include_bytes!("../assets/icons/archive.svg"),
            Self::Minimize => include_bytes!("../assets/icons/minimize.svg"),
            Self::Maximize => include_bytes!("../assets/icons/maximize.svg"),
            Self::Restore => include_bytes!("../assets/icons/restore.svg"),
        }
    }
}

/// `icon` at its 12 pixel size, drawn in the colour `pick` takes from the active theme's
/// tokens.
pub fn icon<'a>(icon: Icon, pick: fn(&Tokens) -> Color) -> Svg<'a> {
    iced::widget::svg(svg::Handle::from_memory(icon.source()))
        .width(SIZE)
        .height(SIZE)
        .style(move |theme: &Theme, _status| svg::Style {
            color: Some(pick(theme::tokens(theme))),
        })
}

/// `icon` at its 12 pixel size, centered in all the space it is given, drawn in the colour
/// `pick` chooses from the active theme and whether the mouse is over that space. Filling a
/// button with it lets the icon follow the button's hover, which a button's own style cannot
/// pass on to an icon.
pub fn centered<'a>(icon: Icon, pick: impl Fn(&Theme, svg::Status) -> Color + 'a) -> Svg<'a> {
    // `ScaleDown` keeps the icon at its own size, as the space is larger, and centers it.
    iced::widget::svg(svg::Handle::from_memory(icon.source()))
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(ContentFit::ScaleDown)
        .style(move |theme: &Theme, status| svg::Style {
            color: Some(pick(theme, status)),
        })
}
