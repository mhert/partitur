//! The layout every tab body shares, the bar that reports errors under the toolbar, and the
//! banner that marks an archived session on the Session and Env tabs.

use iced::widget::{button, container, row, space, text};
use iced::{Alignment, Element, Length, Theme};
use partitur_core::{Archived, SessionId, day_month};

use crate::app::Message;
use crate::projects_view::ProjectsMessage;
use crate::{style, theme};

/// The widest the content column grows, in logical pixels.
const MAX_WIDTH: f32 = 900.0;

/// Places `content` in the centered column every tab uses.
///
/// The column is at most 900 logical pixels wide with 24 pixels of padding left and right,
/// is centered horizontally and fills the height below the toolbar.
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

/// The bar under the toolbar that shows `error` with a button that dismisses it.
pub fn error_bar(error: &str) -> Element<'_, Message> {
    let dismiss = button(text("dismiss").size(theme::SMALL))
        .style(style::danger_button)
        .on_press(Message::DismissError);
    container(
        row![text(error).width(Length::Fill), dismiss]
            .spacing(12)
            .align_y(Alignment::Center),
    )
    .padding([4, 24])
    .width(Length::Fill)
    .style(|theme: &Theme| {
        let tint = theme::tokens(theme).failed;
        container::Style {
            background: Some(tint.bg.into()),
            text_color: Some(tint.fg),
            ..container::Style::default()
        }
    })
    .into()
}

/// The banner across the window under the breadcrumb of the archived `session`: when it was
/// archived on the left, the `Restore` button (found by `banner-restore`) on the right.
pub fn archived_banner<'a>(session: &SessionId, archived: Archived) -> Element<'a, Message> {
    let restore = button(text("Restore").size(theme::SMALL))
        .padding([2, 10])
        .style(style::outlined_button)
        .on_press(Message::Projects(ProjectsMessage::Restore(session.clone())));
    container(
        row![
            text(format!("Archived {}", day_month(archived.at))).size(theme::SMALL),
            space::horizontal(),
            container(restore).id("banner-restore"),
        ]
        .align_y(Alignment::Center),
    )
    .padding([6, 24])
    .width(Length::Fill)
    .style(|theme: &Theme| {
        let tokens = theme::tokens(theme);
        container::Style {
            background: Some(tokens.archive.into()),
            text_color: Some(tokens.text),
            ..container::Style::default()
        }
    })
    .into()
}
