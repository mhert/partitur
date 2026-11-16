//! The small markers rows carry: pills for a status or a kind, key caps for shortcuts and
//! the dots of the status line. A child session that waits for its orchestrator at a gate or
//! with a question carries `waits for orchestrator` in the muted text colour on the neutral
//! surface, in both themes, instead of the amber `asks you`: it does not wait for the user.

use iced::widget::{Space, container, text};
use iced::{Border, Color, Element, Pixels, Theme};
use partitur_core::{Status, background_shown};

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

/// The word of a child session that waits for its orchestrator, not for the user.
pub const WAITS_FOR_ORCHESTRATOR: &str = "waits for orchestrator";

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

/// The pill of a session at `status` whose agent runs `background` in the background:
/// [`WAITS_FOR_ORCHESTRATOR`] in the [`delegated_tint`] when it is `delegated`, the running
/// background tasks in the running tint once its turn is over, else its status pill.
pub fn session<'a, Message: 'a>(
    status: Status,
    background: usize,
    delegated: bool,
) -> Element<'a, Message> {
    if delegated {
        pill(WAITS_FOR_ORCHESTRATOR, delegated_tint)
    } else if background_shown(status, background) {
        pill(session_word(status, background, false), |tokens| {
            tokens.running
        })
    } else {
        self::status(status)
    }
}

/// The tint of a session that waits for its orchestrator: muted letters on the neutral
/// surface, so it stays behind the sessions that need the user.
fn delegated_tint(tokens: &Tokens) -> Tint {
    Tint {
        fg: tokens.muted,
        bg: tokens.done.bg,
    }
}

/// What the pill of a session at `status` running `background` says:
/// [`WAITS_FOR_ORCHESTRATOR`] when it is `delegated`, `1 background task`, `N background tasks` once its
/// turn is over and any run, else the word of its status.
pub fn session_word(status: Status, background: usize, delegated: bool) -> String {
    match background {
        _ if delegated => WAITS_FOR_ORCHESTRATOR.to_owned(),
        _ if !background_shown(status, background) => status_word(status).to_owned(),
        1 => "1 background task".to_owned(),
        count => format!("{count} background tasks"),
    }
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

/// The label of the key that holds a shortcut's command, the one iced's
/// `Modifiers::command` reads: `⌘` on macOS and `Ctrl` elsewhere.
pub const COMMAND_KEY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

/// The words of the shortcut that is the command key and `key`, as the platform writes it:
/// `⌘K` on macOS and `Ctrl K` elsewhere.
pub fn command_shortcut(key: &str) -> String {
    let gap = if cfg!(target_os = "macos") { "" } else { " " };
    format!("{COMMAND_KEY}{gap}{key}")
}

/// A key cap naming a shortcut, such as [`command_shortcut`]'s `⌘K` or `Ctrl K`, or `Enter`.
pub fn key_cap<'a, Message: 'a>(keys: impl text::IntoFragment<'a>) -> Element<'a, Message> {
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
    keys: impl text::IntoFragment<'a>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_names_the_command_key_the_platform_shows() {
        if cfg!(target_os = "macos") {
            assert_eq!(COMMAND_KEY, "⌘");
            assert_eq!(command_shortcut("K"), "⌘K");
        } else {
            assert_eq!(COMMAND_KEY, "Ctrl");
            assert_eq!(command_shortcut("K"), "Ctrl K");
        }
    }

    #[test]
    fn a_session_whose_turn_is_over_names_its_running_background_tasks() {
        assert_eq!(session_word(Status::Done, 1, false), "1 background task");
        assert_eq!(session_word(Status::Done, 2, false), "2 background tasks");
        assert_eq!(session_word(Status::Idle, 3, false), "3 background tasks");
        assert_eq!(session_word(Status::Done, 0, false), "done");
        assert_eq!(session_word(Status::Running, 2, false), "running");
        assert_eq!(session_word(Status::WaitingForYou, 2, false), "asks you");
    }

    #[test]
    fn a_delegated_session_waits_for_its_orchestrator_in_the_muted_colour() {
        assert_eq!(
            session_word(Status::WaitingForYou, 0, true),
            "waits for orchestrator"
        );
        assert_eq!(
            session_word(Status::Done, 2, true),
            "waits for orchestrator"
        );
        for tokens in [&theme::LIGHT, &theme::DARK] {
            let tint = delegated_tint(tokens);
            assert_eq!(tint.fg, tokens.muted);
            assert_eq!(tint.bg, tokens.done.bg);
            assert_ne!(tint.fg, tokens.asks_you.fg);
        }
    }
}
