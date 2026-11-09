//! The styles the views share: text colours, surfaces, buttons, switches, menus and
//! Markdown, all read from the active theme's tokens.

use iced::overlay::menu;
use iced::widget::markdown::{Highlight, Settings};
use iced::widget::{Space, button, container, text, toggler};
use iced::{Border, Color, Element, Length, Padding, Pixels, Theme};

use crate::theme::{self, Tokens};

/// The corner radius of cards and the composer.
pub const CARD_RADIUS: f32 = 8.0;

/// The corner radius of rows, event rows and buttons.
pub const ROW_RADIUS: f32 = 6.0;

/// A text style in the colour `pick` takes from the active theme's tokens.
pub fn ink(pick: fn(&Tokens) -> Color) -> impl Fn(&Theme) -> text::Style {
    move |theme| text::Style {
        color: Some(pick(theme::tokens(theme))),
    }
}

/// Labels and hints.
pub fn muted(theme: &Theme) -> text::Style {
    ink(|tokens| tokens.muted)(theme)
}

/// Secondary values.
pub fn soft(theme: &Theme) -> text::Style {
    ink(|tokens| tokens.text_soft)(theme)
}

/// Separators such as `·` and `›`.
pub fn dim(theme: &Theme) -> text::Style {
    ink(|tokens| tokens.dim)(theme)
}

/// A filled surface in the colour `pick` takes from the tokens, without a border.
pub fn fill(pick: fn(&Tokens) -> Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(pick(theme::tokens(theme)).into()),
        ..container::Style::default()
    }
}

/// A one-pixel line across the full width, in the colour `pick` takes from the tokens.
pub fn hairline<'a, Message: 'a>(pick: fn(&Tokens) -> Color) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(fill(pick))
        .into()
}

/// A card: the panel colour inside a one-pixel border with rounded corners.
pub fn card(theme: &Theme) -> container::Style {
    bordered(theme, CARD_RADIUS)
}

/// An event row: like a card, with the smaller radius of rows.
pub fn event_row(theme: &Theme) -> container::Style {
    bordered(theme, ROW_RADIUS)
}

/// The panel colour inside a one-pixel border with corners of `radius`.
fn bordered(theme: &Theme, radius: f32) -> container::Style {
    let tokens = theme::tokens(theme);
    container::Style {
        background: Some(tokens.panel.into()),
        border: Border::default()
            .rounded(radius)
            .width(1)
            .color(tokens.border),
        ..container::Style::default()
    }
}

/// A row button: no chrome until hovered, then a faint background.
pub fn row_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        background: hovered(status).then(|| tokens.border_soft.into()),
        text_color: tokens.text,
        border: Border::default().rounded(ROW_RADIUS),
        ..button::Style::default()
    }
}

/// A button that looks like an event row: the panel colour inside a one-pixel border, a
/// step lighter while hovered.
pub fn event_row_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    let background = if hovered(status) {
        tokens.border_soft
    } else {
        tokens.panel
    };
    button::Style {
        background: Some(background.into()),
        text_color: tokens.text,
        border: Border::default()
            .rounded(ROW_RADIUS)
            .width(1)
            .color(tokens.border),
        ..button::Style::default()
    }
}

/// The highlighted row of a tree: the selection colour behind it, hovered or not.
pub fn selected_row_button(theme: &Theme, _status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        background: Some(tokens.selection.into()),
        text_color: tokens.text_strong,
        border: Border::default().rounded(ROW_RADIUS),
        ..button::Style::default()
    }
}

/// A text button in the muted colour that takes the text colour while hovered.
pub fn link_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        text_color: if hovered(status) {
            tokens.text
        } else {
            tokens.muted
        },
        ..button::Style::default()
    }
}

/// A text button in the colour of secondary values that takes the text colour while
/// hovered: a value that can be clicked.
pub fn soft_link_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        text_color: if hovered(status) {
            tokens.text
        } else {
            tokens.text_soft
        },
        ..button::Style::default()
    }
}

/// The primary button: the text colour as background, the window background as label, so
/// it is the lightest thing on the dark theme and the darkest on the light one.
pub fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    let strength = strength(status);
    let background = if hovered(status) {
        tokens.text_strong
    } else {
        tokens.text
    };
    button::Style {
        background: Some(background.scale_alpha(strength).into()),
        text_color: tokens.bg,
        border: Border::default().rounded(ROW_RADIUS),
        ..button::Style::default()
    }
}

/// A secondary button: an outline in the input border colour around soft text.
pub fn outlined_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    let strength = strength(status);
    button::Style {
        background: hovered(status).then(|| tokens.border_soft.into()),
        text_color: tokens.text_soft.scale_alpha(strength),
        border: Border::default()
            .rounded(ROW_RADIUS)
            .width(1)
            .color(tokens.border_input.scale_alpha(strength)),
        ..button::Style::default()
    }
}

/// A text button in the colour of removals, for controls that stop work.
pub fn danger_button(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = theme::tokens(theme);
    button::Style {
        background: hovered(status).then(|| tokens.border_soft.into()),
        text_color: tokens.diff_del.scale_alpha(strength(status)),
        border: Border::default().rounded(ROW_RADIUS),
        ..button::Style::default()
    }
}

/// A switch: a soft track with a muted knob while off, the soft text colour with a knob in
/// the window's colour while on, and a muted label.
pub fn switch(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let tokens = theme::tokens(theme);
    let on = match status {
        toggler::Status::Active { is_toggled }
        | toggler::Status::Hovered { is_toggled }
        | toggler::Status::Disabled { is_toggled } => is_toggled,
    };
    let (track, knob) = if on {
        (tokens.text_soft, tokens.bg)
    } else {
        (tokens.border_input, tokens.muted_strong)
    };
    toggler::Style {
        background: track.into(),
        background_border_width: 0.0,
        background_border_color: track,
        foreground: knob.into(),
        foreground_border_width: 0.0,
        foreground_border_color: knob,
        text_color: Some(tokens.muted),
        border_radius: None,
        padding_ratio: 0.1,
    }
}

/// The menu a picker opens: a bordered panel whose hovered entry takes the selection
/// colour.
pub fn menu(theme: &Theme) -> menu::Style {
    let tokens = theme::tokens(theme);
    menu::Style {
        background: tokens.panel.into(),
        border: Border::default()
            .rounded(ROW_RADIUS)
            .width(1)
            .color(tokens.border),
        text_color: tokens.text,
        selected_text_color: tokens.text_strong,
        selected_background: tokens.selection.into(),
        shadow: iced::Shadow::default(),
    }
}

/// How Markdown is drawn with `tokens`: prose in the interface font and size, inline code
/// monospaced on a soft chip, code blocks monospaced, links in the colour of tools.
///
/// The settings hold the colours themselves, not a way to look them up, so they are built
/// for the token set of the active theme.
pub fn markdown(tokens: &Tokens) -> Settings {
    let style = iced::widget::markdown::Style {
        font: theme::UI,
        inline_code_highlight: Highlight {
            background: tokens.border_soft.into(),
            border: Border::default().rounded(3),
        },
        inline_code_padding: Padding::from([0, 5]),
        inline_code_color: tokens.text,
        inline_code_font: theme::MONO,
        code_block_font: theme::MONO,
        link_color: tokens.tools_mcp,
    };
    Settings {
        code_size: Pixels(theme::SMALL),
        spacing: Pixels(6.0),
        ..Settings::with_text_size(theme::TEXT, style)
    }
}

/// Whether the pointer is on the button or pressing it.
fn hovered(status: button::Status) -> bool {
    matches!(status, button::Status::Hovered | button::Status::Pressed)
}

/// How strongly a button shows its colours: fully, or at half strength while disabled.
fn strength(status: button::Status) -> f32 {
    if status == button::Status::Disabled {
        0.5
    } else {
        1.0
    }
}
