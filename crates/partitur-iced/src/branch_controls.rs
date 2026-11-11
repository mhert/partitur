//! The controls that pick a session's branch: the switch between the three branch modes, the
//! picker of the branch a new branch starts from, and the checkbox that fetches that base
//! first. The new-session sheet shows them, and so does the edit form of a proposed child.
//!
//! Each control takes the message it sends as an `Option`: `None` draws it disabled, as
//! while a Create or an approval is in flight. A picker that cannot be opened is a frame with
//! text, since iced's pick list has no disabled state; the base picker shows `loading…` there
//! until git has described the repository, and `per child` when there is no common base.

use iced::widget::{Id, button, checkbox, container, pick_list, row, text};
use iced::{Border, Color, Element, Theme};
use partitur_core::BranchMode;

use crate::app::Message;
use crate::session_view::with_chevron_inset;
use crate::{style, theme};

/// The width of a picker.
pub(crate) const PICKER_WIDTH: f32 = 240.0;

/// The room a picker keeps around its value: above and below, then left and right.
pub(crate) const PICKER_PADDING: [f32; 2] = [6.0, 10.0];

/// The corner radius of a picker.
const PICKER_RADIUS: f32 = 6.0;

/// The side of a checkbox.
pub(crate) const CHECKBOX_SIZE: f32 = 14.0;

/// The space between a checkbox and its label.
pub(crate) const CHECKBOX_SPACING: f32 = 8.0;

/// The room inside the mode switch around its segments.
const SWITCH_INSET: f32 = 2.0;

/// The room a segment of the mode switch keeps around its label.
const SEGMENT_PADDING: [f32; 2] = [4.0, 12.0];

/// The corner radius of a segment of the mode switch.
const SEGMENT_RADIUS: f32 = 4.0;

/// The label of `mode` on the switch.
pub(crate) fn mode_label(mode: BranchMode) -> &'static str {
    match mode {
        BranchMode::Worktree => "Worktree",
        BranchMode::NewBranch => "New branch",
        BranchMode::CheckedOut => "Checked-out",
    }
}

/// The segmented switch over the `offered` modes, in their order, with `current` selected;
/// each segment is found by `id` of its mode and sends `on_pick` with it.
///
/// `on_pick: None` renders every control disabled (a pending Create or approval).
pub(crate) fn mode_switch<'a>(
    offered: &[BranchMode],
    current: BranchMode,
    id: impl Fn(BranchMode) -> Id,
    on_pick: Option<impl Fn(BranchMode) -> Message + 'a>,
) -> Element<'a, Message> {
    let message = move |mode| on_pick.as_ref().map(|on_pick| on_pick(mode));
    let segments = offered.iter().map(|&mode| {
        let selected = mode == current;
        let segment = button(text(mode_label(mode)).size(theme::SMALL))
            .padding(SEGMENT_PADDING)
            .style(move |theme: &Theme, status| segment(theme, status, selected))
            .on_press_maybe(message(mode));
        container(segment).id(id(mode)).into()
    });
    container(row(segments).spacing(SWITCH_INSET))
        .padding(SWITCH_INSET)
        .style(framed)
        .into()
}

/// The picker over `bases`, found by `id`, with `base` selected, that sends `on_pick` with
/// the branch picked. While git is `loading` it is a frame that says `loading…`, without
/// bases one that says `per child`, and with `on_pick: None` one that shows `base`.
pub(crate) fn base_picker<'a>(
    bases: &[String],
    base: Option<&str>,
    loading: bool,
    id: Id,
    on_pick: Option<impl Fn(String) -> Message + 'a>,
) -> Element<'a, Message> {
    let control = match on_pick {
        _ if loading => frame(text("loading…").size(theme::SMALL).style(style::dim).into()),
        _ if bases.is_empty() => frame(
            text("per child")
                .size(theme::SMALL)
                .style(style::soft)
                .into(),
        ),
        None => frame(
            text(base.unwrap_or_default().to_owned())
                .size(theme::SMALL)
                .style(style::soft)
                .into(),
        ),
        Some(on_pick) => {
            let picker = pick_list(bases.to_vec(), base.map(str::to_owned), on_pick)
                .width(PICKER_WIDTH)
                .padding(PICKER_PADDING)
                .text_size(theme::SMALL)
                .handle(pick_list::Handle::None)
                .style(framed_picker)
                .menu_style(style::menu);
            with_chevron_inset(picker, PICKER_PADDING[1])
        }
    };
    container(control).id(id).into()
}

/// The checkbox, found by `id`, that fetches `remote` before the new branch starts from
/// `<remote>/<base>`, and sends `on_toggle`. It cannot be checked, and says why, when the base
/// has no remote counterpart or there is no base; while git is `loading` it says `loading…`.
pub(crate) fn fetch_toggle<'a>(
    fetch: bool,
    remote: Option<&str>,
    base: Option<&str>,
    loading: bool,
    id: Id,
    on_toggle: Option<impl Fn(bool) -> Message + 'a>,
) -> Element<'a, Message> {
    let (label, possible) = match (remote, base) {
        _ if loading => ("loading…".to_owned(), false),
        (Some(remote), Some(base)) => (
            format!("Fetch {remote} and start from {remote}/{base}"),
            true,
        ),
        (None, Some(base)) => (format!("{base} has no remote counterpart"), false),
        (_, None) => ("no base branch to fetch".to_owned(), false),
    };
    fetch_checkbox(label, fetch, id, on_toggle.filter(|_| possible))
}

/// A checkbox with the small `label`, found by `id`, checked when `fetch`, that sends
/// `on_toggle`; disabled with `on_toggle: None`.
pub(crate) fn fetch_checkbox<'a>(
    label: String,
    fetch: bool,
    id: Id,
    on_toggle: Option<impl Fn(bool) -> Message + 'a>,
) -> Element<'a, Message> {
    let toggle = checkbox(fetch)
        .label(label)
        .on_toggle_maybe(on_toggle)
        .size(CHECKBOX_SIZE)
        .spacing(CHECKBOX_SPACING)
        .text_size(theme::SMALL);
    container(toggle).id(id).into()
}

/// `content` in the frame of a picker, for a picker that cannot be opened.
pub(crate) fn frame(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .width(PICKER_WIDTH)
        .padding(PICKER_PADDING)
        .style(framed)
        .into()
}

/// The look of a picker's frame around something that is no pick list: the window colour
/// inside the picker's outline.
fn framed(theme: &Theme) -> container::Style {
    let tokens = theme::tokens(theme);
    container::Style {
        background: Some(tokens.bg.into()),
        border: picker_border(tokens.border),
        ..container::Style::default()
    }
}

/// A picker with a frame: the window colour inside a one-pixel outline, which takes the
/// input border colour under the pointer and while the picker is open.
pub(crate) fn framed_picker(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let tokens = theme::tokens(theme);
    let outline = match status {
        pick_list::Status::Active => tokens.border,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => tokens.border_input,
    };
    pick_list::Style {
        text_color: tokens.text,
        placeholder_color: tokens.muted,
        handle_color: tokens.muted,
        background: tokens.bg.into(),
        border: picker_border(outline),
    }
}

/// The rounded one-pixel outline of a picker in `color`.
pub(crate) fn picker_border(color: Color) -> Border {
    Border::default()
        .rounded(PICKER_RADIUS)
        .width(1)
        .color(color)
}

/// A segment of the mode switch: the selection colour behind the `selected` one, a faint
/// background under the pointer, and thinned colours while the switch is disabled.
fn segment(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let tokens = theme::tokens(theme);
    let strength = if status == button::Status::Disabled {
        0.5
    } else {
        1.0
    };
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (background, label) = if selected {
        (Some(tokens.selection), tokens.text_strong)
    } else if hovered {
        (Some(tokens.border_soft), tokens.text)
    } else {
        (None, tokens.text_soft)
    };
    button::Style {
        background: background.map(|color| color.scale_alpha(strength).into()),
        text_color: label.scale_alpha(strength),
        border: Border::default().rounded(SEGMENT_RADIUS),
        ..button::Style::default()
    }
}
