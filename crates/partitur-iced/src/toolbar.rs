//! The toolbar under the system's title bar: the wordmark on the left, the tabs centered
//! and, on the right, the button that opens the jump panel, with the keys that do the same.

use iced::widget::{Space, button, column, container, row, text};
use iced::{Alignment, Element, Length, Theme};

use crate::app::{Message, Tab};
use crate::jump::JumpMessage;
use crate::{badge, style, theme};

/// The toolbar's height in logical pixels.
const HEIGHT: f32 = 44.0;

/// The width of the wordmark's slot and of the equally wide slot of the jump button right
/// of the tabs, which together keep the tabs centered in the window.
const SIDE: f32 = 180.0;

/// The thickness of the line under the active tab.
const UNDERLINE: f32 = 2.0;

/// The application's name as the toolbar shows it.
const WORDMARK: &str = "Partitur";

/// The tabs in the order the toolbar shows them, which is the order `Tab` cycles through.
pub(crate) const TABS: [Tab; 4] = [Tab::Projects, Tab::Session, Tab::Spec, Tab::Env];

/// The toolbar with `active` marked as the selected tab, over the line that parts it from
/// the tab's content. The Spec tab can be picked only when `spec_enabled`; otherwise it is
/// drawn dim and does nothing.
pub fn toolbar<'a>(active: Tab, spec_enabled: bool) -> Element<'a, Message> {
    let wordmark = text(WORDMARK)
        .size(theme::HEADING)
        .font(theme::UI_SEMIBOLD)
        .style(style::ink(|tokens| tokens.muted_strong));
    let tabs = row(TABS.into_iter().map(|tab| {
        let enabled = tab != Tab::Spec || spec_enabled;
        tab_button(tab, tab == active, enabled)
    }))
    .spacing(4)
    .height(Length::Fill);
    let bar = container(
        row![
            container(wordmark).width(SIDE),
            container(tabs).center_x(Length::Fill),
            container(jump_button()).align_right(SIDE),
        ]
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .height(HEIGHT)
    .padding([0, 16])
    .style(style::fill(|tokens| tokens.panel));

    column![bar, style::hairline(|tokens| tokens.border)].into()
}

/// The button that opens the jump panel: what it does in words, then the keys that do the
/// same.
fn jump_button<'a>() -> Element<'a, Message> {
    button(
        row![
            text("Jump to session").size(theme::SMALL),
            badge::key_cap("Ctrl K")
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([4, 8])
    .style(style::link_button)
    .on_press(Message::Jump(JumpMessage::Open))
    .into()
}

/// The label a tab's button shows.
fn label(tab: Tab) -> &'static str {
    match tab {
        Tab::Projects => "Projects",
        Tab::Session => "Session",
        Tab::Spec => "Spec",
        Tab::Env => "Env",
    }
}

/// One tab's button: its label over an underline that only the active tab colors. A tab
/// that is not `enabled` cannot be pressed.
fn tab_button<'a>(tab: Tab, active: bool, enabled: bool) -> Element<'a, Message> {
    let font = if active {
        theme::UI_SEMIBOLD
    } else {
        theme::UI
    };
    let underline = container(Space::new())
        .width(Length::Fill)
        .height(UNDERLINE)
        .style(move |theme: &Theme| container::Style {
            background: active.then(|| theme::tokens(theme).text.into()),
            ..container::Style::default()
        });
    // An explicit `Shrink` keeps the label's width: the underline only fills what the
    // button takes instead of stretching the button across the bar.
    let content = column![
        container(text(label(tab)).font(font))
            .padding([0, 18])
            .center_y(Length::Fill),
        underline
    ]
    .width(Length::Shrink)
    .height(Length::Fill);

    button(content)
        .padding(0)
        .height(Length::Fill)
        .style(move |theme, status| tab_style(theme, status, active))
        .on_press_maybe(enabled.then_some(Message::SelectTab(tab)))
        .into()
}

/// A tab button: plain text, in the text colour when active or hovered, dim while disabled
/// and muted otherwise.
fn tab_style(theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        text_color: if status == button::Status::Disabled {
            tokens.dim
        } else if active || status == button::Status::Hovered {
            tokens.text
        } else {
            tokens.muted
        },
        ..button::Style::default()
    }
}
