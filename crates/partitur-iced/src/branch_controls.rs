//! The controls that pick a session's branch: the two switches of the folder (`Main checkout`
//! or `Worktree`) and of the branch (`As is`, `Existing` or `New`), the type-ahead picker of an
//! existing branch with the `use existing` link, the picker of the branch a new branch starts
//! from, and the checkbox that fetches that base first. The new-session sheet shows them, and
//! so does the edit form of a child on the card of a proposed split or session.
//!
//! Each control takes the message it sends as an `Option`: `None` draws it disabled, as
//! while a Create or an approval is in flight. A switch always shows all its segments; one the
//! session is not offered is drawn disabled, so the folder switch offers `Worktree` only where
//! a worktree can be made and the branch switch never offers `As is` in a worktree. A picker
//! that cannot be opened is a frame with text, since iced's pick list and combo box have no
//! disabled state; the base picker shows `loading…` there until git has described the
//! repository, and `per child` when there is no common base. The base picker can mark one
//! branch as the proposer's, `<branch>  (proposer's branch)`, which the card of a proposed
//! session does for the branch its proposer works on.
//!
//! The existing-branch picker lists the repository's local branches as the core orders them,
//! those checked out in a worktree first: each by its name, followed by the worktree's folder
//! (the home folder written `~`), by `checked out` for the branch of the main checkout, or by
//! `worktree gone` when git lists its only worktree as prunable. Typing filters the list and
//! Enter picks the first entry left. Its list lives in the interface's state
//! ([`ExistingPick`]), which is built anew only when the branches change, so what is typed
//! survives every other redraw; while git has not answered it is a frame that says `loading…`,
//! and while disabled one that shows the branch picked.

use std::fmt;
use std::path::Path;

use iced::widget::{Id, button, checkbox, combo_box, container, pick_list, row, text, text_input};
use iced::{Border, Element, Theme};
use partitur_core::{BranchKind, BranchMode, ExistingOption, Folder};

use crate::app::Message;
use crate::controls::{picker_border, with_chevron_inset};
use crate::{home_path, style, theme};

/// The width of a picker.
pub(crate) const PICKER_WIDTH: f32 = 240.0;

/// The room a picker keeps around its value: above and below, then left and right.
pub(crate) const PICKER_PADDING: [f32; 2] = [6.0, 10.0];

/// The side of a checkbox.
pub(crate) const CHECKBOX_SIZE: f32 = 14.0;

/// The space between a checkbox and its label.
pub(crate) const CHECKBOX_SPACING: f32 = 8.0;

/// The room inside a switch around its segments.
const SWITCH_INSET: f32 = 2.0;

/// The room a segment of a switch keeps around its label.
const SEGMENT_PADDING: [f32; 2] = [4.0, 12.0];

/// The corner radius of a segment of a switch.
const SEGMENT_RADIUS: f32 = 4.0;

/// The name of `mode`, as a row that sums up a child's branch writes it.
pub(crate) fn mode_label(mode: BranchMode) -> &'static str {
    match mode {
        BranchMode::Worktree => "Worktree",
        BranchMode::NewBranch => "New branch",
        BranchMode::CheckedOut => "Checked-out",
        BranchMode::ExistingBranch => "Existing branch",
        BranchMode::ExistingWorktree => "Existing worktree",
    }
}

/// The segmented switch over `segments`, each a value, its label and whether it is disabled,
/// in their order, with `current` selected; each segment is found by `id` of its value and
/// sends `on_pick` with it unless it is disabled.
///
/// `on_pick: None` renders every segment disabled (a pending Create or approval).
pub(crate) fn switch<'a, T: Copy + Eq + 'a>(
    segments: &[(T, &'static str, bool)],
    current: T,
    id: impl Fn(T) -> Id,
    on_pick: Option<impl Fn(T) -> Message + 'a>,
) -> Element<'a, Message> {
    let message = move |value, disabled: bool| {
        on_pick
            .as_ref()
            .filter(|_| !disabled)
            .map(|on_pick| on_pick(value))
    };
    let segments = segments.iter().map(|&(value, label, disabled)| {
        let selected = value == current;
        let segment = button(text(label).size(theme::SMALL))
            .padding(SEGMENT_PADDING)
            .style(move |theme: &Theme, status| segment(theme, status, selected))
            .on_press_maybe(message(value, disabled));
        container(segment).id(id(value)).into()
    });
    container(row(segments).spacing(SWITCH_INSET))
        .padding(SWITCH_INSET)
        .style(framed)
        .into()
}

/// The segments of the folder switch, `Main checkout` and `Worktree`, each disabled when no
/// mode of `modes` works in that folder.
pub(crate) fn folder_segments(modes: &[BranchMode]) -> Vec<(Folder, &'static str, bool)> {
    [
        (Folder::MainCheckout, "Main checkout"),
        (Folder::Worktree, "Worktree"),
    ]
    .into_iter()
    .map(|(folder, label)| {
        let offered = modes.iter().any(|mode| mode.folder() == folder);
        (folder, label, !offered)
    })
    .collect()
}

/// The segments of the branch switch in `folder`, `As is`, `Existing` and `New`, each disabled
/// when `modes` does not offer it there; `As is` is never offered in a worktree.
pub(crate) fn kind_segments(
    modes: &[BranchMode],
    folder: Folder,
) -> Vec<(BranchKind, &'static str, bool)> {
    [
        (BranchKind::AsIs, "As is"),
        (BranchKind::Existing, "Existing"),
        (BranchKind::New, "New"),
    ]
    .into_iter()
    .map(|(kind, label)| {
        let offered =
            BranchMode::from_parts(folder, kind).is_some_and(|mode| modes.contains(&mode));
        (kind, label, !offered)
    })
    .collect()
}

/// One entry of the existing-branch picker: the branch and what follows its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingLabel {
    /// The branch.
    name: String,
    /// Its worktree's folder, `checked out` or `worktree gone`; `None` for a branch checked
    /// out nowhere.
    mark: Option<String>,
}

impl ExistingLabel {
    /// The entry for `option`, its worktree's folder written from `home` with `~`.
    fn of(option: &ExistingOption, home: Option<&Path>) -> Self {
        let mark = match &option.worktree {
            Some(path) => Some(home_path::abbreviated(path, home)),
            None if option.main => Some("checked out".to_owned()),
            None if option.gone => Some("worktree gone".to_owned()),
            None => None,
        };
        Self {
            name: option.name.clone(),
            mark,
        }
    }
}

impl fmt::Display for ExistingLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)?;
        if let Some(mark) = &self.mark {
            write!(formatter, "  {mark}")?;
        }
        Ok(())
    }
}

/// What the existing-branch picker keeps between redraws: its combo box's state, with what
/// is typed in it, and the branches that state was built from.
#[derive(Debug, Default)]
pub(crate) struct ExistingPick {
    /// The combo box's state over the entries of `options`.
    state: combo_box::State<ExistingLabel>,
    /// The branches the state lists.
    options: Vec<ExistingOption>,
}

impl ExistingPick {
    /// Lists `options`, their worktrees written from `home` with `~`; the state, and what is
    /// typed in it, is kept when the branches are the ones it lists already.
    pub(crate) fn sync(&mut self, options: &[ExistingOption], home: Option<&Path>) {
        if self.options == options {
            return;
        }
        let labels = options
            .iter()
            .map(|option| ExistingLabel::of(option, home))
            .collect();
        self.state = combo_box::State::new(labels);
        self.options = options.to_vec();
    }

    /// The entries listed, in their order.
    pub(crate) fn labels(&self) -> impl Iterator<Item = &ExistingLabel> {
        self.state.options().iter()
    }

    /// The entry of the branch `picked`, `None` when none is picked or it is not listed.
    pub(crate) fn label_of(&self, picked: Option<&str>) -> Option<&ExistingLabel> {
        let picked = picked?;
        self.labels().find(|label| label.name == picked)
    }
}

/// The type-ahead picker over the branches `pick` lists, with the branch `picked` shown, that
/// sends `on_pick` with the branch picked; the caller gives it its id. While git is `loading`
/// it is a frame that says `loading…`, and with `on_pick: None` one that shows `picked`.
pub(crate) fn existing_picker<'a>(
    pick: &'a ExistingPick,
    picked: Option<&str>,
    loading: bool,
    on_pick: Option<impl Fn(String) -> Message + 'static>,
) -> Element<'a, Message> {
    match on_pick {
        _ if loading => frame(text("loading…").size(theme::SMALL).style(style::dim).into()),
        None => frame(
            text(picked.unwrap_or_default().to_owned())
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft)
                .into(),
        ),
        Some(on_pick) => combo_box(
            &pick.state,
            "find a branch",
            pick.label_of(picked),
            move |label: ExistingLabel| on_pick(label.name),
        )
        .width(PICKER_WIDTH)
        .padding(PICKER_PADDING)
        .size(theme::SMALL)
        .font(theme::MONO)
        .input_style(field)
        .menu_style(style::menu)
        .into(),
    }
}

/// The `use existing` link, which works on the existing branch the typed name names instead:
/// it sends `on_press`, and is disabled with `on_press: None`; the caller gives it its id.
pub(crate) fn use_existing_link<'a>(on_press: Option<Message>) -> Element<'a, Message> {
    button(text("use existing").size(theme::SMALL))
        .padding(0)
        .style(style::link_button)
        .on_press_maybe(on_press)
        .into()
}

/// A field that is typed in: the window colour inside a picker's outline, which takes the
/// input border colour under the pointer and while the field has the focus.
pub(crate) fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let tokens = theme::tokens(theme);
    let outline = match status {
        text_input::Status::Focused { .. } | text_input::Status::Hovered => tokens.border_input,
        text_input::Status::Active | text_input::Status::Disabled => tokens.border,
    };
    text_input::Style {
        background: tokens.bg.into(),
        border: picker_border(outline),
        icon: tokens.muted,
        placeholder: tokens.dim,
        value: if status == text_input::Status::Disabled {
            tokens.text_soft
        } else {
            tokens.text
        },
        selection: tokens.text_selection,
    }
}

/// One branch the base picker offers: its `name`, marked when it is the `proposer`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaseOption {
    /// The branch.
    pub(crate) name: String,
    /// Whether it is the branch the session that proposed this one works on.
    pub(crate) proposer: bool,
}

impl fmt::Display for BaseOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)?;
        if self.proposer {
            formatter.write_str("  (proposer's branch)")?;
        }
        Ok(())
    }
}

/// The `bases` as the picker offers them, with the one named `proposer` marked.
pub(crate) fn base_options(bases: &[String], proposer: Option<&str>) -> Vec<BaseOption> {
    bases
        .iter()
        .map(|name| BaseOption {
            name: name.clone(),
            proposer: proposer == Some(name.as_str()),
        })
        .collect()
}

/// The picker over `bases`, found by `id`, with `base` selected and the branch `proposer`
/// marked as the proposer's, that sends `on_pick` with the branch picked. While git is
/// `loading` it is a frame that says `loading…`, without bases one that says `per child`, and
/// with `on_pick: None` one that shows `base`.
pub(crate) fn base_picker<'a>(
    bases: &[String],
    base: Option<&str>,
    proposer: Option<&str>,
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
            let options = base_options(bases, proposer);
            let selected = base.map(|base| BaseOption {
                name: base.to_owned(),
                proposer: proposer == Some(base),
            });
            let picker = pick_list(options, selected, move |picked: BaseOption| {
                on_pick(picked.name)
            })
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
    frame_of_width(content, PICKER_WIDTH)
}

/// `content` in the frame of a picker that is `width` wide.
pub(crate) fn frame_of_width(content: Element<'_, Message>, width: f32) -> Element<'_, Message> {
    container(content)
        .width(width)
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

/// A segment of a switch: the selection colour behind the `selected` one, a faint
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
