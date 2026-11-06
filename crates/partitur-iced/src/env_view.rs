//! The Env tab: what the selected session runs with.
//!
//! For now only the session's breadcrumb.

use iced::Element;
use partitur_core::EnvViewModel;

use crate::app::Message;
use crate::layout;

/// An interaction on the Env tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvMessage {}

/// The Env tab for the selected session `model`.
pub fn view<'a>(model: &EnvViewModel) -> Element<'a, Message> {
    layout::empty(model.breadcrumb.join(" › "))
}
