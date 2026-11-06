//! The Projects tab: the open projects and their sessions.
//!
//! For now only the empty state and the button that will open a project folder.

use iced::Element;
use iced::widget::{button, column, text};

use crate::app::Message;
use crate::layout;

/// What the Projects tab remembers between frames.
#[derive(Debug, Default)]
pub struct ProjectsState {}

/// An interaction on the Projects tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectsMessage {
    /// The user asked to open a project folder.
    OpenFolder,
}

/// The Projects tab.
pub fn view(_state: &ProjectsState) -> Element<'_, Message> {
    let open =
        button("open project folder").on_press(Message::Projects(ProjectsMessage::OpenFolder));
    layout::centered(column![text("No projects open"), open].spacing(12).into())
}
