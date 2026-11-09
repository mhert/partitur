//! The row under the toolbar of the Session and Env tabs that tells which session is shown:
//! the project, the session above when there is one, and the session itself.

use iced::widget::{Row, column, container, text};
use iced::{Alignment, Element, Length};

use crate::app::Message;
use crate::{style, theme};

/// The row's height in logical pixels.
const HEIGHT: f32 = 40.0;

/// The breadcrumb row for `crumbs`, outermost first, followed by `trailing` (pills and
/// remarks about the session), centered over the line that parts it from the content.
///
/// The last crumb is the session shown and stands out; the others are muted.
pub fn breadcrumb<'a>(
    crumbs: &[String],
    trailing: impl IntoIterator<Item = Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut line = Row::new().spacing(10).align_y(Alignment::Center);
    let last = crumbs.len().saturating_sub(1);
    for (index, crumb) in crumbs.iter().enumerate() {
        let crumb = text(crumb.clone()).size(theme::SMALL);
        line = if index == last {
            line.push(crumb.font(theme::UI_MEDIUM))
        } else {
            line.push(crumb.style(style::muted))
                .push(text("›").size(theme::SMALL).style(style::dim))
        };
    }
    let line = trailing.into_iter().fold(line, Row::push);
    column![
        container(line).center_x(Length::Fill).center_y(HEIGHT),
        style::hairline(|tokens| tokens.border_soft),
    ]
    .into()
}
