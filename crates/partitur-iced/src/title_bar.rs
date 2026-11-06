//! The window's own title bar: the tabs centered, the window controls on the right, and
//! everywhere else a handle that moves the window.
//!
//! The window has no system decorations, so this bar provides everything they would:
//! pressing it starts a move, a double click toggles maximized, and the three controls
//! minimize, maximize and close.

use iced::widget::{Space, button, column, container, mouse_area, row, rule, space, text};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, Tab};

/// The bar's height in logical pixels.
const HEIGHT: f32 = 36.0;

/// The thickness of the line under the active tab.
const UNDERLINE: f32 = 2.0;

/// The tabs in the order the bar shows them.
const TABS: [Tab; 3] = [Tab::Projects, Tab::Session, Tab::Env];

/// The title bar with `active` marked as the selected tab.
///
/// A row of a flexible spacer, the centered tab buttons and the window controls ("—", "▢",
/// "✕"); the spacer and the controls' container share the free width equally, so the tabs
/// stay centered in the window. Every press that no button takes becomes
/// [`Message::DragWindow`], a double click [`Message::ToggleMaximize`].
pub fn title_bar<'a>(active: Tab) -> Element<'a, Message> {
    let tabs = row(TABS.into_iter().map(|tab| tab_button(tab, tab == active)))
        .spacing(4)
        .align_y(Alignment::End);
    let controls = row![
        control("—", Message::Minimize, control_style),
        control("▢", Message::ToggleMaximize, control_style),
        control("✕", Message::CloseWindow, close_style),
    ];
    let bar = container(
        row![
            space::horizontal(),
            tabs,
            container(controls).align_right(Length::Fill),
        ]
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .height(HEIGHT)
    .style(bar_style);

    mouse_area(column![bar, rule::horizontal(1)])
        .on_press(Message::DragWindow)
        .on_double_click(Message::ToggleMaximize)
        .into()
}

/// The label a tab's button shows.
fn label(tab: Tab) -> &'static str {
    match tab {
        Tab::Projects => "Projects",
        Tab::Session => "Session",
        Tab::Env => "Env",
    }
}

/// One tab's button: its label over an underline that only the active tab colors.
fn tab_button<'a>(tab: Tab, active: bool) -> Element<'a, Message> {
    let underline = container(Space::new())
        .width(Length::Fill)
        .height(UNDERLINE)
        .style(move |theme| underline_style(theme, active));
    // An explicit `Shrink` keeps the label's width: the underline only fills what the text
    // takes instead of stretching the button across the bar.
    let content = column![text(label(tab)), underline]
        .spacing(4)
        .width(Length::Shrink)
        .align_x(Alignment::Center);

    button(content)
        .padding([4, 12])
        .style(move |theme, status| tab_style(theme, status, active))
        .on_press(Message::SelectTab(tab))
        .into()
}

/// One window control showing `glyph`.
fn control(
    glyph: &str,
    message: Message,
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'_, Message> {
    button(text(glyph).center().width(Length::Fill))
        .width(46)
        .height(HEIGHT)
        .style(style)
        .on_press(message)
        .into()
}

/// The bar's background: a step away from the window background so the bar reads as chrome.
fn bar_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.background.weak.color.into()),
        text_color: Some(palette.background.weak.text),
        ..container::Style::default()
    }
}

/// The active tab's underline takes the palette's primary color; the others stay invisible.
fn underline_style(theme: &Theme, active: bool) -> container::Style {
    container::Style {
        background: active.then(|| theme.extended_palette().primary.base.color.into()),
        ..container::Style::default()
    }
}

/// A tab button: plain text, full strength when active, dimmed otherwise.
fn tab_style(theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let palette = theme.extended_palette();
    let text_color = if active || status == button::Status::Hovered {
        palette.background.weak.text
    } else {
        palette.background.weak.text.scale_alpha(0.6)
    };
    button::Style {
        text_color,
        border: Border::default(),
        ..button::Style::default()
    }
}

/// Minimize and maximize: transparent until hovered.
fn control_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then(|| palette.background.strong.color.into()),
        text_color: palette.background.weak.text,
        ..button::Style::default()
    }
}

/// Close: like the other controls, but hovering it shows the palette's danger color.
fn close_style(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    match status {
        button::Status::Hovered | button::Status::Pressed => button::Style {
            background: Some(palette.danger.base.color.into()),
            text_color: palette.danger.base.text,
            ..button::Style::default()
        },
        button::Status::Active | button::Status::Disabled => control_style(theme, status),
    }
}
