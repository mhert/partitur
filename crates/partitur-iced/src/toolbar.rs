//! The toolbar, which is the window's title bar: the wordmark on the left, the tabs centered
//! and, on the right, the button that opens the jump panel, with the keys that do the same.
//!
//! When Partitur draws the window's frame itself, the window's minimize, maximize and close
//! buttons follow the jump button in the right corner, and pressing the bar anywhere outside
//! its buttons moves the window, a double click maximizes or restores it and a right click
//! opens the system's window menu. On macOS the bar keeps room at its left for the traffic
//! lights. When the system draws the frame, the toolbar sits under the system's title bar and
//! does none of that.

use iced::widget::svg;
use iced::widget::{Id, Space, button, column, container, mouse_area, opaque, row, text};
use iced::{Alignment, Color, Element, Length, Padding, Theme};

use crate::app::{Message, Tab};
use crate::chrome::{Chrome, WindowAction};
use crate::icon::{self, Icon};
use crate::jump::JumpMessage;
use crate::{badge, style, theme};

/// The toolbar's height in logical pixels.
const HEIGHT: f32 = 44.0;

/// The width of the wordmark's slot, not counting the room kept free left of it.
const SIDE: f32 = 180.0;

/// The width of one of the window's buttons.
const CONTROL: f32 = 40.0;

/// The space between the bar's edges and its content, except at the right when the window's
/// buttons sit in its corner.
const GUTTER: f32 = 16.0;

/// The thickness of the line under the active tab.
const UNDERLINE: f32 = 2.0;

/// The application's name as the toolbar shows it.
const WORDMARK: &str = "Partitur";

/// The tabs in the order the toolbar shows them, which is the order `Tab` cycles through.
pub(crate) const TABS: [Tab; 4] = [Tab::Projects, Tab::Session, Tab::Spec, Tab::Env];

/// The toolbar with `active` marked as the selected tab, over the line that parts it from
/// the tab's content. A tab can be picked only when `enabled` lets it through; otherwise it
/// is drawn dim and does nothing. `chrome` decides whether the bar draws the window's
/// buttons, whether pressing it moves the window and how much room it keeps free at its
/// left; `maximized` turns the maximize button into the one that restores the window.
pub(crate) fn toolbar<'a>(
    active: Tab,
    enabled: impl Fn(Tab) -> bool,
    chrome: Chrome,
    maximized: bool,
) -> Element<'a, Message> {
    let wordmark = text(WORDMARK)
        .size(theme::HEADING)
        .font(theme::UI_SEMIBOLD)
        .style(style::ink(|tokens| tokens.muted_strong));
    let tabs = row(TABS
        .into_iter()
        .map(|tab| tab_button(tab, tab == active, enabled(tab))))
    .spacing(4)
    .height(Length::Fill);
    let mut right = row![jump_button()].align_y(Alignment::Center);
    if chrome.controls {
        right = right
            .push(Space::new().width(8))
            .extend(controls(maximized));
    }
    let bar = container(
        row![
            container(wordmark)
                .width(SIDE + chrome.leading_inset)
                .padding(Padding::ZERO.left(chrome.leading_inset)),
            container(tabs).center_x(Length::Fill),
            right,
        ]
        .height(Length::Fill)
        .align_y(Alignment::Center),
    )
    .height(HEIGHT)
    .padding(Padding {
        left: GUTTER,
        right: if chrome.controls { 0.0 } else { GUTTER },
        ..Padding::ZERO
    })
    .style(style::fill(|tokens| tokens.panel));
    let bar: Element<'a, Message> = if chrome.drags {
        mouse_area(bar)
            .on_press(Message::Window(WindowAction::Drag))
            .on_double_click(Message::Window(WindowAction::ToggleMaximize))
            .on_right_press(Message::Window(WindowAction::Menu))
            .into()
    } else {
        bar.into()
    };

    column![bar, style::hairline(|tokens| tokens.border)].into()
}

/// The window's minimize, maximize and close buttons, the maximize button showing the icon
/// that restores the window while it is `maximized`.
fn controls<'a>(maximized: bool) -> [Element<'a, Message>; 3] {
    let maximize = if maximized {
        Icon::Restore
    } else {
        Icon::Maximize
    };
    [
        control(
            "window-minimize",
            Icon::Minimize,
            WindowAction::Minimize,
            false,
        ),
        control(
            "window-maximize",
            maximize,
            WindowAction::ToggleMaximize,
            false,
        ),
        control("window-close", Icon::Close, WindowAction::Close, true),
    ]
}

/// One window button named `id`, showing `icon` and asking `action` when pressed. While
/// hovered or pressed it takes the selection's surface, or the danger surface with a white
/// icon when it is the `close` button.
///
/// The icon fills the whole button, so the mouse is over the icon exactly when it is over the
/// button, and the icon's own hover status picks its colour in step with the button's.
fn control<'a>(
    id: &'static str,
    icon: Icon,
    action: WindowAction,
    close: bool,
) -> Element<'a, Message> {
    let icon = icon::centered(icon, move |theme, status| {
        let tokens = theme::tokens(theme);
        match (status, close) {
            (svg::Status::Hovered, true) => Color::WHITE,
            (svg::Status::Hovered, false) => tokens.text,
            (svg::Status::Idle, _) => tokens.muted,
        }
    });
    let control = button(icon)
        .width(CONTROL)
        .height(HEIGHT)
        .padding(0)
        .style(move |theme, status| control_style(theme, status, close))
        .on_press(Message::Window(action));
    container(control).id(Id::new(id)).into()
}

/// A window button: no surface while idle, the selection's surface while hovered or pressed,
/// or the danger surface for the `close` button.
fn control_style(theme: &Theme, status: button::Status, close: bool) -> button::Style {
    let tokens = theme::tokens(theme);
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let surface = if close {
        tokens.danger_edge
    } else {
        tokens.selection
    };
    button::Style {
        background: hovered.then(|| surface.into()),
        ..button::Style::default()
    }
}

/// The button that opens the jump panel: what it does in words, then the keys that do the
/// same.
fn jump_button<'a>() -> Element<'a, Message> {
    button(
        row![
            text("Jump to session").size(theme::SMALL),
            badge::key_cap(badge::command_shortcut("K"))
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
/// that is not `enabled` cannot be pressed, and pressing it does not move the window either.
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

    // A disabled button lets a press through to the bar under it; `opaque` keeps it.
    opaque(
        button(content)
            .padding(0)
            .height(Length::Fill)
            .style(move |theme, status| tab_style(theme, status, active))
            .on_press_maybe(enabled.then_some(Message::SelectTab(tab))),
    )
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
