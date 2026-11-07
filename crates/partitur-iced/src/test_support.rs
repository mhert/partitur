//! Fixtures the view tests share: two configured backends, and a session whose backend is
//! no longer configured.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use futures::channel::mpsc;
use partitur_core::testing::{MemoryStore, RecordingBackend};
use partitur_core::{App, BackendId, BackendInfo, Scope};
use tempfile::TempDir;

use crate::app::{State, boot};

/// A backend id that follows the naming rule.
pub(crate) fn backend_id(id: &str) -> BackendId {
    BackendId::try_from(id).unwrap()
}

/// The configured backend `id`, started by the command `command`.
pub(crate) fn backend(id: &str, command: &str) -> BackendInfo {
    BackendInfo {
        id: backend_id(id),
        name: id.to_owned(),
        command: command.to_owned(),
    }
}

/// `claude-main` (the default) and `claude-work`, each with its own command.
pub(crate) fn two_backends() -> (Vec<BackendInfo>, BackendId) {
    (
        vec![
            backend("claude-main", "claude-main-acp"),
            backend("claude-work", "claude-work-acp"),
        ],
        backend_id("claude-main"),
    )
}

/// A state whose current session was saved on the backend `gone`, which the configuration
/// (`claude-main` and `claude-work`) no longer names.
pub(crate) fn state_on_removed_backend(dir: &TempDir) -> State {
    let store = MemoryStore::new();
    let (backends, default) = two_backends();
    let before = vec![backends[0].clone(), backend("gone", "gone-acp")];
    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store.clone()),
        before,
        default.clone(),
    )
    .unwrap();
    let project = app.open_project(dir.path()).unwrap();
    let session = app.new_session(project, Scope::Project).unwrap();
    app.switch_backend(session.clone(), backend_id("gone"))
        .unwrap();
    drop(app);

    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store),
        backends,
        default,
    )
    .unwrap();
    app.select_session(session).unwrap();
    boot(app, mpsc::unbounded().1).0
}
