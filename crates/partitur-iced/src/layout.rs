//! The layout every tab body shares, and the bar that reports errors under the title bar.

use iced::widget::{button, container, row, text};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;

/// The widest the content column grows, in logical pixels.
const MAX_WIDTH: f32 = 900.0;

/// Places `content` in the centered column every tab uses.
///
/// The column is at most 900 logical pixels wide with 24 pixels of padding left and right,
/// is centered horizontally and fills the height below the title bar.
pub fn centered(content: Element<'_, Message>) -> Element<'_, Message> {
    let column = container(content)
        .width(Length::Fill)
        .max_width(MAX_WIDTH)
        .padding([0, 24]);
    container(column)
        .center_x(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// A tab body that only says `message`, for a tab with nothing to show.
pub fn empty<'a>(message: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    centered(text(message).into())
}

/// The bar under the title bar that shows `error` with a button that dismisses it.
pub fn error_bar(error: &str) -> Element<'_, Message> {
    let dismiss = button("dismiss")
        .style(|theme: &Theme, status| button::Style {
            text_color: theme.extended_palette().danger.base.text,
            ..button::text(theme, status)
        })
        .on_press(Message::DismissError);
    container(
        row![text(error).width(Length::Fill), dismiss]
            .spacing(12)
            .align_y(Alignment::Center),
    )
    .padding([4, 24])
    .width(Length::Fill)
    .style(container::danger)
    .into()
}
