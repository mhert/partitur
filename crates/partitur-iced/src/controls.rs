//! The controls the tabs and sheets share: the pickers' chevron, the entries of the backend
//! picker and of a picker over a session's choice of model or effort, the outline of a
//! framed picker, and how far a bar of context usage is filled.
//!
//! The pickers draw no handle of their own: [`with_chevron`] and [`with_chevron_inset`] lay
//! a chevron over the room a pick list keeps for one, so a click on the chevron opens the
//! picker like one on the value.

use std::fmt;

use iced::widget::{container, stack};
use iced::{Border, Color, Element, Length, Padding};
use partitur_core::{BackendId, BackendInfo, ConfigValueId};

use crate::app::Message;
use crate::icon::{Icon, icon};

/// The side of a picker's chevron, in logical pixels.
const CHEVRON_SIZE: f32 = 10.0;

/// The corner radius of a picker.
const PICKER_RADIUS: f32 = 6.0;

/// `picker` with a chevron at its right end. A pick list keeps room for a handle there
/// even when it draws none; the chevron lies over that room and takes no clicks, so a
/// click on it opens the picker like one on the value.
pub(crate) fn with_chevron<'a>(picker: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    with_chevron_inset(picker, 0.0)
}

/// `picker` with a chevron `inset` logical pixels in from its right end, as
/// [`with_chevron`] draws it, for a picker whose frame the chevron must not touch.
pub(crate) fn with_chevron_inset<'a>(
    picker: impl Into<Element<'a, Message>>,
    inset: f32,
) -> Element<'a, Message> {
    let chevron = icon(Icon::ChevronDown, |tokens| tokens.muted)
        .width(CHEVRON_SIZE)
        .height(CHEVRON_SIZE);
    stack![
        picker.into(),
        container(chevron)
            .align_right(Length::Fill)
            .center_y(Length::Fill)
            .padding(Padding::default().right(inset))
    ]
    .into()
}

/// The rounded one-pixel outline of a picker in `color`.
pub(crate) fn picker_border(color: Color) -> Border {
    Border::default()
        .rounded(PICKER_RADIUS)
        .width(1)
        .color(color)
}

/// One configured backend in the backend picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BackendChoice {
    pub(crate) id: BackendId,
    pub(crate) name: String,
}

impl From<&BackendInfo> for BackendChoice {
    fn from(info: &BackendInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
        }
    }
}

impl fmt::Display for BackendChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// One value in a picker, shown by its name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: ConfigValueId,
    pub(crate) name: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// How many of a bar's `width` pixels (or equal parts of any other kind) are filled when
/// `used` of `size` tokens are used: none for an unknown size, all of them for a usage
/// beyond the size.
pub(crate) fn usage_fill(used: u64, size: u64, width: u16) -> u16 {
    // Twice the bits of a count, so no count is too large to multiply by the width.
    let filled = (u128::from(used.min(size)) * u128::from(width))
        .checked_div(u128::from(size))
        .unwrap_or(0);
    u16::try_from(filled).unwrap_or(width)
}
