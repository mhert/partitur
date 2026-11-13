//! The small markers rows carry: pills for a status or a kind, key caps for shortcuts and
//! the dots of the status line.

use iced::widget::{Space, container, text};
use iced::{Border, Color, Element, Pixels, Theme};
use partitur_core::Status;

use crate::style;
use crate::theme::{self, Tint, Tokens};

/// The corner radius of pills and key caps.
const RADIUS: f32 = 4.0;

/// The side of a status dot, in logical pixels.
const DOT: f32 = 7.0;

/// The height of a pill's line of text, in logical pixels: with the padding above and below
/// it, a pill is 18 pixels high.
const PILL_LINE_HEIGHT: f32 = 16.0;

/// The pill that marks an orchestrator session.
pub const ORCHESTRATOR: &str = "orchestrator";

/// How much of the window's colour the letters of a key cap on the primary button keep.
const ON_PRIMARY_TEXT: f32 = 0.7;

/// How much of the window's colour the outline of a key cap on the primary button keeps.
const ON_PRIMARY_OUTLINE: f32 = 0.3;

/// A pill with `label` in the tint `pick` takes from the active theme's tokens.
pub fn pill<'a, Message: 'a>(
    label: impl text::IntoFragment<'a>,
    pick: fn(&Tokens) -> Tint,
) -> Element<'a, Message> {
    container(
        text(label)
            .size(theme::TINY)
            .line_height(Pixels(PILL_LINE_HEIGHT)),
    )
    .padding([1, 8])
    .style(move |theme: &Theme| {
        let tint = pick(theme::tokens(theme));
        container::Style {
            background: Some(tint.bg.into()),
            text_color: Some(tint.fg),
            border: Border::default().rounded(RADIUS),
            ..container::Style::default()
        }
    })
    .into()
}

/// The neutral pill's surface alone, for a pill that holds more than a label.
pub fn surface(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme::tokens(theme).done.bg.into()),
        border: Border::default().rounded(RADIUS),
        ..container::Style::default()
    }
}

/// The pill that tells where a session stands. An idle session's pill is only outlined: no
/// surface, a one-pixel border and muted letters, so it stays behind the pills of sessions
/// that work or need the user.
pub fn status<'a, Message: 'a>(status: Status) -> Element<'a, Message> {
    let pick: fn(&Tokens) -> Tint = match status {
        Status::Idle => return idle(),
        Status::Running => |tokens| tokens.running,
        Status::WaitingForYou | Status::Interrupted => |tokens| tokens.asks_you,
        Status::Done => |tokens| tokens.done,
        Status::Failed => |tokens| tokens.failed,
    };
    pill(status_word(status), pick)
}

/// The word that names `status` on its pill and wherever a session's status is written out.
pub fn status_word(status: Status) -> &'static str {
    match status {
        Status::Idle => "idle",
        Status::Running => "running",
        Status::WaitingForYou => "asks you",
        Status::Interrupted => "interrupted",
        Status::Done => "done",
        Status::Failed => "failed",
    }
}

/// The outlined pill of an idle session.
fn idle<'a, Message: 'a>() -> Element<'a, Message> {
    outlined(status_word(Status::Idle))
}

/// A pill with `label` that is only outlined: no surface, a one-pixel border and muted
/// letters. It is as high as every other pill: its border takes the place of a pixel of
/// padding above and below.
pub fn outlined<'a, Message: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(
        text(label)
            .size(theme::TINY)
            .line_height(Pixels(PILL_LINE_HEIGHT))
            .style(style::muted),
    )
    .padding([0, 7])
    .style(|theme: &Theme| container::Style {
        border: Border::default()
            .rounded(RADIUS)
            .width(1)
            .color(theme::tokens(theme).border),
        ..container::Style::default()
    })
    .into()
}

/// A neutral pill with `label`, for facts such as a tool's kind or a repository's name.
pub fn chip<'a, Message: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    pill(label, |tokens| tokens.done)
}

/// The pill that marks an orchestrator session.
pub fn orchestrator<'a, Message: 'a>() -> Element<'a, Message> {
    pill(ORCHESTRATOR, |tokens| tokens.orchestrator)
}

/// A key cap naming a shortcut in words every platform uses, such as `Ctrl K` or `Enter`.
pub fn key_cap<'a, Message: 'a>(keys: &'a str) -> Element<'a, Message> {
    cap(keys, |tokens| tokens.muted_strong, |tokens| tokens.border)
}

/// A key cap on the archive button: soft letters inside the archive outline.
pub fn key_cap_on_archive<'a, Message: 'a>(keys: &'a str) -> Element<'a, Message> {
    cap(
        keys,
        |tokens| tokens.text_soft,
        |tokens| tokens.archive_edge,
    )
}

/// A key cap on the delete button: letters in the failed colour inside the danger edge.
pub fn key_cap_on_delete<'a, Message: 'a>(keys: &'a str) -> Element<'a, Message> {
    cap(keys, |tokens| tokens.failed.fg, |tokens| tokens.danger_edge)
}

/// A key cap on the primary button, whose surface is the text colour: the cap's letters
/// and outline are the window's colour, the label's, thinned so the cap stays behind the
/// label.
pub fn key_cap_on_primary<'a, Message: 'a>(keys: &'a str) -> Element<'a, Message> {
    cap(
        keys,
        |tokens| tokens.bg.scale_alpha(ON_PRIMARY_TEXT),
        |tokens| tokens.bg.scale_alpha(ON_PRIMARY_OUTLINE),
    )
}

/// A key cap with `keys` written in the colour `ink` picks from the tokens, inside an
/// outline in the colour `outline` picks.
fn cap<'a, Message: 'a>(
    keys: &'a str,
    ink: fn(&Tokens) -> Color,
    outline: fn(&Tokens) -> Color,
) -> Element<'a, Message> {
    container(
        text(keys)
            .size(theme::TINY)
            .font(theme::MONO)
            .style(style::ink(ink)),
    )
    .padding([0, 5])
    .style(move |theme: &Theme| container::Style {
        border: Border::default()
            .rounded(RADIUS)
            .width(1)
            .color(outline(theme::tokens(theme))),
        ..container::Style::default()
    })
    .into()
}

/// A round dot in the colour `pick` takes from the active theme's tokens.
pub fn dot<'a, Message: 'a>(pick: fn(&Tokens) -> Color) -> Element<'a, Message> {
    container(Space::new())
        .width(DOT)
        .height(DOT)
        .style(move |theme: &Theme| container::Style {
            background: Some(pick(theme::tokens(theme)).into()),
            border: Border::default().rounded(DOT / 2.0),
            ..container::Style::default()
        })
        .into()
}
