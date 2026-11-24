//! The modal layer every sheet and panel over the window shares.
//!
//! [`modal`] lays a dimming backdrop over the page and places a card on it. The backdrop
//! takes every click meant for the page and sends the message that closes what it shows; the
//! card is opaque to clicks, so only a click beside it reaches the backdrop. The backdrop is
//! the window's colour at [`BACKDROP_STRENGTH`] in both themes, the same for the confirmation
//! and new session sheets, the viewer and the jump panel.
//!
//! The sheets' own surface and measures live here too: [`sheet_style`], [`WIDTH`],
//! [`PADDING`], [`GAP`] and [`TITLE_SIZE`].

use iced::widget::{container, mouse_area, opaque, stack};
use iced::{Alignment, Border, Element, Length, Padding, Theme};

use crate::app::Message;
use crate::theme;

/// The width of a sheet, in logical pixels.
pub(crate) const WIDTH: f32 = 560.0;

/// The room a sheet keeps around its content: above and below, then left and right.
pub(crate) const PADDING: [f32; 2] = [20.0, 22.0];

/// The space between the parts of a sheet.
pub(crate) const GAP: f32 = 16.0;

/// The corner radius of a sheet.
const RADIUS: f32 = 10.0;

/// The size of a sheet's title.
pub(crate) const TITLE_SIZE: f32 = 15.0;

/// How much of the window's colour the backdrop lays over the page.
pub(crate) const BACKDROP_STRENGTH: f32 = 0.72;

/// `page` under a dimming backdrop with `card` over it, centred across and placed `align`
/// down, inside `padding`. A click on the backdrop beside the card sends `on_dismiss`.
pub(crate) fn modal<'a>(
    page: Element<'a, Message>,
    card: Element<'a, Message>,
    align: Alignment,
    padding: impl Into<Padding>,
    on_dismiss: Message,
) -> Element<'a, Message> {
    let backdrop = mouse_area(
        container(opaque(card))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(align)
            .padding(padding)
            .style(backdrop_style),
    )
    .on_press(on_dismiss);
    stack![page, opaque(backdrop)].into()
}

/// The backdrop: the window's colour at [`BACKDROP_STRENGTH`].
pub(crate) fn backdrop_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(
            theme::tokens(theme)
                .bg
                .scale_alpha(BACKDROP_STRENGTH)
                .into(),
        ),
        ..container::Style::default()
    }
}

/// A sheet's surface: the panel colour inside a one-pixel outline in the input border
/// colour, with rounder corners than a card.
pub(crate) fn sheet_style(theme: &Theme) -> container::Style {
    let tokens = theme::tokens(theme);
    container::Style {
        background: Some(tokens.panel.into()),
        border: Border::default()
            .rounded(RADIUS)
            .width(1)
            .color(tokens.border_input),
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use iced::Background;
    use iced::theme::Mode;

    use super::{BACKDROP_STRENGTH, backdrop_style};
    use crate::theme;

    #[test]
    fn backdrop_is_the_window_colour_at_0_72() {
        assert!((BACKDROP_STRENGTH - 0.72).abs() < f32::EPSILON);
        for mode in [Mode::Light, Mode::Dark] {
            let theme = theme::theme(mode);
            let Some(Background::Color(colour)) = backdrop_style(&theme).background else {
                panic!("the backdrop has no colour");
            };
            assert_eq!(
                colour,
                theme::tokens(&theme).bg.scale_alpha(BACKDROP_STRENGTH)
            );
        }
    }
}
