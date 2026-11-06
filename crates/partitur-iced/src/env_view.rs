//! The Env tab: what the selected session runs with.
//!
//! One centered column under the session's breadcrumb: the `AGENTS.md` files that apply to
//! the session (each path copies itself when clicked, with chips for where the file applies
//! and whether it was sent to the agent), the backend and the folder it runs in, and the
//! current model and reasoning effort.

use std::path::PathBuf;

use iced::widget::{Column, button, column, row, scrollable, text};
use iced::{Alignment, Element, Length, Padding, Task};
use partitur_core::{EnvViewModel, InstructionFileView, InstructionScope};

use crate::app::Message;
use crate::layout;
use crate::projects_view::{bold, chip, dimmed, row_style};

/// An interaction on the Env tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvMessage {
    /// The user clicked an instruction file's path: it goes to the clipboard.
    CopyPath(PathBuf),
}

/// The Env tab for the selected session `model`.
///
/// The element owns what it shows, so it does not borrow `model`, which the caller builds
/// afresh for every frame. A model or effort the backend does not offer reads "default".
pub fn view<'a>(model: &EnvViewModel) -> Element<'a, Message> {
    let content = column![
        text(model.breadcrumb.join(" › ")).font(bold()).size(18),
        section("Instructions", instructions(&model.instruction_files)),
        section(
            "Backend",
            column![
                text(model.backend.clone()),
                text(model.cwd.display().to_string()).style(dimmed),
            ]
            .spacing(4),
        ),
        section("Model", text(or_default(model.model.as_ref()))),
        section("Effort", text(or_default(model.effort.as_ref()))),
    ]
    .spacing(24)
    .padding(Padding::ZERO.top(16).bottom(16));
    layout::centered(scrollable(content).width(Length::Fill).into())
}

/// Applies `message` to the Env tab and returns the follow-up work: copying a path puts it
/// on the clipboard.
pub fn update(message: EnvMessage) -> Task<Message> {
    match message {
        EnvMessage::CopyPath(path) => iced::clipboard::write(path.display().to_string()),
    }
}

/// A heading over `body`.
fn section<'a>(title: &'a str, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![text(title).font(bold()), body.into()]
        .spacing(8)
        .into()
}

/// One row per instruction file, or a note that none applies.
fn instructions<'a>(files: &[InstructionFileView]) -> Element<'a, Message> {
    if files.is_empty() {
        return text("No AGENTS.md applies to this session")
            .style(dimmed)
            .into();
    }
    files
        .iter()
        .fold(Column::new().spacing(2), |rows, file| {
            rows.push(instruction(file))
        })
        .into()
}

/// The file's path as a button that copies it, then its scope and state chips.
fn instruction<'a>(file: &InstructionFileView) -> Element<'a, Message> {
    let path = button(text(file.path.display().to_string()))
        .style(row_style)
        .on_press(Message::Env(EnvMessage::CopyPath(file.path.clone())));
    let scope = match file.scope {
        InstructionScope::Global => "global",
        InstructionScope::Repository => "repository",
    };
    let state = match (&file.skipped_reason, file.injected) {
        (Some(reason), _) => format!("skipped: {reason}"),
        (None, true) => "injected".to_owned(),
        (None, false) => "not injected yet".to_owned(),
    };
    row![path, chip(scope), chip(state)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// The current choice's name, or "default" when the backend offers no such option.
fn or_default(name: Option<&String>) -> String {
    name.map_or_else(|| "default".to_owned(), Clone::clone)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;

    use futures::channel::mpsc;
    use iced_test::simulator;
    use partitur_core::testing::{Call, MemoryStore, RecordingBackend, single_claude};
    use partitur_core::{App, BackendEvent, BackendSessionId, Scope};
    use tempfile::TempDir;

    use super::*;
    use crate::app::{State, Tab, boot, update, view};

    /// A project folder with an `AGENTS.md` (holding `global`) and the repository `api` with
    /// its own `AGENTS.md`.
    fn project_dir(global: &[u8]) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        fs::write(dir.path().join("AGENTS.md"), global).unwrap();
        fs::write(dir.path().join("api").join("AGENTS.md"), "api only").unwrap();
        dir
    }

    /// A state on the Env tab whose current session runs in the repository `api` of `dir`
    /// and was created by the backend.
    fn repository_session(dir: &TempDir) -> State {
        let backend = RecordingBackend::new();
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(MemoryStore::new()),
            backends,
            default,
        )
        .unwrap();
        let mut state = boot(app, mpsc::unbounded().1).0;
        let project = state.app.open_project(dir.path()).unwrap();
        let repository = state.app.tree().projects[0].repositories[0].id.clone();
        let session = state
            .app
            .new_session(project, Scope::Repository(repository))
            .unwrap();
        let request = backend
            .calls()
            .iter()
            .find_map(Call::as_new_session)
            .map(|(request, ..)| request)
            .unwrap();
        let created = BackendEvent::SessionCreated {
            request,
            session,
            backend_session: BackendSessionId::new("b-1"),
            config: Vec::new(),
        };
        let _task = update(&mut state, Message::Backend(created));
        state.tab = Tab::Env;
        assert_eq!(state.error, None);
        state
    }

    fn shown(path: &std::path::Path) -> String {
        path.display().to_string()
    }

    #[test]
    fn the_env_view_lists_instruction_files_with_their_state() {
        let dir = project_dir(b"global rules");
        let mut state = repository_session(&dir);
        let global = shown(&dir.path().join("AGENTS.md"));
        let repository = shown(&dir.path().join("api").join("AGENTS.md"));

        let mut ui = simulator(view(&state));
        ui.find(global.as_str()).unwrap();
        ui.find(repository.as_str()).unwrap();
        ui.find("global").unwrap();
        ui.find("repository").unwrap();
        ui.find("not injected yet").unwrap();
        assert!(ui.find("injected").is_err());
        drop(ui);

        state.app.submit_prompt("Hello", Vec::new()).unwrap();

        let mut ui = simulator(view(&state));
        ui.find(global.as_str()).unwrap();
        ui.find(repository.as_str()).unwrap();
        ui.find("injected").unwrap();
        assert!(ui.find("not injected yet").is_err());
        ui.find("claude").unwrap();
        ui.find(shown(&dir.path().join("api")).as_str()).unwrap();
        ui.find("default").unwrap();
    }

    #[test]
    fn an_unreadable_instruction_file_shows_why_it_was_skipped() {
        let dir = project_dir(&[0xff, 0xfe, 0xfd]);
        let state = repository_session(&dir);
        let model = state.app.env_view().unwrap();
        let skipped = model
            .instruction_files
            .iter()
            .find_map(|file| file.skipped_reason.clone())
            .unwrap();

        let mut ui = simulator(view(&state));

        ui.find(format!("skipped: {skipped}").as_str()).unwrap();
        ui.find(shown(&dir.path().join("AGENTS.md")).as_str())
            .unwrap();
        ui.find("not injected yet").unwrap();
    }

    #[test]
    fn clicking_an_instruction_path_copies_it() {
        let dir = project_dir(b"global rules");
        let state = repository_session(&dir);
        let path = dir.path().join("api").join("AGENTS.md");

        let mut ui = simulator(view(&state));
        ui.click(shown(&path).as_str()).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Env(EnvMessage::CopyPath(path))]);
    }

    #[test]
    fn without_instruction_files_the_env_view_says_so() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("api").join(".git")).unwrap();
        let state = repository_session(&dir);

        let mut ui = simulator(view(&state));

        ui.find("No AGENTS.md applies to this session").unwrap();
        ui.find("Model").unwrap();
        ui.find("Effort").unwrap();
    }
}
