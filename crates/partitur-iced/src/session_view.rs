//! The Session tab: the selected session's transcript and prompt.
//!
//! For now only the session's breadcrumb.

use iced::Element;
use partitur_core::SessionViewModel;

use crate::app::Message;
use crate::layout;

/// What the Session tab remembers between frames.
#[derive(Debug, Default)]
pub struct SessionState {}

/// An interaction on the Session tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMessage {}

/// The Session tab for the selected session `model`.
pub fn view<'a>(model: &SessionViewModel<'_>, _state: &'a SessionState) -> Element<'a, Message> {
    layout::empty(model.breadcrumb.join(" › "))
}
